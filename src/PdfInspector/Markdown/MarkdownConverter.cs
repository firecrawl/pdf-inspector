// Ported from reference/src/markdown/mod.rs
using System.Text;
using PdfInspector.Extractor;
using PdfInspector.Structure;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>
/// Markdown conversion with structure detection: headers by font size, lists,
/// code blocks and paragraphs, with tables and images interleaved at their
/// positions.
/// </summary>
public static class MarkdownConverter
{
    private const string Module = "markdown";

    /// <summary>A band's items, their page-local indices, and the rects and lines scoped to it.</summary>
    private sealed record BandSpec(
        List<TextItem> Items,
        List<int> IndexMap,
        List<PdfRect> Rects,
        List<PdfLine> Lines);

    /// <summary>Converts plain text to markdown, detecting lists and code blocks only.</summary>
    public static string ToMarkdown(string text, MarkdownOptions options)
    {
        var output = new StringBuilder();
        var inCodeBlock = false;

        foreach (var rawLine in SplitLines(text))
        {
            var trimmed = rawLine.Trim();

            if (trimmed.Length == 0)
            {
                // The reference tracks an in-list flag here, but never reads it in a
                // way that changes the output, so it is omitted.
                if (inCodeBlock)
                {
                    output.Append("```\n");
                    inCodeBlock = false;
                }

                output.Append('\n');
                continue;
            }

            if (options.DetectLists && Classify.IsListItem(trimmed))
            {
                output.Append(Classify.FormatListItem(trimmed)).Append('\n');
                continue;
            }

            if (options.DetectCode && Classify.IsCodeLike(trimmed))
            {
                if (!inCodeBlock)
                {
                    output.Append("```\n");
                    inCodeBlock = true;
                }

                output.Append(trimmed).Append('\n');
                continue;
            }

            if (inCodeBlock)
            {
                output.Append("```\n");
                inCodeBlock = false;
            }

            output.Append(trimmed).Append('\n');
        }

        if (inCodeBlock)
        {
            output.Append("```\n");
        }

        return output.ToString();
    }

    /// <summary>Splits text into lines, dropping the empty tail a trailing newline leaves.</summary>
    private static IEnumerable<string> SplitLines(string text)
    {
        var split = text.Split('\n');
        var count = split.Length > 0 && split[^1].Length == 0 ? split.Length - 1 : split.Length;
        for (var i = 0; i < count; i++)
        {
            yield return split[i].TrimEnd('\r');
        }
    }

    /// <summary>Converts positioned text items to markdown with structure detection.</summary>
    public static string ToMarkdownFromItems(List<TextItem> items, MarkdownOptions options) =>
        ToMarkdownFromItemsWithRects(items, options, []);

    /// <summary>Converts already-grouped lines to markdown.</summary>
    public static string ToMarkdownFromLines(List<TextLine> lines, MarkdownOptions options) =>
        Convert.ToMarkdownFromLines(lines, options);

    /// <summary>Converts positioned text items to markdown, using rects for table detection.</summary>
    public static string ToMarkdownFromItemsWithRects(
        List<TextItem> items,
        MarkdownOptions options,
        IReadOnlyList<PdfRect> rects) =>
        ToMarkdownFromItemsWithRectsAndLines(items, options, rects, [], new Dictionary<uint, float>(), null, []);

