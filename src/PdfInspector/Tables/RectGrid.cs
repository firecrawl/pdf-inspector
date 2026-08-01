// Ported from reference/src/tables/detect_rects.rs
using System.Text;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>A normalised rectangle in device space, anchored at its bottom-left corner.</summary>
internal readonly record struct RectBox(float X, float Y, float W, float H)
{
    public float Left => X;

    public float Right => X + W;

    public float Bottom => Y;

    public float Top => Y + H;

    public float Area => W * H;
}

/// <summary>Disjoint-set forest with component sizes, used to cluster rectangles.</summary>
internal sealed class UnionFind(int n)
{
    private readonly int[] _parent = [.. Enumerable.Range(0, n)];
    private readonly int[] _rank = new int[n];
    private readonly int[] _size = Enumerable.Repeat(1, n).ToArray();

    public int Find(int x)
    {
        // Iterative, so a pathological chain cannot overflow the stack.
        var root = x;
        while (_parent[root] != root)
        {
            root = _parent[root];
        }

        while (_parent[x] != root)
        {
            var next = _parent[x];
            _parent[x] = root;
            x = next;
        }

        return root;
    }

    public void Union(int a, int b)
    {
        var ra = Find(a);
        var rb = Find(b);

        if (ra == rb)
        {
            return;
        }

        var newSize = _size[ra] + _size[rb];

        if (_rank[ra] < _rank[rb])
        {
            _parent[ra] = rb;
            _size[rb] = newSize;
        }
        else if (_rank[ra] > _rank[rb])
        {
            _parent[rb] = ra;
            _size[ra] = newSize;
        }
        else
        {
            _parent[rb] = ra;
            _size[ra] = newSize;
            _rank[ra]++;
        }
    }

    public int ComponentSize(int x) => _size[Find(x)];
}

/// <summary>
/// Grid construction from drawn rectangles. Many PDFs draw cell borders with
/// rectangle operators, so the rectangles' edges give the grid directly.
/// </summary>
internal static class RectGrid
{
    private const string Module = "tables";

    /// <summary>
    /// No real table has thousands of cell rectangles. Once a component passes
    /// this size it is a vector drawing or a page-spanning clip path, and its
    /// overlap checks are skipped so pathological pages stay fast.
    /// </summary>
    public const int MaxClusterRects = 2000;

    /// <summary>True when two rectangles overlap once each is expanded by the tolerance.</summary>
    public static bool RectsOverlap(in RectBox a, in RectBox b, float tol) =>
        !(a.Right + tol < b.Left - tol
            || b.Right + tol < a.Left - tol
            || a.Top + tol < b.Bottom - tol
            || b.Top + tol < a.Bottom - tol);

    /// <summary>
    /// Clusters rectangles by spatial overlap, returning the index groups that
    /// reach <paramref name="minSize"/>.
    /// </summary>
    public static List<List<int>> ClusterRects(IReadOnlyList<RectBox> rects, float tolerance, int minSize)
    {
        var n = rects.Count;
        var uf = new UnionFind(n);

        for (var i = 0; i < n; i++)
        {
            // A rect already in an oversized component cannot contribute to a
            // usable table, so further comparisons are pointless.
            if (uf.ComponentSize(i) >= MaxClusterRects)
            {
                continue;
            }

            for (var j = i + 1; j < n; j++)
            {
                if (!RectsOverlap(rects[i], rects[j], tolerance))
                {
                    continue;
                }

                uf.Union(i, j);

                if (uf.ComponentSize(i) >= MaxClusterRects)
                {
                    break;
                }
            }
        }

        var groups = new Dictionary<int, List<int>>();
        for (var i = 0; i < n; i++)
        {
            var root = uf.Find(i);
            if (!groups.TryGetValue(root, out var group))
            {
                group = [];
                groups[root] = group;
            }

            group.Add(i);
        }

        // Ordered by root index, for deterministic output.
        return [.. groups
            .Where(kv => kv.Value.Count >= minSize)
            .OrderBy(kv => kv.Key)
            .Select(kv => kv.Value)];
    }

