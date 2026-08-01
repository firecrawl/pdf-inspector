// Ported from reference/src/tables/detect_lines.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>A horizontal rule: its baseline and horizontal extent.</summary>
internal readonly record struct HorizontalRule(float Y, float XMin, float XMax)
{
    public float Width => XMax - XMin;
}

/// <summary>A vertical rule: its x position and vertical extent.</summary>
internal readonly record struct VerticalRule(float X, float YMin, float YMax)
{
    public float Height => YMax - YMin;
}

/// <summary>
/// Detects tables from the path operators that draw ruled gridlines. Many
/// government forms use strokes rather than rectangles, so this is the second
/// detection strategy, tried after rectangles and before the heuristics.
/// </summary>
internal static class LineDetector
{
    private const string Module = "tables";

    internal const float RuleYTolerance = 2.0f;
    internal const float RuleJoinGap = 6.0f;
    internal const float RuleSpanTolerance = 8.0f;
    internal const float TextRowTolerance = 2.5f;

    public static List<Table> DetectTablesFromLines(IReadOnlyList<TextItem> items, IReadOnlyList<PdfLine> lines, uint page) =>
        Detect(items, lines, page, allowTextAnchors: true, allowAlternatives: true);

    /// <summary>
    /// Detects only tables whose grid is backed by explicit vector geometry.
    /// Region-level callers need physical cell boundaries for crop boxes, so
    /// columns inferred from sparse rules are deliberately excluded.
    /// </summary>
    public static List<Table> DetectVectorGridTablesFromLines(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfLine> lines,
        uint page) =>
        Detect(items, lines, page, allowTextAnchors: false, allowAlternatives: false);

