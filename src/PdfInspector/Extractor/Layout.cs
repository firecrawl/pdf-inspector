// Ported from reference/src/extractor/layout.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>Groups text items into lines, honouring column layout and reading order.</summary>
internal static class Layout
{
    private const string Module = "layout";

    /// <summary>Shared empty collections, so the convenience overloads allocate nothing.</summary>
    private static readonly Dictionary<uint, float> NoThresholds = [];

    private static readonly HashSet<uint> NoTablePages = [];

    private static readonly Dictionary<uint, List<(float X0, float Y0, float X1, float Y1)>> NoChartRegions = [];

    private static readonly Dictionary<uint, List<ImageRegion>> NoImageRegions = [];

    /// <summary>Groups items into lines with the default thresholds, suppressing page numbers.</summary>
    public static List<TextLine> GroupIntoLines(List<TextItem> items) =>
        GroupIntoLinesWithThresholds(items, NoThresholds, NoTablePages);

    /// <summary>
    /// Groups items into lines while keeping standalone numeric headers and
    /// footers. Plain-text extraction uses this path because every extracted
    /// item is part of that API's result; markdown conversion drops page numbers
    /// as a deliberate presentation cleanup.
    /// </summary>
    public static List<TextLine> GroupIntoLinesPreservingAllText(List<TextItem> items) =>
        GroupIntoLinesImpl(items, NoThresholds, NoTablePages, NoChartRegions, NoImageRegions,
            filterPageNumbers: false);

    /// <summary>
    /// Groups items into lines using per-page adaptive thresholds from
    /// letter-spacing detection.
    /// </summary>
    public static List<TextLine> GroupIntoLinesWithThresholds(
        List<TextItem> items,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlySet<uint> tablePages) =>
        GroupIntoLinesWithThresholdsAndCharts(items, pageThresholds, tablePages, NoChartRegions);

    /// <summary>
    /// As <see cref="GroupIntoLinesWithThresholds"/>, but items inside chart
    /// regions are hidden from column detection: chart text scattered across the
    /// page fills the gutter in the projection histogram, so a two-column page
    /// would read as one and same-baseline items from both columns would fuse.
    /// </summary>
    public static List<TextLine> GroupIntoLinesWithThresholdsAndCharts(
        List<TextItem> items,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlySet<uint> tablePages,
        IReadOnlyDictionary<uint, List<(float X0, float Y0, float X1, float Y1)>> chartRegions) =>
        GroupIntoLinesWithThresholdsAndRegions(items, pageThresholds, tablePages, chartRegions, NoImageRegions);

    public static List<TextLine> GroupIntoLinesWithThresholdsAndRegions(
        List<TextItem> items,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlySet<uint> tablePages,
        IReadOnlyDictionary<uint, List<(float X0, float Y0, float X1, float Y1)>> chartRegions,
        IReadOnlyDictionary<uint, List<ImageRegion>> imageRegions) =>
        GroupIntoLinesImpl(items, pageThresholds, tablePages, chartRegions, imageRegions, filterPageNumbers: true);

