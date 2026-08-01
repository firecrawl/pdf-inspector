using System.Diagnostics;
using System.Globalization;
using PdfInspector.Detector;
using PdfInspector.Extractor;
using PdfInspector.Markdown;
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;

namespace PdfInspector.Bench;

/// <summary>
/// Splits one extraction into its pipeline stages and reports time and
/// allocation for each, so optimisation work targets what actually costs.
/// </summary>
/// <remarks>
/// This deliberately re-implements the sequence <c>ProcessPdfMem</c> runs
/// rather than instrumenting the library itself: the shipping path stays free
/// of measurement hooks, and the stages here are coarse enough that drift
/// between the two would be obvious in the totals.
/// </remarks>
internal static class PhaseBreakdown
{
    public static void Run(string path, int iterations)
    {
        var bytes = File.ReadAllBytes(path);
        var name = Path.GetFileNameWithoutExtension(path);

        // Warm every stage before measuring any of them.
        for (var i = 0; i < 3; i++)
        {
            _ = PdfProcessor.ProcessPdfMem(bytes);
        }

        var phases = new[] { "parse", "cmaps", "extract", "detect", "markdown" };
        var times = new double[phases.Length];
        var allocs = new long[phases.Length];

        for (var iter = 0; iter < iterations; iter++)
        {
            GC.Collect(2, GCCollectionMode.Forced, blocking: true);
            GC.WaitForPendingFinalizers();

            var sw = Stopwatch.StartNew();
            var a0 = GC.GetTotalAllocatedBytes(precise: true);

            var doc = PdfDocument.Load(bytes);
            Record(0, sw, ref a0);

            var cmaps = FontCMaps.FromDocument(doc);
            Record(1, sw, ref a0);

            var extraction = TextExtractor.ExtractPositionedText(doc, cmaps, null);
            Record(2, sw, ref a0);

            _ = PdfDetector.DetectFromDocument(doc, (uint)doc.PageCount, new DetectionConfig());
            Record(3, sw, ref a0);

            _ = MarkdownConverter.ToMarkdownFromItemsWithRectsAndLines(
                extraction.Items,
                new MarkdownOptions(),
                extraction.Rects,
                extraction.Lines,
                extraction.PageThresholds,
                null,
                []);
            Record(4, sw, ref a0);

            void Record(int phase, Stopwatch stopwatch, ref long allocBefore)
            {
                times[phase] += stopwatch.Elapsed.TotalMilliseconds;
                var now = GC.GetTotalAllocatedBytes(precise: true);
                allocs[phase] += now - allocBefore;
                allocBefore = now;
                stopwatch.Restart();
            }
        }

        var err = Console.Error;
        err.WriteLine($"phase breakdown: {name} ({iterations} iterations)");
        err.WriteLine($"{"phase",-12} {"ms",9} {"share",7} {"alloc MiB",11}");

        var totalMs = times.Sum();
        for (var i = 0; i < phases.Length; i++)
        {
            var ms = times[i] / iterations;
            var share = totalMs > 0 ? times[i] / totalMs * 100.0 : 0.0;
            var mib = allocs[i] / (double)iterations / (1 << 20);
            err.WriteLine(string.Create(CultureInfo.InvariantCulture,
                $"{phases[i],-12} {ms,9:F2} {share,6:F1}% {mib,11:F2}"));
        }

        err.WriteLine(string.Create(CultureInfo.InvariantCulture,
            $"{"total",-12} {totalMs / iterations,9:F2} {100.0,6:F1}% "
            + $"{allocs.Sum() / (double)iterations / (1 << 20),11:F2}"));
    }
}
