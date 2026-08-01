// Ported from reference/src/tables/grid.rs
using System.Text;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Column and row boundary detection, plus cell assignment, for heuristically
/// detected tables.
/// </summary>
internal static class Grid
{
    private const string Module = "tables";

    /// <summary>
    /// Clusters item x positions into column boundaries. The clustering
    /// threshold adapts: a bimodal gap distribution means densely packed
    /// columns, where a lower edge-based threshold separates them correctly
    /// without over-splitting wide ones.
    /// </summary>
    public static List<float> FindColumnBoundaries(
        List<(int Index, TextItem Item)> items,
        TableDetectionMode mode)
    {
        var xPositions = items.Select(e => e.Item.X).ToList();
        xPositions.Sort(FloatTotalOrder.Instance);

        if (xPositions.Count == 0)
        {
            return [];
        }

        var xRange = xPositions[^1] - xPositions[0];
        var avgGap = xPositions.Count > 1 ? xRange / (xPositions.Count - 1) : 60.0f;

        var clusterThreshold = Math.Clamp(avgGap, 25.0f, 50.0f);
        var useEdgeClustering = false;

        // Consecutive gaps separate into within-column jitter and
        // between-column spacing; the largest jump in the sorted sequence is
        // the natural break between the two.
        var consecGaps = new List<float>();
        for (var i = 0; i + 1 < xPositions.Count; i++)
        {
            var gap = xPositions[i + 1] - xPositions[i];
            if (gap > 0.1f)
            {
                consecGaps.Add(gap);
            }
        }

        if (consecGaps.Count > 2)
        {
            consecGaps.Sort(FloatTotalOrder.Instance);

            var bestSplit = consecGaps.Count / 2;
            var bestJump = 0.0f;

            // At least three values on each side, so a single wide page-margin
            // gap cannot dominate the split.
            var minSide = Math.Min(3, consecGaps.Count / 2);

            for (var i = 0; i + 1 < consecGaps.Count; i++)
            {
                var leftCount = i + 1;
                var rightCount = consecGaps.Count - i - 1;
                if (leftCount < minSide || rightCount < minSide)
                {
                    continue;
                }

                var jump = consecGaps[i + 1] - consecGaps[i];
                if (jump > bestJump)
                {
                    bestJump = jump;
                    bestSplit = i;
                }
            }

            var threshold = (consecGaps[bestSplit] + consecGaps[Math.Min(bestSplit + 1, consecGaps.Count - 1)]) / 2.0f;

            if (threshold < 15.0f && bestJump > 2.0f && xPositions.Count > 500)
            {
                // A dense table, such as a many-column schedule: edge-based
                // clustering avoids the centre drift that merges narrow columns.
                clusterThreshold = Math.Clamp(threshold, 8.0f, 25.0f);
                useEdgeClustering = true;
            }
            else if (bestJump > 10.0f && threshold < clusterThreshold)
            {
                // An unambiguous bimodal signal with fewer items: lower the
                // threshold but keep centre-based clustering.
                clusterThreshold = MathF.Max(threshold, 8.0f);
            }
        }

        List<List<float>> clusterXs = [[xPositions[0]]];

        for (var i = 1; i < xPositions.Count; i++)
        {
            var x = xPositions[i];
            var lastCluster = clusterXs[^1];
            var reference = useEdgeClustering ? lastCluster[^1] : lastCluster.SumF32() / lastCluster.Count;

            if (x - reference > clusterThreshold)
            {
                clusterXs.Add([x]);
            }
            else
            {
                lastCluster.Add(x);
            }
        }

        // Multi-line wrapped headers sit at slightly different x positions than
        // their data, splitting one logical column into two clusters.
        var columnsBeforeMerge = clusterXs.Count;
        if (columnsBeforeMerge >= 3)
        {
            clusterXs = MergeNumericAdjacentClusters(clusterXs, items, clusterThreshold);
        }

        var columns = clusterXs.Select(xs => xs.SumF32() / xs.Count).ToList();

        // Each column must carry more than a stray item.
        var minItemsPerCol = Math.Max(items.Count / Math.Max(columns.Count, 1) / 4, 2);
        columns = columns
            .Where(colX => items.Count(e => MathF.Abs(e.Item.X - colX) < clusterThreshold) >= minItemsPerCol)
            .ToList();

        Log.Debug(Module, () =>
            $"  find_column_boundaries: {columns.Count} columns (merged from {columnsBeforeMerge}), " +
            $"threshold={clusterThreshold:F1}, {items.Count} items");

        // Paragraphs concentrate items at the left margin while tables spread
        // them evenly, so a column holding most of the page is prose.
        if (mode == TableDetectionMode.BodyFont)
        {
            foreach (var colX in columns)
            {
                var count = items.Count(e => MathF.Abs(e.Item.X - colX) < clusterThreshold);
                if ((float)count / items.Count > 0.60f)
                {
                    return [];
                }
            }
        }

        return columns;
    }