    /// <summary>
    /// Converts positioned text items to markdown, using rectangles and line
    /// segments for table detection. Line-based detection runs first as the
    /// strongest structural evidence, then rect-based, then the heuristic fallback
    /// on whatever items remain unclaimed.
    /// </summary>
    internal static string ToMarkdownFromItemsWithRectsAndLines(
        List<TextItem> items,
        MarkdownOptions options,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<PdfLine> pdfLines,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles,
        IReadOnlyList<StructTable> structTables)
    {
        if (items.Count == 0)
        {
            return string.Empty;
        }

        var images = new List<TextItem>();
        var pageImageRegions = new Dictionary<uint, List<ImageRegion>>();
        var textItems = new List<TextItem>();

        foreach (var item in items)
        {
            switch (item.Kind)
            {
                case ItemKind.Image:
                    if (!pageImageRegions.TryGetValue(item.Page, out var regions))
                    {
                        regions = [];
                        pageImageRegions[item.Page] = regions;
                    }

                    regions.Add(new ImageRegion(item.X, item.Y, item.X + item.Width, item.Y + item.Height));
                    if (options.IncludeImages)
                    {
                        images.Add(item);
                    }

                    break;

                case ItemKind.Link:
                    // Link items are separated out and, as in the reference, never
                    // reach the output: their URLs already ride along on the text
                    // items they annotate.
                    break;

                default:
                    textItems.Add(item);
                    break;
            }
        }

        var fontStats = Analysis.CalculateFontStatsFromItems(textItems);
        var baseSize = options.BaseFontSize ?? fontStats.MostCommonSize;

        var tableItems = new HashSet<int>();
        var pageTables = new Dictionary<uint, List<PositionedMarkdown>>();

        // Group items by page once, keeping their global indices.
        var pageGroups = new Dictionary<uint, List<(int GlobalIndex, TextItem Item)>>();
        for (var globalIdx = 0; globalIdx < textItems.Count; globalIdx++)
        {
            var item = textItems[globalIdx];
            if (!pageGroups.TryGetValue(item.Page, out var group))
            {
                group = [];
                pageGroups[item.Page] = group;
            }

            group.Add((globalIdx, item));
        }

        // Bucketed once. Every per-page consumer below filters the document's
        // rects and lines down to its own page, so handing each the whole list
        // walks all of them once per page — quadratic on a document with many
        // pages and many drawn rects.
        var rectsByPage = GroupByPage(rects, r => r.Page);
        var linesByPage = GroupByPage(pdfLines, l => l.Page);

        // A page's chart regions must not steer column detection during line
        // grouping: their text fills the gutter and fuses two-column lines.
        var pageChartMap = new Dictionary<uint, List<ChartRegion>>();
        foreach (var page in pageGroups.Keys)
        {
            var pageItemsRef = pageGroups[page].Select(p => p.Item).ToList();
            var regions = RectTables.DetectChartRegions(pageItemsRef, PageRects(rectsByPage, page), page)
                .Select(r => new ChartRegion(r.Left, r.Bottom, r.Right, r.Top))
                .ToList();
            if (regions.Count > 0)
            {
                pageChartMap[page] = regions;
            }
        }

        var pages = pageGroups.Keys.OrderBy(p => p).ToList();
        var pageCount = (pages.Count > 0 ? pages[^1] : 0u) + 1;

        var pageBandSplits = new Dictionary<uint, List<XBand>>();

        // A chart page with two prose columns treats the chart's vertical span as a
        // full-width separator and reads each surrounding prose zone by column.
        var pageChartProseSplits = new Dictionary<uint, float>();
        var pageChartProseOrders = new Dictionary<uint, ChartProseOrder>();

        foreach (var page in pages)
        {
            var group = pageGroups[page];
            var pageItems = group.Select(p => p.Item).ToList();

            // Bar charts drawn as filled rects read as cell rects or aligned text and
            // get gridded into phantom tables, so their items are excluded from every
            // detector below and flow through as plain text.
            var chartRegions = pageChartMap.GetValueOrDefault(page, []);
            bool InChart(TextItem item) => ChartRegions.ItemIsInChartRegion(item, chartRegions);
            var pageLayoutItems = ChartRegions.ItemsOutsideChartRegions(pageItems, chartRegions);

            // Columns are detected on chart-free text. Chart labels and values often
            // fill the prose gutter, hiding real columns and letting body text reach
            // heuristic detection as one page-wide region.
            var detectedColumns = Columns.DetectColumns(pageLayoutItems, page, false).Count >= 2;

            if (chartRegions.Count > 0)
            {
                Log.Debug(Module, () =>
                    $"page {page}: {chartRegions.Count} chart region(s) masked from table detection");
            }

            // Repeated prose anchors give a second, chart-scoped column signal. It
            // does not partition table detection — a narrow or partly spanning gutter
            // is too ambiguous for that — but it can reject a body-font table
            // hypothesis and later order prose within chart-separated zones. Several
            // charts create narrow vertical zones whose local column structure needs
            // stronger region-graph reasoning, so those pages stay on the
            // conservative full-page grouping path.
            float? chartProseSplit = null;
            if (chartRegions.Count == 1
                && PageSplits.ChartPageProseColumnSplit(pageLayoutItems) is { } candidateSplit
                && PageSplits.ChartSpansProseSplit(chartRegions[0], candidateSplit))
            {
                chartProseSplit = candidateSplit;
            }

            var chartProseColumns = chartProseSplit is not null;

            var bands = PageSplits.SplitSideBySide(pageItems);

            // A rect table crossing a proposed boundary means the "gutter" is really
            // the gap between ruled and borderless table columns, so splitting there
            // would cleave the table in half.
            if (bands.Count > 0 && PageSplits.RectClusterSpansBandBoundary(
                pageItems, PageRects(rectsByPage, page), page, bands))
            {
                Log.Debug(Module, () => $"page {page}: side-by-side split vetoed by spanning rect cluster");
                bands.Clear();
            }

            // Rect hint regions catch a side-by-side layout whose text gap is too
            // narrow for the projection split — calendars with month columns about
            // 10pt apart.
            if (bands.Count == 0)
            {
                bands = PageSplits.SplitFromHintRegions(pageItems, PageRects(rectsByPage, page), page);

                // Only hint-derived splits are tracked for non-table line grouping:
                // projection splits already scope table detection, and their non-table
                // items should flow through normal grouping.
                if (bands.Count > 0)
                {
                    pageBandSplits[page] = [.. bands];
                }
            }

            // Anchor-derived prose splits are lower-confidence than physical gutters,
            // so they do not partition table detection; they apply later, only to the
            // vertical zones outside the chart.
            ChartProseOrder? chartProseOrder = null;
            if (chartProseSplit is { } splitX && chartRegions.Count > 0)
            {
                pageChartProseSplits[page] = splitX;
                chartProseOrder = new ChartProseOrder(splitX, chartRegions[0]);
                pageChartProseOrders[page] = chartProseOrder.Value;
            }

            var bandSpecs = BuildBandSpecs(
                bands, pageItems, PageRects(rectsByPage, page), PageLines(linesByPage, page), page);

            // When the page splits into bands but no band produces a table, the whole
            // page is retried as one band. This catches borderless tables whose column
            // alignment the side-by-side split misread as page-layout columns.
            var wasSplit = bandSpecs.Count > 1;
            Log.Debug(Module, () => $"page {page}: {bandSpecs.Count} bands (was_split={wasSplit})");

            var mergedBand = wasSplit
                ? new BandSpec(
                    [.. pageItems],
                    [.. Enumerable.Range(0, pageItems.Count)],
                    rects.Where(r => r.Page == page).ToList(),
                    pdfLines.Where(l => l.Page == page).ToList())
                : new BandSpec([], [], [], []);

            foreach (var band in bandSpecs)
            {
                if (band.Items.Count == 0)
                {
                    continue;
                }

                DetectBandTables(
                    band, group, page, baseSize, chartRegions, InChart, chartProseOrder,
                    chartProseColumns, wasSplit, structTables, tableItems, pageTables);
            }

            // Thin-rect border synthesis is the last resort for PDFs that draw table
            // borders as thin filled rectangles, common in spreadsheet exports. It
            // only runs when every other method found nothing on this page.
            if (!pageTables.ContainsKey(page))
            {
                SynthesizeThinRectTables(
                    rects, page, textItems, InChart, chartProseOrder, tableItems, pageTables);
            }

            // Merged-band retry: the page split into bands but no band produced a
            // table, so retry heuristic detection over all items at once.
            if (wasSplit && !pageTables.ContainsKey(page) && mergedBand.Items.Count > 0)
            {
                Log.Debug(Module, () =>
                    $"page {page}: merged-band retry ({mergedBand.Items.Count} items, was_split={wasSplit})");
                MergedBandRetry(
                    mergedBand, group, page, baseSize, chartRegions, InChart, chartProseOrder,
                    detectedColumns, tableItems, pageTables);
            }
        }

        // Images are removed before line grouping too, so they take the same logical
        // chart-page position as tables before reinsertion.
        var pageImages = new Dictionary<uint, List<PositionedMarkdown>>();
        foreach (var img in images)
        {
            var imgName = img.Text.StartsWith("[Image: ", StringComparison.Ordinal) && img.Text.EndsWith(']')
                ? img.Text["[Image: ".Length..^1]
                : img.Text;

            if (!pageImages.TryGetValue(img.Page, out var list))
            {
                list = [];
                pageImages[img.Page] = list;
            }

            list.Add(new PositionedMarkdown
            {
                Y = img.Y,
                X = img.X,
                Markdown = $"![Image: {imgName}](image)\n",
                ChartOrder = pageChartProseOrders.TryGetValue(img.Page, out var order) ? order : null,
            });
        }

        // Structure-tree coverage is measured over ALL text items, before table
        // filtering, to decide whether structure-aware generation is worth using.
        var effectiveStructRoles = HasUsableStructCoverage(textItems, structRoles) ? structRoles : null;

        var nonTableItems = textItems.Where((_, idx) => !tableItems.Contains(idx)).ToList();

        var pagesWithText = nonTableItems.Select(i => i.Page).ToHashSet();
        var tableOnlyPages = pageTables.Keys.Where(p => !pagesWithText.Contains(p)).ToHashSet();

        Convert.MergeContinuationTables(pageTables, tableOnlyPages);

        // Pages with detected tables suppress relative-valley column detection, where
        // a table's column gaps would read as page gutters.
        var tablePageSet = pageTables.Keys.ToHashSet();

        var chartMapForLayout = pageChartMap.ToDictionary(
            kv => kv.Key,
            kv => kv.Value.Select(r => ((float, float, float, float))r).ToList());

        var lines = GroupLines(
            nonTableItems, pageThresholds, tablePageSet, chartMapForLayout, pageImageRegions,
            pageBandSplits, pageChartProseSplits, pageChartMap);

        if (options.StripHeadersFooters)
        {
            lines = Preprocess.StripRepeatedLines(lines, pageCount);
        }

        var bandSplitPageSet = pageBandSplits.Keys.ToHashSet();
        bandSplitPageSet.UnionWith(pageChartProseSplits.Keys);

        return Convert.ToMarkdownFromLinesWithTablesAndImages(
            lines, options, pageTables, pageImages, pageChartMap, bandSplitPageSet, effectiveStructRoles);
    }