    /// <summary>
    /// Splits a cluster at its widest horizontal gap, when both sides keep
    /// enough rectangles to stand alone.
    /// </summary>
    public static (List<RectBox> Left, List<RectBox> Right)? SplitWideCluster(
        IReadOnlyList<RectBox> rects,
        float minGap,
        int minGroupSize)
    {
        if (rects.Count < minGroupSize * 2)
        {
            return null;
        }

        var intervals = rects
            .Select(r => (Start: r.Left, End: r.Right))
            .OrderBy(i => i.Start, FloatTotalOrder.Instance)
            .ToList();

        // Contiguous horizontal bands, so the gaps between them are real.
        var merged = new List<(float Start, float End)>();
        foreach (var (start, end) in intervals)
        {
            if (merged.Count > 0 && start <= merged[^1].End + 1.0f)
            {
                merged[^1] = (merged[^1].Start, MathF.Max(merged[^1].End, end));
                continue;
            }

            merged.Add((start, end));
        }

        if (merged.Count < 2)
        {
            return null;
        }

        var bestGap = 0.0f;
        var bestSplitX = 0.0f;

        for (var i = 1; i < merged.Count; i++)
        {
            var gap = merged[i].Start - merged[i - 1].End;
            if (gap > bestGap)
            {
                bestGap = gap;
                bestSplitX = (merged[i - 1].End + merged[i].Start) / 2.0f;
            }
        }

        if (bestGap < minGap)
        {
            return null;
        }

        var left = rects.Where(r => r.X + (r.W / 2.0f) < bestSplitX).ToList();
        var right = rects.Where(r => r.X + (r.W / 2.0f) >= bestSplitX).ToList();

        return left.Count >= minGroupSize && right.Count >= minGroupSize ? (left, right) : null;
    }

    /// <summary>Collapses edge values that fall within the tolerance, returning sorted unique edges.</summary>
    public static List<float> SnapEdges(IReadOnlyList<float> values, float tolerance)
    {
        var sorted = values.OrderBy(v => v, FloatTotalOrder.Instance).ToList();
        var snapped = new List<float>();

        foreach (var v in sorted)
        {
            if (snapped.Count > 0 && MathF.Abs(v - snapped[^1]) <= tolerance)
            {
                continue;
            }

            snapped.Add(v);
        }

        return snapped;
    }

    /// <summary>How a grid-building attempt ended.</summary>
    private enum GridOutcome
    {
        Ok,

        /// <summary>
        /// Structurally valid but too few rows carried content, which a
        /// page-background rect can cause. Worth retrying without it.
        /// </summary>
        FewNonEmptyRows,

        Failed,
    }

    /// <summary>Builds a table from a cluster of rectangles, retrying without page backgrounds if needed.</summary>
    public static Table? DetectTableFromRectGroup(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page)
    {
        var noSkip = new bool[groupRects.Count];

        var (outcome, table) = TryBuildGrid(items, groupRects, page, noSkip, strict: false);
        switch (outcome)
        {
            case GridOutcome.Ok:
                return table;
            case GridOutcome.Failed:
                return null;
        }

        // A full-page background fill adds spurious margin columns and collapses
        // the rows, so the retry excludes it from edge extraction.
        const float OriginTol = 5.0f;

        var groupXMin = groupRects.Min(r => r.Left);
        var groupXMax = groupRects.Max(r => r.Right);
        var groupYMin = groupRects.Min(r => r.Bottom);
        var groupYMax = groupRects.Max(r => r.Top);
        var groupW = groupXMax - groupXMin;
        var groupH = groupYMax - groupYMin;

        var isPageBg = groupRects
            .Select(r => r.X < OriginTol && r.Y < OriginTol && r.W >= groupW * 0.95f && r.H >= groupH * 0.9f)
            .ToArray();

        // Only worth retrying for a grid large enough that the retry's stricter
        // thresholds still mean something.
        var ys = new List<float>();
        foreach (var r in groupRects)
        {
            ys.Add(r.Bottom);
            ys.Add(r.Top);
        }

        var yEdgeCount = SnapEdges(ys, 6.0f).Count;

        if (isPageBg.Any(b => b) && yEdgeCount >= 12)
        {
            Log.Debug(Module, "  retrying without page-background rects");
            var (retryOutcome, retryTable) = TryBuildGrid(items, groupRects, page, isPageBg, strict: true);
            if (retryOutcome == GridOutcome.Ok)
            {
                return retryTable;
            }
        }

        return null;
    }

