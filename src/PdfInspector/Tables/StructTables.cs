// Ported from reference/src/tables/detect_struct.rs
using PdfInspector.Structure;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Structure-tree table detection. When a PDF carries a well-formed structure
/// tree with <c>/Table</c> &gt; <c>/TR</c> &gt; <c>/TD|TH</c> elements linked to
/// MCIDs, tables are built straight from the semantic hierarchy — no geometry
/// heuristics needed.
/// </summary>
internal static class StructTables
{
    private const string Module = "tables";

    /// <summary>A structure-tree cell resolved against the page's text items.</summary>
    private sealed class MatchedCell
    {
        public string Text { get; init; } = string.Empty;

        public List<int> ItemIndices { get; init; } = [];

        /// <summary>Leftmost x of the cell's items, absent when nothing matched.</summary>
        public float? X { get; init; }

        /// <summary>Topmost y of the cell's items, absent when nothing matched.</summary>
        public float? Y { get; init; }
    }

    /// <summary>
    /// Column x positions taken from the first row that resolves each column —
    /// the original, order-only scheme, kept as the fallback for inference.
    /// </summary>
    private static List<float> LegacyColumnPositions(
        List<StructTableRow> pageRows,
        Dictionary<long, List<int>> mcidToItems,
        IReadOnlyList<TextItem> items,
        uint page,
        int numCols)
    {
        var colPositions = new List<float>(numCols);
        for (var col = 0; col < numCols; col++)
        {
            var position = 0.0f;
            foreach (var row in pageRows)
            {
                if (col >= row.Cells.Count)
                {
                    continue;
                }

                float? x = null;
                foreach (var (mcid, p) in row.Cells[col].Mcids)
                {
                    if (p != page || !mcidToItems.TryGetValue(mcid, out var indices))
                    {
                        continue;
                    }

                    foreach (var idx in indices)
                    {
                        x = x is { } current ? MathF.Min(current, items[idx].X) : items[idx].X;
                    }
                }

                if (x is { } found)
                {
                    position = found;
                    break;
                }
            }

            colPositions.Add(position);
        }

        return colPositions;
    }

    /// <summary>
    /// Derives column anchors from the row that resolved the most cell positions,
    /// topping up from the remaining cell x values and then the fallback list.
    /// </summary>
    private static List<float> InferColumnPositions(
        List<List<MatchedCell>> rawRows,
        List<float> fallbackPositions,
        int numCols)
    {
        const float SameColumnTolerance = 18.0f;

        var anchors = new List<float>();
        var bestCount = -1;
        foreach (var row in rawRows)
        {
            // Ties go to the last row, matching the reference's max_by_key.
            var count = row.Count(cell => cell.X is not null);
            if (count >= bestCount)
            {
                bestCount = count;
                anchors = row.Where(cell => cell.X is not null).Select(cell => cell.X!.Value).ToList();
            }
        }

        if (anchors.Count > numCols)
        {
            anchors.RemoveRange(numCols, anchors.Count - numCols);
        }

        var additionalPositions = rawRows
            .SelectMany(row => row.Where(cell => cell.X is not null).Select(cell => cell.X!.Value))
            .OrderBy(x => x, FloatTotalOrder.Instance)
            .ToList();

        foreach (var x in additionalPositions)
        {
            if (anchors.Count >= numCols)
            {
                break;
            }

            if (anchors.All(existing => MathF.Abs(x - existing) > SameColumnTolerance))
            {
                anchors.Add(x);
                anchors.Sort(FloatTotalOrder.Instance);
            }
        }

        if (anchors.Count < numCols)
        {
            foreach (var x in fallbackPositions)
            {
                if (anchors.Count >= numCols)
                {
                    break;
                }

                if (anchors.All(existing => MathF.Abs(x - existing) > SameColumnTolerance))
                {
                    anchors.Add(x);
                    anchors.Sort(FloatTotalOrder.Instance);
                }
            }
        }

        if (anchors.Count == 0)
        {
            return [.. fallbackPositions];
        }

        while (anchors.Count < numCols)
        {
            anchors.Add(anchors[^1]);
        }

        return anchors;
    }

