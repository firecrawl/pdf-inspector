using System.Diagnostics;
using System.Globalization;
using System.Text;

namespace PdfInspector.Bench;

/// <summary>
/// Steady-state throughput harness for the library.
/// </summary>
/// <remarks>
/// Timing the CLI measures process start, assembly load and JIT far more than
/// it measures the extractor. This runs the library in-process instead: warm
/// up until the hot paths are tier-1 compiled, then time repeated runs and
/// report the distribution. Every run also carries a machine fingerprint (see
/// <see cref="MachineProfile"/>) so numbers taken on different VMs can be
/// scaled into agreement.
/// </remarks>
internal static class Program
{
    private static int Main(string[] args)
    {
        if (args.Contains("--help") || args.Contains("-h"))
        {
            PrintUsage();
            return 0;
        }

        if (Array.IndexOf(args, "--phases") is var phaseIdx && phaseIdx >= 0)
        {
            var target = phaseIdx + 1 < args.Length ? args[phaseIdx + 1] : null;
            if (target is null)
            {
                Console.Error.WriteLine("--phases needs a fixture path");
                return 1;
            }

            PhaseBreakdown.Run(target, IterationsFor(args));
            return 0;
        }

        if (Array.IndexOf(args, "--detectors") is var detIdx && detIdx >= 0)
        {
            var target = detIdx + 1 < args.Length ? args[detIdx + 1] : null;
            if (target is null)
            {
                Console.Error.WriteLine("--detectors needs a fixture path");
                return 1;
            }

            DetectorBreakdown.Run(target, IterationsFor(args));
            return 0;
        }

        var options = BenchOptions.Parse(args);
        var fixtures = ResolveFixtures(options);

        if (fixtures.Count == 0)
        {
            Console.Error.WriteLine($"No fixtures matched. Looked in {options.FixtureDirectory}");
            return 1;
        }

        var err = Console.Error;
        err.WriteLine($"cpu: {MachineProfile.Describe()}");

        var calibration = MachineProfile.Calibrate();
        err.WriteLine($"calibration: {MachineProfile.FormatCalibration(calibration)}");
        err.WriteLine(
            $"harness: warmup>={options.WarmupIterations} iters, "
            + $"measure>={options.MinIterations} iters or {options.BudgetMs} ms per fixture");
        err.WriteLine();

        var results = new List<FixtureResult>(fixtures.Count);

        foreach (var path in fixtures)
        {
            var result = Measure(path, options);
            results.Add(result);
            err.WriteLine(result.ToLine());
        }

        err.WriteLine();
        err.WriteLine(Summarise(results));

        if (options.JsonPath is { } jsonPath)
        {
            File.WriteAllText(jsonPath, BuildJson(calibration, results, options));
            err.WriteLine($"wrote {jsonPath}");
        }

        return 0;
    }

    private static int IterationsFor(string[] args)
    {
        var idx = Array.IndexOf(args, "--iters");
        return idx >= 0 && idx + 1 < args.Length
            && int.TryParse(args[idx + 1], CultureInfo.InvariantCulture, out var n)
            ? n
            : 10;
    }

    private static void PrintUsage()
    {
        var err = Console.Error;
        err.WriteLine("Usage: pdf-inspector-bench [options]");
        err.WriteLine();
        err.WriteLine("  --fixtures <dir>   Directory of .pdf fixtures");
        err.WriteLine("  --filter <substr>  Only fixtures whose name contains this");
        err.WriteLine("  --iters <n>        Minimum timed iterations per fixture (default 5)");
        err.WriteLine("  --warmup <n>       Minimum warmup iterations per fixture (default 3)");
        err.WriteLine("  --budget <ms>      Keep timing a fixture until this budget is spent (default 3000)");
        err.WriteLine("  --json <path>      Write machine-readable results here");
        err.WriteLine("  --phases <pdf>     Per-stage time and allocation for one fixture");
        err.WriteLine("  --detectors <pdf>  Per-detector time over one fixture's geometry");
    }

    private static List<string> ResolveFixtures(BenchOptions options)
    {
        if (!Directory.Exists(options.FixtureDirectory))
        {
            return [];
        }

        return
        [
            .. Directory.GetFiles(options.FixtureDirectory, "*.pdf")
                .Where(f => !Path.GetFileName(f).StartsWith("encrypted-", StringComparison.Ordinal))
                .Where(f => options.Filter is null
                    || Path.GetFileName(f).Contains(options.Filter, StringComparison.OrdinalIgnoreCase))
                .OrderBy(f => f, StringComparer.Ordinal),
        ];
    }