    /// <summary>
    /// The core grid build. Rects flagged in <paramref name="skipRects"/> are
    /// excluded from edge extraction and merged-cell propagation, though they
    /// still count toward the fill check. Strict mode raises the content
    /// thresholds, since it runs only as a retry.
    /// </summary>
    private static (GridOutcome Outcome, Table? Table) TryBuildGrid(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page,
        bool[] skipRects,
        bool strict)
    {
        var xEdgeValues = new List<float>();
        var yEdgeValues = new List<float>();

        for (var i = 0; i < groupRects.Count; i++)
        {
            var r = groupRects[i];
            if (!skipRects[i])
            {
                xEdgeValues.Add(r.Left);
                xEdgeValues.Add(r.Right);
            }

            yEdgeValues.Add(r.Bottom);
            yEdgeValues.Add(r.Top);
        }

        var xEdges = SnapEdges(xEdgeValues, 6.0f);
        var yEdges = SnapEdges(yEdgeValues, 6.0f);

        Log.Debug(Module, () =>
            $"  edges: {xEdges.Count} x, {yEdges.Count} y — grid " +
            $"{Math.Max(yEdges.Count - 1, 0)}x{Math.Max(xEdges.Count - 1, 0)}");

        if (xEdges.Count < 3 || yEdges.Count < 4)
        {
            Log.Debug(Module, () =>
                $"  rejected: {xEdges.Count} x-edges, {yEdges.Count} y-edges (need >=3, >=4)");
            return (GridOutcome.Failed, null);
        }

        // Columns run left to right; rows run top to bottom, so the highest y first.
        var colEdges = xEdges;
        var rowEdges = yEdges.OrderByDescending(v => v, FloatTotalOrder.Instance).ToList();

        var numCols = colEdges.Count - 1;
        var numRows = rowEdges.Count - 1;

        if (numCols < 2 || numRows < 2)
        {
            return (GridOutcome.Failed, null);
        }

        // Form-style PDFs with scattered field boxes produce huge sparse grids.
        // Statistical lookup tables legitimately reach the low twenties.
        if (numCols > 25)
        {
            Log.Debug(Module, () => $"  rejected: {numCols} columns > 25");
            return (GridOutcome.Failed, null);
        }

        var filledCells = 0u;
        for (var row = 0; row < numRows; row++)
        {
            var yTop = rowEdges[row];
            var yBot = rowEdges[row + 1];

            for (var col = 0; col < numCols; col++)
            {
                var xLeft = colEdges[col];
                var xRight = colEdges[col + 1];

                const float Tol = 6.0f;
                var covered = groupRects.Any(r =>
                    r.Left <= xLeft + Tol
                    && r.Right >= xRight - Tol
                    && r.Bottom <= yTop + Tol
                    && r.Top >= yBot - Tol);

                if (covered)
                {
                    filledCells++;
                }
            }
        }

        var totalCells = (float)(numCols * numRows);
        var fillRatio = filledCells / totalCells;

        Log.Debug(Module, () =>
            $"  grid: {numRows}x{numCols} = {(uint)totalCells} cells, {filledCells} filled, ratio={fillRatio:F2}");

        if (fillRatio < 0.3f)
        {
            Log.Debug(Module, () => $"  rejected: fill ratio {fillRatio:F2} < 0.30");
            return (GridOutcome.Failed, null);
        }

        var (cells, itemIndices) = AssignItemsToGrid(items, colEdges, rowEdges, page);

        // A rect spanning several grid rows is a merged cell; its text belongs
        // in the first sub-row. Wide tables skip this, because there a spanning
        // rect is usually row-group shading rather than a merged cell.
        if (numCols <= 10)
        {
            PropagateMergedCells(cells, colEdges, rowEdges, groupRects, skipRects);
        }

        var columns = new List<float>(numCols);
        for (var c = 0; c < numCols; c++)
        {
            columns.Add((colEdges[c] + colEdges[c + 1]) / 2.0f);
        }

        var rows = new List<float>(numRows);
        for (var r = 0; r < numRows; r++)
        {
            rows.Add((rowEdges[r] + rowEdges[r + 1]) / 2.0f);
        }

        if (itemIndices.Count == 0)
        {
            Log.Debug(Module, "  rejected: no text items assigned to grid");
            return (GridOutcome.Failed, null);
        }

        var nonEmptyRows = cells.Count(row => row.Any(c => c.Trim().Length > 0));
        var minRows = strict ? numRows / 2 : 2;

        if (nonEmptyRows < minRows)
        {
            Log.Debug(Module, () => $"  rejected: only {nonEmptyRows} non-empty rows (need {minRows})");
            return (GridOutcome.FewNonEmptyRows, null);
        }

        var nonEmptyCells = cells.SelectMany(row => row).Count(c => c.Trim().Length > 0);
        var contentRatio = nonEmptyCells / totalCells;
        var minContent = strict ? 0.40f : 0.25f;

        if (contentRatio < minContent)
        {
            Log.Debug(Module, () =>
                $"  rejected: content ratio {contentRatio:F2} < {minContent:F2} " +
                $"({nonEmptyCells} non-empty / {(uint)totalCells} total)");
            return (GridOutcome.Failed, null);
        }

        // A very long cell in strict mode means a paragraph was caught in the grid.
        if (strict)
        {
            var maxCellLen = cells.SelectMany(row => row).Select(TextUtils.ByteLength).DefaultIfEmpty(0).Max();
            if (maxCellLen > 200)
            {
                Log.Debug(Module, () => $"  rejected: max cell length {maxCellLen} > 200 (likely paragraph text)");
                return (GridOutcome.Failed, null);
            }
        }

        // Outer columns beyond the text are trimmed; an empty interior column
        // means the grid is wrong.
        int? firstNonEmpty = null;
        int? lastNonEmpty = null;

        for (var col = 0; col < numCols; col++)
        {
            if (cells.Any(row => col < row.Count && row[col].Trim().Length > 0))
            {
                firstNonEmpty ??= col;
                lastNonEmpty = col;
            }
        }

        if (firstNonEmpty is not { } firstCol || lastNonEmpty is not { } lastCol || lastCol <= firstCol)
        {
            Log.Debug(Module, "  rejected: no content columns");
            return (GridOutcome.Failed, null);
        }

        for (var col = firstCol; col <= lastCol; col++)
        {
            if (!cells.Any(row => col < row.Count && row[col].Trim().Length > 0))
            {
                Log.Debug(Module, () => $"  rejected: interior column {col} is completely empty");
                return (GridOutcome.Failed, null);
            }
        }

        if (firstCol > 0 || lastCol < numCols - 1)
        {
            Log.Debug(Module, () =>
                $"  trimmed {numCols - 1 - lastCol + firstCol} empty outer columns ({firstCol}..={lastCol})");

            columns = columns[firstCol..(lastCol + 1)];
            cells = [.. cells.Select(row => row[firstCol..(lastCol + 1)])];
        }

        return (GridOutcome.Ok, Table.Create(columns, rows, cells, itemIndices));
    }