    /// <summary>
    /// Assigns cell x positions to columns, in order, minimising total distance.
    /// A ragged row has fewer cells than columns, so the gaps have to land where
    /// the geometry says they do rather than at the end.
    /// </summary>
    private static List<int> AlignPositionsToColumns(IReadOnlyList<float> cellXs, IReadOnlyList<float> columns)
    {
        if (cellXs.Count == 0 || columns.Count == 0)
        {
            return [];
        }

        if (cellXs.Count >= columns.Count)
        {
            return [.. Enumerable.Range(0, Math.Min(cellXs.Count, columns.Count))];
        }

        var dp = new float[cellXs.Count + 1][];
        var take = new bool[cellXs.Count + 1][];
        for (var i = 0; i <= cellXs.Count; i++)
        {
            dp[i] = new float[columns.Count + 1];
            take[i] = new bool[columns.Count + 1];
            Array.Fill(dp[i], float.PositiveInfinity);
        }

        Array.Fill(dp[0], 0.0f);

        for (var i = 1; i <= cellXs.Count; i++)
        {
            for (var j = 1; j <= columns.Count; j++)
            {
                var skipCost = dp[i][j - 1];
                var takeCost = dp[i - 1][j - 1] + MathF.Abs(cellXs[i - 1] - columns[j - 1]);
                if (takeCost <= skipCost)
                {
                    dp[i][j] = takeCost;
                    take[i][j] = true;
                }
                else
                {
                    dp[i][j] = skipCost;
                }
            }
        }

        var assignments = new List<int>(cellXs.Count);
        var row = cellXs.Count;
        var col = columns.Count;
        while (row > 0 && col > 0)
        {
            if (take[row][col])
            {
                assignments.Add(col - 1);
                row--;
                col--;
            }
            else
            {
                col--;
            }
        }

        assignments.Reverse();
        return assignments;
    }

    /// <summary>Lays out each row's cells against the inferred column anchors.</summary>
    private static (List<List<string>> Cells, List<float> RowPositions, List<int> ItemIndices)
        AlignStructRows(List<List<MatchedCell>> rawRows, List<float> colPositions)
    {
        var cells = new List<List<string>>(rawRows.Count);
        var rowPositions = new List<float>(rawRows.Count);
        var allItemIndices = new List<int>();

        foreach (var row in rawRows)
        {
            var presentCells = row
                .Where(cell => cell.ItemIndices.Count > 0 || cell.Text.Length > 0 || cell.X is not null)
                .ToList();
            var cellXs = presentCells.Where(c => c.X is not null).Select(c => c.X!.Value).ToList();
            var assignments = cellXs.Count == presentCells.Count
                ? AlignPositionsToColumns(cellXs, colPositions)
                : [.. Enumerable.Range(0, Math.Min(presentCells.Count, colPositions.Count))];

            var rowCells = new List<string>(colPositions.Count);
            for (var i = 0; i < colPositions.Count; i++)
            {
                rowCells.Add(string.Empty);
            }

            for (var i = 0; i < presentCells.Count && i < assignments.Count; i++)
            {
                var cell = presentCells[i];
                var colIdx = assignments[i];
                if (cell.Text.Length > 0)
                {
                    rowCells[colIdx] = rowCells[colIdx].Length > 0
                        ? rowCells[colIdx] + " " + cell.Text
                        : cell.Text;
                }

                allItemIndices.AddRange(cell.ItemIndices);
            }

            var rowY = 0.0f;
            var sawY = false;
            foreach (var cell in row)
            {
                if (cell.Y is { } y)
                {
                    rowY = sawY ? MathF.Max(rowY, y) : y;
                    sawY = true;
                }
            }

            cells.Add(rowCells);
            rowPositions.Add(rowY);
        }

        return (cells, rowPositions, allItemIndices);
    }

