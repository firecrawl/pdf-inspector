// Ported from reference/src/tables/detect_heuristic.rs
using System.Text;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Finds tables by clustering item positions, without help from drawn rules or
/// a structure tree. This is the last of the three detection strategies and the
/// only one that works on tables with no ruling at all.
/// </summary>
internal static class HeuristicDetector
{
    private const string Module = "tables";

    /// <summary>
    /// Merges adjacent items on the same line into words. PDF text is often
    /// emitted one glyph at a time, and hundreds of single-character items
    /// confuse column detection.
    /// </summary>
    /// <returns>
    /// The merged items, and for each of them the original indices it absorbed.
    /// </returns>
    public static (List<TextItem> Merged, List<List<int>> IndexMap) MergeAdjacentItems(
        IReadOnlyList<TextItem> items)
    {
        if (items.Count == 0)
        {
            return ([], []);
        }

        const float YTolerance = 5.0f;
        var lineGroups = new List<(float Y, List<(int Index, TextItem Item)> Group)>();

        for (var idx = 0; idx < items.Count; idx++)
        {
            var item = items[idx];
            var placed = false;

            foreach (var (y, group) in lineGroups)
            {
                if (MathF.Abs(item.Y - y) < YTolerance)
                {
                    group.Add((idx, item));
                    placed = true;
                    break;
                }
            }

            if (!placed)
            {
                lineGroups.Add((item.Y, [(idx, item)]));
            }
        }

        foreach (var (_, group) in lineGroups)
        {
            group.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X));
        }

        // Top of page first.
        lineGroups.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));

        var mergedItems = new List<TextItem>();
        var indexMap = new List<List<int>>();

        foreach (var (_, group) in lineGroups)
        {
            var i = 0;
            while (i < group.Count)
            {
                var (firstIdx, firstItem) = group[i];
                var text = new StringBuilder(firstItem.Text);
                var endX = firstItem.X + firstItem.Width;
                var indices = new List<int> { firstIdx };
                var xGapMax = firstItem.FontSize * 0.5f;

                var j = i + 1;
                while (j < group.Count)
                {
                    var (nextIdx, nextItem) = group[j];

                    if (MathF.Abs(nextItem.FontSize - firstItem.FontSize) > firstItem.FontSize * 0.20f)
                    {
                        break;
                    }

                    var gap = nextItem.X - endX;

                    // A gap this wide is an inter-column boundary.
                    if (gap > xGapMax)
                    {
                        break;
                    }

                    // A large overlap means a different column drawn over this one.
                    if (gap < -firstItem.FontSize * 0.5f)
                    {
                        break;
                    }

                    // Within a word the characters touch; between words a gap shows.
                    if (gap > firstItem.FontSize * 0.08f)
                    {
                        text.Append(' ');
                    }

                    text.Append(nextItem.Text);
                    endX = nextItem.X + nextItem.Width;
                    indices.Add(nextIdx);
                    j++;
                }

                var merged = firstItem.Clone();
                merged.Text = text.ToString();
                merged.Width = endX - firstItem.X;

                mergedItems.Add(merged);
                indexMap.Add(indices);

                i = j;
            }
        }

        return (mergedItems, indexMap);
    }

    /// <summary>Expands consolidated financial items, recording each result's source index.</summary>
    private static (List<TextItem> Expanded, List<int> IndexMap) ExpandConsolidatedItems(
        IReadOnlyList<TextItem> items)
    {
        var expanded = new List<TextItem>(items.Count);
        var indexMap = new List<int>(items.Count);

        for (var origIdx = 0; origIdx < items.Count; origIdx++)
        {
            if (Financial.TrySplitFinancialItem(items[origIdx]) is { } subItems)
            {
                foreach (var sub in subItems)
                {
                    expanded.Add(sub);
                    indexMap.Add(origIdx);
                }
            }
            else
            {
                expanded.Add(items[origIdx]);
                indexMap.Add(origIdx);
            }
        }

        return (expanded, indexMap);
    }

    /// <summary>Detects tables among one page's text items.</summary>
    /// <param name="skipBodyFont">
    /// Set on multi-column pages, where the body-font pass produces false positives.
    /// </param>
    public static List<Table> DetectTables(IReadOnlyList<TextItem> items, float baseFontSize, bool skipBodyFont)
    {
        if (items.Count < 6)
        {
            return [];
        }

        var (mergedItems, mergeMap) = MergeAdjacentItems(items);
        var (expandedItems, expandMap) = ExpandConsolidatedItems(mergedItems);

        var tables = new List<Table>();
        var claimedIndices = new HashSet<int>();

        // Pass one: tables set below the body font, the usual case.
        var tableFontThreshold = baseFontSize * 0.90f;

        var tableCandidates = new List<(int Index, TextItem Item)>();
        for (var i = 0; i < expandedItems.Count; i++)
        {
            var item = expandedItems[i];
            if (item.FontSize <= tableFontThreshold && item.FontSize >= 6.0f)
            {
                tableCandidates.Add((i, item));
            }
        }

        if (tableCandidates.Count >= 6)
        {
            foreach (var (yMin, yMax) in FindTableRegions(tableCandidates))
            {
                var regionItems = tableCandidates
                    .Where(e => e.Item.Y >= yMin && e.Item.Y <= yMax)
                    .ToList();

                if (regionItems.Count < 6)
                {
                    continue;
                }

                if (DetectTableInRegion(regionItems, TableDetectionMode.SmallFont) is not { } table)
                {
                    continue;
                }

                Grid.RecoverHeaderRow(table, expandedItems, tableFontThreshold);
                TryAddLabelColumn(table, tableCandidates, claimedIndices, yMin, yMax);

                foreach (var idx in table.ItemIndices)
                {
                    claimedIndices.Add(idx);
                }

                tables.Add(table);
            }
        }

        // Pass two: tables set at the body font, which needs stricter criteria.
        if (!skipBodyFont)
        {
            var bodyFontLow = baseFontSize * 0.85f;
            var bodyFontHigh = baseFontSize * 1.05f;

            var bodyCandidates = new List<(int Index, TextItem Item)>();
            for (var i = 0; i < expandedItems.Count; i++)
            {
                var item = expandedItems[i];
                if (!claimedIndices.Contains(i)
                    && item.FontSize >= bodyFontLow
                    && item.FontSize <= bodyFontHigh
                    && item.FontSize >= 6.0f)
                {
                    bodyCandidates.Add((i, item));
                }
            }

            Log.Debug(Module, () =>
                $"body-font pass: {bodyCandidates.Count} candidates (base={baseFontSize:F1}, " +
                $"range={bodyFontLow:F1}..{bodyFontHigh:F1})");

            if (bodyCandidates.Count >= 6)
            {
                var regions = FindTableRegionsStrict(bodyCandidates);
                Log.Debug(Module, () => $"body-font: {regions.Count} strict regions found");

                foreach (var (yMin, yMax, _, _) in regions)
                {
                    // The full x range is used: the strict bounds from qualifying
                    // rows can exclude continuation lines in wrapped cells, and
                    // the y bounds already scope the table.
                    var regionItems = bodyCandidates
                        .Where(e => e.Item.Y >= yMin && e.Item.Y <= yMax)
                        .ToList();

                    Log.Debug(Module, () =>
                        $"  region y={yMin:F0}..{yMax:F0}: {regionItems.Count} items of {bodyCandidates.Count} candidates");

                    if (regionItems.Count < 6)
                    {
                        continue;
                    }

                    if (DetectTableInRegion(regionItems, TableDetectionMode.BodyFont) is { } table)
                    {
                        tables.Add(table);
                    }
                }
            }
        }

        // Indices map back through the expansion and the merge to the caller's items.
        foreach (var table in tables)
        {
            var originalIndices = new HashSet<int>();
            foreach (var expIdx in table.ItemIndices)
            {
                foreach (var origIdx in mergeMap[expandMap[expIdx]])
                {
                    originalIndices.Add(origIdx);
                }
            }

            table.ItemIndices.Clear();
            table.ItemIndices.AddRange(originalIndices.Order());

            Log.Debug(Module, () =>
                $"  heuristic table: {table.Rows.Count}x{table.Columns.Count}, {table.ItemIndices.Count} item indices");
        }

        return tables;
    }

    /// <summary>Clusters candidate y positions into the bands that may hold tables.</summary>
    private static List<(float YMin, float YMax)> FindTableRegions(List<(int Index, TextItem Item)> items)
    {
        if (items.Count == 0)
        {
            return [];
        }

        var yPositions = items.Select(e => e.Item.Y).ToList();
        yPositions.Sort(FloatTotalOrder.Instance);

        var regions = new List<(float, float)>();

        // A small threshold, so a header separates from its content.
        const float GapThreshold = 30.0f;

        var regionStart = yPositions[0];
        var regionEnd = yPositions[0];
        var regionCount = 1;

        for (var i = 1; i < yPositions.Count; i++)
        {
            var y = yPositions[i];
            if (y - regionEnd > GapThreshold)
            {
                if (regionCount >= 4)
                {
                    regions.Add((regionStart - 5.0f, regionEnd + 5.0f));
                }

                regionStart = y;
                regionEnd = y;
                regionCount = 1;
            }
            else
            {
                regionEnd = y;
                regionCount++;
            }
        }

        if (regionCount >= 4)
        {
            regions.Add((regionStart - 5.0f, regionEnd + 5.0f));
        }

        return regions;
    }

    /// <summary>
    /// Finds regions for body-font candidates using structural criteria: rows
    /// need several distinct x clusters, and those clusters must line up across
    /// rows. Tables hold their columns fixed; paragraph text does not.
    /// </summary>
    private static List<(float YMin, float YMax, float XMin, float XMax)> FindTableRegionsStrict(
        List<(int Index, TextItem Item)> items)
    {
        if (items.Count == 0)
        {
            return [];
        }

        var rowGroups = new List<(float Center, List<float> XPositions)>();

        foreach (var (_, item) in items)
        {
            var found = false;
            foreach (var (center, xPositions) in rowGroups)
            {
                if (MathF.Abs(item.Y - center) < 8.0f)
                {
                    xPositions.Add(item.X);
                    found = true;
                    break;
                }
            }

            if (!found)
            {
                rowGroups.Add((item.Y, [item.X]));
            }
        }

        var qualifyingRows = new List<(float Y, List<float> ClusterStarts)>();

        foreach (var (y, xPositions) in rowGroups)
        {
            var sortedXs = new List<float>(xPositions);
            sortedXs.Sort(FloatTotalOrder.Instance);

            if (sortedXs.Count == 0)
            {
                continue;
            }

            var clusterStarts = new List<float> { sortedXs[0] };
            var lastX = sortedXs[0];

            for (var i = 1; i < sortedXs.Count; i++)
            {
                if (sortedXs[i] - lastX > 20.0f)
                {
                    clusterStarts.Add(sortedXs[i]);
                    lastX = sortedXs[i];
                }
            }

            if (clusterStarts.Count >= 2)
            {
                qualifyingRows.Add((y, clusterStarts));
            }
        }

        Log.Debug(Module, () =>
            $"find_table_regions_strict: {rowGroups.Count} row groups, " +
            $"{qualifyingRows.Count} qualifying (2+ X-clusters)");

        if (qualifyingRows.Count < 3)
        {
            return [];
        }

        qualifyingRows.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Y, b.Y));

        // An adaptive gap handles wrapped cells, where qualifying rows sit
        // further apart than the nominal line pitch.
        var maxGap = 25.0f;
        if (qualifyingRows.Count >= 3)
        {
            var gaps = new List<float>();
            for (var i = 0; i + 1 < qualifyingRows.Count; i++)
            {
                gaps.Add(MathF.Abs(qualifyingRows[i + 1].Y - qualifyingRows[i].Y));
            }

            gaps.Sort(FloatTotalOrder.Instance);
            maxGap = MathF.Max(gaps[gaps.Count / 2] * 3.0f, 25.0f);
        }

        var candidateRegions = new List<List<(float Y, List<float> ClusterStarts)>>();
        var currentRegion = new List<(float Y, List<float> ClusterStarts)> { qualifyingRows[0] };

        for (var i = 1; i < qualifyingRows.Count; i++)
        {
            var row = qualifyingRows[i];
            if (row.Y - currentRegion[^1].Y > maxGap)
            {
                if (currentRegion.Count >= 3)
                {
                    candidateRegions.Add(currentRegion);
                }

                currentRegion = [row];
            }
            else
            {
                currentRegion.Add(row);
            }
        }

        if (currentRegion.Count >= 3)
        {
            candidateRegions.Add(currentRegion);
        }

        // A real table's column positions agree across rows; paragraph text has
        // varying word positions line to line.
        var regions = new List<(float, float, float, float)>();

        foreach (var regionRows in candidateRegions)
        {
            var totalScore = 0.0f;
            var pairCount = 0u;
            const float Tolerance = 10.0f;

            for (var i = 0; i < regionRows.Count; i++)
            {
                for (var j = i + 1; j < regionRows.Count; j++)
                {
                    var centersA = regionRows[i].ClusterStarts;
                    var centersB = regionRows[j].ClusterStarts;

                    var matchesA = centersA.Count(a => centersB.Any(b => MathF.Abs(a - b) < Tolerance));
                    var matchesB = centersB.Count(b => centersA.Any(a => MathF.Abs(a - b) < Tolerance));

                    var maxLen = Math.Max(centersA.Count, centersB.Count);
                    if (maxLen > 0)
                    {
                        totalScore += (float)(matchesA + matchesB) / (2 * maxLen);
                        pairCount++;
                    }
                }
            }

            var avgScore = pairCount > 0 ? totalScore / pairCount : 0.0f;
            Log.Debug(Module, () =>
                $"  candidate region: {regionRows.Count} rows, avg alignment score={avgScore:F2}");

            if (avgScore < 0.5f)
            {
                continue;
            }

            var yMin = regionRows[0].Y;
            var yMax = regionRows[^1].Y;
            var xMin = regionRows.SelectMany(r => r.ClusterStarts).Min();
            var xMax = regionRows.SelectMany(r => r.ClusterStarts).Max();

            regions.Add((yMin - 5.0f, yMax + 5.0f, xMin - 15.0f, xMax + 50.0f));
        }

        return regions;
    }

    /// <summary>Builds and validates a table from the items in one region.</summary>
    private static Table? DetectTableInRegion(List<(int Index, TextItem Item)> items, TableDetectionMode mode)
    {
        var columns = Grid.FindColumnBoundaries(items, mode);
        if (columns.Count is < 2 or > 25)
        {
            Log.Debug(Module, () => $"  detect_table_in_region: rejected {columns.Count} cols (need 2..25)");
            return null;
        }

        var rows = Grid.FindRowBoundaries(items);
        if (rows.Count < 2)
        {
            Log.Debug(Module, () => $"  detect_table_in_region: rejected {rows.Count} rows (need 2+)");
            return null;
        }

        Log.Debug(Module, () =>
            $"  detect_table_in_region: {columns.Count} cols, {rows.Count} rows, {items.Count} items");

        var colAlignment = CheckColumnAlignment(items, columns, mode);
        var minAlignment = mode == TableDetectionMode.SmallFont ? 0.5f : 0.7f;

        if (colAlignment < minAlignment)
        {
            Log.Debug(Module, () =>
                $"  detect_table_in_region: rejected alignment {colAlignment:F2} < {minAlignment:F2} " +
                $"({columns.Count} cols, {rows.Count} rows)");
            return null;
        }

        var cellItems = new List<List<List<TextItem>>>();
        for (var r = 0; r < rows.Count; r++)
        {
            var row = new List<List<TextItem>>();
            for (var c = 0; c < columns.Count; c++)
            {
                row.Add([]);
            }

            cellItems.Add(row);
        }

        var itemIndices = new List<int>();

        foreach (var (idx, item) in items)
        {
            var col = Grid.FindColumnIndex(columns, item.X);
            var row = Grid.FindRowIndex(rows, item.Y);

            if (col is { } c && row is { } r)
            {
                cellItems[r][c].Add(item);
                itemIndices.Add(idx);
            }
        }

        // Leading form-metadata rows are identified before the indices settle.
        var (firstTableRow, excludedItems) = FindFirstTableRow(cellItems, rows, items);

        itemIndices = itemIndices.Where(idx => !excludedItems.Contains(idx)).ToList();

        if (firstTableRow > 0)
        {
            rows = rows[firstTableRow..];
            cellItems = cellItems[firstTableRow..];
        }

        var cells = new List<List<string>>(rows.Count);
        foreach (var rowItems in cellItems)
        {
            var rowCells = new List<string>(columns.Count);
            foreach (var colItems in rowItems)
            {
                var rtl = TextUtils.IsRtlText(colItems.Select(i => i.Text));
                colItems.Sort((a, b) => rtl
                    ? FloatTotalOrder.Instance.Compare(b.X, a.X)
                    : FloatTotalOrder.Instance.Compare(a.X, b.X));

                rowCells.Add(Grid.JoinCellItems(colItems));
            }

            cells.Add(rowCells);
        }

        // Hierarchical contents entries indent across several x levels, leaving
        // the leftmost column sparse, so a narrow contents page is exempt from
        // the first-column and paragraph checks. Narrow only: a wide two-up
        // index renders poorly as a flat list.
        var isNarrowToc = columns.Count <= 5 && TableOfContents.IsTableOfContents(cells);

        var rowsWithFirstCol = cells.Count(row => row[0].Length > 0);
        if (rowsWithFirstCol < rows.Count / 4 && !isNarrowToc)
        {
            Log.Debug(Module, () =>
                $"  validation 1 fail: {rowsWithFirstCol}/{rows.Count} rows have first col");
            return null;
        }

        var rowsWithMultiCols = cells.Count(row => row.Count(c => c.Length > 0) >= 2);
        var multiColThreshold = mode == TableDetectionMode.SmallFont
            ? Math.Max(rows.Count / 3, 1)
            : Math.Max(rows.Count / 2, 1);

        if (rowsWithMultiCols < multiColThreshold)
        {
            Log.Debug(Module, () =>
                $"  validation 2 fail: {rowsWithMultiCols}/{rows.Count} rows multi-col (need {multiColThreshold})");
            return null;
        }

        if (rows.Count > 200)
        {
            return null;
        }

        var totalFilled = cells.Sum(row => row.Count(c => c.Length > 0));
        var avgCellsPerRow = (float)totalFilled / rows.Count;
        if (avgCellsPerRow < 1.5f)
        {
            Log.Debug(Module, () => $"  validation 4 fail: avg_cells={avgCellsPerRow:F1} < 1.5");
            return null;
        }

        if (IsKeyValueLayout(cells))
        {
            Log.Debug(Module, "  validation 5 fail: key-value layout");
            return null;
        }

        if (!HasConsistentColumns(cells))
        {
            Log.Debug(Module, "  validation 6 fail: inconsistent columns");
            return null;
        }

        if (!HasTableLikeContent(cells, mode))
        {
            Log.Debug(Module, "  validation 7 fail: no table-like content");
            return null;
        }

        if (IsParagraphContent(cells) && !isNarrowToc)
        {
            Log.Debug(Module, "  validation 8 fail: paragraph content");
            return null;
        }

        // A wide index where every cell carries a "label ... page" fragment
        // renders poorly in any structured form; text flow is the best fallback.
        if (TableOfContents.IsInlineLeaderIndex(cells))
        {
            Log.Debug(Module, "  validation 9 fail: inline-leader index");
            return null;
        }

        Log.Debug(Module, () =>
            $"table detected: {rows.Count} rows x {columns.Count} cols, {itemIndices.Count} items");

        return Table.Create(columns, rows, cells, itemIndices);
    }

    /// <summary>True when the cells read as label/value pairs rather than a table.</summary>
    private static bool IsKeyValueLayout(List<List<string>> cells)
    {
        if (cells.Count == 0)
        {
            return false;
        }

        var numCols = cells[0].Count;
        var labelLikeFirstCol = 0;
        var rowsWithTwoOrLess = 0;

        foreach (var row in cells)
        {
            if (row.Count(c => c.Length > 0) <= 2)
            {
                rowsWithTwoOrLess++;
            }

            var first = row.Count > 0 ? row[0].Trim() : string.Empty;
            if (first.EndsWith(':')
                || (first.Length > 3 && first.All(c => char.IsUpper(c) || char.IsWhiteSpace(c) || c is '(' or ')')))
            {
                labelLikeFirstCol++;
            }
        }

        var pctTwoOrLess = (float)rowsWithTwoOrLess / cells.Count;
        var pctLabelLike = (float)labelLikeFirstCol / cells.Count;

        return pctTwoOrLess > 0.7f && pctLabelLike > 0.5f && numCols <= 6;
    }

    /// <summary>True when the filled-column count is consistent enough across rows.</summary>
    private static bool HasConsistentColumns(List<List<string>> cells)
    {
        if (cells.Count < 3)
        {
            return true;
        }

        var filledCounts = cells.Select(row => row.Count(c => c.Length > 0)).ToList();

        var countFreq = new Dictionary<int, int>();
        foreach (var count in filledCounts)
        {
            countFreq[count] = countFreq.GetValueOrDefault(count) + 1;
        }

        // Ties favour the higher column count, for deterministic output.
        var mostCommonCount = 0;
        var bestFreq = -1;
        foreach (var (count, freq) in countFreq)
        {
            if (freq > bestFreq || (freq == bestFreq && count > mostCommonCount))
            {
                bestFreq = freq;
                mostCommonCount = count;
            }
        }

        // Very wide tables have inherently variable fill, so they get a wider
        // tolerance and a lower ratio.
        var numCols = cells[0].Count;
        var tolerance = numCols > 15 ? numCols / 4 : 2;

        var consistentRows = filledCounts.Count(c =>
            c >= Math.Max(mostCommonCount - tolerance, 0) && c <= mostCommonCount + tolerance);

        var minRatio = numCols > 15 ? 0.25f : 0.40f;
        return (float)consistentRows / cells.Count > minRatio;
    }

    /// <summary>True when the cells carry the numeric or short-value content tables hold.</summary>
    private static bool HasTableLikeContent(List<List<string>> cells, TableDetectionMode mode)
    {
        var dataLikeCells = 0;
        var totalCells = 0;

        // The header row is skipped.
        foreach (var row in cells.Skip(1))
        {
            foreach (var cell in row)
            {
                var trimmed = cell.Trim();
                if (trimmed.Length == 0)
                {
                    continue;
                }

                totalCells++;
                if (LooksLikeTableData(trimmed))
                {
                    dataLikeCells++;
                }
            }
        }

        if (totalCells == 0)
        {
            return false;
        }

        var pctData = (float)dataLikeCells / totalCells;
        var numCols = cells.Count > 0 ? cells[0].Count : 0;
        var minPct = mode == TableDetectionMode.SmallFont ? 0.2f : 0.3f;

        // Wide tables skip the content check: a text-only category or program
        // list is legitimate once it has passed the structural validations.
        if (pctData > minPct || numCols >= 3)
        {
            return true;
        }

        // A two-column body-font table with short cells is a definition list
        // rather than paragraph text.
        if (numCols == 2 && mode == TableDetectionMode.BodyFont)
        {
            var lengths = cells
                .Skip(1)
                .SelectMany(row => row)
                .Where(c => c.Trim().Length > 0)
                .Select(c => c.Trim().Length)
                .ToList();

            if (lengths.Count > 0)
            {
                return lengths.Sum() / lengths.Count <= 25;
            }
        }

        return false;
    }

    /// <summary>True when a cell value reads as table data: a number, code, date, or specification.</summary>
    private static bool LooksLikeTableData(string s)
    {
        s = s.Trim();
        if (s.Length == 0)
        {
            return false;
        }

        if (LooksLikeNumber(s))
        {
            return true;
        }

        // Dates in any of the common orders.
        if (s.Length <= 10
            && s.Count(char.IsAsciiDigit) >= 4
            && (s.Contains('/') || s.Contains('-'))
            && s.All(c => char.IsAsciiDigit(c) || c is '/' or '-'))
        {
            return true;
        }

        // Part numbers and model codes: short alphanumerics carrying a digit.
        if (s.Length <= 10 && s.All(char.IsLetterOrDigit) && s.Any(char.IsAsciiDigit))
        {
            return true;
        }

        // A measurement with a unit.
        var hasNumber = s.Any(char.IsAsciiDigit);
        var hasUnit = s.Contains('°')
            || s.Contains('V')
            || s.Contains('A')
            || s.Contains("Hz", StringComparison.Ordinal)
            || s.Contains("mA", StringComparison.Ordinal)
            || s.Contains('µ')
            || s.Contains("pin", StringComparison.Ordinal)
            || s.Contains("MHz", StringComparison.Ordinal)
            || s.Contains("kHz", StringComparison.Ordinal);

        if (hasNumber && hasUnit)
        {
            return true;
        }

        // A package designation such as "D (SOIC, 8)".
        if (s.Contains('(') && s.Contains(')') && s.Any(char.IsAsciiDigit))
        {
            return true;
        }

        // A temperature range.
        return (s.Contains("°C", StringComparison.Ordinal) || s.Contains("°F", StringComparison.Ordinal))
            && s.Contains("to", StringComparison.Ordinal);
    }

    private static bool LooksLikeNumber(string s)
    {
        s = s.Trim();
        return s.Length > 0
            && s.All(c => char.IsAsciiDigit(c) || c is '.' or ',' or '-' or '+')
            && s.Any(char.IsAsciiDigit);
    }

    /// <summary>
    /// True when the cells are really paragraph fragments. Multi-column prose
    /// falsely read as a table leaves many empty cells, hyphenated word breaks
    /// across "columns", and long sentence fragments.
    /// </summary>
    private static bool IsParagraphContent(List<List<string>> cells)
    {
        if (cells.Count == 0)
        {
            return false;
        }

        var numCols = cells[0].Count;
        var totalCells = cells.Count * numCols;
        if (totalCells == 0)
        {
            return false;
        }

        var filled = cells
            .SelectMany(r => r)
            .Select(c => c.Trim())
            .Where(c => c.Length > 0)
            .ToList();

        if (filled.Count < 4)
        {
            return false;
        }

        var emptyRatio = 1.0f - ((float)filled.Count / totalCells);

        // A cell ending in a hyphen after a letter is a word break across
        // columns; real cells almost never do that.
        var hyphenBreaks = filled.Count(c => c.Length > 1 && c.EndsWith('-') && char.IsLetter(c[^2]));

        if ((float)hyphenBreaks / filled.Count > 0.03f)
        {
            return true;
        }

        if (emptyRatio > 0.55f && cells.Count > 10)
        {
            return true;
        }

        // Letter-spaced text — a space between every character — is never table
        // data. Nine characters minimum, so short codes do not match.
        var letterSpaced = filled.Count(c =>
        {
            if (c.Length < 9)
            {
                return false;
            }

            for (var i = 0; i + 3 < c.Length; i++)
            {
                var a = char.IsLetter(c[i]) && c[i + 1] == ' ' && char.IsLetter(c[i + 2]) && c[i + 3] == ' ';
                var b = c[i] == ' ' && char.IsLetter(c[i + 1]) && c[i + 2] == ' ' && char.IsLetter(c[i + 3]);
                if (!a && !b)
                {
                    return false;
                }
            }

            return true;
        });

        if (letterSpaced > 0 && (float)letterSpaced / filled.Count > 0.08f)
        {
            return true;
        }

        var longCells = filled.Count(c => c.Length > 60);
        var longRatio = (float)longCells / filled.Count;
        var avgLen = (float)filled.Sum(c => c.Length) / filled.Count;

        return (avgLen > 40.0f && longRatio > 0.2f) || longRatio > 0.3f;
    }

    /// <summary>The fraction of items that sit at a detected column position.</summary>
    private static float CheckColumnAlignment(
        List<(int Index, TextItem Item)> items,
        List<float> columns,
        TableDetectionMode mode)
    {
        var tolerance = mode == TableDetectionMode.SmallFont ? 40.0f : 30.0f;
        var aligned = items.Count(e => columns.Any(col => MathF.Abs(e.Item.X - col) < tolerance));
        return (float)aligned / items.Count;
    }

    /// <summary>
    /// Finds the first row that is genuinely table content rather than form
    /// metadata, and the item indices belonging to the rows skipped.
    /// </summary>
    public static (int FirstRow, HashSet<int> Excluded) FindFirstTableRow(
        List<List<List<TextItem>>> cellItems,
        List<float> rows,
        List<(int Index, TextItem Item)> originalItems)
    {
        var excludedItems = new HashSet<int>();

        var cells = cellItems
            .Select(row => row.Select(Grid.JoinCellItems).ToList())
            .ToList();

        if (cells.Count == 0)
        {
            return (0, excludedItems);
        }

        var totalCols = cells[0].Count;
        var firstTableRow = 0;

        static bool IsFormCell(string cell)
        {
            var text = cell.Trim();
            return (text.EndsWith(':') && text.Length > 1)
                || (text.Contains(": ", StringComparison.Ordinal) && !LooksLikeNumber(text));
        }

        for (var rowIdx = 0; rowIdx < cells.Count; rowIdx++)
        {
            var row = cells[rowIdx];
            var filledCells = row.Where(c => c.Trim().Length > 0).ToList();
            var filledCount = filledCells.Count;
            var fillRatio = (float)filledCount / totalCols;

            // A form row is one where most filled cells look like labels, or a
            // very sparse row with any such cell.
            var formCellCount = filledCells.Count(IsFormCell);
            var hasFormPatterns = formCellCount > 0 && (formCellCount * 2 >= filledCount || fillRatio < 0.3f);

            var numericCount = filledCells.Count(c => LooksLikeNumber(c.Trim()));
            var hasData = numericCount >= 2;

            if (hasFormPatterns)
            {
                continue;
            }

            // A row with duplicate cells is a spanning super-header sitting above
            // the real column headers. Using it would produce duplicate column
            // names, so it is skipped when a better candidate follows.
            if (filledCount >= 2 && !hasData)
            {
                var textCounts = new Dictionary<string, int>(StringComparer.Ordinal);
                foreach (var cell in filledCells)
                {
                    var key = cell.Trim();
                    textCounts[key] = textCounts.GetValueOrDefault(key) + 1;
                }

                if (textCounts.Values.Any(count => count >= 2))
                {
                    var hasBetterBelow = cells.Skip(rowIdx + 1).Take(3).Any(r =>
                    {
                        var nextFilled = r.Count(c => c.Trim().Length > 0);
                        var nextFill = (float)nextFilled / totalCols;
                        var nextNumeric = r.Count(c => LooksLikeNumber(c.Trim()));
                        return nextFill >= 0.4f || nextNumeric >= 2;
                    });

                    if (hasBetterBelow)
                    {
                        continue;
                    }
                }
            }

            if (hasData)
            {
                firstTableRow = rowIdx;
                break;
            }

            // A dense row with no form pattern is a table header.
            if (fillRatio >= 0.4f)
            {
                firstTableRow = rowIdx;
                break;
            }

            if (fillRatio < 0.3f)
            {
                continue;
            }

            // A moderately sparse row could be part of a multi-line header; the
            // row below decides.
            if (rowIdx + 1 < cells.Count)
            {
                var nextRow = cells[rowIdx + 1];
                var nextFilled = nextRow.Count(c => c.Trim().Length > 0);
                var nextFillRatio = (float)nextFilled / totalCols;
                var nextHasForm = nextRow.Any(IsFormCell);

                if ((nextFillRatio >= 0.4f || nextRow.Count(c => LooksLikeNumber(c.Trim())) >= 2) && !nextHasForm)
                {
                    firstTableRow = rowIdx;
                    break;
                }
            }
        }

        if (firstTableRow > 0)
        {
            const float YTolerance = 15.0f;
            foreach (var (idx, item) in originalItems)
            {
                foreach (var rowY in rows.Take(firstTableRow))
                {
                    if (MathF.Abs(item.Y - rowY) < YTolerance)
                    {
                        excludedItems.Add(idx);
                        break;
                    }
                }
            }
        }

        return (firstTableRow, excludedItems);
    }

    /// <summary>
    /// Recovers a label column for a numeric-only table. Balance sheets put row
    /// descriptions to the left of their figures, and indentation makes those
    /// labels' x positions too varied to cluster into a column of their own.
    /// </summary>
    private static void TryAddLabelColumn(
        Table table,
        List<(int Index, TextItem Item)> allCandidates,
        HashSet<int> claimedIndices,
        float yMin,
        float yMax)
    {
        if (table.Columns.Count is < 2 or > 3 || table.Rows.Count < 5)
        {
            return;
        }

        // The table must be predominantly numeric, with no text labels anywhere.
        var numericCells = table.Cells.SelectMany(row => row).Count(cell =>
        {
            var text = cell.Trim();
            if (text.Length == 0)
            {
                return false;
            }

            var dataChars = text.Count(c => char.IsAsciiDigit(c) || ",.-+%€$£¥()".Contains(c, StringComparison.Ordinal));
            return (float)dataChars / text.Length >= 0.6f;
        });

        var totalNonEmpty = table.Cells.SelectMany(row => row).Count(c => c.Trim().Length > 0);
        if (totalNonEmpty == 0 || (float)numericCells / totalNonEmpty < 0.7f)
        {
            return;
        }

        var tableXMin = table.Columns.Count > 0 ? table.Columns[0] : float.MaxValue;
        const float YTol = 5.0f;

        var labelItemsPerRow = new List<List<(int Index, TextItem Item)>>();
        var foundCount = 0;

        foreach (var rowY in table.Rows)
        {
            var rowLabels = allCandidates
                .Where(e => !claimedIndices.Contains(e.Index)
                    && !table.ItemIndices.Contains(e.Index)
                    && MathF.Abs(e.Item.Y - rowY) < YTol
                    && e.Item.X < tableXMin - 10.0f
                    && e.Item.Y >= yMin
                    && e.Item.Y <= yMax)
                .OrderBy(e => e.Item.X, FloatTotalOrder.Instance)
                .ToList();

            if (rowLabels.Count > 0)
            {
                foundCount++;
            }

            labelItemsPerRow.Add(rowLabels);
        }

        // At least two rows in five must carry a label.
        if (foundCount < table.Rows.Count * 2 / 5)
        {
            return;
        }

        Log.Debug(Module, () =>
            $"recovering label column: {foundCount}/{table.Rows.Count} rows have labels to the left");

        var labelColX = labelItemsPerRow.SelectMany(items => items.Select(e => e.Item.X)).DefaultIfEmpty(0f).Min();

        table.Columns.Insert(0, labelColX);
        for (var rowIdx = 0; rowIdx < labelItemsPerRow.Count; rowIdx++)
        {
            var labelText = string.Join(" ", labelItemsPerRow[rowIdx].Select(e => e.Item.Text));
            table.Cells[rowIdx].Insert(0, labelText);

            foreach (var (idx, _) in labelItemsPerRow[rowIdx])
            {
                table.ItemIndices.Add(idx);
            }
        }
    }
}