    private static List<Table> Detect(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfLine> lines,
        uint page,
        bool allowTextAnchors,
        bool allowAlternatives)
    {
        var pageLines = lines.Where(l => l.Page == page).ToList();
        if (pageLines.Count == 0)
        {
            return [];
        }

        var (horizontals, verticals) = ClassifyRules(pageLines);

        if (horizontals.Count < 2)
        {
            return [];
        }

        var alternatives = new List<Table>();
        if (allowAlternatives)
        {
            if (TextAnchorTables.BuildDenseRowAnchorTable(items, horizontals, verticals, page) is { } dense)
            {
                alternatives.Add(dense);
            }

            alternatives.AddRange(TextAnchorTables.BuildOpenEdgeGridTables(items, horizontals, verticals, page));
        }

        // Booktabs and response-form tables draw horizontal rules only. Those
        // rules describe bands rather than cell boundaries, so columns come
        // from the first text row and rows from text baselines, before the
        // endpoint-grid path can collapse adjacent tables together.
        if (allowTextAnchors)
        {
            var anchorCandidates = TextAnchorTables.DetectTextAnchorRuleTables(
                items, horizontals, verticals, lines, page);

            if (anchorCandidates.Count > 0)
            {
                return CombineWithAnchorTables(items, lines, page, alternatives, anchorCandidates);
            }
        }

        if (horizontals.Count < 3)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        // With no drawn verticals, the break points between per-cell horizontal
        // segments encode the column boundaries. Catalogue and finding-aid
        // layouts are drawn this way.
        List<float>? implicitColEdges = verticals.Count < 2
            ? DeriveColumnsFromHorizontalSegments(horizontals)
            : null;

        if (verticals.Count < 2 && implicitColEdges is null)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        var colsFromSegments = implicitColEdges is not null;

        Log.Debug(Module, () =>
            $"detect_lines p{page}: {horizontals.Count} horiz, {verticals.Count} vert lines " +
            $"(of {pageLines.Count} total on page)" +
            (colsFromSegments ? " — columns from horizontal segments" : string.Empty));

        var rowEdges = RectGrid.SnapEdges(horizontals.Select(h => h.Y).ToList(), 3.0f);
        var colEdges = implicitColEdges
            ?? RectGrid.SnapEdges(verticals.Select(v => v.X).ToList(), 3.0f);

        Log.Debug(Module, () =>
            $"detect_lines p{page}: {rowEdges.Count} row edges, {colEdges.Count} col edges after snap");

        // Two columns and two rows minimum: a lone column of horizontal lines
        // is separator ruling, not a table.
        if (rowEdges.Count < 3 || colEdges.Count < 3)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        // Beyond twenty columns this is almost certainly a diagram.
        if (colEdges.Count > 21 || rowEdges.Count > 80)
        {
            Log.Debug(Module, () =>
                $"detect_lines p{page}: rejected — too many edges ({rowEdges.Count}x{colEdges.Count})");
            return SelectTableHypothesis([], alternatives, page);
        }

        var tableXMin = colEdges[0];
        var tableXMax = colEdges[^1];
        var tableWidth = tableXMax - tableXMin;

        if (tableWidth < 50.0f)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        var tableYMin = rowEdges[0];
        var tableYMax = rowEdges[^1];
        var tableHeight = MathF.Abs(tableYMax - tableYMin);

        if (tableHeight < 20.0f)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        // A decorative outer border has only four edges. Real full-page tables
        // span the same dimensions but carry many internal rules, so the
        // rejection applies only to a bare frame.
        if (tableWidth > 500.0f && tableHeight > 700.0f && horizontals.Count <= 4 && verticals.Count <= 4)
        {
            Log.Debug(Module, () =>
                $"detect_lines p{page}: rejected — page-spanning frame ({tableWidth:F0}×{tableHeight:F0}, " +
                $"{horizontals.Count} h + {verticals.Count} v)");
            return SelectTableHypothesis([], alternatives, page);
        }

        // Enough horizontal rules must span a meaningful width. Full-width
        // rules are ideal, but many partial ones are equally good evidence.
        var spanningH = horizontals.Count(h => h.Width > tableWidth * 0.5f);
        var partialH = horizontals.Count(h => h.Width > tableWidth * 0.15f);

        if (spanningH < 3 && partialH < 6)
        {
            Log.Debug(Module, () =>
                $"detect_lines p{page}: rejected — {spanningH} spanning + {partialH} partial H lines");
            return SelectTableHypothesis([], alternatives, page);
        }

        // The same test for verticals, skipped when the columns came from
        // segment endpoints: there are no verticals to check, and the endpoint
        // consistency test already served as the guard.
        var spanningV = 0;
        if (!colsFromSegments)
        {
            var s = verticals.Count(v => v.Height > tableHeight * 0.3f);
            var p = verticals.Count(v => v.Height > tableHeight * 0.10f);

            if (s < 2 && p < 4)
            {
                Log.Debug(Module, () =>
                    $"detect_lines p{page}: rejected — {s} spanning + {p} partial V lines");
                return SelectTableHypothesis([], alternatives, page);
            }

            spanningV = s;
        }

        var rowEdgesDesc = rowEdges.OrderByDescending(v => v, FloatTotalOrder.Instance).ToList();

        Log.Debug(Module, () =>
            $"detect_lines p{page}: {rowEdgesDesc.Count} row_edges, {colEdges.Count} col_edges, " +
            $"table=({tableXMin:F0},{tableYMin:F0})-({tableXMax:F0},{tableYMax:F0}), " +
            $"spanning_h={spanningH}, spanning_v={spanningV}");

        var (cells, itemIndices) = RectGrid.AssignItemsToGrid(items, colEdges, rowEdgesDesc, page);

        var nonEmptyRows = cells.Count(row => row.Any(cell => cell.Length > 0));
        if (nonEmptyRows < 2)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        var numColsGrid = cells.Count > 0 ? cells[0].Count : 0;
        var totalCells = cells.Count * numColsGrid;

        if (totalCells > 0)
        {
            var filledCells = cells.SelectMany(row => row).Count(cell => cell.Length > 0);
            if ((float)filledCells / totalCells < 0.15f)
            {
                return SelectTableHypothesis([], alternatives, page);
            }
        }

        // Charts concentrate their text on one axis; a real table spreads data
        // across columns.
        var colsWithContent = Enumerable.Range(0, numColsGrid)
            .Count(c => cells.Any(row => c < row.Count && row[c].Length > 0));

        if (colsWithContent < 2)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        // A chart grid on a textbook page captures scattered labels but misses
        // the bulk of the page's text.
        var pageItemCount = items.Count(i => i.Page == page);
        if (pageItemCount > 0 && (float)itemIndices.Count / pageItemCount < 0.20f)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        // Near-identical row spacing means chart gridlines. Spreadsheet exports
        // are uniform too, so the threshold is tight.
        if (rowEdgesDesc.Count >= 5)
        {
            var spacings = new List<float>();
            for (var i = 0; i + 1 < rowEdgesDesc.Count; i++)
            {
                spacings.Add(MathF.Abs(rowEdgesDesc[i] - rowEdgesDesc[i + 1]));
            }

            var meanSpacing = spacings.Sum() / spacings.Count;
            if (meanSpacing > 0.1f)
            {
                var variance = spacings.Sum(s => (s - meanSpacing) * (s - meanSpacing)) / spacings.Count;
                if (MathF.Sqrt(variance) / meanSpacing < 0.02f)
                {
                    return SelectTableHypothesis([], alternatives, page);
                }
            }
        }

        var numCols = colEdges.Count - 1;
        var numRows = rowEdgesDesc.Count - 1;

        if (numRows < 2 || numCols < 2)
        {
            return SelectTableHypothesis([], alternatives, page);
        }

        Log.Debug(Module, () =>
            $"detect_lines p{page}: ACCEPTED {numRows}x{numCols} grid, {itemIndices.Count} items " +
            $"captured of {pageItemCount} on page, non_empty_rows={nonEmptyRows}, " +
            $"cols_with_content={colsWithContent}");

        var legacy = new List<Table>
        {
            Table.Create(colEdges, rowEdgesDesc[..numRows], cells, itemIndices),
        };

        return SelectTableHypothesis(legacy, alternatives, page);
    }