    /// <summary>True when at least half the text items carry a structure-tree role.</summary>
    private static bool HasUsableStructCoverage(
        List<TextItem> textItems,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles)
    {
        if (structRoles is null || textItems.Count == 0)
        {
            return false;
        }

        var tagged = textItems.Count(item =>
            item.Mcid is { } mcid
            && structRoles.TryGetValue(item.Page, out var pageRoles)
            && pageRoles.ContainsKey(mcid));

        var coverage = tagged / (float)textItems.Count;
        Log.Debug(Module, () =>
            $"structure tree coverage: {tagged}/{textItems.Count} items ({coverage * 100.0f:F0}%)");
        return coverage >= 0.5f;
    }

    /// <summary>Buckets a list by page number, preserving each page's original order.</summary>
    private static Dictionary<uint, List<T>> GroupByPage<T>(IReadOnlyList<T> values, Func<T, uint> pageOf)
    {
        var groups = new Dictionary<uint, List<T>>();
        foreach (var value in values)
        {
            var page = pageOf(value);
            if (!groups.TryGetValue(page, out var group))
            {
                group = [];
                groups[page] = group;
            }

            group.Add(value);
        }

        return groups;
    }

    private static List<PdfRect> PageRects(Dictionary<uint, List<PdfRect>> byPage, uint page) =>
        byPage.TryGetValue(page, out var rects) ? rects : [];