    /// <summary>True when the text reads as a number: digits with optional sign, separators, or percent.</summary>
    private static bool IsNumericText(string s)
    {
        s = s.Trim();
        if (s.Length == 0)
        {
            return false;
        }

        return s.All(c => char.IsAsciiDigit(c) || c is '.' or ',' or '-' or '+' or '%')
            && s.Any(char.IsAsciiDigit);
    }

    /// <summary>
    /// Merges a sparse header cluster into the dense numeric data cluster beside
    /// it, so a wrapped header does not split its column in two.
    /// </summary>
    private static List<List<float>> MergeNumericAdjacentClusters(
        List<List<float>> clusters,
        List<(int Index, TextItem Item)> items,
        float threshold)
    {
        (float Center, int Count, float NumericFraction) ComputeInfo(List<float> xs)
        {
            var center = xs.SumF32() / xs.Count;
            var total = 0;
            var numeric = 0;

            foreach (var (_, item) in items)
            {
                if (MathF.Abs(item.X - center) < threshold)
                {
                    total++;
                    if (IsNumericText(item.Text))
                    {
                        numeric++;
                    }
                }
            }

            return (center, total, total > 0 ? (float)numeric / total : 0.0f);
        }

        // Slightly beyond the clustering threshold, to catch header/data splits.
        var mergeDist = threshold * 1.5f;

        var merged = true;
        while (merged)
        {
            merged = false;
            var i = 0;

            while (i + 1 < clusters.Count)
            {
                var infoA = ComputeInfo(clusters[i]);
                var infoB = ComputeInfo(clusters[i + 1]);
                var dist = MathF.Abs(infoB.Center - infoA.Center);

                if (dist > mergeDist)
                {
                    i++;
                    continue;
                }

                var (sparse, dense) = infoA.Count < infoB.Count ? (infoA, infoB) : (infoB, infoA);

                var shouldMerge = dense.NumericFraction > 0.50f
                    && sparse.Count <= dense.Count / 2
                    && sparse.Count <= 5;

                if (shouldMerge)
                {
                    Log.Debug(Module, () =>
                        $"  merging column clusters: center {infoA.Center:F1} ({infoA.Count} items, " +
                        $"{infoA.NumericFraction * 100:F0}% numeric) + {infoB.Center:F1} ({infoB.Count} items, " +
                        $"{infoB.NumericFraction * 100:F0}% numeric), dist={dist:F1}");

                    clusters[i].AddRange(clusters[i + 1]);
                    clusters.RemoveAt(i + 1);
                    merged = true;

                    // The merged cluster may absorb the next one too.
                }
                else
                {
                    i++;
                }
            }
        }

        return clusters;
    }

    /// <summary>
    /// Clusters item y positions into row boundaries. The threshold sits between
    /// intra-row jitter and inter-row spacing, so uniformly spaced tables do not
    /// merge their rows.
    /// </summary>
    public static List<float> FindRowBoundaries(List<(int Index, TextItem Item)> items)
    {
        var yPositions = items.Select(e => e.Item.Y).ToList();
        yPositions.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        if (yPositions.Count == 0)
        {
            return [];
        }

        var fontSizes = items.Select(e => e.Item.FontSize).ToList();
        fontSizes.Sort(FloatTotalOrder.Instance);
        var medianFont = fontSizes[fontSizes.Count / 2];
        var clusterThreshold = MathF.Max(medianFont * 0.8f, 4.0f);

        var rows = new List<float>();
        var clusterItems = new List<float> { yPositions[0] };

        for (var i = 1; i < yPositions.Count; i++)
        {
            var y = yPositions[i];
            var clusterCenter = clusterItems.SumF32() / clusterItems.Count;

            if (clusterCenter - y >= clusterThreshold)
            {
                rows.Add(clusterCenter);
                clusterItems = [y];
            }
            else
            {
                clusterItems.Add(y);
            }
        }

        if (clusterItems.Count > 0)
        {
            rows.Add(clusterItems.SumF32() / clusterItems.Count);
        }

        return rows;
    }

    /// <summary>The column an x position falls in, or null when it is too far from any.</summary>
    public static int? FindColumnIndex(List<float> columns, float x)
    {
        if (columns.Count == 0)
        {
            return null;
        }

        // The tolerance follows the tightest column spacing, so narrow columns
        // do not steal each other's items.
        float threshold;
        if (columns.Count >= 2)
        {
            var minGap = float.PositiveInfinity;
            for (var i = 0; i + 1 < columns.Count; i++)
            {
                minGap = MathF.Min(minGap, MathF.Abs(columns[i + 1] - columns[i]));
            }

            threshold = Math.Clamp(minGap / 2.0f, 25.0f, 50.0f);
        }
        else
        {
            threshold = 50.0f;
        }

        var bestIndex = 0;
        var bestDistance = MathF.Abs(x - columns[0]);

        for (var i = 1; i < columns.Count; i++)
        {
            var distance = MathF.Abs(x - columns[i]);
            if (distance < bestDistance)
            {
                bestDistance = distance;
                bestIndex = i;
            }
        }

        return bestDistance < threshold ? bestIndex : null;
    }