    /// <summary>
    /// Warms a fixture until its code is tier-1, then times it repeatedly.
    /// </summary>
    private static FixtureResult Measure(string path, BenchOptions options)
    {
        var name = Path.GetFileNameWithoutExtension(path);
        var bytes = File.ReadAllBytes(path);

        // Warmup: enough calls to promote the hot methods out of tier-0, or as
        // many as the warmup wall clock allows.
        var (warmups, outputBytes) = WarmUntilStable(bytes, options);

        // Settle the heap so the timed section starts from a known state and a
        // collection triggered by warmup garbage is not charged to it.
        GC.Collect(2, GCCollectionMode.Forced, blocking: true);
        GC.WaitForPendingFinalizers();
        GC.Collect(2, GCCollectionMode.Forced, blocking: true);

        var samples = new List<double>(options.MinIterations * 2);
        var allocBefore = GC.GetTotalAllocatedBytes(precise: true);
        var gen0Before = GC.CollectionCount(0);
        var gen2Before = GC.CollectionCount(2);

        var totalSw = Stopwatch.StartNew();
        do
        {
            var sw = Stopwatch.StartNew();
            outputBytes = RunOnce(bytes);
            samples.Add(sw.Elapsed.TotalMilliseconds);

            if (totalSw.ElapsedMilliseconds > options.BudgetMs)
            {
                break;
            }
        }
        while (samples.Count < options.MinIterations);

        var allocated = GC.GetTotalAllocatedBytes(precise: true) - allocBefore;

        return new FixtureResult
        {
            Name = name,
            InputBytes = bytes.Length,
            OutputBytes = outputBytes,
            Samples = samples,
            AllocatedBytesPerIteration = allocated / samples.Count,
            Gen0Collections = GC.CollectionCount(0) - gen0Before,
            Gen2Collections = GC.CollectionCount(2) - gen2Before,
            WarmupIterations = warmups,
        };
    }

    /// <summary>
    /// Repeats the fixture for a fixed stretch of wall clock, which is long
    /// enough for the code under test to finish tiering up.
    /// </summary>
    /// <remarks>
    /// <para>
    /// A fixed three-pass warmup is badly wrong on .NET, and so is a convergence
    /// rule. Measured directly: td9264 reported 84 ms run alone and 20 ms when
    /// other fixtures had already warmed the shared code. Same work, four times
    /// the number, purely an artefact of the harness — and a rule that stopped
    /// once the timings held steady for five passes still read 110 ms, because
    /// tier-up is a delayed step down rather than a glide.
    /// </para>
    /// <para>
    /// The cause is call counting. A method reaches tier-1 only after enough
    /// invocations, and one extraction calls most of the pipeline's
    /// per-document methods exactly once. Loop bodies escape this through
    /// on-stack replacement, which is why the multi-second fixtures are
    /// insensitive to warmup length, but everything called once per page or once
    /// per document needs real repetition.
    /// </para>
    /// <para>
    /// A fixed pass count does not work either, because how many passes that
    /// takes scales with how small the document is: 300 passes settle td9264,
    /// while <c>wireless_two_col_no_rects</c> — a quarter the size — still reads
    /// 13.4 ms at 100 passes, 4.9 at 400 and 4.0 at 1000. What the two have in
    /// common is the wall clock: both are settled after roughly four seconds of
    /// repetition. So that is the rule, and it needs no cap — a fixture whose
    /// single pass already takes seconds satisfies it after one or two, which is
    /// all such a fixture needs.
    /// </para>
    /// </remarks>
    private static (int Warmups, long OutputBytes) WarmUntilStable(byte[] bytes, BenchOptions options)
    {
        var warmupMs = Math.Max(options.BudgetMs, MinWarmupMs);

        var sw = Stopwatch.StartNew();
        var warmups = 0;
        long outputBytes = 0;

        while (true)
        {
            outputBytes = RunOnce(bytes);
            warmups++;

            if (warmups >= options.WarmupIterations && sw.ElapsedMilliseconds >= warmupMs)
            {
                break;
            }
        }

        return (warmups, outputBytes);
    }

    /// <summary>
    /// How long a fixture is repeated before it is measured. Four seconds is
    /// where the smallest fixture stops improving; five leaves margin.
    /// </summary>
    private const int MinWarmupMs = 5_000;

    /// <summary>
    /// One full extraction. The result is consumed so nothing folds away, and
    /// this is the same call the CLI's <c>--raw</c> path makes.
    /// </summary>
    private static long RunOnce(byte[] bytes)
    {
        var result = PdfProcessor.ProcessPdfMem(bytes);
        return result.Markdown?.Length ?? 0;
    }

    private static string Summarise(List<FixtureResult> results)
    {
        var totalMedian = results.Sum(r => r.Median);
        var totalAlloc = results.Sum(r => (double)r.AllocatedBytesPerIteration);
        return string.Create(CultureInfo.InvariantCulture,
            $"total: {totalMedian:F1} ms median across {results.Count} fixtures, "
            + $"{totalAlloc / (1 << 20):F1} MiB allocated per pass");
    }

