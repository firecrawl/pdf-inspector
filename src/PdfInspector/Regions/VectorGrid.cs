// Ported from reference/src/lib.rs
using PdfInspector.Extractor;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Regions;

/// <summary>A region-scoped vector grid, in the shape TSR callers consume.</summary>
public sealed class VectorGridDetection
{
    /// <summary>HTML-like structure tokens for the TSR path.</summary>
    public List<string> StructureTokens { get; init; } = [];

    /// <summary>One crop-pixel bbox per <c>&lt;td&gt;</c> token, in document order.</summary>
    public List<float[]> CellBBoxes { get; init; } = [];
}

/// <summary>Which geometry backed the grid whose edges are being rebuilt.</summary>
internal enum VectorGridSource
{
    Rects,
    Lines,
}

/// <summary>
/// Vector ruled-line and rectangle grid detection inside one page region. The
/// returned shape deliberately matches <see cref="TsrTableInput"/>'s structure
/// fields, so a caller can hand it straight to the structure-aware extractors
/// and let the existing PDF-text cell fill populate the contents.
/// </summary>
public static class VectorGrid
{
    /// <summary>Detects a vector grid inside one page region.</summary>
    /// <param name="buffer">The PDF file bytes.</param>
    /// <param name="pageIdx">The 0-indexed page number.</param>
    /// <param name="regionPdfPtBBox">The region as [x1, y1, x2, y2] in PDF points, top-left origin.</param>
    /// <param name="renderDpi">The DPI the crop image was rendered at.</param>
    public static VectorGridDetection? DetectVectorGridInRegionMem(
        byte[] buffer,
        uint pageIdx,
        float[] regionPdfPtBBox,
        float renderDpi)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = PdfProcessor.LoadDocumentOrThrow(buffer, null);

        var page1Idx = pageIdx + 1;
        var page = doc.GetPage((int)page1Idx);
        if (page is null)
        {
            return null;
        }

        var neededPages = new HashSet<uint> { page1Idx };
        var fontCMaps = FontCMaps.FromDocumentPagesFast(doc, neededPages);
        var pageH = RegionGeometry.GetPageHeight(doc, page) ?? 792.0f;
        var extraction = ContentStreamExtractor.ExtractPageTextItems(
            doc, page, page1Idx, fontCMaps, false, new FontStyleCache());
        var items = extraction.Items;
        TextUtils.FixLetterspacedItems(items);

        var coords = extraction.CoordsRotated ? RegionCoordSpace.Rotated90Ccw : RegionCoordSpace.Standard;
        if (coords == RegionCoordSpace.Rotated90Ccw)
        {
            // The TSR crop contract is top-left page coordinates, while rotated
            // extraction normalises vector geometry into a synthetic space.
            // Returning nothing is safer than emitting misleading bboxes.
            return null;
        }

        float rx1 = regionPdfPtBBox[0], ry1 = regionPdfPtBBox[1];
        float rx2 = regionPdfPtBBox[2], ry2 = regionPdfPtBBox[3];
        var bounds = RegionGeometry.Bounds(rx1, ry1, rx2, ry2, pageH, coords);

        var itemsInRegion = items.Where(item => RegionGeometry.OverlapsItem(item, bounds)).ToList();
        if (itemsInRegion.Count == 0)
        {
            return null;
        }

        var rectsInRegion = extraction.Rects.Where(r => RegionGeometry.OverlapsRect(r, bounds)).ToList();
        var linesInRegion = extraction.Lines.Where(l => RegionGeometry.OverlapsLine(l, bounds)).ToList();

        // Match the geometry pipeline's priority: rect-backed grids first, then
        // line-backed grids. The detector output is only the validity gate; the
        // bboxes below are rebuilt from the filtered vector geometry, because a
        // Table stores centres for some rect paths and row starts for line paths.
        var (rectTables, _) = RectTables.DetectTablesFromRects(itemsInRegion, rectsInRegion, page1Idx);
        foreach (var table in rectTables)
        {
            var result = ResultFromTable(
                table, VectorGridSource.Rects, rectsInRegion, linesInRegion,
                regionPdfPtBBox, renderDpi, pageH, coords);
            if (result is not null)
            {
                return result;
            }
        }

