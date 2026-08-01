// Ported from reference/src/lib.rs
using PdfInspector.Extractor;
using PdfInspector.Pdf;
using PdfInspector.Quality;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Regions;

/// <summary>One region's extracted text and whether it should be trusted.</summary>
public sealed class RegionText
{
    /// <summary>The extracted text; empty when the region holds no text items.</summary>
    public required string Text { get; init; }

    /// <summary>
    /// True when the text should not be trusted and OCR should run instead:
    /// the region is empty, the page uses glyph-id-encoded fonts, or the
    /// extracted text fails the garbage and encoding checks.
    /// </summary>
    public required bool NeedsOcr { get; init; }

    /// <summary>A machine-readable OCR reason, when the cause is known.</summary>
    public string? OcrReason { get; init; }
}

/// <summary>The region results for one page.</summary>
public sealed class PageRegionResult
{
    /// <summary>The 0-indexed page number.</summary>
    public required uint Page { get; init; }

    /// <summary>Per-region results, parallel to the input regions.</summary>
    public List<RegionText> Regions { get; init; } = [];
}

/// <summary>The per-page extraction state the region APIs share.</summary>
internal sealed class RegionPageCache
{
    public Dictionary<uint, List<TextItem>> ItemsByPage { get; } = [];

    public Dictionary<uint, List<PdfRect>> RectsByPage { get; } = [];

    public Dictionary<uint, List<PdfLine>> LinesByPage { get; } = [];

    public Dictionary<uint, float> PageHeights { get; } = [];

    public HashSet<uint> GidPages { get; } = [];

    public Dictionary<uint, float> PageThresholds { get; } = [];

    public HashSet<uint> RotatedPages { get; } = [];

    public float HeightOf(uint page) => PageHeights.GetValueOrDefault(page, 792.0f);

    public float ThresholdOf(uint page) => PageThresholds.GetValueOrDefault(page, 0.10f);

    public RegionCoordSpace CoordSpaceOf(uint page) => RotatedPages.Contains(page)
        ? RegionCoordSpace.Rotated90Ccw
        : RegionCoordSpace.Standard;

    /// <summary>
    /// Extracts every needed page once, in the fast mode that skips TrueType
    /// fallback parsing. Fonts that cannot be decoded from ToUnicode alone
    /// produce empty or garbage text, which trips the needs-OCR path — the
    /// intended fallback for these regions.
    /// </summary>
    public static RegionPageCache Build(PdfDocument doc, HashSet<uint> neededPages)
    {
        var cache = new RegionPageCache();
        var fontCMaps = FontCMaps.FromDocumentPagesFast(doc, neededPages);
        var styleCache = new FontStyleCache();

        for (var i = 0; i < doc.PageCount; i++)
        {
            var pageNum = (uint)(i + 1);
            if (!neededPages.Contains(pageNum))
            {
                continue;
            }

            var page = doc.GetPage(i + 1);
            if (page is null)
            {
                continue;
            }

            cache.PageHeights[pageNum] = RegionGeometry.GetPageHeight(doc, page) ?? 792.0f;

            var extraction = ContentStreamExtractor.ExtractPageTextItems(
                doc, page, pageNum, fontCMaps, false, styleCache);
            var items = extraction.Items;
            var threshold = TextUtils.FixLetterspacedItems(items);
            if (threshold > 0.10f)
            {
                cache.PageThresholds[pageNum] = threshold;
            }

            if (extraction.HasGidFonts)
            {
                cache.GidPages.Add(pageNum);
            }

            if (extraction.CoordsRotated)
            {
                cache.RotatedPages.Add(pageNum);
            }

            cache.ItemsByPage[pageNum] = items;
            cache.RectsByPage[pageNum] = extraction.Rects;
            cache.LinesByPage[pageNum] = extraction.Lines;
        }

        return cache;
    }
}

/// <summary>
/// Region-scoped extraction, for hybrid OCR pipelines: a layout model detects
/// regions in a rendered page image, and these entry points pull the PDF text
/// that falls inside each one — avoiding GPU OCR for text-based pages.
/// </summary>
public static class RegionExtraction
{
    /// <summary>
    /// Extracts text inside bounding-box regions from an in-memory PDF.
    /// </summary>
    /// <param name="buffer">The PDF file bytes.</param>
    /// <param name="pageRegions">
    /// Per-page regions as (0-indexed page number, list of [x1, y1, x2, y2]).
    /// Coordinates are in PDF points with a top-left origin, matching typical
    /// layout-model output after coordinate conversion.
    /// </param>
    /// <returns>One result per entry in <paramref name="pageRegions"/>.</returns>
    public static List<PageRegionResult> ExtractTextInRegionsMem(
        byte[] buffer,
        IReadOnlyList<(uint Page, IReadOnlyList<float[]> Regions)> pageRegions)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = PdfProcessor.LoadDocumentOrThrow(buffer, null);

