// Ported from reference/src/lib.rs
using PdfInspector.Extractor;
using PdfInspector.Pdf;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Regions;

/// <summary>Which coordinate convention a page's extracted items live in.</summary>
internal enum RegionCoordSpace
{
    /// <summary>Bottom-left origin, the ordinary PDF user space.</summary>
    Standard,

    /// <summary>The synthetic space rotated pages are normalised into.</summary>
    Rotated90Ccw,
}

/// <summary>A region's bounds in the extractor's coordinate space.</summary>
internal readonly struct RegionBounds(float xMin, float yMin, float xMax, float yMax)
{
    public readonly float XMin = xMin;
    public readonly float YMin = yMin;
    public readonly float XMax = xMax;
    public readonly float YMax = yMax;
}

/// <summary>
/// The geometry shared by every region-scoped API: coordinate flipping,
/// overlap predicates, and text collection inside a bbox.
/// </summary>
internal static class RegionGeometry
{
    /// <summary>
    /// Inclusion margin shared by the region/item overlap predicates and the
    /// exclusive-assignment area score — these must stay in sync: an item that
    /// passes the boolean guard must always have positive overlap area.
    /// </summary>
    private const float RegionMargin = 1.5f;

    /// <summary>Reads a page's height in points from its MediaBox.</summary>
    public static float? GetPageHeight(PdfDocument doc, PdfDictionary page)
    {
        var mediaBox = doc.Resolve(page.Get("MediaBox"));
        if (mediaBox is not PdfArray array || array.Count < 4)
        {
            return null;
        }

        var y1 = ObjToFloat(doc.Resolve(array[1]));
        var y2 = ObjToFloat(doc.Resolve(array[3]));
        return y1 is null || y2 is null ? null : MathF.Abs(y2.Value - y1.Value);
    }

    private static float? ObjToFloat(PdfObject? obj) => obj switch
    {
        PdfInteger i => i.Value,
        PdfReal r => (float)r.Value,
        _ => null,
    };

    /// <summary>
    /// Collects text items that fall inside a region bbox (top-left origin,
    /// PDF points) and returns them as one string in reading order.
    /// </summary>
    public static string CollectTextInRegion(
        IReadOnlyList<TextItem> items,
        float rx1,
        float ry1,
        float rx2,
        float ry2,
        float pageHeight) =>
        CollectTextInRegionWithOptions(
            items, rx1, ry1, rx2, ry2, pageHeight, InferRegionCoordSpace(items), 0.10f);

    public static string CollectTextInRegionWithOptions(
        IReadOnlyList<TextItem> items,
        float rx1,
        float ry1,
        float rx2,
        float ry2,
        float pageHeight,
        RegionCoordSpace coordSpace,
        float adaptiveThreshold)
    {
        var bounds = Bounds(rx1, ry1, rx2, ry2, pageHeight, coordSpace);
        var matched = items.Where(item => OverlapsItem(item, bounds)).Select(i => i.Clone()).ToList();
        return CollectTextFromMatchedItems(matched, adaptiveThreshold);
    }