    private static List<PdfLine> PageLines(Dictionary<uint, List<PdfLine>> byPage, uint page) =>
        byPage.TryGetValue(page, out var lines) ? lines : [];

    /// <summary>Builds the per-band item, index, rect and line slices for a page.</summary>
    private static List<BandSpec> BuildBandSpecs(
        List<XBand> bands,
        List<TextItem> pageItems,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<PdfLine> pdfLines,
        uint page)
    {
        if (bands.Count == 0)
        {
            return
            [
                new BandSpec(
                    [.. pageItems],
                    [.. Enumerable.Range(0, pageItems.Count)],
                    rects.Where(r => r.Page == page).ToList(),
                    pdfLines.Where(l => l.Page == page).ToList()),
            ];
        }

        var specs = new List<BandSpec>(bands.Count);
        foreach (var (xLo, xHi) in bands)
        {
            // A small margin keeps edge items from being clipped away.
            const float Margin = 2.0f;

            var bandItems = new List<TextItem>();
            var idxMap = new List<int>();
            for (var idx = 0; idx < pageItems.Count; idx++)
            {
                if (pageItems[idx].X >= xLo - Margin && pageItems[idx].X < xHi + Margin)
                {
                    bandItems.Add(pageItems[idx]);
                    idxMap.Add(idx);
                }
            }

            specs.Add(new BandSpec(
                bandItems,
                idxMap,
                PageSplits.FilterRectsToBand(rects, page, xLo, xHi),
                PageSplits.FilterLinesToBand(pdfLines, page, xLo, xHi)));
        }

        return specs;
    }

