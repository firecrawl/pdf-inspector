// Ported from reference/src/tables/structured.rs
using System.Text;

namespace PdfInspector.Tables;

/// <summary>
/// A resolved table-structure-recognition cell: its grid metadata plus its bbox
/// in page PDF-points, top-left origin.
/// </summary>
public sealed class StructuredCell
{
    /// <summary>Zero-indexed grid row.</summary>
    public int Row { get; set; }

    /// <summary>Zero-indexed grid column.</summary>
    public int Col { get; set; }

    /// <summary>1 for a normal cell.</summary>
    public int RowSpan { get; set; } = 1;

    /// <summary>1 for a normal cell.</summary>
    public int ColSpan { get; set; } = 1;

    /// <summary>True when the cell is a <c>&lt;th&gt;</c> or sits inside <c>&lt;thead&gt;</c>.</summary>
    public bool IsHeader { get; set; }

    /// <summary>Cell text, filled in by the caller after overlap-testing PDF items.</summary>
    public string Text { get; set; } = string.Empty;

    /// <summary>Axis-aligned bbox <c>[x1, y1, x2, y2]</c> in page PDF-points, top-left origin.</summary>
    public float[] PagePtBBox { get; set; } = new float[4];
}

/// <summary>
/// Structure-recovery-aware (TSR) table assembly. Consumes the raw output of an
/// external table-structure recognition model — SLANet on PaddleOCR, say: a flat
/// list of HTML structure tokens plus a parallel list of per-cell bboxes. Each
/// cell open-tag is paired with its bbox in document order, row and column
/// position is tracked with rowspan/colspan awareness, and the result renders as
/// a markdown pipe table.
/// </summary>
/// <remarks>
/// No real HTML parser is needed: the token grammar is restricted (see
/// <see cref="ParseStructure"/>), so a small state machine suffices. Cell text is
/// supplied separately by the caller, typically by overlap-testing PDF text items
/// against each cell's page-PDF-point bbox.
/// </remarks>
public static class StructuredCells
{
    /// <summary>A parsed cell slot, before the caller fills in text and page coordinates.</summary>
    internal sealed class CellSlot
    {
        public int Row { get; init; }

        public int Col { get; init; }

        public int RowSpan { get; init; }

        public int ColSpan { get; init; }

        public bool IsHeader { get; init; }

        /// <summary>Index into the parallel cell-bbox array.</summary>
        public int BBoxIndex { get; init; }
    }

    /// <summary>
    /// Parses a sequence of SLANet structure tokens into ordered cell slots.
    /// </summary>
    /// <remarks>
    /// The token grammar needs no real HTML parsing:
    /// <list type="bullet">
    /// <item>Section markers — <c>&lt;thead&gt;</c>, <c>&lt;/thead&gt;</c>,
    /// <c>&lt;tbody&gt;</c>, <c>&lt;/tbody&gt;</c> — and wrapper tokens
    /// (<c>&lt;html&gt;</c>, <c>&lt;body&gt;</c>, <c>&lt;table&gt;</c> and their
    /// closing variants) are tracked or skipped.</item>
    /// <item><c>&lt;tr&gt;</c> opens a new row; <c>&lt;/tr&gt;</c> is informational.</item>
    /// <item>An empty cell arrives as one token: <c>&lt;td&gt;&lt;/td&gt;</c> or
    /// <c>&lt;th&gt;&lt;/th&gt;</c>.</item>
    /// <item>A cell with attributes spans several tokens: <c>&lt;td</c> (or
    /// <c>&lt;th</c>), then fragments like <c> colspan="4"</c>, then <c>&gt;</c>,
    /// then later <c>&lt;/td&gt;</c>.</item>
    /// </list>
    /// Cells inside <c>&lt;thead&gt;</c> and any <c>&lt;th&gt;</c> are flagged as
    /// headers. Rowspan and colspan are honoured, and a prior row's rowspan pushes
    /// later-row cells to the right.
    /// </remarks>
    internal static List<CellSlot> ParseStructure(IReadOnlyList<string> tokens)
    {
        var slots = new List<CellSlot>();
        var occupied = new HashSet<(int Row, int Col)>();
        var row = 0;
        var col = 0;
        var bboxIdx = 0;
        var inThead = false;
        var startedFirstRow = false;

        var i = 0;
        while (i < tokens.Count)
        {
            var tok = tokens[i].Trim();
            switch (tok)
            {
                case "<thead>":
                    inThead = true;
                    break;

                case "</thead>":
                    inThead = false;
                    break;

                case "<tr>":
                    if (startedFirstRow)
                    {
                        row++;
                    }

                    col = 0;
                    startedFirstRow = true;
                    break;

                case "<td></td>":
                case "<th></th>":
                {
                    var isTh = tok == "<th></th>";
                    while (occupied.Contains((row, col)))
                    {
                        col++;
                    }

                    slots.Add(new CellSlot
                    {
                        Row = row,
                        Col = col,
                        RowSpan = 1,
                        ColSpan = 1,
                        IsHeader = inThead || isTh,
                        BBoxIndex = bboxIdx,
                    });
                    bboxIdx++;
                    col++;
                    break;
                }

                case "<td":
                case "<th":
                {
                    var isTh = tok == "<th";
                    var rowspan = 1;
                    var colspan = 1;

                    // Consume attribute fragments until the closing ">".
                    i++;
                    while (i < tokens.Count && tokens[i].Trim() != ">")
                    {
                        if (ParseIntAttr(tokens[i], "rowspan") is { } r)
                        {
                            rowspan = Math.Max(r, 1);
                        }
                        else if (ParseIntAttr(tokens[i], "colspan") is { } c)
                        {
                            colspan = Math.Max(c, 1);
                        }

                        i++;
                    }

                    // i now points at ">", or off the end if the token run is malformed.
                    while (occupied.Contains((row, col)))
                    {
                        col++;
                    }

                    slots.Add(new CellSlot
                    {
                        Row = row,
                        Col = col,
                        RowSpan = rowspan,
                        ColSpan = colspan,
                        IsHeader = inThead || isTh,
                        BBoxIndex = bboxIdx,
                    });

                    for (var r = row; r < row + rowspan; r++)
                    {
                        for (var c = col; c < col + colspan; c++)
                        {
                            occupied.Add((r, c));
                        }
                    }

                    bboxIdx++;
                    col += colspan;
                    break;
                }

                default:
                    // Wrapper and informational tokens are no-ops.
                    break;
            }

            i++;
        }

        return slots;
    }

