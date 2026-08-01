using System.Diagnostics;
using System.Globalization;
using PdfInspector.Extractor;
using PdfInspector.Markdown;
using PdfInspector.Pdf;
using PdfInspector.Tables;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Bench;

/// <summary>
/// Times the table detectors in isolation over a document's real geometry.
/// </summary>
/// <remarks>
/// The extract and markdown stages both run table detection — the extractor
/// needs it to tell a table's ruling lines from a text underline — so a stage
/// timing alone cannot say whether the cost is in the detectors or around them.
/// This runs each detector directly over every page's already-extracted items
/// and rects, which separates the two.
/// </remarks>
internal static class DetectorBreakdown
{
    public static void Run(string path, int iterations)
    {
        var bytes = File.ReadAllBytes(path);
        var name = Path.GetFileNameWithoutExtension(path);

        var doc = PdfDocument.Load(bytes);
        var cmaps = FontCMaps.FromDocument(doc);
        var extraction = TextExtractor.ExtractPositionedText(doc, cmaps, null);

        var pages = extraction.Items.Select(i => i.Page)
            .Concat(extraction.Rects.Select(r => r.Page))
            .Distinct()
            .Order()
            .ToList();

        // Bucket by page once; the detectors are called per page and the
        // filtering should not be charged to them.
        var itemsByPage = pages.ToDictionary(p => p, p => extraction.Items.Where(i => i.Page == p).ToList());
        var rectsByPage = pages.ToDictionary(p => p, p => extraction.Rects.Where(r => r.Page == p).ToList());
        var linesByPage = pages.ToDictionary(p => p, p => extraction.Lines.Where(l => l.Page == p).ToList());

        var baseFontSize = Analysis.CalculateFontStatsFromItems(extraction.Items).MostCommonSize;

        var totalRects = extraction.Rects.Count;
        var totalItems = extraction.Items.Count;
        Console.Error.WriteLine(
            $"detector breakdown: {name} — {pages.Count} pages, "
            + $"{totalItems} items, {totalRects} rects, {extraction.Lines.Count} lines");

        Warm();

        var rectMs = Time(() =>
        {
            foreach (var p in pages)
            {
                _ = RectTables.DetectTablesFromRects(itemsByPage[p], rectsByPage[p], p);
            }
        }, iterations);

        var lineMs = Time(() =>
        {
            foreach (var p in pages)
            {
                _ = LineDetector.DetectTablesFromLines(itemsByPage[p], linesByPage[p], p);
            }
        }, iterations);

        var heuristicMs = Time(() =>
        {
            foreach (var p in pages)
            {
                _ = HeuristicDetector.DetectTables(itemsByPage[p], baseFontSize, false);
            }
        }, iterations);

        var err = Console.Error;
        err.WriteLine($"{"detector",-14} {"ms/pass",10}");
        err.WriteLine(string.Create(CultureInfo.InvariantCulture, $"{"rect",-14} {rectMs,10:F2}"));
        err.WriteLine(string.Create(CultureInfo.InvariantCulture, $"{"line",-14} {lineMs,10:F2}"));
        err.WriteLine(string.Create(CultureInfo.InvariantCulture, $"{"heuristic",-14} {heuristicMs,10:F2}"));
        err.WriteLine(string.Create(CultureInfo.InvariantCulture,
            $"{"sum",-14} {rectMs + lineMs + heuristicMs,10:F2}"));

        void Warm()
        {
            for (var i = 0; i < 2; i++)
            {
                foreach (var p in pages)
                {
                    _ = RectTables.DetectTablesFromRects(itemsByPage[p], rectsByPage[p], p);
                    _ = LineDetector.DetectTablesFromLines(itemsByPage[p], linesByPage[p], p);
                    _ = HeuristicDetector.DetectTables(itemsByPage[p], baseFontSize, false);
                }
            }
        }
    }

    private static double Time(Action action, int iterations)
    {
        GC.Collect(2, GCCollectionMode.Forced, blocking: true);
        var sw = Stopwatch.StartNew();
        for (var i = 0; i < iterations; i++)
        {
            action();
        }

        return sw.Elapsed.TotalMilliseconds / iterations;
    }
}