    /// <summary>The row a y position falls in, or null when it is too far from any.</summary>
    public static int? FindRowIndex(List<float> rows, float y)
    {
        const float Threshold = 15.0f;

        if (rows.Count == 0)
        {
            return null;
        }

        var bestIndex = 0;
        var bestDistance = MathF.Abs(y - rows[0]);

        for (var i = 1; i < rows.Count; i++)
        {
            var distance = MathF.Abs(y - rows[i]);
            if (distance < bestDistance)
            {
                bestDistance = distance;
                bestIndex = i;
            }
        }

        return bestDistance < Threshold ? bestIndex : null;
    }

    /// <summary>
    /// Joins a cell's items, applying the same spacing rules a text line uses:
    /// no space around hyphens, inside brackets, or across a script shift.
    /// </summary>
    public static string JoinCellItems(IReadOnlyList<TextItem> items)
    {
        var result = new StringBuilder();

        for (var i = 0; i < items.Count; i++)
        {
            var text = items[i].Text.Trim();
            if (text.Length == 0)
            {
                continue;
            }

            if (result.Length == 0)
            {
                result.Append(text);
                continue;
            }

            var prevItem = items[i - 1];

            var prevEndsWithHyphen = result[^1] == '-';
            var currIsHyphen = text == "-";
            var currStartsWithHyphen = text.StartsWith('-');
            var prevEndsWithOpenDelimiter = result[^1] is '(' or '[' or '{';
            var currStartsWithCloseDelimiter = text[0] is ')' or ']' or '}';

            var fontRatio = items[i].FontSize / prevItem.FontSize;
            var reverseFontRatio = prevItem.FontSize / items[i].FontSize;
            var yDiff = MathF.Abs(items[i].Y - prevItem.Y);

            var isSubSuper = fontRatio < 0.85f && yDiff > 1.0f;
            var wasSubSuper = reverseFontRatio < 0.85f && yDiff > 1.0f;

            if (prevEndsWithHyphen
                || currIsHyphen
                || currStartsWithHyphen
                || isSubSuper
                || wasSubSuper
                || prevEndsWithOpenDelimiter
                || currStartsWithCloseDelimiter)
            {
                result.Append(text);
            }
            else
            {
                result.Append(' ').Append(text);
            }
        }

        return result.ToString();
    }

    /// <summary>
    /// Recovers a header row for a small-font table by looking at body-font
    /// items just above its first row. Tables often set their header at the body
    /// size while data rows use a smaller one, so the small-font pass excludes it.
    /// </summary>
    public static void RecoverHeaderRow(Table table, IReadOnlyList<TextItem> allItems, float smallFontThreshold)
    {
        if (table.Rows.Count == 0 || table.Columns.Count == 0)
        {
            return;
        }

        var firstRowY = table.Rows[0];

        var rowGapLimit = 30.0f;
        if (table.Rows.Count >= 2)
        {
            var avgSpacing = (table.Rows[0] - table.Rows[^1]) / (table.Rows.Count - 1);
            rowGapLimit = Math.Clamp(avgSpacing * 2.0f, 10.0f, 40.0f);
        }

        var candidates = new List<(int Index, TextItem Item)>();
        for (var i = 0; i < allItems.Count; i++)
        {
            var item = allItems[i];
            if (item.FontSize > smallFontThreshold && item.Y > firstRowY && item.Y <= firstRowY + rowGapLimit)
            {
                candidates.Add((i, item));
            }
        }

        if (candidates.Count == 0)
        {
            return;
        }

        candidates.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Item.Y, a.Item.Y));

        var headerYGroups = new List<(float Y, List<(int Index, TextItem Item)> Group)>();
        foreach (var candidate in candidates)
        {
            var placed = false;
            foreach (var (y, group) in headerYGroups)
            {
                if (MathF.Abs(candidate.Item.Y - y) < 5.0f)
                {
                    group.Add(candidate);
                    placed = true;
                    break;
                }
            }

            if (!placed)
            {
                headerYGroups.Add((candidate.Item.Y, [candidate]));
            }
        }

        // Groups run from the top down, so the last is closest to the table.
        var (headerY, headerItems) = headerYGroups[^1];

        var headerCells = new List<string>(new string[table.Columns.Count]);
        for (var i = 0; i < headerCells.Count; i++)
        {
            headerCells[i] = string.Empty;
        }

        var mappedCount = 0;
        var headerIndices = new List<int>();

        foreach (var (index, item) in headerItems)
        {
            if (FindColumnIndex(table.Columns, item.X) is not { } col)
            {
                continue;
            }

            var text = item.Text.Trim();
            if (text.Length == 0)
            {
                continue;
            }

            if (headerCells[col].Length > 0)
            {
                headerCells[col] += " ";
            }

            headerCells[col] += text;
            mappedCount++;
            headerIndices.Add(index);
        }

        // Two populated columns is the minimum that reads as a real header.
        var populated = headerCells.Count(c => c.Length > 0);
        if (populated < 2 || mappedCount < 2)
        {
            return;
        }

        table.Rows.Insert(0, headerY);
        table.Cells.Insert(0, headerCells);
        table.ItemIndices.AddRange(headerIndices);
    }
}