    private static string BuildJson(
        CalibrationResult calibration,
        List<FixtureResult> results,
        BenchOptions options)
    {
        var sb = new StringBuilder();
        sb.Append("{\"impl\":\"csharp\",");
        sb.Append(CultureInfo.InvariantCulture, $"\"runtime\":\".NET {Environment.Version}\",");
        sb.Append(CultureInfo.InvariantCulture, $"\"cpu\":{JsonString(MachineProfile.CpuModel())},");
        sb.Append(CultureInfo.InvariantCulture, $"\"cores\":{Environment.ProcessorCount},");
        sb.Append(CultureInfo.InvariantCulture, $"\"isa\":{JsonString(MachineProfile.WidestIsa())},");
        sb.Append(CultureInfo.InvariantCulture, $"\"budgetMs\":{options.BudgetMs},");
        sb.Append(CultureInfo.InvariantCulture, $"\"calibration\":{calibration.ToJson()},");
        sb.Append("\"fixtures\":[");
        for (var i = 0; i < results.Count; i++)
        {
            if (i > 0)
            {
                sb.Append(',');
            }

            sb.Append(results[i].ToJson());
        }

        sb.Append("]}");
        return sb.ToString();
    }

    internal static string JsonString(string value)
    {
        var sb = new StringBuilder(value.Length + 2);
        sb.Append('"');
        foreach (var ch in value)
        {
            switch (ch)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\r': sb.Append("\\r"); break;
                case '\t': sb.Append("\\t"); break;
                default:
                    if (ch < 0x20)
                    {
                        sb.Append(CultureInfo.InvariantCulture, $"\\u{(int)ch:x4}");
                    }
                    else
                    {
                        sb.Append(ch);
                    }

                    break;
            }
        }

        sb.Append('"');
        return sb.ToString();
    }
}

/// <summary>Command-line options for the harness.</summary>
internal sealed class BenchOptions
{
    public string FixtureDirectory { get; init; } = "reference/tests/fixtures";

    public string? Filter { get; init; }

    public int MinIterations { get; init; } = 5;

    public int WarmupIterations { get; init; } = 3;

    public int BudgetMs { get; init; } = 3000;

    public string? JsonPath { get; init; }

    public static BenchOptions Parse(string[] args)
    {
        string? Value(string name)
        {
            var idx = Array.IndexOf(args, name);
            return idx >= 0 && idx + 1 < args.Length ? args[idx + 1] : null;
        }

        int IntValue(string name, int fallback) =>
            Value(name) is { } v && int.TryParse(v, CultureInfo.InvariantCulture, out var parsed)
                ? parsed
                : fallback;

        return new BenchOptions
        {
            FixtureDirectory = Value("--fixtures") ?? "reference/tests/fixtures",
            Filter = Value("--filter"),
            MinIterations = IntValue("--iters", 5),
            WarmupIterations = IntValue("--warmup", 3),
            BudgetMs = IntValue("--budget", 3000),
            JsonPath = Value("--json"),
        };
    }
}

/// <summary>One fixture's timing distribution.</summary>
internal sealed class FixtureResult
{
    public required string Name { get; init; }

    public required int InputBytes { get; init; }

    public required long OutputBytes { get; init; }

    public required List<double> Samples { get; init; }

    public required long AllocatedBytesPerIteration { get; init; }

    public required int Gen0Collections { get; init; }

    public required int Gen2Collections { get; init; }

    public required int WarmupIterations { get; init; }

    /// <summary>
    /// The headline figure. The median resists the occasional stall a shared
    /// VM inflicts far better than the mean does.
    /// </summary>
    public double Median => Percentile(0.50);

    public double Min => Samples.Min();

    public double Mean => Samples.Average();

    public double P95 => Percentile(0.95);

    public double StdDev
    {
        get
        {
            if (Samples.Count < 2)
            {
                return 0.0;
            }

            var mean = Mean;
            var sumSq = Samples.Sum(s => (s - mean) * (s - mean));
            return Math.Sqrt(sumSq / (Samples.Count - 1));
        }
    }

    private double Percentile(double q)
    {
        var sorted = Samples.Order().ToList();
        if (sorted.Count == 0)
        {
            return 0.0;
        }

        var idx = (int)Math.Round(q * (sorted.Count - 1), MidpointRounding.AwayFromZero);
        return sorted[Math.Clamp(idx, 0, sorted.Count - 1)];
    }

    public string ToLine() => string.Create(CultureInfo.InvariantCulture,
        $"{Name,-40} {Median,9:F2} ms  (min {Min,8:F2}  p95 {P95,8:F2}  sd {StdDev,6:F2})  "
        + $"n={Samples.Count,-3} w={WarmupIterations,-4} alloc {AllocatedBytesPerIteration / 1048576.0,7:F1} MiB  "
        + $"gc {Gen0Collections}/{Gen2Collections}");

    public string ToJson() => string.Create(CultureInfo.InvariantCulture,
        $"{{\"name\":{Program.JsonString(Name)},\"inputBytes\":{InputBytes},"
        + $"\"outputBytes\":{OutputBytes},\"iterations\":{Samples.Count},"
        + $"\"warmups\":{WarmupIterations},"
        + $"\"medianMs\":{Median:F4},\"minMs\":{Min:F4},\"meanMs\":{Mean:F4},"
        + $"\"p95Ms\":{P95:F4},\"stdDevMs\":{StdDev:F4},"
        + $"\"allocBytesPerIter\":{AllocatedBytesPerIteration},"
        + $"\"gen0\":{Gen0Collections},\"gen2\":{Gen2Collections}}}");
}