    private static List<TextLine> GroupIntoLinesImpl(
        List<TextItem> items,
        IReadOnlyDictionary<uint, float> pageThresholds,
        IReadOnlySet<uint> tablePages,
        IReadOnlyDictionary<uint, List<(float X0, float Y0, float X1, float Y1)>> chartRegions,
        IReadOnlyDictionary<uint, List<ImageRegion>> imageRegions,
        bool filterPageNumbers)
    {
        if (items.Count == 0)
        {
            return [];
        }

        var source = filterPageNumbers
            ? items.Where(i => !Columns.IsPageNumber(i)).ToList()
            : items;

        var pages = source.Select(i => i.Page).Distinct().OrderBy(p => p).ToList();
        var allLines = new List<TextLine>();

        foreach (var page in pages)
        {
            var pageItems = source.Where(i => i.Page == page).ToList();

            // The threshold computed before embedded-space removal carries the
            // full signal; ordinary pages use the default.
            var adaptiveThreshold = pageThresholds.TryGetValue(page, out var t)
                ? t
                : TextUtils.DefaultJoinThreshold;

            // Image-backed region graphs recover local and asymmetric column
            // flows that a whole-page projection cannot represent. Charts have
            // their own positioned-region ordering and stay on that path.
            if (!chartRegions.ContainsKey(page) && imageRegions.TryGetValue(page, out var regions))
            {
                var preliminaryColumns = Columns.DetectColumns(pageItems, page, tablePages.Contains(page));
                float? detectedSplit = preliminaryColumns.Count == 2 ? preliminaryColumns[0].XMax : null;

                if (ReadingOrder.InferImageAnchoredFlow(pageItems, regions, detectedSplit) is { } band)
                {
                    Log.Debug(Module, () =>
                        $"page {page}: image-anchored region graph split={band.SplitX:F1} " +
                        $"y=[{band.YBottom:F1}..{band.YTop:F1}]");

                    foreach (var node in ReadingOrder.BuildRegionGraph(pageItems, band))
                    {
                        Log.Debug(Module, () => $"page {page}: region node {node.Kind} items={node.Items.Count}");
                        allLines.AddRange(GroupSingleColumn(node.Items, adaptiveThreshold));
                    }

                    continue;
                }
            }

            // Column detection runs blind to chart-internal text.
            List<ColumnRegion> columns;
            if (chartRegions.TryGetValue(page, out var charts) && charts.Count > 0)
            {
                var columnInput = pageItems.Where(it =>
                {
                    var cx = it.X + (it.Width / 2.0f);
                    // Tight bounds: only chart-internal text is hidden; rows
                    // adjacent to the chart belong to the column layout.
                    return !charts.Any(r =>
                        cx >= r.X0 - 2.0f && cx <= r.X1 + 2.0f && it.Y >= r.Y0 - 2.0f && it.Y <= r.Y1 + 2.0f);
                }).ToList();

                columns = Columns.DetectColumns(columnInput, page, tablePages.Contains(page));
            }
            else
            {
                columns = Columns.DetectColumns(pageItems, page, tablePages.Contains(page));
            }

            if (columns.Count <= 1)
            {
                allLines.AddRange(GroupSingleColumn(pageItems, adaptiveThreshold));
                continue;
            }

            // Lines running the full page width — titles, section headers,
            // footers — are masked first. Split across column buckets they would
            // corrupt newspaper detection and reading order.
            var spanningMask = Columns.IdentifySpanningLines(pageItems, columns);
            var premaskedCount = spanningMask.Count(m => m);
            if (premaskedCount > 0)
            {
                Log.Debug(Module, () => $"page {page}: pre-masked {premaskedCount} spanning-line items");
            }

            var spanningItems = new List<TextItem>();
            var columnItems = new List<TextItem>();

            for (var i = 0; i < pageItems.Count; i++)
            {
                if (spanningMask[i] || Columns.SpansMultipleColumns(pageItems[i], columns))
                {
                    spanningItems.Add(pageItems[i]);
                }
                else
                {
                    columnItems.Add(pageItems[i]);
                }
            }

            // Each item goes to the column it overlaps most, rather than the one
            // containing its centre, which avoids gutter mis-assignment.
            var colBuckets = new List<List<TextItem>>();
            for (var i = 0; i < columns.Count; i++)
            {
                colBuckets.Add([]);
            }

            foreach (var item in columnItems)
            {
                var itemLeft = item.X;
                var itemRight = item.X + TextUtils.EffectiveWidth(item);
                var bestCol = 0;
                var bestOverlap = float.NegativeInfinity;

                for (var ci = 0; ci < columns.Count; ci++)
                {
                    var overlap = MathF.Max(
                        MathF.Min(itemRight, columns[ci].XMax) - MathF.Max(itemLeft, columns[ci].XMin),
                        0.0f);

                    if (overlap > bestOverlap)
                    {
                        bestOverlap = overlap;
                        bestCol = ci;
                    }
                }

                colBuckets[bestCol].Add(item);
            }

            Log.Debug(Module, () =>
                $"page {page}: {columns.Count} columns, {spanningItems.Count} spanning items");

            var perColumnLines = colBuckets
                .Select(bucket => GroupSingleColumn(bucket, adaptiveThreshold))
                .ToList();

            var spanningLines = GroupSingleColumn(spanningItems, adaptiveThreshold);

            var isNewspaper = IsNewspaperLayout(perColumnLines, columns);
            Log.Debug(Module, () => $"page {page}: layout={(isNewspaper ? "newspaper" : "tabular")}");

            if (isNewspaper)
            {
                allLines.AddRange(EmitNewspaperOrder(perColumnLines, spanningLines));
            }
            else
            {
                allLines.AddRange(EmitTabularOrder(perColumnLines, spanningLines));
            }
        }

        return allLines;
    }