    /// <summary>
    /// Parses an attribute fragment such as <c> colspan="4"</c> or
    /// <c>rowspan='2'</c>, tolerating leading whitespace and either quote style.
    /// </summary>
    private static int? ParseIntAttr(string s, string name)
    {
        var trimmed = s.Trim();
        if (!trimmed.StartsWith(name, StringComparison.Ordinal))
        {
            return null;
        }

        var rest = trimmed[name.Length..].TrimStart();
        if (!rest.StartsWith('='))
        {
            return null;
        }

        var value = rest[1..].TrimStart().Trim('"', '\'');
        return int.TryParse(value, out var parsed) ? parsed : null;
    }

    /// <summary>
    /// Converts a SLANet polygon into an axis-aligned <c>[x1, y1, x2, y2]</c>
    /// rect. The 8-element form lists four corners; the corner order is ignored
    /// and min/max taken, so rotated polygons collapse to a sane bounding box. The
    /// 4-element form is already axis-aligned, as older SLANet variants emit.
    /// </summary>
    internal static float[]? PolygonToAabb(IReadOnlyList<float> coords)
    {
        switch (coords.Count)
        {
            case 4:
                return
                [
                    MathF.Min(coords[0], coords[2]),
                    MathF.Min(coords[1], coords[3]),
                    MathF.Max(coords[0], coords[2]),
                    MathF.Max(coords[1], coords[3]),
                ];

            case 8:
            {
                var xs = new[] { coords[0], coords[2], coords[4], coords[6] };
                var ys = new[] { coords[1], coords[3], coords[5], coords[7] };
                var x1 = xs.Aggregate(float.PositiveInfinity, MathF.Min);
                var y1 = ys.Aggregate(float.PositiveInfinity, MathF.Min);
                var x2 = xs.Aggregate(float.NegativeInfinity, MathF.Max);
                var y2 = ys.Aggregate(float.NegativeInfinity, MathF.Max);
                return float.IsFinite(x1) && float.IsFinite(y1) && float.IsFinite(x2) && float.IsFinite(y2)
                    ? [x1, y1, x2, y2]
                    : null;
            }

            default:
                return null;
        }
    }

    /// <summary>
    /// Converts a cell rect from crop image-pixel space to page PDF-points
    /// (top-left origin), given the crop's PDF-point offset on the page and the
    /// DPI the crop image was rendered at.
    /// </summary>
    internal static float[] CellPxToPagePt(float[] cellPx, float renderDpi, float[] cropOriginPt)
    {
        var ptPerPx = renderDpi > 0.0f ? 72.0f / renderDpi : 1.0f;
        var xOff = cropOriginPt[0];
        var yOff = cropOriginPt[1];
        return
        [
            (cellPx[0] * ptPerPx) + xOff,
            (cellPx[1] * ptPerPx) + yOff,
            (cellPx[2] * ptPerPx) + xOff,
            (cellPx[3] * ptPerPx) + yOff,
        ];
    }

