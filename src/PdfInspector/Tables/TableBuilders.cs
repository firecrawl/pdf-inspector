// Ported from reference/src/tables/mod.rs
using PdfInspector.Extractor;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Table constructors that work from geometry a caller already supplies —
/// cluster rectangles or layout-detected column boundaries — rather than running
/// full heuristic detection.
/// </summary>
internal static class TableBuilders
{
    private const string Module = "tables";

    /// <summary>
    /// Builds a table from items plus cluster rectangles, as calendar-style
    /// layouts need. Rectangle X positions become column boundaries and the table
    /// is constructed directly, bypassing heuristic detection. Merged
    /// multi-number items are split first.
    /// </summary>
    public static Table? TryBuildRectGuidedTable(IReadOnlyList<TextItem> items, IReadOnlyList<RectBox> clusterRects)
    {
        if (items.Count == 0 || clusterRects.Count == 0)
        {
            return null;
        }

        // Column boundaries from rectangle left edges, snapped at 2pt.
        var xLefts = clusterRects.Select(r => r.X).OrderBy(x => x, FloatTotalOrder.Instance).ToList();
        var colBoundaries = new List<float>();
        foreach (var x in xLefts)
        {
            if (colBoundaries.Count == 0 || MathF.Abs(x - colBoundaries[^1]) > 2.0f)
            {
                colBoundaries.Add(x);
            }
        }

        if (colBoundaries.Count < 5)
        {
            return null;
        }

        // Interpolate missing boundaries: holidays and non-work days may carry no
        // rectangle, leaving gaps. Any gap over 1.5× the median spacing is filled
        // with evenly spaced boundaries so every day still gets a column.
        if (colBoundaries.Count >= 2)
        {
            var spacings = new List<float>(colBoundaries.Count - 1);
            for (var i = 0; i + 1 < colBoundaries.Count; i++)
            {
                spacings.Add(colBoundaries[i + 1] - colBoundaries[i]);
            }

            spacings.Sort(FloatTotalOrder.Instance);
            var medianSpacing = spacings[spacings.Count / 2];
            var threshold = medianSpacing * 1.5f;

            var filled = new List<float> { colBoundaries[0] };
            for (var i = 1; i < colBoundaries.Count; i++)
            {
                var gap = colBoundaries[i] - colBoundaries[i - 1];
                if (gap > threshold)
                {
                    var n = (int)MathF.Round(gap / medianSpacing, MidpointRounding.AwayFromZero);
                    if (n >= 2)
                    {
                        var step = gap / n;
                        for (var j = 1; j < n; j++)
                        {
                            filled.Add(colBoundaries[i - 1] + (j * step));
                        }
                    }
                }

                filled.Add(colBoundaries[i]);
            }

            colBoundaries = filled;
        }

        var expandedItems = new List<(TextItem Item, int OrigIndex)>();
        for (var idx = 0; idx < items.Count; idx++)
        {
            foreach (var split in SplitMergedNumbers(items[idx], colBoundaries))
            {
                expandedItems.Add((split, idx));
            }
        }

        // Row boundaries from item Y positions, at 5pt tolerance.
        var yValues = expandedItems
            .Select(p => p.Item.Y)
            .OrderByDescending(y => y, FloatTotalOrder.Instance)
            .ToList();
        var rowBoundaries = new List<float>();
        foreach (var y in yValues)
        {
            if (rowBoundaries.Count == 0 || MathF.Abs(rowBoundaries[^1] - y) > 5.0f)
            {
                rowBoundaries.Add(y);
            }
        }

        if (rowBoundaries.Count == 0)
        {
            return null;
        }

        var nRows = rowBoundaries.Count;
        var nCols = colBoundaries.Count;
        var cells = new List<List<string>>(nRows);
        for (var r = 0; r < nRows; r++)
        {
            var row = new List<string>(nCols);
            for (var c = 0; c < nCols; c++)
            {
                row.Add(string.Empty);
            }

            cells.Add(row);
        }

        var usedIndices = new List<int>();

        // Legend text sits beyond the table's rightmost column, so bound X.
        var colSpacing = colBoundaries.Count >= 2
            ? (colBoundaries[^1] - colBoundaries[0]) / (colBoundaries.Count - 1)
            : 20.0f;
        var maxX = colBoundaries[^1] + (colSpacing * 1.5f);

        foreach (var (item, origIdx) in expandedItems)
        {
            if (item.X > maxX)
            {
                continue;
            }

            var row = rowBoundaries.FindIndex(ry => MathF.Abs(ry - item.Y) <= 5.0f);

            // The rightmost boundary at or left of the item. The 4pt slack catches
            // annotation items ("Memorial Day") that sit slightly before the next
            // boundary.
            var col = colBoundaries.FindLastIndex(cx => item.X >= cx - 4.0f);

            if (row >= 0 && col >= 0)
            {
                var cell = cells[row][col];
                cells[row][col] = cell.Length > 0 ? cell + " " + item.Text.Trim() : item.Text.Trim();
                usedIndices.Add(origIdx);
            }
        }

        // Strip tilde-leader noise: legend text from the right of the page bleeds
        // into the last column.
        foreach (var row in cells)
        {
            for (var c = 0; c < row.Count; c++)
            {
                var pos = row[c].IndexOf("~~~", StringComparison.Ordinal);
                if (pos >= 0)
                {
                    row[c] = row[c][..pos].TrimEnd();
                }
            }
        }

        var bestRowFill = cells.Max(row => row.Count(c => c.Length > 0));
        if (bestRowFill < 5)
        {
            return null;
        }

        usedIndices.Sort();
        var deduped = usedIndices.Distinct().ToList();

        return Table.Create(colBoundaries, rowBoundaries, cells, deduped);
    }

