using System.Globalization;
using System.Runtime.Intrinsics.X86;
using System.Text;

namespace PdfInspector.Bench;

/// <summary>
/// Identifies the machine a run happened on, and measures three fixed kernels
/// that both implementations run identically.
/// </summary>
/// <remarks>
/// Cloud VMs move between host generations and throttle under contention, so a
/// millisecond figure from one session is not comparable with another on its
/// own. The kernels below give every run a speed fingerprint: divide two runs'
/// kernel timings to get the factor the machine changed by, then apply it to
/// the fixture timings before comparing them. The Rust harness measures the
/// same three kernels with the same constants, so the factor also crosses
/// implementations.
/// </remarks>
internal static class MachineProfile
{
    /// <summary>The CPU model, core count and widest vector ISA available.</summary>
    public static string Describe()
    {
        var model = ReadCpuModel() ?? "unknown CPU";
        var cores = Environment.ProcessorCount;
        var isa = WidestIsa();
        var os = Environment.OSVersion.VersionString;
        return $"{model} | {cores} logical cores | {isa} | .NET {Environment.Version} | {os}";
    }

    public static string CpuModel() => ReadCpuModel() ?? "unknown CPU";

    /// <summary>The widest vector instruction set the runtime will actually emit.</summary>
    public static string WidestIsa()
    {
        if (Avx512F.IsSupported)
        {
            return Avx512BW.IsSupported ? "AVX-512F+BW" : "AVX-512F";
        }

        if (Avx2.IsSupported)
        {
            return "AVX2";
        }

        return Sse42.IsSupported ? "SSE4.2" : "scalar";
    }

    private static string? ReadCpuModel()
    {
        try
        {
            foreach (var line in File.ReadLines("/proc/cpuinfo"))
            {
                if (line.StartsWith("model name", StringComparison.Ordinal))
                {
                    var idx = line.IndexOf(':');
                    if (idx >= 0)
                    {
                        return line[(idx + 1)..].Trim();
                    }
                }
            }
        }
        catch (IOException)
        {
            // A machine without procfs still benchmarks; it just goes unnamed.
        }

        return null;
    }

    // ---------------------------------------------------------------------
    // Calibration kernels. Keep these byte-for-byte equivalent to the Rust
    // harness in bench/rust/src/kernels.rs — same constants, same iteration
    // counts, same arithmetic. Changing one without the other silently breaks
    // cross-implementation scaling.
    // ---------------------------------------------------------------------

    public const int IntKernelIterations = 50_000_000;
    public const int FloatKernelIterations = 50_000_000;
    public const int MemKernelBytes = 32 << 20;
    public const int MemKernelPasses = 20;

    /// <summary>
    /// A xorshift64* chain. Every step depends on the previous one, so neither
    /// compiler can vectorise or reorder it — this measures scalar ALU latency
    /// and effective clock, nothing else.
    /// </summary>
    public static ulong IntKernel(int iterations)
    {
        ulong x = 0x2545F4914F6CDD1DUL;
        for (var i = 0; i < iterations; i++)
        {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x *= 0x2545F4914F6CDD1DUL;
        }

        return x;
    }

    /// <summary>
    /// A scalar float multiply-add chain, again strictly sequential. Measures
    /// floating-point latency, which is what the layout and table heuristics
    /// spend their time on.
    /// </summary>
    public static float FloatKernel(int iterations)
    {
        var acc = 1.0f;
        for (var i = 0; i < iterations; i++)
        {
            acc = (acc * 1.0000001f) + 0.000001f;
            if (acc > 1e18f)
            {
                acc = 1.0f;
            }
        }

        return acc;
    }

    /// <summary>
    /// A streaming sum over a buffer far larger than L2, unrolled across eight
    /// independent accumulators.
    /// </summary>
    /// <remarks>
    /// The unrolling is what makes this a memory measurement rather than a
    /// codegen one. A plain accumulate loop leaves each add waiting on the
    /// previous one, so whichever compiler vectorises more aggressively wins by
    /// a factor that has nothing to do with the machine — the first draft of
    /// this kernel read 7.05 GiB/s under Rust and 3.89 GiB/s under .NET on the
    /// same CPU for exactly that reason. Eight independent chains saturate the
    /// load ports either way, so both sides end up bandwidth-bound.
    /// </remarks>
    public static ulong MemKernel(ulong[] buffer, int passes)
    {
        ulong a0 = 0, a1 = 0, a2 = 0, a3 = 0, a4 = 0, a5 = 0, a6 = 0, a7 = 0;
        var length = buffer.Length - (buffer.Length % 8);

        for (var p = 0; p < passes; p++)
        {
            for (var i = 0; i < length; i += 8)
            {
                a0 += buffer[i];
                a1 += buffer[i + 1];
                a2 += buffer[i + 2];
                a3 += buffer[i + 3];
                a4 += buffer[i + 4];
                a5 += buffer[i + 5];
                a6 += buffer[i + 6];
                a7 += buffer[i + 7];
            }
        }

        return a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7;
    }

    /// <summary>Runs the three kernels and reports each one's rate.</summary>
    public static CalibrationResult Calibrate()
    {
        // One short warm pass so the kernels are running tier-1 code before
        // they are timed.
        _ = IntKernel(IntKernelIterations / 50);
        _ = FloatKernel(FloatKernelIterations / 50);

        var buffer = new ulong[MemKernelBytes / sizeof(ulong)];
        for (var i = 0; i < buffer.Length; i++)
        {
            buffer[i] = (ulong)i * 0x9E3779B97F4A7C15UL;
        }

        _ = MemKernel(buffer, 1);

        var sw = System.Diagnostics.Stopwatch.StartNew();
        var intSink = IntKernel(IntKernelIterations);
        var intNs = sw.Elapsed.TotalNanoseconds / IntKernelIterations;

        sw.Restart();
        var floatSink = FloatKernel(FloatKernelIterations);
        var floatNs = sw.Elapsed.TotalNanoseconds / FloatKernelIterations;

        sw.Restart();
        var memSink = MemKernel(buffer, MemKernelPasses);
        var memSeconds = sw.Elapsed.TotalSeconds;
        var memGiB = (double)MemKernelBytes * MemKernelPasses / (1 << 30);

        // Keeping the results alive stops the JIT deleting the kernels wholesale.
        Sink = intSink ^ memSink ^ (ulong)floatSink;

        return new CalibrationResult(intNs, floatNs, memGiB / memSeconds);
    }

    /// <summary>Holds kernel results so nothing is optimised away as dead.</summary>
    public static ulong Sink;

    public static string FormatCalibration(CalibrationResult c) =>
        string.Create(CultureInfo.InvariantCulture,
            $"int {c.IntNsPerOp:F3} ns/op | float {c.FloatNsPerOp:F3} ns/op | mem {c.MemGiBPerSec:F2} GiB/s");
}

/// <summary>The three machine-speed kernels' results.</summary>
internal readonly record struct CalibrationResult(
    double IntNsPerOp,
    double FloatNsPerOp,
    double MemGiBPerSec)
{
    public string ToJson()
    {
        var sb = new StringBuilder();
        sb.Append(CultureInfo.InvariantCulture,
            $"{{\"intNsPerOp\":{IntNsPerOp:F6},\"floatNsPerOp\":{FloatNsPerOp:F6},");
        sb.Append(CultureInfo.InvariantCulture, $"\"memGiBPerSec\":{MemGiBPerSec:F6}}}");
        return sb.ToString();
    }
}