    /// <summary>
    /// Lays out each row's cells in declaration order, padding or truncating to
    /// the column count. This is the original scheme, kept because it is right
    /// whenever the structure tree's cell order is trustworthy.
    /// </summary>
    private static (List<List<string>> Cells, List<float> RowPositions, List<int> ItemIndices)
        LeftAlignStructRows(List<List<MatchedCell>> rawRows, int numCols)
    {
        var cells = new List<List<string>>(rawRows.Count);
        var rowPositions = new List<float>(rawRows.Count);
        var allItemIndices = new List<int>();

        foreach (var row in rawRows)
        {
            var rowCells = row.Select(cell => cell.Text).ToList();
            if (rowCells.Count > numCols)
            {
                rowCells.RemoveRange(numCols, rowCells.Count - numCols);
            }

            while (rowCells.Count < numCols)
            {
                rowCells.Add(string.Empty);
            }

            cells.Add(rowCells);
            allItemIndices.AddRange(row.SelectMany(cell => cell.ItemIndices));

            var rowY = 0.0f;
            var sawY = false;
            foreach (var cell in row)
            {
                if (cell.Y is { } y)
                {
                    rowY = sawY ? MathF.Max(rowY, y) : y;
                    sawY = true;
                }
            }

            rowPositions.Add(rowY);
        }

        return (cells, rowPositions, allItemIndices);
    }