    /// <summary>
    /// Reconciles anchor-derived tables with the geometry-only path. Sparse-rule
    /// and physical-grid tables can share a page, so only the graphics belonging
    /// to accepted anchor bands are removed before re-running detection.
    /// </summary>
    private static List<Table> CombineWithAnchorTables(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfLine> lines,
        uint page,
        List<Table> alternatives,
        List<TextAnchorTable> anchorCandidates)
    {
        var remainingLines = lines
            .Where(line => !anchorCandidates.Any(t => t.OverlapsLine(line)))
            .ToList();

        var anchorTables = anchorCandidates.Select(c => c.Table).ToList();

        // Geometry and the dense/open-edge alternatives are re-evaluated on
        // what is left, without recursing into sparse text anchors. The
        // geometry-only result is kept as a fallback: an inferred header can
        // otherwise reach into an accepted anchor band and displace a valid
        // physical grid.
        var vectorTables = Detect(items, remainingLines, page, allowTextAnchors: false, allowAlternatives: false);
        var inferredTables = Detect(items, remainingLines, page, allowTextAnchors: false, allowAlternatives: true);

        inferredTables.RemoveAll(table => anchorTables.Any(anchor => TablesShareItems(table, anchor)));

        var remainingTables = CombineNonOverlappingTables(inferredTables, vectorTables);

        // A page-wide alternative that swallows two already-independent sparse
        // tables is a synthetic merge, not a stronger hypothesis.
        alternatives.RemoveAll(alternative => OverlapsMultipleTables(alternative, anchorTables));

        var competing = new List<Table>(anchorTables);
        competing.AddRange(alternatives);

        return CombineNonOverlappingTables(SelectNonOverlappingHypotheses(competing), remainingTables);
    }

    /// <summary>Classifies page lines as horizontal or vertical, within two degrees of axis.</summary>
    private static (List<HorizontalRule> Horizontals, List<VerticalRule> Verticals) ClassifyRules(
        List<PdfLine> pageLines)
    {
        var horizontals = new List<HorizontalRule>();
        var verticals = new List<VerticalRule>();

        var angleTolerance = MathF.Tan(2.0f * MathF.PI / 180.0f);

        foreach (var line in pageLines)
        {
            var dx = MathF.Abs(line.X2 - line.X1);
            var dy = MathF.Abs(line.Y2 - line.Y1);
            var length = MathF.Sqrt((dx * dx) + (dy * dy));

            // Very short strokes are decorations and tick marks.
            if (length < 20.0f)
            {
                continue;
            }

            if (dx > 0.01f && dy / dx <= angleTolerance)
            {
                horizontals.Add(new HorizontalRule(
                    (line.Y1 + line.Y2) / 2.0f,
                    MathF.Min(line.X1, line.X2),
                    MathF.Max(line.X1, line.X2)));
            }
            else if (dy > 0.01f && dx / dy <= angleTolerance)
            {
                verticals.Add(new VerticalRule(
                    (line.X1 + line.X2) / 2.0f,
                    MathF.Min(line.Y1, line.Y2),
                    MathF.Max(line.Y1, line.Y2)));
            }

            // Diagonals are ignored.
        }

        return (horizontals, verticals);
    }