    /// <summary>
    /// Assigns text items to the cells the edges define, returning the cell text
    /// and the indices of the items consumed.
    /// </summary>
    public static (List<List<string>> Cells, List<int> Indices) AssignItemsToGrid(
        IReadOnlyList<TextItem> items,
        List<float> colEdges,
        List<float> rowEdges,
        uint page)
    {
        var numCols = colEdges.Count - 1;
        var numRows = rowEdges.Count - 1;

        var cellItems = new List<List<List<TextItem>>>(numRows);
        for (var r = 0; r < numRows; r++)
        {
            var row = new List<List<TextItem>>(numCols);
            for (var c = 0; c < numCols; c++)
            {
                row.Add([]);
            }

            cellItems.Add(row);
        }

        var indices = new List<int>();

        for (var idx = 0; idx < items.Count; idx++)
        {
            var item = items[idx];
            if (item.Page != page)
            {
                continue;
            }

            // The item's horizontal centre and its baseline decide the cell.
            var cx = item.X + (item.Width / 2.0f);
            var cy = item.Y;

            int? col = null;
            for (var c = 0; c < numCols; c++)
            {
                if (cx >= colEdges[c] - 2.0f && cx <= colEdges[c + 1] + 2.0f)
                {
                    col = c;
                    break;
                }
            }

            int? row = null;
            for (var r = 0; r < numRows; r++)
            {
                if (cy >= rowEdges[r + 1] - 2.0f && cy <= rowEdges[r] + 2.0f)
                {
                    row = r;
                    break;
                }
            }

            if (col is { } c2 && row is { } r2)
            {
                cellItems[r2][c2].Add(item);
                indices.Add(idx);
            }
        }

        var cells = new List<List<string>>(numRows);
        foreach (var rowItems in cellItems)
        {
            var rowCells = new List<string>(numCols);
            foreach (var colItems in rowItems)
            {
                colItems.Sort((a, b) =>
                {
                    var byY = FloatTotalOrder.Instance.Compare(b.Y, a.Y);
                    return byY != 0 ? byY : FloatTotalOrder.Instance.Compare(a.X, b.X);
                });

                var text = string.Join(" ", colItems.Select(i => i.Text.Trim()).Where(t => t.Length > 0));
                rowCells.Add(RemoveInnerDelimiterSpaces(text));
            }

            cells.Add(rowCells);
        }

        return (cells, indices);
    }

