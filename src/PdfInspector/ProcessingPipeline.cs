// Ported from reference/src/lib.rs
using System.Diagnostics;
using PdfInspector.Detector;
using PdfInspector.Extractor;
using PdfInspector.Markdown;
using PdfInspector.Pdf;
using PdfInspector.Quality;
using PdfInspector.Structure;
using PdfInspector.Tables;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector;

/// <summary>
/// The extract-and-convert half of the processing pipeline, run once detection
/// has decided the document is worth reading.
/// </summary>
internal sealed class ProcessingPipeline(
    PdfDocument doc,
    PdfOptions options,
    PdfType pdfType,
    uint pageCount,
    List<uint> pagesNeedingOcr)
{
    private const string Module = "lib";

    /// <summary>Extracts the document's text, converts it, and assembles the result.</summary>
    public PdfProcessResult Run(
        PdfTypeResult detection,
        SortedDictionary<uint, List<string>> detectionOcrReasons,
        Stopwatch timer)
    {
        var confidence = detection.Confidence;
        var extracted = Extract();
        var (structRoles, structTables) = ReadStructureTree();

        string? markdown = null;
        var layout = new LayoutComplexity();
        var hasEncodingIssues = false;
        var gidPages = new HashSet<uint>();
        var textQualityPages = new List<uint>();
        var textQualityReasons = new SortedDictionary<uint, List<string>>();

        if (extracted is not null)
        {
            var stageReasons = new SortedDictionary<uint, List<string>>();
            var (items, rects, lines) = SuppressGarbagePages(extracted, stageReasons);

            var textQuality = TextQuality.AnalyzeTextQuality(items);
            MergeOcrReasons(stageReasons, textQuality.ReasonsByPage);
            layout = LayoutAnalysis.ComputeLayoutComplexity(items, rects, lines);

            markdown = options.Mode == ProcessMode.Analyze
                ? null
                : MarkdownConverter.ToMarkdownFromItemsWithRectsAndLines(
                    items, options.Markdown, rects, lines, extracted.PageThresholds, structRoles, structTables);

            hasEncodingIssues = stageReasons.Count > 0
                || textQuality.HasEncodingIssues
                || (markdown is not null && TextQuality.DetectEncodingIssues(markdown));

            gidPages = extracted.GidEncodedPages;
            textQualityPages = textQuality.PagesNeedingOcr;
            textQualityReasons = stageReasons;
        }

        // Predominantly garbage text on an image-backed document means the text
        // layer came from a bad OCR pass, so callers should run proper OCR.
        if (pdfType == PdfType.Mixed && markdown is not null && TextQuality.IsGarbageText(markdown))
        {
            pdfType = PdfType.Scanned;
            markdown = null;
            confidence = 0.95f;
        }

        // Garbage text on a text-based document means undecodable fonts, such as
        // Identity-H without a ToUnicode CMap for a non-Latin script. The useless
        // markdown is dropped and every page flagged.
        var forceOcrAll = false;
        if (pdfType == PdfType.TextBased && markdown is not null && TextQuality.IsGarbageText(markdown))
        {
            Log.Debug(Module, "TextBased PDF has garbage text — flagging all pages for OCR");
            markdown = null;
            hasEncodingIssues = true;
            forceOcrAll = true;
        }

        var allGid = gidPages.Count > 0 && gidPages.Count >= pageCount;

        if (forceOcrAll)
        {
            pagesNeedingOcr = [.. Enumerable.Range(1, (int)pageCount).Select(p => (uint)p)];
        }

        if (gidPages.Count > 0)
        {
            Log.Debug(Module, () =>
                $"pages with gid-encoded fonts (need OCR): [{string.Join(',', gidPages.Order())}]");
            AddPages(pagesNeedingOcr, gidPages);
        }

        if (textQualityPages.Count > 0)
        {
            Log.Debug(Module, () =>
                $"pages with OCR reason {OcrReason.SuspectedGarbledText} (need OCR): " +
                $"[{string.Join(',', textQualityPages)}]");
            AddPages(pagesNeedingOcr, textQualityPages);
        }

        // Sparse extraction: when a text-based PDF yields very few characters per
        // page, the text is probably embedded in images or forms that need OCR.
        // Only meaningful once markdown was actually generated.
        if (pdfType == PdfType.TextBased
            && pageCount > 0
            && pagesNeedingOcr.Count == 0
            && markdown is not null)
        {
            // The reference measures the markdown in UTF-8 bytes, so a non-Latin
            // document clears this floor sooner than its character count implies.
            var mdLen = Text.TextUtils.ByteLength(markdown);
            var charsPerPage = mdLen / (float)pageCount;
            if (charsPerPage < 50.0f && mdLen < 500)
            {
                Log.Debug(Module, () =>
                    $"sparse extraction: {charsPerPage:F0} chars/page — recommending OCR for all {pageCount} pages");
                pagesNeedingOcr = [.. Enumerable.Range(1, (int)pageCount).Select(p => (uint)p)];
            }
        }

        if (allGid)
        {
            Log.Debug(Module, () =>
                $"all {pageCount} pages have gid-encoded fonts — suppressing markdown output");
            markdown = null;
        }

        // Detector reasons merge with the markdown stage's garbled detection,
        // deduped per page.
        MergeOcrReasons(detectionOcrReasons, textQualityReasons);

        return new PdfProcessResult
        {
            PdfType = pdfType,
            Markdown = markdown,
            PageCount = pageCount,
            ProcessingTimeMs = timer.ElapsedMilliseconds,
            PagesNeedingOcr = pagesNeedingOcr,
            OcrReasonsByPage = PdfProcessor.ToPageOcrReasons(detectionOcrReasons),
            Title = detection.Title,
            Confidence = confidence,
            Layout = layout,
            HasEncodingIssues = hasEncodingIssues,
        };
    }

    /// <summary>
    /// Extracts positioned text. A mixed or template PDF whose normal extraction
    /// yields garbage is retried with invisible text included, which unlocks an
    /// OCR text layer hidden behind a scanned image.
    /// </summary>
    private DocumentExtraction? Extract()
    {
        var fontCMaps = FontCMaps.FromDocument(doc);

        DocumentExtraction? result;
        try
        {
            result = TextExtractor.ExtractPositionedText(doc, fontCMaps, options.PageFilter);
        }
        catch (Exception) when (pdfType == PdfType.Mixed)
        {
            // Normal extraction failed on a mixed PDF, so try the invisible layer.
            return TryExtractInvisible(fontCMaps);
        }

        if (pdfType != PdfType.Mixed)
        {
            return result;
        }

        var sample = string.Concat(result.Items.Take(200).Select(i => i.Text));
        return TextQuality.IsGarbageText(sample) || sample.Trim().Length == 0
            ? TryExtractInvisible(fontCMaps) ?? result
            : result;
    }

    /// <summary>Extracts with invisible text included, tolerating failure.</summary>
    private DocumentExtraction? TryExtractInvisible(FontCMaps fontCMaps)
    {
        try
        {
            return TextExtractor.ExtractPositionedTextIncludeInvisible(doc, fontCMaps, options.PageFilter);
        }
        catch (Exception)
        {
            // Extraction failure is non-fatal for a mixed PDF.
            return null;
        }
    }

    /// <summary>Reads the tagged structure tree, when the document has one.</summary>
    private (Dictionary<uint, Dictionary<long, StructRole>>? Roles, List<StructTable> Tables) ReadStructureTree()
    {
        var tree = StructTree.FromDocument(doc);
        if (tree is null)
        {
            return (null, []);
        }

        var pageIds = doc.PageIds;
        var roles = tree.McidToRoles(pageIds);
        var tables = tree.ExtractTables(pageIds);

        if (roles.Count > 0)
        {
            Log.Debug(Module, () =>
                $"structure tree: {roles.Count} pages with MCID roles, {tree.McidCount} total MCIDs, " +
                $"{tables.Count} tagged tables");
        }

        return (roles.Count == 0 ? null : roles, tables);
    }

    /// <summary>
    /// Strips items from pages whose CID-as-Unicode passthrough produced garbage.
    /// This only applies to a text-based PDF: on a mixed one the OCR flags come
    /// from template images rather than font encoding.
    /// </summary>
    private (List<TextItem> Items, List<PdfRect> Rects, List<PdfLine> Lines) SuppressGarbagePages(
        DocumentExtraction extracted,
        SortedDictionary<uint, List<string>> ocrReasons)
    {
        var items = extracted.Items;
        var rects = extracted.Rects;
        var lines = extracted.Lines;

        if (pagesNeedingOcr.Count == 0 || pdfType != PdfType.TextBased)
        {
            return (items, rects, lines);
        }

        var garbagePages = new HashSet<uint>();
        foreach (var pg in pagesNeedingOcr)
        {
            var pageText = string.Concat(items.Where(i => i.Page == pg).Select(i => i.Text));
            if (TextQuality.IsCidGarbage(pageText))
            {
                garbagePages.Add(pg);
            }
        }

        if (garbagePages.Count == 0)
        {
            return (items, rects, lines);
        }

        Log.Debug(Module, () =>
            $"suppressing garbage text from OCR-flagged pages: [{string.Join(',', garbagePages.Order())}]");

        foreach (var page in garbagePages)
        {
            AddOcrReason(ocrReasons, page, OcrReason.SuspectedGarbledText);
        }

        return (
            items.Where(i => !garbagePages.Contains(i.Page)).ToList(),
            rects.Where(r => !garbagePages.Contains(r.Page)).ToList(),
            lines.Where(l => !garbagePages.Contains(l.Page)).ToList());
    }

    /// <summary>Records an OCR reason for a page, without duplicating it.</summary>
    internal static void AddOcrReason(
        SortedDictionary<uint, List<string>> reasonsByPage,
        uint page,
        string reason)
    {
        if (!reasonsByPage.TryGetValue(page, out var reasons))
        {
            reasons = [];
            reasonsByPage[page] = reasons;
        }

        if (!reasons.Contains(reason))
        {
            reasons.Add(reason);
        }
    }

    /// <summary>Merges one reason map into another, deduping per page.</summary>
    private static void MergeOcrReasons(
        SortedDictionary<uint, List<string>> reasonsByPage,
        SortedDictionary<uint, List<string>> extra)
    {
        foreach (var (page, reasons) in extra)
        {
            foreach (var reason in reasons)
            {
                AddOcrReason(reasonsByPage, page, reason);
            }
        }
    }

    /// <summary>Adds pages to the OCR list, keeping it sorted and free of duplicates.</summary>
    private static void AddPages(List<uint> pagesNeedingOcr, IEnumerable<uint> pages)
    {
        foreach (var page in pages)
        {
            if (!pagesNeedingOcr.Contains(page))
            {
                pagesNeedingOcr.Add(page);
            }
        }

        pagesNeedingOcr.Sort();
    }
}