        var lineTables = LineDetector.DetectVectorGridTablesFromLines(itemsInRegion, linesInRegion, page1Idx);
        foreach (var table in lineTables)
        {
            var result = ResultFromTable(
                table, VectorGridSource.Lines, rectsInRegion, linesInRegion,
                regionPdfPtBBox, renderDpi, pageH, coords);
            if (result is not null)
            {
                return result;
            }
        }

        return null;
    }

    private static VectorGridDetection? ResultFromTable(
        Table table,
        VectorGridSource source,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<PdfLine> lines,
        float[] cropPdfPtBBox,
        float renderDpi,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        var numRows = table.Cells.Count;
        var numCols = table.Cells.Count > 0 ? table.Cells[0].Count : 0;
        if (numRows == 0 || numCols == 0 || table.Cells.Any(row => row.Count != numCols))
        {
            return null;
        }

        var edges = source == VectorGridSource.Rects
            ? RectGridEdges(rects, numCols, numRows) ?? InferredGridEdges(table, rects, lines, numCols, numRows)
            : LineGridEdges(table, lines, numCols, numRows) ?? InferredGridEdges(table, rects, lines, numCols, numRows);
        if (edges is null)
        {
            return null;
        }

        var (xEdges, yEdges) = edges.Value;
        if (xEdges.Count != numCols + 1 || yEdges.Count != numRows + 1)
        {
            return null;
        }

        var structureTokens = new List<string>((numRows * (numCols + 2)) + 2) { "<table>" };
        var cellBBoxes = new List<float[]>(numRows * numCols);

        // The v1 structural output stays uniform: the downstream TSR text-fill
        // path does not need header semantics, and reliable header detection can
        // be layered on later without changing the geometry contract.
        for (var r = 0; r < numRows; r++)
        {
            structureTokens.Add("<tr>");
            for (var c = 0; c < numCols; c++)
            {
                structureTokens.Add("<td></td>");
                var bboxPx = ExtractedCellToCropPx(
                    [xEdges[c], yEdges[r + 1], xEdges[c + 1], yEdges[r]],
                    cropPdfPtBBox, renderDpi, pageHeight, coordSpace);
                if (bboxPx is null || !CropPxBBoxIsPlausible(bboxPx, cropPdfPtBBox, renderDpi))
                {
                    return null;
                }

                cellBBoxes.Add(bboxPx);
            }

            structureTokens.Add("</tr>");
        }

        structureTokens.Add("</table>");
        return new VectorGridDetection { StructureTokens = structureTokens, CellBBoxes = cellBBoxes };
    }

    private static bool CropPxBBoxIsPlausible(float[] bboxPx, float[] cropPdfPtBBox, float renderDpi)
    {
        var ppi = renderDpi > 0.0f ? renderDpi / 72.0f : 1.0f;
        var cropW = MathF.Abs(cropPdfPtBBox[2] - cropPdfPtBBox[0]) * ppi;
        var cropH = MathF.Abs(cropPdfPtBBox[3] - cropPdfPtBBox[1]) * ppi;
        const float slack = 1.0f;
        return bboxPx[0] >= -slack
            && bboxPx[1] >= -slack
            && bboxPx[2] <= cropW + slack
            && bboxPx[3] <= cropH + slack;
    }

    private static (List<float> X, List<float> Y)? LineGridEdges(
        Table table,
        IReadOnlyList<PdfLine> lines,
        int numCols,
        int numRows)
    {
        if (lines.Count == 0 || table.Columns.Count != numCols + 1 || table.Rows.Count != numRows)
        {
            return null;
        }

        var angleTolerance = MathF.Tan(2.0f * MathF.PI / 180.0f);
        var ys = new List<float>();

        foreach (var line in lines)
        {
            var dx = MathF.Abs(line.X2 - line.X1);
            var dy = MathF.Abs(line.Y2 - line.Y1);
            var length = MathF.Sqrt((dx * dx) + (dy * dy));
            if (length < 20.0f)
            {
                continue;
            }

            if (dx > 0.01f && dy / dx <= angleTolerance)
            {
                ys.Add((line.Y1 + line.Y2) * 0.5f);
            }
        }

        var xEdges = new List<float>(table.Columns);
        xEdges.Sort(FloatTotalOrder.Instance);

        var snappedY = SnapVectorEdges(ys, true);
        var yEdges = new List<float>(numRows + 1);
        foreach (var rowTop in table.Rows)
        {
            var matched = snappedY.FirstOrDefault(y => MathF.Abs(y - rowTop) <= 3.0f, rowTop);
            yEdges.Add(matched);
        }

        if (yEdges.Count == 0)
        {
            return null;
        }

        var lastTop = yEdges[^1];
        var below = snappedY.Where(y => y < lastTop - 3.0f).ToList();
        if (below.Count == 0)
        {
            return null;
        }

        yEdges.Add(below.Max());

        return xEdges.Count == numCols + 1 && yEdges.Count == numRows + 1 ? (xEdges, yEdges) : null;
    }

    private static (List<float> X, List<float> Y)? RectGridEdges(
        IReadOnlyList<PdfRect> rects,
        int numCols,
        int numRows)
    {
        if (rects.Count == 0)
        {
            return null;
        }

        var xs = new List<float>();
        var ys = new List<float>();
        foreach (var rect in rects)
        {
            var (x1, y1, x2, y2) = RegionGeometry.NormalizedRectEdges(rect);
            if (x2 - x1 < 5.0f || y2 - y1 < 5.0f)
            {
                continue;
            }

            xs.Add(x1);
            xs.Add(x2);
            ys.Add(y1);
            ys.Add(y2);
        }

        var xEdges = SnapVectorEdges(xs, false);
        var yEdges = SnapVectorEdges(ys, true);
        return xEdges.Count == numCols + 1 && yEdges.Count == numRows + 1 ? (xEdges, yEdges) : null;
    }

    private static (List<float> X, List<float> Y)? InferredGridEdges(
        Table table,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<PdfLine> lines,
        int numCols,
        int numRows)
    {
        var bounds = VectorGeometryBounds(rects, lines);

        List<float>? xEdges;
        if (table.Columns.Count == numCols + 1)
        {
            xEdges = new List<float>(table.Columns);
            xEdges.Sort(FloatTotalOrder.Instance);
        }
        else
        {
            xEdges = InferAscendingEdges(
                table.Columns, numCols, bounds is { } bx ? (bx.XMin, bx.XMax) : null);
        }

        if (xEdges is null)
        {
            return null;
        }

        List<float>? yEdges;
        if (table.Rows.Count == numRows + 1)
        {
            yEdges = new List<float>(table.Rows);
            yEdges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));
        }
        else
        {
            yEdges = InferDescendingEdges(
                table.Rows, numRows, bounds is { } by ? (by.YMin, by.YMax) : null);
        }

        return yEdges is null ? null : (xEdges, yEdges);
    }

    private static List<float>? InferAscendingEdges(
        IReadOnlyList<float> positions,
        int expectedCenters,
        (float Min, float Max)? bounds)
    {
        if (positions.Count != expectedCenters || positions.Count == 0)
        {
            return null;
        }

        var centers = new List<float>(positions);
        centers.Sort(FloatTotalOrder.Instance);
        if (centers.Count == 1)
        {
            return null;
        }

        var edges = new List<float>(centers.Count + 1);
        var firstGap = centers[1] - centers[0];
        var lastGap = centers[^1] - centers[^2];
        var left = bounds is { } b1 && float.IsFinite(b1.Min) && b1.Min < centers[0]
            ? b1.Min
            : centers[0] - (firstGap * 0.5f);
        var right = bounds is { } b2 && float.IsFinite(b2.Max) && b2.Max > centers[^1]
            ? b2.Max
            : centers[^1] + (lastGap * 0.5f);

        edges.Add(left);
        for (var i = 0; i + 1 < centers.Count; i++)
        {
            edges.Add((centers[i] + centers[i + 1]) * 0.5f);
        }

        edges.Add(right);

        return StrictlyOrdered(edges, false) ? edges : null;
    }

    private static List<float>? InferDescendingEdges(
        IReadOnlyList<float> positions,
        int expectedCenters,
        (float Min, float Max)? bounds)
    {
        if (positions.Count != expectedCenters || positions.Count == 0)
        {
            return null;
        }

        var centers = new List<float>(positions);
        centers.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));
        if (centers.Count == 1)
        {
            return null;
        }

        var edges = new List<float>(centers.Count + 1);
        var firstGap = centers[0] - centers[1];
        var lastGap = centers[^2] - centers[^1];
        var top = bounds is { } b1 && float.IsFinite(b1.Max) && b1.Max > centers[0]
            ? b1.Max
            : centers[0] + (firstGap * 0.5f);
        var bottom = bounds is { } b2 && float.IsFinite(b2.Min) && b2.Min < centers[^1]
            ? b2.Min
            : centers[^1] - (lastGap * 0.5f);

        edges.Add(top);
        for (var i = 0; i + 1 < centers.Count; i++)
        {
            edges.Add((centers[i] + centers[i + 1]) * 0.5f);
        }

        edges.Add(bottom);

        return StrictlyOrdered(edges, true) ? edges : null;
    }

    /// <summary>Collapses near-coincident edge positions into cluster means.</summary>
    private static List<float> SnapVectorEdges(List<float> values, bool descending)
    {
        var sorted = values.Where(float.IsFinite).ToList();
        sorted.Sort(FloatTotalOrder.Instance);

        var snapped = new List<float>();
        var cluster = new List<float>();
        foreach (var value in sorted)
        {
            if (cluster.Count > 0 && MathF.Abs(value - cluster[^1]) <= 3.0f)
            {
                cluster.Add(value);
            }
            else
            {
                if (cluster.Count > 0)
                {
                    snapped.Add(cluster.SumF32() / cluster.Count);
                }

                cluster = [value];
            }
        }

        if (cluster.Count > 0)
        {
            snapped.Add(cluster.SumF32() / cluster.Count);
        }

        if (descending)
        {
            snapped.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));
        }

        return snapped;
    }

    private static bool StrictlyOrdered(IReadOnlyList<float> values, bool descending)
    {
        for (var i = 0; i + 1 < values.Count; i++)
        {
            var a = values[i];
            var b = values[i + 1];
            if (!float.IsFinite(a) || !float.IsFinite(b) || (descending ? a <= b : a >= b))
            {
                return false;
            }
        }

        return true;
    }

    private static RegionBounds? VectorGeometryBounds(
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<PdfLine> lines)
    {
        RegionBounds? bounds = null;

        void Include(float x1, float y1, float x2, float y2)
        {
            var next = new RegionBounds(
                MathF.Min(x1, x2), MathF.Min(y1, y2), MathF.Max(x1, x2), MathF.Max(y1, y2));
            bounds = bounds is { } prev
                ? new RegionBounds(
                    MathF.Min(prev.XMin, next.XMin),
                    MathF.Min(prev.YMin, next.YMin),
                    MathF.Max(prev.XMax, next.XMax),
                    MathF.Max(prev.YMax, next.YMax))
                : next;
        }

        foreach (var rect in rects)
        {
            var (x1, y1, x2, y2) = RegionGeometry.NormalizedRectEdges(rect);
            Include(x1, y1, x2, y2);
        }

        foreach (var line in lines)
        {
            Include(line.X1, line.Y1, line.X2, line.Y2);
        }

        return bounds;
    }

    /// <summary>Converts an extracted-space cell bbox into crop-image pixels.</summary>
    private static float[]? ExtractedCellToCropPx(
        float[] bbox,
        float[] cropPdfPtBBox,
        float renderDpi,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        var pageBox = ExtractedBBoxToPageTopLeft(bbox, pageHeight, coordSpace);
        float x1 = pageBox[0], y1 = pageBox[1], x2 = pageBox[2], y2 = pageBox[3];
        if (!float.IsFinite(x1) || !float.IsFinite(y1) || !float.IsFinite(x2) || !float.IsFinite(y2))
        {
            return null;
        }

        if (x1 >= x2 || y1 >= y2)
        {
            return null;
        }

        var ppi = renderDpi > 0.0f ? renderDpi / 72.0f : 1.0f;
        var cropX1 = cropPdfPtBBox[0];
        var cropY1 = cropPdfPtBBox[1];
        return
        [
            (x1 - cropX1) * ppi,
            (y1 - cropY1) * ppi,
            (x2 - cropX1) * ppi,
            (y2 - cropY1) * ppi,
        ];
    }

    private static float[] ExtractedBBoxToPageTopLeft(
        float[] bbox,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        float x1 = bbox[0], y1 = bbox[1], x2 = bbox[2], y2 = bbox[3];
        var xMin = MathF.Min(x1, x2);
        var xMax = MathF.Max(x1, x2);
        var yMin = MathF.Min(y1, y2);
        var yMax = MathF.Max(y1, y2);

        return coordSpace == RegionCoordSpace.Standard
            ? [xMin, pageHeight - yMax, xMax, pageHeight - yMin]
            : [-yMax, pageHeight - xMax, -yMin, pageHeight - xMin];
    }
}