    /// <summary>Drops the spaces that joining leaves just inside brackets.</summary>
    private static string RemoveInnerDelimiterSpaces(string text)
    {
        var result = new StringBuilder(text.Length);

        for (var i = 0; i < text.Length; i++)
        {
            if (text[i] == ' ')
            {
                var afterOpen = result.Length > 0 && result[^1] is '(' or '[' or '{';
                var beforeClose = i + 1 < text.Length && text[i + 1] is ')' or ']' or '}';

                if (afterOpen || beforeClose)
                {
                    continue;
                }
            }

            result.Append(text[i]);
        }

        return result.ToString();
    }

    /// <summary>
    /// Consolidates the text of a vertically merged cell into its first
    /// sub-row, so the downstream continuation merge collapses the sub-rows
    /// correctly.
    /// </summary>
    private static void PropagateMergedCells(
        List<List<string>> cells,
        List<float> colEdges,
        List<float> rowEdges,
        IReadOnlyList<RectBox> groupRects,
        bool[] skipRects)
    {
        var numCols = colEdges.Count - 1;
        var numRows = rowEdges.Count - 1;
        const float Tol = 6.0f;

        for (var col = 0; col < numCols; col++)
        {
            for (var rectIdx = 0; rectIdx < groupRects.Count; rectIdx++)
            {
                // A page background spans every row and would collapse the
                // whole column into its first cell.
                if (skipRects[rectIdx])
                {
                    continue;
                }

                var rect = groupRects[rectIdx];

                if (rect.Left > colEdges[col] + Tol || rect.Right < colEdges[col + 1] - Tol)
                {
                    continue;
                }

                // Real overlap is required, not mere tolerance slack: a rect
                // whose top equals a row's bottom lies entirely below that row,
                // and counting it would cascade unrelated text into one cell.
                bool Spans(int r)
                {
                    var rowTop = rowEdges[r];
                    var rowBot = rowEdges[r + 1];
                    var overlap = MathF.Max(MathF.Min(rowTop, rect.Top) - MathF.Max(rowBot, rect.Bottom), 0.0f);
                    return overlap > Tol;
                }

                int? firstRow = null;
                int? lastRow = null;

                for (var r = 0; r < numRows; r++)
                {
                    if (Spans(r))
                    {
                        firstRow ??= r;
                        lastRow = r;
                    }
                }

                if (firstRow is not { } first || lastRow is not { } last || last <= first)
                {
                    continue;
                }

                var combined = new StringBuilder();
                for (var row = first; row <= last; row++)
                {
                    var text = cells[row][col].Trim();
                    if (text.Length == 0)
                    {
                        continue;
                    }

                    if (combined.Length > 0)
                    {
                        combined.Append(' ');
                    }

                    combined.Append(text);
                }

                cells[first][col] = combined.ToString();
                for (var row = first + 1; row <= last; row++)
                {
                    cells[row][col] = string.Empty;
                }
            }
        }
    }

    /// <summary>
    /// True when the rectangles are full-width horizontal bands, as row-stripe
    /// shading draws. Those yield only two distinct x edges, so ordinary grid
    /// detection sees a single column and fails.
    /// </summary>
    public static bool IsRowStripePattern(IReadOnlyList<RectBox> rects)
    {
        if (rects.Count < 3)
        {
            return false;
        }

        var widths = rects.Select(r => r.W).OrderBy(w => w, FloatTotalOrder.Instance).ToList();
        var medianWidth = widths[widths.Count / 2];

        if (medianWidth <= 200.0f)
        {
            return false;
        }

        var withinTolerance = rects.Count(r => MathF.Abs(r.W - medianWidth) <= medianWidth * 0.10f);
        return (float)withinTolerance / rects.Count > 0.75f;
    }
}
