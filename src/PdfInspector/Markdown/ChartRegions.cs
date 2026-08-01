// Ported from reference/src/markdown/mod.rs
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>
/// A chart's bounding box in page space, given by two opposite corners. Text
/// falling inside one belongs to a figure, not to the document's prose.
/// </summary>
internal readonly record struct ChartRegion(float X0, float Y0, float X1, float Y1)
{
    public float Left => MathF.Min(X0, X1);

    public float Right => MathF.Max(X0, X1);

    public float Bottom => MathF.Min(Y0, Y1);

    public float Top => MathF.Max(Y0, Y1);

    /// <summary>Converts to the tuple form the layout engine takes.</summary>
    public static implicit operator (float X0, float Y0, float X1, float Y1)(ChartRegion r) =>
        (r.X0, r.Y0, r.X1, r.Y1);

    /// <summary>Converts from the layout engine's tuple form.</summary>
    public static implicit operator ChartRegion((float X0, float Y0, float X1, float Y1) t) =>
        new(t.X0, t.Y0, t.X1, t.Y1);
}

/// <summary>Chart-region membership tests shared by conversion and layout.</summary>
internal static class ChartRegions
{
    /// <summary>How far outside a chart's box its own labels may still sit.</summary>
    public const float ChartRegionPad = 20.0f;

    /// <summary>The slack applied when deciding which stream zone a block falls in.</summary>
    public const float ChartSeparatorPad = 8.0f;

    /// <summary>
    /// True when an item just outside a chart's box is one of its labels — an
    /// axis category, a data value, a caption — rather than body text that
    /// happens to sit nearby.
    /// </summary>
    private static bool IsChartAdjacentLabel(TextItem item, ChartRegion region)
    {
        var text = item.Text.Trim();
        var isBareBullet = text is "•" or "●" or "○" or "◦" or "-" or "*";
        if (text.Length == 0 || Classify.IsListItem(text) || isBareBullet)
        {
            return false;
        }

        var itemLeft = MathF.Min(item.X, item.X + item.Width);
        var itemRight = MathF.Max(item.X, item.X + item.Width);
        var itemWidth = MathF.Max(itemRight - itemLeft, 1.0f);
        var chartWidth = MathF.Max(region.Right - region.Left, 1.0f);
        var horizontalOverlap = MathF.Max(
            MathF.Min(itemRight, region.Right) - MathF.Max(itemLeft, region.Left),
            0.0f);
        var mostlyInsideChartWidth = horizontalOverlap >= itemWidth * 0.8f;

        var verticalGap = item.Y < region.Bottom
            ? region.Bottom - item.Y
            : item.Y > region.Top ? item.Y - region.Top : 0.0f;

        var isCaption = Classify.IsCaptionLine(text);
        var em = MathF.Max(MathF.Max(item.Height, item.FontSize), 1.0f);
        var compactLabel = itemWidth <= em * 18.5f;
        var categoryBand = Math.Clamp(em * 1.85f, 6.0f, ChartRegionPad);
        var closeToChartEdge = isCaption ? verticalGap <= ChartRegionPad : verticalGap <= categoryBand;
        var categorySized = itemWidth <= chartWidth * 0.75f;

        return verticalGap <= ChartRegionPad
            && (compactLabel
                || isCaption
                || (mostlyInsideChartWidth && closeToChartEdge && categorySized));
    }

    /// <summary>True when the item belongs to any of the page's chart regions.</summary>
    public static bool ItemIsInChartRegion(TextItem item, IReadOnlyList<ChartRegion> regions)
    {
        foreach (var region in regions)
        {
            var cx = item.X + (item.Width / 2.0f);
            var withinPaddedX = cx >= region.X0 - ChartRegionPad && cx <= region.X1 + ChartRegionPad;
            var withinCoreY = item.Y >= region.Y0 && item.Y <= region.Y1;
            var withinPaddedY = item.Y >= region.Y0 - ChartRegionPad
                && item.Y <= region.Y1 + ChartRegionPad
                && IsChartAdjacentLabel(item, region);

            if (withinPaddedX && (withinCoreY || withinPaddedY))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>The items that fall outside every chart region.</summary>
    public static List<TextItem> ItemsOutsideChartRegions(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<ChartRegion> regions) =>
        items.Where(item => !ItemIsInChartRegion(item, regions)).ToList();
}