    /// <summary>
    /// Splits a text item holding several whitespace-separated tokens — "10 11 12
    /// ... 31" — into one item per token, each snapped to the nearest column
    /// boundary.
    /// </summary>
    private static List<TextItem> SplitMergedNumbers(TextItem item, List<float> colBoundaries)
    {
        var tokens = item.Text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        if (tokens.Length <= 1)
        {
            return [item.Clone()];
        }

        // Consecutive leading numeric tokens are day numbers like "10 11 12".
        var leadingNumeric = 0;
        while (leadingNumeric < tokens.Length && tokens[leadingNumeric].All(char.IsAsciiDigit))
        {
            leadingNumeric++;
        }

        if (leadingNumeric == 0)
        {
            return [item.Clone()];
        }

        var tokenWidth = item.Width / tokens.Length;
        var result = new List<TextItem>(leadingNumeric + 1);

        // The enclosing boundary — the rightmost at or left of the item — then one
        // boundary per leading number. Searching from the right avoids overshooting
        // when the item starts between boundaries.
        var startColIdx = colBoundaries.FindLastIndex(cx => cx <= item.X + 2.0f);
        var startCol = startColIdx >= 0 ? startColIdx : 0;

        for (var i = 0; i < leadingNumeric; i++)
        {
            var colIdx = startCol + i;
            float snappedX;
            if (colIdx < colBoundaries.Count)
            {
                snappedX = colBoundaries[colIdx];
            }
            else
            {
                // Out of boundaries: distribute evenly and snap back.
                var rawX = item.X + (i * tokenWidth) + (tokenWidth / 2.0f);
                var found = colBoundaries.FindLastIndex(cx => cx <= rawX + 2.0f);
                snappedX = found >= 0 ? colBoundaries[found] : rawX;
            }

            var split = item.Clone();
            split.Text = tokens[i];
            split.X = snappedX;
            split.Width = tokenWidth;
            result.Add(split);
        }

        // Trailing non-numeric tokens become an annotation at the last numeric column.
        if (leadingNumeric < tokens.Length)
        {
            var annotation = item.Clone();
            annotation.Text = string.Join(' ', tokens[leadingNumeric..]);
            annotation.X = result.Count > 0 ? result[^1].X : item.X;
            annotation.Width = tokenWidth;
            result.Add(annotation);
        }

        return result;
    }