    /// <summary>
    /// Pulls a header row that the structure tree never claimed back into the
    /// table. Some producers tag the body rows but leave the header as loose
    /// content just above; without this the column labels are lost.
    /// </summary>
    private static void RecoverUnclaimedHeaderRow(Table table, IReadOnlyList<TextItem> items, bool hasRaggedRows)
    {
        if (!hasRaggedRows || table.Rows.Count == 0 || table.Columns.Count < 3)
        {
            return;
        }

        const float MaxHeaderDistance = 90.0f;
        const float MaxGapToTable = 35.0f;
        const float MaxInterHeaderGap = 25.0f;
        const int MaxHeaderRows = 3;
        const float YTolerance = 5.0f;

        var topRowY = table.Rows[0];
        var xMin = (table.Columns.Count > 0 ? table.Columns[0] : 0.0f) - 25.0f;
        var xMax = (table.Columns.Count > 0 ? table.Columns[^1] : 0.0f) + 120.0f;
        var claimed = table.ItemIndices.ToHashSet();

        var candidateRows = new List<(float Y, List<(int Index, TextItem Item)> Items)>();
        for (var idx = 0; idx < items.Count; idx++)
        {
            var item = items[idx];
            if (claimed.Contains(idx)
                || item.Text.Trim().Length == 0
                || item.Y <= topRowY
                || item.Y - topRowY > MaxHeaderDistance
                || item.X < xMin
                || item.X > xMax)
            {
                continue;
            }

            var existing = candidateRows.FindIndex(r => MathF.Abs(item.Y - r.Y) < YTolerance);
            if (existing >= 0)
            {
                candidateRows[existing].Items.Add((idx, item));
            }
            else
            {
                candidateRows.Add((item.Y, [(idx, item)]));
            }
        }

        if (candidateRows.Count == 0)
        {
            return;
        }

        foreach (var (_, rowItems) in candidateRows)
        {
            rowItems.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X));
        }

        candidateRows.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Y, b.Y));

        if (candidateRows[0].Y - topRowY > MaxGapToTable)
        {
            return;
        }

        var selectedRows = new List<(float Y, List<(int Index, TextItem Item)> Items)> { candidateRows[0] };
        var prevY = candidateRows[0].Y;
        foreach (var candidate in candidateRows.Skip(1))
        {
            if (selectedRows.Count >= MaxHeaderRows || candidate.Y - prevY > MaxInterHeaderGap)
            {
                break;
            }

            prevY = candidate.Y;
            selectedRows.Add(candidate);
        }

        var assignedRows = new List<(float Y, List<string> Cells, List<int> Indices)>();
        var closestRowPopulated = 0;
        var combinedCols = new HashSet<int>();

        for (var rowIdx = 0; rowIdx < selectedRows.Count; rowIdx++)
        {
            var (rowY, rowItems) = selectedRows[rowIdx];
            if (rowItems.Count > table.Columns.Count)
            {
                return;
            }

            var rowXs = rowItems.Select(p => p.Item.X).ToList();
            var assignments = AlignPositionsToColumns(rowXs, table.Columns);
            if (assignments.Count != rowItems.Count)
            {
                return;
            }

            var rowCells = new List<string>(table.Columns.Count);
            for (var i = 0; i < table.Columns.Count; i++)
            {
                rowCells.Add(string.Empty);
            }

            var rowIndices = new List<int>(rowItems.Count);
            var populatedCols = new HashSet<int>();

            for (var i = 0; i < rowItems.Count; i++)
            {
                var (idx, item) = rowItems[i];
                var colIdx = assignments[i];
                var text = item.Text.Trim();
                if (text.Length == 0)
                {
                    continue;
                }

                rowCells[colIdx] = rowCells[colIdx].Length > 0 ? rowCells[colIdx] + " " + text : text;
                rowIndices.Add(idx);
                populatedCols.Add(colIdx);
            }

            if (rowIdx == 0)
            {
                closestRowPopulated = populatedCols.Count;
            }

            combinedCols.UnionWith(populatedCols);
            assignedRows.Add((rowY, rowCells, rowIndices));
        }

        var requiredCols = table.Columns.Count <= 4 ? table.Columns.Count : table.Columns.Count - 1;
        if (closestRowPopulated < 2 || combinedCols.Count < requiredCols)
        {
            return;
        }

        var headerCells = new List<string>(table.Columns.Count);
        for (var i = 0; i < table.Columns.Count; i++)
        {
            headerCells.Add(string.Empty);
        }

        var headerIndices = new List<int>();

        // Bottom-up, so a two-line header reads in visual order once joined.
        for (var i = assignedRows.Count - 1; i >= 0; i--)
        {
            var (_, rowCells, rowIndices) = assignedRows[i];
            for (var colIdx = 0; colIdx < rowCells.Count; colIdx++)
            {
                if (rowCells[colIdx].Length == 0)
                {
                    continue;
                }

                headerCells[colIdx] = headerCells[colIdx].Length > 0
                    ? headerCells[colIdx] + " " + rowCells[colIdx]
                    : rowCells[colIdx];
            }

            headerIndices.AddRange(rowIndices);
        }

        var headerY = topRowY;
        var sawY = false;
        foreach (var (y, _, _) in assignedRows)
        {
            headerY = sawY ? MathF.Max(headerY, y) : y;
            sawY = true;
        }

        table.Rows.Insert(0, headerY);
        table.Cells.Insert(0, headerCells);
        table.ItemIndices.AddRange(headerIndices);
        table.ItemIndices.Sort();
        Dedup(table.ItemIndices);
    }

    /// <summary>Removes consecutive duplicates from a sorted index list, in place.</summary>
    private static void Dedup(List<int> sorted)
    {
        var write = 0;
        for (var read = 0; read < sorted.Count; read++)
        {
            if (write == 0 || sorted[read] != sorted[write - 1])
            {
                sorted[write++] = sorted[read];
            }
        }

        if (write < sorted.Count)
        {
            sorted.RemoveRange(write, sorted.Count - write);
        }
    }

    /// <summary>
    /// Builds tables from structure-tree descriptors by matching MCIDs to text
    /// items. A table whose cells resolve at under 30% coverage is rejected: the
    /// structure tree is stale or broken and its geometry cannot be trusted.
    /// </summary>
    public static List<Table> DetectTablesFromStructTree(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<StructTable> structTables,
        uint page)
    {
        if (structTables.Count == 0)
        {
            return [];
        }

        var mcidToItems = new Dictionary<long, List<int>>();
        for (var idx = 0; idx < items.Count; idx++)
        {
            var item = items[idx];
            if (item.Page == page && item.Mcid is { } mcid)
            {
                if (!mcidToItems.TryGetValue(mcid, out var list))
                {
                    list = [];
                    mcidToItems[mcid] = list;
                }

                list.Add(idx);
            }
        }

        var tables = new List<Table>();

        foreach (var st in structTables)
        {
            var pageRows = st.Rows
                .Where(row => row.Cells.Any(cell => cell.Mcids.Any(m => m.Page == page)))
                .ToList();

            Log.Debug(Module, () =>
                $"page {page}: struct table has {pageRows.Count} rows on this page (from {st.Rows.Count} total)");

            if (pageRows.Count < 2)
            {
                continue;
            }

            var numCols = pageRows.Max(r => r.Cells.Count);
            if (numCols < 2)
            {
                continue;
            }

            var rawRows = new List<List<MatchedCell>>();
            var totalCells = 0u;
            var matchedCells = 0u;

            foreach (var row in pageRows)
            {
                var rowCells = new List<MatchedCell>(row.Cells.Count);
                foreach (var cell in row.Cells)
                {
                    totalCells++;

                    var cellItems = new List<(int Index, TextItem Item)>();
                    foreach (var (mcid, p) in cell.Mcids)
                    {
                        if (p == page && mcidToItems.TryGetValue(mcid, out var indices))
                        {
                            foreach (var idx in indices)
                            {
                                cellItems.Add((idx, items[idx]));
                            }
                        }
                    }

                    if (cellItems.Count > 0)
                    {
                        matchedCells++;
                    }

                    // Top to bottom (descending Y), then left to right.
                    cellItems.Sort((a, b) =>
                    {
                        var cmp = b.Item.Y.CompareTo(a.Item.Y);
                        return cmp != 0 ? cmp : a.Item.X.CompareTo(b.Item.X);
                    });

                    float? x = null;
                    float? y = null;
                    foreach (var (_, item) in cellItems)
                    {
                        x = x is { } cx ? MathF.Min(cx, item.X) : item.X;
                        y = y is { } cy ? MathF.Max(cy, item.Y) : item.Y;
                    }

                    rowCells.Add(new MatchedCell
                    {
                        Text = string.Join(' ', cellItems.Select(p => p.Item.Text)),
                        ItemIndices = cellItems.Select(p => p.Index).ToList(),
                        X = x,
                        Y = y,
                    });
                }

                rawRows.Add(rowCells);
            }

            var coverage = totalCells > 0 ? matchedCells / (float)totalCells : 0.0f;
            Log.Debug(Module, () =>
                $"page {page}: struct table {pageRows.Count}x{numCols}, {matchedCells}/{totalCells} cells matched " +
                $"({coverage * 100.0f:F0}%)");
            if (totalCells == 0 || coverage < 0.3f)
            {
                continue;
            }

            var hasRaggedRows = rawRows.Any(row => row.Count(cell => cell.X is not null) < numCols);
            var firstRowHasTaggedHeader = pageRows.Count > 0
                && pageRows[0].Cells.Count(cell => cell.IsHeader) * 2 >= pageRows[0].Cells.Count;

            var fallbackColPositions = LegacyColumnPositions(pageRows, mcidToItems, items, page, numCols);
            var (legacyCells, legacyRowPositions, legacyItemIndices) = LeftAlignStructRows(rawRows, numCols);
            legacyItemIndices.Sort();
            Dedup(legacyItemIndices);
            var legacyTable = Table.Create(
                [.. fallbackColPositions],
                legacyRowPositions,
                legacyCells,
                legacyItemIndices);

            var colPositions = InferColumnPositions(rawRows, fallbackColPositions, numCols);
            var (alignedCells, alignedRowPositions, alignedItemIndices) = AlignStructRows(rawRows, colPositions);
            alignedItemIndices.Sort();
            Dedup(alignedItemIndices);

            var alignedTable = Table.Create(colPositions, alignedRowPositions, alignedCells, alignedItemIndices);
            var itemCountBeforeHeader = alignedTable.ItemIndices.Count;
            var rowCountBeforeHeader = alignedTable.Cells.Count;
            RecoverUnclaimedHeaderRow(alignedTable, items, hasRaggedRows && !firstRowHasTaggedHeader);

            // The aligned layout only earns its keep when it recovered a header;
            // otherwise the declaration-order layout is the safer reading.
            var recoveredHeader = alignedTable.ItemIndices.Count > itemCountBeforeHeader
                || alignedTable.Cells.Count > rowCountBeforeHeader;

            tables.Add(recoveredHeader ? alignedTable : legacyTable);
        }

        return tables;
    }
}