    /// <summary>Wraps a detected table with the position and markdown it renders at.</summary>
    private static PositionedMarkdown PositionedTable(Table table, ChartProseOrder? chartOrder) => new()
    {
        Y = table.Rows.Count > 0 ? table.Rows[0] : 0.0f,
        X = table.Columns.Count > 0 ? table.Columns[0] : 0.0f,
        Markdown = TableFormat.TableToMarkdown(table),
        ChartOrder = chartOrder,
    };

    /// <summary>Runs the detection strategies over one band, in priority order.</summary>
    private static void DetectBandTables(
        BandSpec band,
        List<(int GlobalIndex, TextItem Item)> group,
        uint page,
        float baseSize,
        List<ChartRegion> chartRegions,
        Func<TextItem, bool> inChart,
        ChartProseOrder? chartProseOrder,
        bool chartProseColumns,
        bool wasSplit,
        IReadOnlyList<StructTable> structTables,
        HashSet<int> tableItems,
        Dictionary<uint, List<PositionedMarkdown>> pageTables)
    {
        var rectClaimed = new HashSet<int>();

        // Chart items are pre-claimed: every detector below skips claimed indices,
        // and text unclaimed by tables flows out as plain lines.
        if (chartRegions.Count > 0)
        {
            for (var idx = 0; idx < band.Items.Count; idx++)
            {
                if (inChart(band.Items[idx]))
                {
                    rectClaimed.Add(idx);
                }
            }
        }

        void ClaimTable(Table table, ChartProseOrder? order, IReadOnlyList<int>? localToBand = null)
        {
            foreach (var idx in table.ItemIndices)
            {
                var bandIdx = localToBand is null
                    ? idx
                    : idx < localToBand.Count ? localToBand[idx] : -1;
                if (bandIdx < 0)
                {
                    continue;
                }

                rectClaimed.Add(bandIdx);
                if (bandIdx < band.IndexMap.Count)
                {
                    var pageIdx = band.IndexMap[bandIdx];
                    if (pageIdx < group.Count)
                    {
                        tableItems.Add(group[pageIdx].GlobalIndex);
                    }
                }
            }

            AddPageTable(pageTables, page, PositionedTable(table, order));
        }

        // 0. Structure-tree detection has the highest priority — it is semantic PDF
        //    tagging. A struct-tree table is only used when it captures a majority
        //    of the band's items; an incomplete tree should fall through to geometry
        //    detection, which sees everything.
        if (structTables.Count > 0)
        {
            foreach (var table in StructTables.DetectTablesFromStructTree(band.Items, structTables, page))
            {
                var coverage = table.ItemIndices.Count / (float)Math.Max(band.Items.Count, 1);
                if (coverage >= 0.5f)
                {
                    ClaimTable(table, chartProseOrder);
                }
            }
        }

        // 1. Rect-based detection, skipping tables that overlap a struct-tree claim.
        var (rectTables, hintRegions) = RectTables.DetectTablesFromRects(band.Items, band.Rects, page);
        foreach (var table in rectTables)
        {
            if (rectClaimed.Count > 0 && table.ItemIndices.Any(rectClaimed.Contains))
            {
                continue;
            }

            ClaimTable(table, chartProseOrder);
        }

        // 2. Line-based detection, when rects found nothing.
        if (rectClaimed.Count == 0)
        {
            foreach (var table in LineDetector.DetectTablesFromLines(band.Items, band.Lines, page))
            {
                ClaimTable(table, chartProseOrder);
            }
        }

        const float HintPadding = 15.0f;

        // 3a. Rect-guided construction on the hint regions.
        if (rectClaimed.Count == 0 && hintRegions.Count > 0)
        {
            foreach (var hint in hintRegions)
            {
                if (hint.ClusterRects.Count == 0)
                {
                    continue;
                }

                var (insideItems, insideMap) = SliceBand(band.Items, (idx, item) =>
                    item.Y >= hint.YBottom - HintPadding
                    && item.Y <= hint.YTop + HintPadding
                    && item.X >= hint.XLeft - HintPadding
                    && item.X <= hint.XRight + HintPadding);

                var table = TableBuilders.TryBuildRectGuidedTable(insideItems, hint.ClusterRects);
                if (table is null)
                {
                    continue;
                }

                ClaimTable(table, chartProseOrder, insideMap);
                foreach (var bandIdx in insideMap)
                {
                    rectClaimed.Add(bandIdx);
                }
            }
        }

        // 3b. The heuristic fallback on whatever remains unclaimed.
        void RunHeuristic(List<TextItem> subsetItems, List<int> indexMap, int minItems)
        {
            if (subsetItems.Count < minItems)
            {
                return;
            }

            // Body-font detection stays available on chart pages, since a real table
            // can share the prose anchors. Only candidates whose cells prove they are
            // parallel prose fragments get rejected.
            var rejectParallelProse = chartProseColumns && !wasSplit;

            foreach (var table in HeuristicDetector.DetectTables(subsetItems, baseSize, false))
            {
                if (rejectParallelProse && ParallelProse.IsParallelProseTable(table))
                {
                    Log.Debug(Module, () =>
                        $"page {page}: rejected {table.Rows.Count}x{table.Columns.Count} parallel-prose table hypothesis");
                    continue;
                }

                ClaimTable(table, chartProseOrder, indexMap);
            }
        }

        if (rectClaimed.Count == 0 && hintRegions.Count == 0)
        {
            RunHeuristic(band.Items, [.. Enumerable.Range(0, band.Items.Count)], 6);
        }
        else if (rectClaimed.Count == 0)
        {
            // Hint regions exist but produced no table, so the heuristic runs
            // separately inside each hint and once over everything outside them.
            foreach (var hint in hintRegions)
            {
                var (insideItems, insideMap) = SliceBand(band.Items, (idx, item) =>
                    item.Y >= hint.YBottom - HintPadding && item.Y <= hint.YTop + HintPadding);
                RunHeuristic(insideItems, insideMap, 6);
                foreach (var bandIdx in insideMap)
                {
                    rectClaimed.Add(bandIdx);
                }
            }

            var (outsideItems, outsideMap) = SliceBand(band.Items, (idx, _) => !rectClaimed.Contains(idx));
            RunHeuristic(outsideItems, outsideMap, 6);
        }
        else
        {
            var (unclaimedItems, unclaimedMap) = SliceBand(band.Items, (idx, _) => !rectClaimed.Contains(idx));
            RunHeuristic(unclaimedItems, unclaimedMap, 6);
        }

        // 4. Column-based construction, for borderless tabular layouts.
        var bandHasTables = band.IndexMap.Any(pageIdx =>
            pageIdx < group.Count && tableItems.Contains(group[pageIdx].GlobalIndex));
        var hasStructuralElements = band.Rects.Count >= 6 || band.Lines.Count >= 4;

        if (!bandHasTables && !hasStructuralElements)
        {
            var table = TableBuilders.TryBuildTableFromColumns(band.Items, page);
            if (table is not null)
            {
                ClaimTable(table, chartProseOrder);
            }
        }
    }