    /// <summary>
    /// Builds a table from layout-detected column boundaries. When the layout
    /// engine reports several tabular (not newspaper) columns, those boundaries
    /// construct a table directly. This covers borderless tables with no
    /// rectangles or lines, where columns exist purely through text alignment —
    /// common in exam and reference tables.
    /// </summary>
    /// <remarks>Requires at least 3 columns, at least 3 rows, and a 40% cell fill rate.</remarks>
    public static Table? TryBuildTableFromColumns(IReadOnlyList<TextItem> items, uint page)
    {
        var columns = Columns.DetectColumns(items, page, false);
        if (columns.Count < 4)
        {
            return null;
        }

        // Refine the columns: look for header-like rows where several items share a
        // Y and space out evenly. A wide column holding two header items splits at
        // the gap between them.
        var pageItems = items.Where(i => i.Page == page).ToList();
        const float HeaderYTol = 3.0f;

        var ys = pageItems.Select(i => i.Y).OrderByDescending(y => y, FloatTotalOrder.Instance).ToList();
        var dedupedYs = new List<float>();
        foreach (var y in ys)
        {
            if (dedupedYs.Count == 0 || MathF.Abs(dedupedYs[^1] - y) >= HeaderYTol)
            {
                dedupedYs.Add(y);
            }
        }

        foreach (var headerY in dedupedYs.Take(5))
        {
            var rowItems = pageItems.Where(i => MathF.Abs(i.Y - headerY) < HeaderYTol).ToList();
            if (rowItems.Count < columns.Count)
            {
                continue;
            }

            var newColumns = new List<ColumnRegion>();
            var didSplit = false;
            foreach (var col in columns)
            {
                var colItems = rowItems.Where(i => i.X >= col.XMin && i.X < col.XMax).ToList();
                if (colItems.Count >= 2)
                {
                    var sorted = colItems.Select(i => i.X).OrderBy(x => x, FloatTotalOrder.Instance).ToList();
                    var firstItem = colItems.First(i => i.X == sorted[0]);
                    var splitX = (sorted[0] + firstItem.Width + sorted[1]) / 2.0f;
                    newColumns.Add(new ColumnRegion(col.XMin, splitX));
                    newColumns.Add(new ColumnRegion(splitX, col.XMax));
                    didSplit = true;
                }
                else
                {
                    newColumns.Add(col);
                }
            }

            if (didSplit)
            {
                var before = columns.Count;
                Log.Debug(Module, () =>
                    $"column refinement: {before} -> {newColumns.Count} columns from header row at y={headerY:F1}");
                columns = newColumns;
                break;
            }
        }

        // Group items per column so newspaper and tabular layouts can be told apart.
        var colBuckets = new List<List<TextItem>>(columns.Count);
        for (var i = 0; i < columns.Count; i++)
        {
            colBuckets.Add([]);
        }

        foreach (var item in items)
        {
            if (item.Page != page)
            {
                continue;
            }

            var itemLeft = item.X;
            var itemRight = item.X + item.Width;
            var spans = columns.Count(col =>
                MathF.Max(MathF.Min(itemRight, col.XMax) - MathF.Max(itemLeft, col.XMin), 0.0f) > 0.0f);
            if (spans > 1)
            {
                continue;
            }

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

        var noThresholds = new Dictionary<uint, float>();
        var noTablePages = new HashSet<uint>();
        var perColumnLines = colBuckets
            .Select(bucket => Layout.GroupIntoLinesWithThresholds([.. bucket], noThresholds, noTablePages))
            .ToList();

        if (Layout.IsNewspaperLayout(perColumnLines, columns))
        {
            return null;
        }

        const float RowYTol = 5.0f;
        var rowYs = new List<float>();
        foreach (var colLines in perColumnLines)
        {
            foreach (var line in colLines)
            {
                if (!rowYs.Any(ry => MathF.Abs(ry - line.Y) < RowYTol))
                {
                    rowYs.Add(line.Y);
                }
            }
        }

        rowYs.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        if (rowYs.Count is < 3 or > 40)
        {
            return null;
        }

        var colXs = columns.Select(c => c.XMin).ToList();
        var cells = new List<List<string>>(rowYs.Count);
        for (var r = 0; r < rowYs.Count; r++)
        {
            var row = new List<string>(columns.Count);
            for (var c = 0; c < columns.Count; c++)
            {
                row.Add(string.Empty);
            }

            cells.Add(row);
        }

        var itemIndices = new List<int>();

        for (var itemIdx = 0; itemIdx < items.Count; itemIdx++)
        {
            var item = items[itemIdx];
            if (item.Page != page)
            {
                continue;
            }

            var itemLeft = item.X;
            var itemRight = item.X + item.Width;
            int? bestCol = null;
            var bestOverlap = 0.0f;
            var spanCount = 0;
            for (var ci = 0; ci < columns.Count; ci++)
            {
                var overlap = MathF.Max(
                    MathF.Min(itemRight, columns[ci].XMax) - MathF.Max(itemLeft, columns[ci].XMin),
                    0.0f);
                if (overlap > 0.0f)
                {
                    spanCount++;
                }

                if (overlap > bestOverlap)
                {
                    bestOverlap = overlap;
                    bestCol = ci;
                }
            }

            if (spanCount > 1 || bestCol is not { } col)
            {
                continue;
            }

            var row = rowYs.FindIndex(ry => MathF.Abs(ry - item.Y) < RowYTol);
            if (row >= 0)
            {
                cells[row][col] = cells[row][col].Length > 0
                    ? cells[row][col] + " " + item.Text
                    : item.Text;
                itemIndices.Add(itemIdx);
            }
        }

        MergeSuperscriptMarkerRows(rowYs, cells);

        var totalCells = rowYs.Count * columns.Count;
        var filledCells = cells.Sum(r => r.Count(c => c.Trim().Length > 0));
        var fillRate = filledCells / (float)totalCells;

        if (fillRate < 0.15f)
        {
            return null;
        }

        // A majority of rows must carry content in two or more columns.
        var multiColRows = cells.Count(row => row.Count(c => c.Trim().Length > 0) >= 2);
        if (multiColRows * 2 < rowYs.Count)
        {
            return null;
        }

        // Reject prose-like content: cells that are long on average mean a
        // multi-column text layout, not a data table. Real table cells run short —
        // 40 characters or fewer — while prose paragraphs run much longer.
        var cellLengths = cells
            .SelectMany(r => r)
            .Where(c => c.Trim().Length > 0)
            .Select(c => c.Trim().Length)
            .ToList();
        if (cellLengths.Count > 0)
        {
            var avgCellLen = cellLengths.Sum() / (float)cellLengths.Count;
            if (avgCellLen > 40.0f)
            {
                return null;
            }

            var longCells = cellLengths.Count(len => len > 80);
            if (longCells / (float)cellLengths.Count > 0.10f)
            {
                return null;
            }
        }

        // Reject cells that read as sentences: too much sentence-ending
        // punctuation means prose text, not table data.
        var proseCells = cells
            .SelectMany(r => r)
            .Count(c =>
            {
                var t = c.Trim();
                return TextUtils.ByteLength(t) > 20 && (t.EndsWith('.') || t.EndsWith('!') || t.EndsWith('?') || t.EndsWith(':'));
            });
        if (filledCells > 0 && proseCells / (float)filledCells > 0.15f)
        {
            return null;
        }

        // Reject newspaper-like asymmetry: a column holding over 60% of the content
        // is a body-text column with side annotations, not a data table.
        var itemsPerCol = new int[columns.Count];
        foreach (var row in cells)
        {
            for (var ci = 0; ci < row.Count; ci++)
            {
                if (row[ci].Trim().Length > 0)
                {
                    itemsPerCol[ci]++;
                }
            }
        }

        var maxColItems = itemsPerCol.Length > 0 ? itemsPerCol.Max() : 0;
        if (filledCells > 0 && maxColItems / (float)filledCells > 0.60f)
        {
            return null;
        }

        Log.Debug(Module, () =>
            $"column-based table: {columns.Count} cols x {rowYs.Count} rows, fill={fillRate * 100.0f:F0}%, " +
            $"multi_col_rows={multiColRows}");

        return Table.Create(colXs, rowYs, cells, itemIndices);
    }

    /// <summary>
    /// Folds a row that holds nothing but a superscript marker into whichever
    /// neighbouring row it sits closest to. Footnote markers set on their own
    /// baseline otherwise occupy a phantom row.
    /// </summary>
    private static void MergeSuperscriptMarkerRows(List<float> rowYs, List<List<string>> cells)
    {
        var rowIdx = 0;
        while (rowIdx < cells.Count)
        {
            var nonEmpty = cells[rowIdx]
                .Select((cell, colIdx) => (ColIdx: colIdx, Text: cell.Trim()))
                .Where(p => p.Text.Length > 0)
                .ToList();

            if (nonEmpty.Count != 1 || !IsSuperscriptMarkerCell(nonEmpty[0].Text))
            {
                rowIdx++;
                continue;
            }

            var (markerCol, marker) = nonEmpty[0];

            int? target = null;
            var bestGap = float.PositiveInfinity;
            if (rowIdx > 0)
            {
                var gap = MathF.Abs(rowYs[rowIdx - 1] - rowYs[rowIdx]);
                if (gap <= 10.0f)
                {
                    target = rowIdx - 1;
                    bestGap = gap;
                }
            }

            if (rowIdx + 1 < cells.Count)
            {
                var gap = MathF.Abs(rowYs[rowIdx] - rowYs[rowIdx + 1]);
                if (gap <= 10.0f && FloatTotalOrder.Instance.Compare(gap, bestGap) < 0)
                {
                    target = rowIdx + 1;
                }
            }

            if (target is not { } targetIdx)
            {
                rowIdx++;
                continue;
            }

            var targetCell = cells[targetIdx][markerCol];
            cells[targetIdx][markerCol] = targetCell.Trim().Length == 0 ? marker : targetCell + marker;
            cells.RemoveAt(rowIdx);
            rowYs.RemoveAt(rowIdx);
        }
    }

    /// <summary>True for a one- or two-character footnote marker glyph.</summary>
    private static bool IsSuperscriptMarkerCell(string value)
    {
        var trimmed = value.Trim();
        return trimmed.Length > 0
            && TextUtils.CharCount(trimmed) <= 2
            && trimmed.All(ch => ch is '*' or '#' or 'o' or 'O' or '°' or 'º' or '†' or '‡');
    }
}