    /// <summary>
    /// Refines TSR cell bboxes into non-overlapping row and column bands.
    /// SLANet-style boxes are often plausible but too tall on dense borderless
    /// tables; native PDF text assignment is more reliable when each parsed row
    /// owns the band between neighbouring row centers instead of the full model
    /// box.
    /// </summary>
    internal static void NormalizeCellBands(IList<StructuredCell> cells)
    {
        if (cells.Count < 2)
        {
            return;
        }

        var rowBands = DeriveAxisBands(cells, Axis.Y);
        var colBands = DeriveAxisBands(cells, Axis.X);

        foreach (var cell in cells)
        {
            var rowEnd = cell.Row + Math.Max(Math.Max(cell.RowSpan, 1) - 1, 0);
            if (rowBands.TryGetValue(cell.Row, out var rowStartBand)
                && rowBands.TryGetValue(rowEnd, out var rowEndBand))
            {
                var clampedY1 = MathF.Max(cell.PagePtBBox[1], rowStartBand.Lo);
                var clampedY2 = MathF.Min(cell.PagePtBBox[3], rowEndBand.Hi);
                if (clampedY1 < clampedY2)
                {
                    cell.PagePtBBox[1] = clampedY1;
                    cell.PagePtBBox[3] = clampedY2;
                }
            }

            var colEnd = cell.Col + Math.Max(Math.Max(cell.ColSpan, 1) - 1, 0);
            if (colBands.TryGetValue(cell.Col, out var colStartBand)
                && colBands.TryGetValue(colEnd, out var colEndBand))
            {
                var clampedX1 = MathF.Max(cell.PagePtBBox[0], colStartBand.Lo);
                var clampedX2 = MathF.Min(cell.PagePtBBox[2], colEndBand.Hi);
                if (clampedX1 < clampedX2)
                {
                    cell.PagePtBBox[0] = clampedX1;
                    cell.PagePtBBox[2] = clampedX2;
                }
            }
        }
    }

    /// <summary>Which axis a band derivation runs along.</summary>
    private enum Axis
    {
        X,
        Y,
    }

    /// <summary>
    /// Derives one band per row or column index, each spanning the midpoints
    /// between neighbouring band centers.
    /// </summary>
    private static Dictionary<int, (float Lo, float Hi)> DeriveAxisBands(IList<StructuredCell> cells, Axis axis)
    {
        var byIndex = new Dictionary<int, List<(float Lo, float Hi)>>();

        int IndexOf(StructuredCell c) => axis == Axis.X ? c.Col : c.Row;
        int SpanOf(StructuredCell c) => Math.Max(axis == Axis.X ? c.ColSpan : c.RowSpan, 1);

        // Non-spanning cells come first, so a colspan or rowspan box does not skew
        // a single column or row center. An index with no non-spanning example
        // falls back to whatever cells are anchored there.
        foreach (var cell in cells)
        {
            if (SpanOf(cell) == 1)
            {
                var idx = IndexOf(cell);
                if (!byIndex.TryGetValue(idx, out var list))
                {
                    list = [];
                    byIndex[idx] = list;
                }

                list.Add(AxisBounds(cell.PagePtBBox, axis));
            }
        }

        foreach (var cell in cells)
        {
            var idx = IndexOf(cell);
            if (!byIndex.ContainsKey(idx))
            {
                byIndex[idx] = [AxisBounds(cell.PagePtBBox, axis)];
            }
        }

        var rows = new List<(int Index, float Center, float MinEdge, float MaxEdge)>();
        foreach (var (idx, bounds) in byIndex)
        {
            var minEdge = float.PositiveInfinity;
            var maxEdge = float.NegativeInfinity;
            var centerSum = 0.0f;
            var count = 0;
            foreach (var (lo, hi) in bounds)
            {
                if (float.IsFinite(lo) && float.IsFinite(hi) && lo < hi)
                {
                    minEdge = MathF.Min(minEdge, lo);
                    maxEdge = MathF.Max(maxEdge, hi);
                    centerSum += (lo + hi) * 0.5f;
                    count++;
                }
            }

            if (count > 0)
            {
                rows.Add((idx, centerSum / count, minEdge, maxEdge));
            }
        }

        if (rows.Count < 2)
        {
            return rows.ToDictionary(r => r.Index, r => (r.MinEdge, r.MaxEdge));
        }

        rows.Sort((a, b) => a.Index.CompareTo(b.Index));

        var bands = new Dictionary<int, (float Lo, float Hi)>();
        for (var i = 0; i < rows.Count; i++)
        {
            var (idx, _, minEdge, maxEdge) = rows[i];
            var lo = i == 0 ? minEdge : (rows[i - 1].Center + rows[i].Center) * 0.5f;
            var hi = i + 1 == rows.Count ? maxEdge : (rows[i].Center + rows[i + 1].Center) * 0.5f;
            if (float.IsFinite(lo) && float.IsFinite(hi) && lo < hi)
            {
                bands[idx] = (lo, hi);
            }
        }

        return bands;
    }