    /// <summary>
    /// Derives column edges from the endpoints of per-cell horizontal segments,
    /// for tables drawn with no vertical dividers at all. An x position
    /// qualifies when it is an endpoint on at least half the rule rows.
    /// </summary>
    internal static List<float>? DeriveColumnsFromHorizontalSegments(List<HorizontalRule> horizontals)
    {
        if (horizontals.Count < 3)
        {
            return null;
        }

        var endpoints = new List<float>(horizontals.Count * 2);
        foreach (var h in horizontals)
        {
            endpoints.Add(h.XMin);
            endpoints.Add(h.XMax);
        }

        var clusters = RectGrid.SnapEdges(endpoints, 5.0f);
        if (clusters.Count < 3)
        {
            return null;
        }

        // Rows are bucketed at a tenth of a point, which tolerates the three-point
        // clustering applied to row edges later.
        var uniqueRows = horizontals.Select(h => (int)MathF.Round(h.Y * 10.0f)).ToHashSet();
        if (uniqueRows.Count < 2)
        {
            return null;
        }

        var minRows = (int)MathF.Ceiling(uniqueRows.Count * 0.5f);

        var qualifying = clusters.Where(clusterX =>
        {
            var rowsTouched = horizontals
                .Where(h => MathF.Abs(h.XMin - clusterX) < 5.0f || MathF.Abs(h.XMax - clusterX) < 5.0f)
                .Select(h => (int)MathF.Round(h.Y * 10.0f))
                .ToHashSet();

            return rowsTouched.Count >= minRows;
        }).ToList();

        return qualifying.Count >= 3 ? qualifying : null;
    }

    // ── Hypothesis selection ─────────────────────────────────────────────

    /// <summary>
    /// Scores how much evidence a table carries. Captured items dominate, then
    /// filled cells and occupied columns; empty cells count slightly against.
    /// </summary>
    internal static int TableEvidenceScore(Table table)
    {
        var filledCells = table.Cells.SelectMany(row => row).Count(cell => cell.Length > 0);
        var occupiedRows = table.Cells.Count(row => row.Any(cell => cell.Length > 0));
        var columnCount = table.Cells.Count > 0 ? table.Cells[0].Count : 0;

        var occupiedColumns = Enumerable.Range(0, columnCount)
            .Count(column => table.Cells.Any(row => column < row.Count && row[column].Length > 0));

        var emptyCells = (table.Cells.Count * columnCount) - filledCells;

        var positiveEvidence = (table.ItemIndices.Count * 100)
            + (filledCells * 25)
            + (occupiedColumns * 60)
            + (occupiedRows * 20);

        return Math.Max(positiveEvidence - (emptyCells * 4), 0);
    }

    /// <summary>Keeps the strongest hypotheses that claim no item twice.</summary>
    internal static List<Table> SelectNonOverlappingHypotheses(List<Table> candidates)
    {
        var ordered = candidates
            .OrderByDescending(TableEvidenceScore)
            .ThenByDescending(t => t.ItemIndices.Count)
            .ToList();

        var selected = new List<Table>();
        var claimedItems = new HashSet<int>();

        foreach (var table in ordered)
        {
            if (table.ItemIndices.Any(claimedItems.Contains))
            {
                continue;
            }

            claimedItems.UnionWith(table.ItemIndices);
            selected.Add(table);
        }

        // Returned top of page first.
        selected.Sort((a, b) => FloatTotalOrder.Instance.Compare(
            b.Rows.Count > 0 ? b.Rows[0] : 0f,
            a.Rows.Count > 0 ? a.Rows[0] : 0f));

        return selected;
    }

    internal static bool TablesShareItems(Table left, Table right) =>
        left.ItemIndices.Any(right.ItemIndices.Contains);

    internal static bool OverlapsMultipleTables(Table candidate, List<Table> tables) =>
        tables.Count(table => TablesShareItems(candidate, table)) > 1;

    /// <summary>Adds the secondary tables that do not collide with the primary ones.</summary>
    internal static List<Table> CombineNonOverlappingTables(List<Table> primary, List<Table> secondary)
    {
        var result = new List<Table>(primary);

        foreach (var table in secondary)
        {
            if (!result.Any(existing => TablesShareItems(table, existing)))
            {
                result.Add(table);
            }
        }

        return result;
    }

    private static List<Table> SelectTableHypothesis(List<Table> legacy, List<Table> alternatives, uint page)
    {
        if (alternatives.Count == 0)
        {
            return legacy;
        }

        if (legacy.Count == 0)
        {
            var selectedAlternatives = SelectNonOverlappingHypotheses(alternatives);
            Log.Debug(Module, () =>
                $"detect_lines p{page}: accepted {selectedAlternatives.Count} alternative table(s) (no legacy candidate)");
            return selectedAlternatives;
        }

        var legacyCount = legacy.Count;
        var candidates = new List<Table>(legacy);
        candidates.AddRange(alternatives);

        var selected = SelectNonOverlappingHypotheses(candidates);
        Log.Debug(Module, () =>
            $"detect_lines p{page}: selected {selected.Count} table(s) from {legacyCount} legacy and alternative hypotheses");

        return selected;
    }
}