        var neededPages = pageRegions.Select(pr => pr.Page + 1).ToHashSet();
        var cache = RegionPageCache.Build(doc, neededPages);

        var results = new List<PageRegionResult>(pageRegions.Count);

        foreach (var (page0Idx, regions) in pageRegions)
        {
            var page1Idx = page0Idx + 1;
            var items = cache.ItemsByPage.GetValueOrDefault(page1Idx);
            var pageH = cache.HeightOf(page1Idx);
            var adaptiveThreshold = cache.ThresholdOf(page1Idx);
            var coords = cache.CoordSpaceOf(page1Idx);

            var pageResults = new List<RegionText>(regions.Count);

            // Exclusive item-to-region assignment. Overlapping layout regions
            // used to pull shared items into every region they touched (the
            // 1.5pt inclusion margin makes borders generous), duplicating whole
            // lines in the final markdown — and downstream duplicate handling
            // sometimes dropped the variant holding a sentence tail, turning
            // duplication into content loss. Each item now belongs to the
            // single region with the largest overlap area; items are
            // partitioned, never suppressed, so nothing can vanish.
            var allBounds = regions
                .Select(rect => RegionGeometry.Bounds(rect[0], rect[1], rect[2], rect[3], pageH, coords))
                .ToList();

            // One pass over the items: assign each to its best-overlap region
            // and bucket the clone directly. `hadCandidates` marks regions that
            // touched at least one item even if every one was assigned
            // elsewhere.
            var regionItems = new List<TextItem>[regions.Count];
            var hadCandidates = new bool[regions.Count];
            for (var i = 0; i < regions.Count; i++)
            {
                regionItems[i] = [];
            }

            if (items is not null)
            {
                foreach (var item in items)
                {
                    var best = -1;
                    var bestArea = 0.0f;
                    for (var ri = 0; ri < allBounds.Count; ri++)
                    {
                        if (!RegionGeometry.OverlapsItem(item, allBounds[ri]))
                        {
                            continue;
                        }

                        hadCandidates[ri] = true;
                        var area = RegionGeometry.ItemOverlapArea(item, allBounds[ri]);
                        if (area > bestArea)
                        {
                            bestArea = area;
                            best = ri;
                        }
                    }

                    if (best >= 0)
                    {
                        regionItems[best].Add(item.Clone());
                    }
                }
            }

            for (var regionIdx = 0; regionIdx < regions.Count; regionIdx++)
            {
                var matched = regionItems[regionIdx];
                var assignedCount = matched.Count;
                var hasTextQualityIssue = TextQuality.RegionItemsHaveDecodingIssue(matched);
                var text = RegionGeometry.CollectTextFromMatchedItems(matched, adaptiveThreshold);
                var hasCidIssue = TextQuality.IsCidGarbage(text);
                var hasEncodingIssue = TextQuality.DetectEncodingIssues(text);
                var ocrReason = hasTextQualityIssue || hasCidIssue || hasEncodingIssue
                    ? Detector.OcrReason.SuspectedGarbledText
                    : null;

                // Per-region text quality rather than blanket page-level GID
                // rejection: a GID font in a logo elsewhere on the page should
                // not force OCR for clean text regions.
                //
                // A region whose only overlapping items were assigned to a
                // better-overlapping neighbour must not fall back to OCR
                // either — the pixels it would re-read belong to that
                // neighbour, and OCR would reintroduce the duplication
                // exclusive assignment removed. This requires zero items
                // assigned *here*: a region whose own items happen to
                // materialise as empty text keeps its OCR fallback.
                var lostToNeighbor = text.Trim().Length == 0
                    && ocrReason is null
                    && assignedCount == 0
                    && hadCandidates[regionIdx];
                var needsOcr = !lostToNeighbor
                    && (ocrReason is not null || text.Trim().Length == 0 || TextQuality.IsGarbageText(text));

                pageResults.Add(new RegionText
                {
                    Text = text,
                    NeedsOcr = needsOcr,
                    OcrReason = ocrReason,
                });
            }

            results.Add(new PageRegionResult { Page = page0Idx, Regions = pageResults });
        }