    /// <summary>The low and high edges of a bbox along the given axis.</summary>
    private static (float Lo, float Hi) AxisBounds(float[] bbox, Axis axis) => axis == Axis.X
        ? (MathF.Min(bbox[0], bbox[2]), MathF.Max(bbox[0], bbox[2]))
        : (MathF.Min(bbox[1], bbox[3]), MathF.Max(bbox[1], bbox[3]));

    /// <summary>
    /// Sanitises cell text for a markdown pipe-table cell: collapses whitespace
    /// runs, drops newlines and tabs since a cell must stay on one line, and
    /// escapes pipes that would otherwise break the table.
    /// </summary>
    private static string SanitizeCell(string text)
    {
        var sb = new StringBuilder(text.Length);
        var prevSpace = false;
        foreach (var c in text)
        {
            switch (c)
            {
                case '|':
                    sb.Append("\\|");
                    prevSpace = false;
                    break;

                case '\n':
                case '\r':
                case '\t':
                case ' ':
                    if (!prevSpace)
                    {
                        sb.Append(' ');
                    }

                    prevSpace = true;
                    break;

                default:
                    sb.Append(c);
                    prevSpace = false;
                    break;
            }
        }

        return sb.ToString().Trim();
    }

    /// <summary>
    /// Renders explicitly positioned cells as a markdown pipe table. Grid
    /// dimensions come from the cells' row, column and span extents. A cell with a
    /// span greater than one renders in its top-left position and the absorbed
    /// grid positions become empty cells, so the markdown stays a valid
    /// rectangular grid that downstream readers can column-count correctly.
    /// </summary>
    /// <remarks>
    /// The separator row is emitted after the LAST row holding a header cell. When
    /// no cell is flagged as a header — the upstream TSR model emitted no
    /// <c>&lt;thead&gt;</c> or <c>&lt;th&gt;</c> — it falls back to after row 0, so
    /// the output remains a valid pipe table.
    /// </remarks>
    public static string CellsToMarkdown(IReadOnlyList<StructuredCell> cells)
    {
        if (cells.Count == 0)
        {
            return string.Empty;
        }

        var numRows = cells.Max(c => c.Row + Math.Max(c.RowSpan, 1));
        var numCols = cells.Max(c => c.Col + Math.Max(c.ColSpan, 1));
        if (numRows <= 0 || numCols <= 0)
        {
            return string.Empty;
        }

        // Clamped into range so a malformed cell with a row past the end cannot
        // push the separator outside the table.
        var headerRows = cells.Where(c => c.IsHeader).Select(c => c.Row).ToList();
        var separatorAfterRow = Math.Min(headerRows.Count > 0 ? headerRows.Max() : 0, numRows - 1);

        var grid = new List<List<string>>(numRows);
        for (var r = 0; r < numRows; r++)
        {
            var row = new List<string>(numCols);
            for (var c = 0; c < numCols; c++)
            {
                row.Add(string.Empty);
            }

            grid.Add(row);
        }

        foreach (var cell in cells)
        {
            if (cell.Row >= 0 && cell.Row < numRows && cell.Col >= 0 && cell.Col < numCols)
            {
                grid[cell.Row][cell.Col] = SanitizeCell(cell.Text);
            }
        }

        var output = new StringBuilder();
        for (var rowIdx = 0; rowIdx < grid.Count; rowIdx++)
        {
            output.Append('|');
            foreach (var cell in grid[rowIdx])
            {
                output.Append(cell).Append('|');
            }

            output.Append('\n');

            if (rowIdx == separatorAfterRow)
            {
                output.Append('|');
                for (var c = 0; c < numCols; c++)
                {
                    output.Append("---|");
                }

                output.Append('\n');
            }
        }

        return output.ToString();
    }
}