    /// <summary>Selects a subset of a band's items, keeping their band-local indices.</summary>
    private static (List<TextItem> Items, List<int> IndexMap) SliceBand(
        List<TextItem> bandItems,
        Func<int, TextItem, bool> predicate)
    {
        var items = new List<TextItem>();
        var map = new List<int>();
        for (var idx = 0; idx < bandItems.Count; idx++)
        {
            if (predicate(idx, bandItems[idx]))
            {
                items.Add(bandItems[idx]);
                map.Add(idx);
            }
        }

        return (items, map);
    }

    /// <summary>Appends a positioned table to a page's list.</summary>
    private static void AddPageTable(
        Dictionary<uint, List<PositionedMarkdown>> pageTables,
        uint page,
        PositionedMarkdown table)
    {
        if (!pageTables.TryGetValue(page, out var list))
        {
            list = [];
            pageTables[page] = list;
        }

        list.Add(table);
    }

    /// <summary>
    /// Synthesises line segments from thin filled rectangles and runs line-based
    /// detection over them. Spreadsheet exports commonly draw table borders that
    /// way.
    /// </summary>
    private static void SynthesizeThinRectTables(
        IReadOnlyList<PdfRect> rects,
        uint page,
        List<TextItem> textItems,
        Func<TextItem, bool> inChart,
        ChartProseOrder? chartProseOrder,
        HashSet<int> tableItems,
        Dictionary<uint, List<PositionedMarkdown>> pageTables)
    {
        var synthLines = new List<PdfLine>();
        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            var (x, w) = r.Width < 0.0f ? (r.X + r.Width, -r.Width) : (r.X, r.Width);
            var (y, h) = r.Height < 0.0f ? (r.Y + r.Height, -r.Height) : (r.Y, r.Height);

            if (h < 2.0f && w >= 10.0f)
            {
                var midY = y + (h / 2.0f);
                synthLines.Add(new PdfLine { X1 = x, Y1 = midY, X2 = x + w, Y2 = midY, Page = page });
            }
            else if (w < 2.0f && h >= 10.0f)
            {
                var midX = x + (w / 2.0f);
                synthLines.Add(new PdfLine { X1 = midX, Y1 = y, X2 = midX, Y2 = y + h, Page = page });
            }
        }