    /// <summary>Assembles matched items into lines, top to bottom then left to right.</summary>
    public static string CollectTextFromMatchedItems(List<TextItem> matched, float adaptiveThreshold)
    {
        if (matched.Count == 0)
        {
            return string.Empty;
        }

        // Simple extraction: the caller already handles reading order and
        // column splitting via its layout model. Sorting top-to-bottom then
        // left-to-right and grouping into lines is enough here.
        var sorted = matched;
        sorted.Sort((a, b) =>
        {
            var byY = FloatTotalOrder.Instance.Compare(b.Y, a.Y);
            return byY != 0 ? byY : FloatTotalOrder.Instance.Compare(a.X, b.X);
        });

        const float yTolerance = 3.0f;
        var lines = new List<TextLine>();

        foreach (var item in sorted)
        {
            var last = lines.Count > 0 ? lines[^1] : null;
            if (last is not null && last.Page == item.Page && MathF.Abs(last.Y - item.Y) < yTolerance)
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

        return string.Join('\n', lines.Select(line => line.Text()));
    }

    /// <summary>
    /// Guesses a coordinate space from the items alone, for direct callers that
    /// have no extractor metadata. Rotated-page normalisation maps y = -oldX, so
    /// most items land at negative Y.
    /// </summary>
    public static RegionCoordSpace InferRegionCoordSpace(IReadOnlyList<TextItem> items)
    {
        var negativeY = items.Count(item => item.Y < 0.0f);
        return items.Count > 0 && negativeY * 2 >= items.Count
            ? RegionCoordSpace.Rotated90Ccw
            : RegionCoordSpace.Standard;
    }

    /// <summary>Flips a top-left-origin bbox into the extractor's coordinate space.</summary>
    public static RegionBounds Bounds(
        float rx1,
        float ry1,
        float rx2,
        float ry2,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        var txMin = MathF.Min(rx1, rx2);
        var txMax = MathF.Max(rx1, rx2);
        var tyMin = MathF.Min(ry1, ry2);
        var tyMax = MathF.Max(ry1, ry2);
        var byMin = pageHeight - tyMax;
        var byMax = pageHeight - tyMin;

        return coordSpace == RegionCoordSpace.Standard
            ? new RegionBounds(txMin, byMin, txMax, byMax)
            : new RegionBounds(byMin, -txMax, byMax, -txMin);
    }

    /// <summary>
    /// Overlap area between an item and region bounds, using the same margin as
    /// the boolean test. This is the exclusive-assignment score.
    /// </summary>
    public static float ItemOverlapArea(TextItem item, RegionBounds bounds)
    {
        var itemXMax = item.X + TextUtils.EffectiveWidth(item);
        var itemYMax = item.Y + item.Height;
        var xOverlap = MathF.Max(
            MathF.Min(itemXMax, bounds.XMax + RegionMargin) - MathF.Max(item.X, bounds.XMin - RegionMargin),
            0.0f);
        var yOverlap = MathF.Max(
            MathF.Min(itemYMax, bounds.YMax + RegionMargin) - MathF.Max(item.Y, bounds.YMin - RegionMargin),
            0.0f);
        return xOverlap * yOverlap;
    }

    public static bool OverlapsItem(TextItem item, RegionBounds bounds)
    {
        var itemXMin = item.X;
        var itemXMax = item.X + TextUtils.EffectiveWidth(item);
        var itemYMin = item.Y;
        var itemYMax = item.Y + item.Height;

        var xOverlap = MathF.Max(
            MathF.Min(itemXMax, bounds.XMax + RegionMargin) - MathF.Max(itemXMin, bounds.XMin - RegionMargin),
            0.0f);
        var yOverlap = MathF.Max(
            MathF.Min(itemYMax, bounds.YMax + RegionMargin) - MathF.Max(itemYMin, bounds.YMin - RegionMargin),
            0.0f);
        return xOverlap > 0.0f && yOverlap > 0.0f;
    }

    public static bool OverlapsRect(PdfRect rect, RegionBounds bounds)
    {
        var (xMin, yMin, xMax, yMax) = NormalizedRectEdges(rect);
        return RangesOverlap(xMin, xMax, bounds.XMin - RegionMargin, bounds.XMax + RegionMargin)
            && RangesOverlap(yMin, yMax, bounds.YMin - RegionMargin, bounds.YMax + RegionMargin);
    }

    public static bool OverlapsLine(PdfLine line, RegionBounds bounds)
    {
        var xMin = MathF.Min(line.X1, line.X2);
        var xMax = MathF.Max(line.X1, line.X2);
        var yMin = MathF.Min(line.Y1, line.Y2);
        var yMax = MathF.Max(line.Y1, line.Y2);
        return RangesOverlap(xMin, xMax, bounds.XMin - RegionMargin, bounds.XMax + RegionMargin)
            && RangesOverlap(yMin, yMax, bounds.YMin - RegionMargin, bounds.YMax + RegionMargin);
    }

    private static bool RangesOverlap(float aMin, float aMax, float bMin, float bMax) =>
        aMax >= bMin && bMax >= aMin;

    /// <summary>
    /// The stricter membership rule the TSR path uses: the item's centre is
    /// inside, or it overlaps by at least 60% on both axes.
    /// </summary>
    public static bool TsrContainsItem(TextItem item, RegionBounds bounds)
    {
        var itemXMin = item.X;
        var itemXMax = item.X + TextUtils.EffectiveWidth(item);
        var itemYMin = item.Y;
        var itemYMax = item.Y + item.Height;

        var centerX = (itemXMin + itemXMax) * 0.5f;
        var centerY = (itemYMin + itemYMax) * 0.5f;
        if (centerX >= bounds.XMin && centerX <= bounds.XMax
            && centerY >= bounds.YMin && centerY <= bounds.YMax)
        {
            return true;
        }

        var xOverlap = MathF.Max(MathF.Min(itemXMax, bounds.XMax) - MathF.Max(itemXMin, bounds.XMin), 0.0f);
        var yOverlap = MathF.Max(MathF.Min(itemYMax, bounds.YMax) - MathF.Max(itemYMin, bounds.YMin), 0.0f);
        var itemWidth = MathF.Max(itemXMax - itemXMin, 0.1f);
        var itemHeight = MathF.Max(itemYMax - itemYMin, 0.1f);

        return xOverlap / itemWidth >= 0.6f && yOverlap / itemHeight >= 0.6f;
    }

    /// <summary>A rectangle's edges, ordered regardless of a negative width or height.</summary>
    public static (float X1, float Y1, float X2, float Y2) NormalizedRectEdges(PdfRect rect)
    {
        var x2 = rect.X + rect.Width;
        var y2 = rect.Y + rect.Height;
        return (MathF.Min(rect.X, x2), MathF.Min(rect.Y, y2), MathF.Max(rect.X, x2), MathF.Max(rect.Y, y2));
    }
}