        return results;
    }

    /// <summary>
    /// Extracts tables inside bounding-box regions from an in-memory PDF.
    /// </summary>
    /// <remarks>
    /// Like <see cref="ExtractTextInRegionsMem"/>, but runs table detection over
    /// the items in each region and returns markdown pipe tables rather than
    /// flat text. When table structure is found, the text is a pipe table and
    /// needs-OCR is false; when no table is found — too few items, poor
    /// alignment, glyph-id fonts — the text is empty and needs-OCR is true so
    /// the caller can fall back to GPU OCR.
    /// </remarks>
    public static List<PageRegionResult> ExtractTablesInRegionsMem(
        byte[] buffer,
        IReadOnlyList<(uint Page, IReadOnlyList<float[]> Regions)> pageRegions)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = PdfProcessor.LoadDocumentOrThrow(buffer, null);

        var neededPages = pageRegions.Select(pr => pr.Page + 1).ToHashSet();
        var cache = RegionPageCache.Build(doc, neededPages);

        var results = new List<PageRegionResult>(pageRegions.Count);

        foreach (var (page0Idx, regions) in pageRegions)
        {
            var page1Idx = page0Idx + 1;
            var items = cache.ItemsByPage.GetValueOrDefault(page1Idx);
            var pageH = cache.HeightOf(page1Idx);
            var coords = cache.CoordSpaceOf(page1Idx);

            var pageResults = new List<RegionText>(regions.Count);

            foreach (var rect in regions)
            {
                // A page-level GID flag is deliberately not a bail-out here: it
                // means some font on the page uses unresolvable glyph ids, but
                // that font may appear only in a logo or header rather than in
                // the table region. The per-region quality checks reject on the
                // actual extracted content instead, so a clean table is not
                // thrown away because of an unrelated decorative font.
                float rx1 = rect[0], ry1 = rect[1], rx2 = rect[2], ry2 = rect[3];
                var bounds = RegionGeometry.Bounds(rx1, ry1, rx2, ry2, pageH, coords);
                var matched = items is null
                    ? []
                    : items.Where(item => RegionGeometry.OverlapsItem(item, bounds))
                        .Select(item => item.Clone())
                        .ToList();

                if (matched.Count == 0)
                {
                    pageResults.Add(new RegionText { Text = string.Empty, NeedsOcr = true });
                    continue;
                }

                if (TextQuality.RegionItemsHaveDecodingIssue(matched))
                {
                    pageResults.Add(new RegionText
                    {
                        Text = string.Empty,
                        NeedsOcr = true,
                        OcrReason = Detector.OcrReason.SuspectedGarbledText,
                    });
                    continue;
                }

                var candidate = DetectRegionTable(cache, matched, bounds, page1Idx, rx1, ry1, rx2, ry2);
                pageResults.Add(candidate is not null
                    ? new RegionText { Text = candidate.Markdown, NeedsOcr = false }
                    : new RegionText { Text = string.Empty, NeedsOcr = true });
            }

            results.Add(new PageRegionResult { Page = page0Idx, Regions = pageResults });
        }

        return results;
    }

    /// <summary>
    /// Runs the rect, line, heuristic, column and key/value detectors over one
    /// region and returns the best candidate that survives the quality gates.
    /// </summary>
    private static TableCandidate? DetectRegionTable(
        RegionPageCache cache,
        List<TextItem> matched,
        RegionBounds bounds,
        uint page1Idx,
        float rx1,
        float ry1,
        float rx2,
        float ry2)
    {
        // The most common font size in the region, in tenths of a point.
        var baseFontSize = 12.0f;
        if (matched.Count > 0)
        {
            var freq = new Dictionary<int, int>();
            foreach (var item in matched)
            {
                var key = (int)(item.FontSize * 10.0f);
                freq[key] = freq.GetValueOrDefault(key) + 1;
            }

            var bestCount = -1;
            var bestKey = 0;
            foreach (var (key, count) in freq)
            {
                // Rust's `max_by_key` keeps the last maximum on a tie.
                if (count >= bestCount)
                {
                    bestCount = count;
                    bestKey = key;
                }
            }

            baseFontSize = bestKey / 10.0f;
        }

        var regionRects = cache.RectsByPage.GetValueOrDefault(page1Idx) is { } rects
            ? rects.Where(r => RegionGeometry.OverlapsRect(r, bounds)).Select(r => r.Clone()).ToList()
            : [];
        var regionLines = cache.LinesByPage.GetValueOrDefault(page1Idx) is { } lines
            ? lines.Where(l => RegionGeometry.OverlapsLine(l, bounds)).Select(l => l.Clone()).ToList()
            : [];

        // The total length of text the page extractor saw inside this region,
        // used by the captured-fragment guard.
        var regionTextChars = matched.Sum(item => TextUtils.CharCount(item.Text));
        var regionArea = MathF.Max(rx2 - rx1, 0.0f) * MathF.Max(ry2 - ry1, 0.0f);
        var lineRegionHasVerticalRules = TableCandidates.HasVerticalRules(regionLines);

        // Each candidate's markdown is quality-gated by the same needs-OCR
        // checks the heuristic-only path used: if a vector detector produces a
        // partial or garbled table, ignore it and try the next path rather than
        // degrade the output. `layoutAssisted` is true because the layout model
        // already identified this region as a table.
        TableCandidate? Evaluate(TableCandidateSource source, Table t)
        {
            var md = TableFormat.TableToMarkdown(t);
            if (md.Trim().Length == 0)
            {
                return null;
            }

            if (TextQuality.IsGarbageText(md)
                || TextQuality.IsCidGarbage(md)
                || TextQuality.DetectEncodingIssues(md)
                || TableCandidates.LooksLikePartialTableEx(md, true))
            {
                return null;
            }

            // Reject extractions that only captured a small fraction of the
            // text actually in the region. Two recurring shapes: "header-only",
            // where the detector found the column-header band cleanly but
            // missed every data row below; and "sparse", where it returned a
            // couple of fragmentary cells despite many lines of text.
            if (TableCandidates.CapturedOnlyAFragment(md, regionTextChars))
            {
                return null;
            }

            // Text-density floor. When a region has plenty of pixel real estate
            // but very few text items, the extractor likely hit a font-CMap
            // failure; the rendered image still shows the text, so the region
            // should fall back to OCR.
            if (source != TableCandidateSource.KeyValue
                && TableCandidates.RegionTextDensityTooLow(regionTextChars, regionArea)
                && !TableCandidates.MarkdownTableBodyIsDense(md))
            {
                return null;
            }

            var shape = TableCandidates.MarkdownTableShapeOf(md);
            TableCandidateIssue? issue;
            if (source == TableCandidateSource.Line
                && lineRegionHasVerticalRules
                && TableCandidates.LineTableCollapsesTextRows(t, matched, shape))
            {
                issue = TableCandidateIssue.LineRowUndercount;
            }
            else if (TableCandidates.WideTableSparsePrefixUndercount(md))
            {
                issue = TableCandidateIssue.SparseWideUndercount;
            }
            else if (source is not (TableCandidateSource.Line or TableCandidateSource.KeyValue)
                && TableCandidates.TextClusterColumnUndercount(matched, shape))
            {
                issue = TableCandidateIssue.TextColumnUndercount;
            }
            else if (TableCandidates.ProseGridFragmentNeedsOcr(md))
            {
                issue = TableCandidateIssue.ProseGridFragment;
            }
            else
            {
                issue = null;
            }

            return new TableCandidate(md, source, shape, issue);
        }

        TableCandidate? FirstOf(TableCandidateSource source, IEnumerable<Table> tables)
        {
            foreach (var t in tables)
            {
                var candidate = Evaluate(source, t);
                if (candidate is not null)
                {
                    return candidate;
                }
            }

            return null;
        }

        var candidates = new List<TableCandidate>();

        if (regionRects.Count > 0)
        {
            var (rectTables, _) = RectTables.DetectTablesFromRects(matched, regionRects, page1Idx);
            var candidate = FirstOf(TableCandidateSource.Rect, rectTables);
            if (candidate is not null)
            {
                candidates.Add(candidate);
            }
        }

        if (regionLines.Count > 0)
        {
            var lineTables = LineDetector.DetectTablesFromLines(matched, regionLines, page1Idx);
            var candidate = FirstOf(TableCandidateSource.Line, lineTables);
            if (candidate is not null)
            {
                candidates.Add(candidate);
            }
        }

        var detected = HeuristicDetector.DetectTables(matched, baseFontSize, false);
        var heuristicCandidate = FirstOf(TableCandidateSource.Heuristic, detected);
        if (heuristicCandidate is not null)
        {
            candidates.Add(heuristicCandidate);
        }

        if (TableBuilders.TryBuildTableFromColumns(matched, page1Idx) is { } columnTable)
        {
            var candidate = Evaluate(TableCandidateSource.Column, columnTable);
            if (candidate is not null)
            {
                candidates.Add(candidate);
            }
        }

        if (KeyValueTables.TryBuildKeyValueTableFromRows(matched, page1Idx) is { } keyValueTable)
        {
            var candidate = Evaluate(TableCandidateSource.KeyValue, keyValueTable);
            if (candidate is not null)
            {
                candidates.Add(candidate);
            }
        }

        return TableCandidates.SelectTableCandidate(candidates);
    }

    /// <summary>
    /// Collects text items that fall inside a region bbox (top-left origin, PDF
    /// points) and returns them as one string in reading order.
    /// </summary>
    public static string CollectTextInRegion(
        IReadOnlyList<TextItem> items,
        float rx1,
        float ry1,
        float rx2,
        float ry2,
        float pageHeight) =>
        RegionGeometry.CollectTextInRegion(items, rx1, ry1, rx2, ry2, pageHeight);
}