        if (synthLines.Count < 10)
        {
            return;
        }

        // Chart text stays out of the thin-rect fallback: a chart's grid rules would
        // otherwise re-grid it.
        var pageText = new List<TextItem>();
        var pageTextMap = new List<int>();
        for (var idx = 0; idx < textItems.Count; idx++)
        {
            if (textItems[idx].Page == page && !inChart(textItems[idx]))
            {
                pageText.Add(textItems[idx]);
                pageTextMap.Add(idx);
            }
        }

        foreach (var table in LineDetector.DetectTablesFromLines(pageText, synthLines, page))
        {
            foreach (var idx in table.ItemIndices)
            {
                if (idx < pageTextMap.Count)
                {
                    tableItems.Add(pageTextMap[idx]);
                }
            }

            AddPageTable(pageTables, page, PositionedTable(table, chartProseOrder));
        }
    }

    /// <summary>
    /// Retries heuristic detection over a page's items merged back into one band,
    /// after a side-by-side split produced no tables.
    /// </summary>
    private static void MergedBandRetry(
        BandSpec mergedBand,
        List<(int GlobalIndex, TextItem Item)> group,
        uint page,
        float baseSize,
        List<ChartRegion> chartRegions,
        Func<TextItem, bool> inChart,
        ChartProseOrder? chartProseOrder,
        bool detectedColumns,
        HashSet<int> tableItems,
        Dictionary<uint, List<PositionedMarkdown>> pageTables)
    {
        var (chartFree, chartFreeMap) = SliceBand(mergedBand.Items, (_, it) => !inChart(it));

        // A chart-derived column signal must not disable body-font detection during
        // this retry: the retry exists for tables that only become visible once false
        // layout bands are recombined. The legacy skip stays for ordinary
        // detected-column pages, and chart-page prose candidates are rejected
        // individually below.
        var skipBodyFont = detectedColumns && chartRegions.Count == 0;

        foreach (var table in HeuristicDetector.DetectTables(chartFree, baseSize, skipBodyFont))
        {
            if (chartRegions.Count > 0 && ParallelProse.IsParallelProseTable(table))
            {
                Log.Debug(Module, () =>
                    $"page {page}: rejected {table.Rows.Count}x{table.Columns.Count} merged-band parallel-prose table hypothesis");
                continue;
            }

            foreach (var idx in table.ItemIndices)
            {
                if (idx >= chartFreeMap.Count)
                {
                    continue;
                }

                var bandIdx = chartFreeMap[idx];
                if (bandIdx < mergedBand.IndexMap.Count)
                {
                    var pageIdx = mergedBand.IndexMap[bandIdx];
                    if (pageIdx < group.Count)
                    {
                        tableItems.Add(group[pageIdx].GlobalIndex);
                    }
                }
            }

            AddPageTable(pageTables, page, PositionedTable(table, chartProseOrder));
        }
    }

    /// <summary>
    /// Groups the non-table items into lines, splitting by band or chart zone
    /// wherever a page demands it.
    /// </summary>
    private static List<TextLine> GroupLines(
        List<TextItem> nonTableItems,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlySet<uint> tablePageSet,
        Dictionary<uint, List<(float X0, float Y0, float X1, float Y1)>> chartMapForLayout,
        Dictionary<uint, List<ImageRegion>> pageImageRegions,
        Dictionary<uint, List<XBand>> pageBandSplits,
        Dictionary<uint, float> pageChartProseSplits,
        Dictionary<uint, List<ChartRegion>> pageChartMap)
    {
        if (pageBandSplits.Count == 0 && pageChartProseSplits.Count == 0)
        {
            return Layout.GroupIntoLinesWithThresholdsAndRegions(
                nonTableItems, pageThresholds, tablePageSet, chartMapForLayout, pageImageRegions);
        }

        // Items separate into physical-band pages, chart/prose pages and ordinary
        // pages. A chart/prose page needs a different reading order: each chart is a
        // full-width separator, while the prose above and below it reads down the
        // left column and then down the right.
        var splitPageItems = new Dictionary<uint, List<TextItem>>();
        var chartProsePageItems = new Dictionary<uint, List<TextItem>>();
        var unsplitItems = new List<TextItem>();

        foreach (var item in nonTableItems)
        {
            if (pageChartProseSplits.ContainsKey(item.Page))
            {
                AddToPage(chartProsePageItems, item.Page, item);
            }
            else if (pageBandSplits.ContainsKey(item.Page))
            {
                AddToPage(splitPageItems, item.Page, item);
            }
            else
            {
                unsplitItems.Add(item);
            }
        }

        var allLines = Layout.GroupIntoLinesWithThresholdsAndRegions(
            unsplitItems, pageThresholds, tablePageSet, chartMapForLayout, pageImageRegions);

        // Each split page's bands group independently, then interleave by Y so paired
        // zones — left and right months, say — appear together.
        foreach (var page in splitPageItems.Keys.OrderBy(p => p).ToList())
        {
            var items = splitPageItems[page];
            var pageLines = new List<TextLine>();

            foreach (var (xLo, xHi) in pageBandSplits[page])
            {
                const float Margin = 2.0f;
                var bandItems = items.Where(i => i.X >= xLo - Margin && i.X < xHi + Margin).ToList();
                if (bandItems.Count > 0)
                {
                    pageLines.AddRange(Layout.GroupIntoLinesWithThresholdsAndCharts(
                        bandItems, pageThresholds, tablePageSet, chartMapForLayout));
                }
            }

            pageLines.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));
            allLines.AddRange(pageLines);
        }

        // A chart/prose page reads as alternating vertical zones: within a prose zone
        // each column groups independently and the columns append in newspaper order,
        // while a chart zone groups across the full width.
        foreach (var page in chartProsePageItems.Keys.OrderBy(p => p).ToList())
        {
            var remaining = chartProsePageItems[page];
            var splitX = pageChartProseSplits[page];
            var chartRegions = pageChartMap[page];

            List<TextLine> GroupProseZone(List<TextItem> zoneItems)
            {
                var zoneLines = new List<TextLine>();
                foreach (var rightColumn in new[] { false, true })
                {
                    var columnItems = zoneItems.Where(item => item.X >= splitX == rightColumn).ToList();
                    if (columnItems.Count > 0)
                    {
                        zoneLines.AddRange(Layout.GroupIntoLinesWithThresholdsAndCharts(
                            columnItems, pageThresholds, tablePageSet, chartMapForLayout));
                    }
                }

                return zoneLines;
            }

            var chartYBands = chartRegions
                .Select(r => (Low: r.Y0 - ChartRegions.ChartSeparatorPad, High: r.Y1 + ChartRegions.ChartSeparatorPad))
                .OrderByDescending(b => b.High, FloatTotalOrder.Instance)
                .ToList();

            var mergedChartYBands = new List<(float Low, float High)>();
            foreach (var (low, high) in chartYBands)
            {
                if (mergedChartYBands.Count > 0 && high >= mergedChartYBands[^1].Low)
                {
                    var last = mergedChartYBands[^1];
                    mergedChartYBands[^1] = (MathF.Min(last.Low, low), MathF.Max(last.High, high));
                    continue;
                }

                mergedChartYBands.Add((low, high));
            }

            foreach (var (low, high) in mergedChartYBands)
            {
                var above = new List<TextItem>();
                var atOrBelow = new List<TextItem>();
                foreach (var item in remaining)
                {
                    if (item.Y > high && !ChartRegions.ItemIsInChartRegion(item, chartRegions))
                    {
                        above.Add(item);
                    }
                    else
                    {
                        atOrBelow.Add(item);
                    }
                }

                allLines.AddRange(GroupProseZone(above));

                var chartZone = new List<TextItem>();
                var below = new List<TextItem>();
                foreach (var item in atOrBelow)
                {
                    if (item.Y >= low || ChartRegions.ItemIsInChartRegion(item, chartRegions))
                    {
                        chartZone.Add(item);
                    }
                    else
                    {
                        below.Add(item);
                    }
                }

                allLines.AddRange(Layout.GroupIntoLinesWithThresholdsAndCharts(
                    chartZone, pageThresholds, tablePageSet, chartMapForLayout));
                remaining = below;
            }

            allLines.AddRange(GroupProseZone(remaining));
        }

        // The three paths accumulate separately, so restore document page order while
        // preserving each page's chosen line order.
        return [.. allLines.OrderBy(line => line.Page)];
    }

    /// <summary>Appends an item to its page's bucket.</summary>
    private static void AddToPage(Dictionary<uint, List<TextItem>> map, uint page, TextItem item)
    {
        if (!map.TryGetValue(page, out var list))
        {
            list = [];
            map[page] = list;
        }

        list.Add(item);
    }
}