    /// <summary>
    /// Newspaper order: each column is an independent flow. Content above the
    /// column region comes first, then the columns in sequence, then what lies
    /// below.
    /// </summary>
    private static List<TextLine> EmitNewspaperOrder(
        List<List<TextLine>> perColumnLines,
        List<TextLine> spanningLines)
    {
        var result = new List<TextLine>();

        var coreColumns = new List<List<TextLine>>();
        var colStragglers = new List<List<TextLine>>();

        foreach (var col in perColumnLines)
        {
            var (core, stragglers) = SplitColumnStragglers(col);
            coreColumns.Add(core);
            colStragglers.Add(stragglers);
        }

        // The topmost line shared by every non-empty column marks where the
        // column region begins.
        var colTop = float.PositiveInfinity;
        foreach (var col in coreColumns)
        {
            if (col.Count > 0)
            {
                colTop = MathF.Min(colTop, col.Max(l => l.Y));
            }
        }

        const float Margin = 5.0f;

        var above = new List<TextLine>();
        var belowSpanning = new List<TextLine>();

        foreach (var line in spanningLines)
        {
            if (line.Y > colTop + Margin)
            {
                above.Add(line);
            }
            else
            {
                belowSpanning.Add(line);
            }
        }

        // Stragglers above the column region join the leading content; below it
        // they stay with their column, so sorting by Y cannot re-interleave them.
        var colBelow = new List<List<TextLine>>();
        for (var i = 0; i < coreColumns.Count; i++)
        {
            colBelow.Add([]);
        }

        for (var ci = 0; ci < colStragglers.Count; ci++)
        {
            foreach (var line in colStragglers[ci])
            {
                if (line.Y > colTop + Margin)
                {
                    above.Add(line);
                }
                else
                {
                    colBelow[ci].Add(line);
                }
            }
        }

        above.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));
        belowSpanning.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));

        result.AddRange(above);
        foreach (var col in coreColumns)
        {
            result.AddRange(col);
        }

        foreach (var cb in colBelow)
        {
            result.AddRange(cb);
        }

        result.AddRange(belowSpanning);
        return result;
    }

    /// <summary>
    /// Tabular order: rows at the same height in different columns form one
    /// logical line, so the columns are interleaved by Y.
    /// </summary>
    private static List<TextLine> EmitTabularOrder(
        List<List<TextLine>> perColumnLines,
        List<TextLine> spanningLines)
    {
        var allPageLines = new List<TextLine>(spanningLines);
        foreach (var colLines in perColumnLines)
        {
            allPageLines.AddRange(colLines);
        }

        // Top of page first, then left to right among lines at the same height.
        allPageLines.Sort((a, b) =>
        {
            var byY = FloatTotalOrder.Instance.Compare(b.Y, a.Y);
            if (byY != 0)
            {
                return byY;
            }

            var ax = a.Items.Count > 0 ? a.Items[0].X : 0.0f;
            var bx = b.Items.Count > 0 ? b.Items[0].X : 0.0f;
            return FloatTotalOrder.Instance.Compare(ax, bx);
        });

        const float YTol = 3.0f;
        var merged = new List<TextLine>();

        foreach (var line in allPageLines)
        {
            if (merged.Count > 0)
            {
                var last = merged[^1];
                if (last.Page == line.Page && MathF.Abs(last.Y - line.Y) < YTol)
                {
                    last.Items.AddRange(line.Items);
                    TextUtils.SortLineItems(last.Items);
                    continue;
                }
            }

            merged.Add(line);
        }

        return merged;
    }

    /// <summary>
    /// True when columns are independent text flows to be read one after
    /// another, rather than rows to interleave by height.
    /// </summary>
    public static bool IsNewspaperLayout(List<List<TextLine>> perColumnLines, List<ColumnRegion> columns)
    {
        if (perColumnLines.Count < 2)
        {
            return false;
        }

        var minLines = perColumnLines.Min(c => c.Count);
        var maxLines = perColumnLines.Max(c => c.Count);

        if (minLines < 5)
        {
            return false;
        }

        if (minLines < 15)
        {
            return IsSidebarLayout(perColumnLines, columns, minLines, maxLines);
        }

        // Two dense, balanced columns are independent prose flows. Table items
        // have already been removed by this point.
        if ((float)minLines / maxLines > 0.7f)
        {
            return true;
        }

        // Unbalanced columns fall back to a collision check: if the shorter
        // column's lines mostly share a baseline with another column's, the page
        // reads as rows rather than flows.
        const float YTol = 5.0f;

        var smallestIdx = 0;
        for (var i = 1; i < perColumnLines.Count; i++)
        {
            if (perColumnLines[i].Count < perColumnLines[smallestIdx].Count)
            {
                smallestIdx = i;
            }
        }

        var smallest = perColumnLines[smallestIdx];
        if (smallest.Count == 0)
        {
            return false;
        }

        var collisions = 0;
        foreach (var line in smallest)
        {
            for (var ci = 0; ci < perColumnLines.Count; ci++)
            {
                if (ci == smallestIdx)
                {
                    continue;
                }

                if (perColumnLines[ci].Any(ol => MathF.Abs(ol.Y - line.Y) < YTol))
                {
                    collisions++;
                    break;
                }
            }
        }

        return (float)collisions / smallest.Count > 0.5f;
    }

    /// <summary>
    /// Recognises a narrow annotation column beside a wide body column, which
    /// reads sequentially even though it has too few lines to qualify outright.
    /// </summary>
    private static bool IsSidebarLayout(
        List<List<TextLine>> perColumnLines,
        List<ColumnRegion> columns,
        int minLines,
        int maxLines)
    {
        // Sidebars pair one body column with one annotation column, never three.
        if (columns.Count != 2 || perColumnLines.Count != 2)
        {
            return false;
        }

        var w0 = columns[0].Width;
        var w1 = columns[1].Width;
        var widthRatio = MathF.Min(w0, w1) / MathF.Max(w0, w1);
        var lineBalance = maxLines > 0 ? (float)minLines / maxLines : 1.0f;
        var narrowWidth = MathF.Min(w0, w1);

        if (widthRatio >= 0.50f || lineBalance >= 0.35f || maxLines < 20 || narrowWidth < 160.0f)
        {
            return false;
        }

        var narrowerIdx = w0 < w1 ? 0 : 1;
        var fewestIdx = perColumnLines[0].Count <= perColumnLines[1].Count ? 0 : 1;

        if (narrowerIdx != fewestIdx)
        {
            return false;
        }

        // Sidebar annotations spread thinly down the page while regular
        // two-column text is dense, so their average line gap is much larger.
        var narrow = perColumnLines[narrowerIdx];
        var wide = perColumnLines[1 - narrowerIdx];

        static float AverageGap(List<TextLine> lines)
        {
            if (lines.Count < 2)
            {
                return 0.0f;
            }

            var ys = lines.Select(l => l.Y).OrderBy(y => y, FloatTotalOrder.Instance).ToList();
            return (ys[^1] - ys[0]) / (lines.Count - 1);
        }

        var narrowGap = AverageGap(narrow);
        var wideGap = AverageGap(wide);

        return wideGap > 0.0f && narrowGap / wideGap >= 2.5f;
    }

    /// <summary>
    /// Splits a column's lines into its densest cluster and the stragglers
    /// around it — header remnants and per-word items from full-width lines.
    /// </summary>
    private static (List<TextLine> Core, List<TextLine> Stragglers) SplitColumnStragglers(List<TextLine> lines)
    {
        if (lines.Count < 3)
        {
            return (lines, []);
        }

        // Lines arrive sorted top-first, so gaps are positive.
        var gaps = new List<float>();
        for (var i = 0; i + 1 < lines.Count; i++)
        {
            gaps.Add(lines[i].Y - lines[i + 1].Y);
        }

        var sortedGaps = new List<float>(gaps);
        sortedGaps.Sort(FloatTotalOrder.Instance);
        var medianGap = sortedGaps[sortedGaps.Count / 2];

        // A gap several times the typical line spacing breaks content clusters.
        var threshold = MathF.Max(medianGap * 3.0f, 30.0f);

        var splitIndices = new List<int>();
        for (var i = 0; i < gaps.Count; i++)
        {
            if (gaps[i] > threshold)
            {
                splitIndices.Add(i);
            }
        }

        if (splitIndices.Count == 0)
        {
            return (lines, []);
        }

        var segments = new List<(int Start, int End)>();
        var start = 0;
        foreach (var si in splitIndices)
        {
            segments.Add((start, si + 1));
            start = si + 1;
        }

        segments.Add((start, lines.Count));

        var coreSeg = 0;
        for (var i = 1; i < segments.Count; i++)
        {
            if (segments[i].End - segments[i].Start > segments[coreSeg].End - segments[coreSeg].Start)
            {
                coreSeg = i;
            }
        }

        var (cs, ce) = segments[coreSeg];
        var core = new List<TextLine>();
        var stragglers = new List<TextLine>();

        for (var i = 0; i < lines.Count; i++)
        {
            if (i >= cs && i < ce)
            {
                core.Add(lines[i]);
            }
            else
            {
                stragglers.Add(lines[i]);
            }
        }

        return (core, stragglers);
    }

    /// <summary>
    /// True when the content stream's order looks chaotic and sorting by
    /// position will read better. Well-ordered documents progress down the page.
    /// </summary>
    private static bool ShouldUseYSorting(List<TextItem> items)
    {
        if (items.Count < 5)
        {
            return false;
        }

        const float JumpThreshold = 50.0f;
        var largeJumpsUp = 0;
        var largeJumpsDown = 0;

        for (var i = 0; i + 1 < items.Count; i++)
        {
            var delta = items[i + 1].Y - items[i].Y;
            if (delta > JumpThreshold)
            {
                largeJumpsUp++;
            }
            else if (delta < -JumpThreshold)
            {
                largeJumpsDown++;
            }
        }

        var totalJumps = largeJumpsUp + largeJumpsDown;
        if (totalJumps < 3)
        {
            return false;
        }

        return (float)largeJumpsUp / totalJumps > 0.4f;
    }

    /// <summary>Groups one column's items into lines, in stream or positional order.</summary>
    private static List<TextLine> GroupSingleColumn(List<TextItem> items, float adaptiveThreshold)
    {
        if (items.Count == 0)
        {
            return [];
        }

        var ordered = items;
        if (ShouldUseYSorting(items))
        {
            ordered = [.. items];
            ordered.Sort((a, b) =>
            {
                var byY = FloatTotalOrder.Instance.Compare(b.Y, a.Y);
                return byY != 0 ? byY : FloatTotalOrder.Instance.Compare(a.X, b.X);
            });
        }

        var lines = new List<TextLine>();
        const float YTolerance = 3.0f;

        foreach (var item in ordered)
        {
            var last = lines.Count > 0 ? lines[^1] : null;

            if (last is not null && ShouldMergeIntoLine(last, item, YTolerance))
            {
                last.Items.Add(item);
            }
            else
            {
                lines.Add(new TextLine
                {
                    Items = [item],
                    Y = item.Y,
                    Page = item.Page,
                    AdaptiveThreshold = adaptiveThreshold,
                });
            }
        }

        foreach (var line in lines)
        {
            TextUtils.SortLineItems(line.Items);
        }

        Log.Debug(Module, () => $"group_single_column: {lines.Count} lines");
        return lines;
    }

    private static bool ShouldMergeIntoLine(TextLine lastLine, TextItem item, float yTolerance)
    {
        if (lastLine.Page != item.Page)
        {
            return false;
        }

        var yDiff = MathF.Abs(lastLine.Y - item.Y);
        if (yDiff >= yTolerance)
        {
            return false;
        }

        // A small vertical shift can still mean a new line: items stacked at the
        // same left margin, or a run starting well to the left of the previous.
        if (yDiff > 0.5f && lastLine.Items.Count > 0)
        {
            var firstItem = lastLine.Items[0];
            if (MathF.Abs(item.X - firstItem.X) < 5.0f)
            {
                return false;
            }

            var lastItem = lastLine.Items[^1];
            if (item.X < lastItem.X - 10.0f)
            {
                return false;
            }
        }

        // Same baseline but separated by a wide void, with the incoming run
        // starting alphabetic: the neighbouring column's body text sharing a
        // baseline, in a gutter too narrow for column detection. Both sides must
        // be multi-word prose, so table-of-contents page numbers, dot leaders,
        // and outline-numbered cells stay joined.
        if (lastLine.Items.Count > 0)
        {
            var lastItem = lastLine.Items[^1];
            var gap = item.X - (lastItem.X + lastItem.Width);
            var gapThreshold = MathF.Max(MathF.Max(item.FontSize, lastItem.FontSize) * 3.0f, 30.0f);

            if (gap > gapThreshold && TextUtils.FirstChar(item.Text.Trim()) is { } first && char.IsLetter(first))
            {
                var incoming = item.Text.Trim();
                var incomingWordy = incoming.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length >= 3
                    && incoming.Count(char.IsLetter) >= 10;

                var lineText = string.Join(" ", lastLine.Items.Select(i => i.Text.Trim()));
                var lineWordy = lineText.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length >= 2
                    && lineText.Count(char.IsLetter) >= 8;

                // A lowercase start is a mid-sentence continuation and splits on
                // prose signals alone. An uppercase start also needs a style
                // mismatch — a bold heading beside regular body text — or
                // same-style label rows would shatter.
                var startsLower = char.IsLower(first);

                // The whole line must be bold for a heading, not merely its last
                // run, so mixed label/value rows stay joined.
                var styleMismatch = lastLine.Items.All(i => i.IsBold) && !item.IsBold;

                if (lineWordy && incomingWordy && (startsLower || styleMismatch))
                {
                    return false;
                }
            }
        }

        return true;
    }
}