/// <summary>Layout complexity analysis: which pages carry tables or columns.</summary>
internal static class LayoutAnalysis
{
    /// <summary>
    /// Computes the document's layout complexity by running the table detectors
    /// per page, with side-by-side band splitting, then checking each page for
    /// multiple columns.
    /// </summary>
    public static LayoutComplexity ComputeLayoutComplexity(
        List<TextItem> items,
        List<PdfRect> rects,
        List<PdfLine> lines)
    {
        var seenPages = items.Select(i => i.Page).Distinct().OrderBy(p => p).ToList();
        var baseSize = Analysis.CalculateFontStatsFromItems(items).MostCommonSize;

        var pagesWithTables = new List<uint>();

        foreach (var page in seenPages)
        {
            var pageItems = items.Where(i => i.Page == page).ToList();
            var bands = PageSplits.SplitSideBySide(pageItems);

            // With no split, a single sentinel band covers the whole page.
            var bandRanges = bands.Count == 0 ? [new XBand(float.MinValue, float.MaxValue)] : bands;

            var foundTable = false;
            foreach (var (xLo, xHi) in bandRanges)
            {
                const float Margin = 2.0f;
                var wholePage = xLo == float.MinValue;

                var bandItems = wholePage
                    ? pageItems
                    : pageItems.Where(item => item.X >= xLo - Margin && item.X < xHi + Margin).ToList();

                var bandRects = wholePage
                    ? rects.Where(r => r.Page == page).ToList()
                    : PageSplits.FilterRectsToBand(rects, page, xLo, xHi);

                var bandLines = wholePage
                    ? lines.Where(l => l.Page == page).ToList()
                    : PageSplits.FilterLinesToBand(lines, page, xLo, xHi);

                // A contents page routes through the table detector but renders as a
                // flat list. It is not a table in any user-facing sense, so it must
                // not count here — doing so would also trip the table-page guard in
                // column detection below.
                static bool HasDataTable(List<Table> tables) => tables.Any(t => t.Kind == TableKind.Data);

                var (rectTables, _) = RectTables.DetectTablesFromRects(bandItems, bandRects, page);
                if (HasDataTable(rectTables))
                {
                    foundTable = true;
                    break;
                }

                if (HasDataTable(LineDetector.DetectTablesFromLines(bandItems, bandLines, page)))
                {
                    foundTable = true;
                    break;
                }

                if (HasDataTable(HeuristicDetector.DetectTables(bandItems, baseSize, false)))
                {
                    foundTable = true;
                    break;
                }
            }

            if (foundTable)
            {
                pagesWithTables.Add(page);
            }
        }

        var pagesWithColumns = seenPages
            .Where(page => Columns.DetectColumns(items, page, pagesWithTables.Contains(page)).Count >= 2)
            .ToList();

        return new LayoutComplexity
        {
            IsComplex = pagesWithTables.Count > 0 || pagesWithColumns.Count > 0,
            PagesWithTables = pagesWithTables,
            PagesWithColumns = pagesWithColumns,
        };
    }
}
