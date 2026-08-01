// Ported from reference/src/extractor/reading_order.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>An image's device-space bounds: two opposite corners, in either order.</summary>
internal readonly record struct ImageRegion(float X0, float Y0, float X1, float Y1)
{
    public float Left => MathF.Min(X0, X1);

    public float Right => MathF.Max(X0, X1);

    public float Bottom => MathF.Min(Y0, Y1);

    public float Top => MathF.Max(Y0, Y1);

    public float Width => MathF.Abs(X1 - X0);

    public float Height => MathF.Abs(Y1 - Y0);
}

/// <summary>A local two-column band, bounded vertically and split at one x.</summary>
internal readonly record struct ColumnFlowBand(float SplitX, float YBottom, float YTop);

internal enum RegionKind
{
    FullWidth,
    Column,
}

/// <summary>One node of the reading-order graph.</summary>
internal sealed class RegionNode
{
    public required RegionKind Kind { get; init; }

    public required List<TextItem> Items { get; init; }
}

/// <summary>
/// Region-graph evidence for page reading order.
///
/// A whole-page column histogram fails when images or spanning captions occupy
/// only part of a page. This turns image geometry and repeated row gutters into
/// a small directed acyclic graph: content above a local column band, the left
/// flow, the right flow, and content below. The inference is deliberately
/// evidence-gated, so ordinary pages stay on the established layout path.
/// </summary>
internal static class ReadingOrder
{
    private const string Module = "layout";

    private const float MinImageWidth = 60.0f;
    private const float MinImageHeight = 40.0f;
    private const float MinRowGutter = 8.0f;
    private const float SplitClusterTolerance = 20.0f;
    private const int MinAlignedRows = 4;

    private sealed class Row
    {
        public float Y;
        public List<TextItem> Items = [];
    }

    private static (float XMin, float XMax)? PageXBounds(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<ImageRegion> images)
    {
        var textMin = items.Count > 0 ? items.Min(i => i.X) : float.PositiveInfinity;
        var textMax = items.Count > 0
            ? items.Max(i => i.X + TextUtils.EffectiveWidth(i))
            : float.NegativeInfinity;

        var imageMin = images.Count > 0 ? images.Min(r => r.Left) : float.PositiveInfinity;
        var imageMax = images.Count > 0 ? images.Max(r => r.Right) : float.NegativeInfinity;

        var xMin = MathF.Min(textMin, imageMin);
        var xMax = MathF.Max(textMax, imageMax);

        return float.IsFinite(xMin) && float.IsFinite(xMax) && xMax > xMin ? (xMin, xMax) : null;
    }

    private static List<Row> GroupRows(IReadOnlyList<TextItem> items)
    {
        const float YTolerance = 3.0f;

        var sorted = items.OrderByDescending(i => i.Y, FloatTotalOrder.Instance).ToList();
        var rows = new List<Row>();

        foreach (var item in sorted)
        {
            var last = rows.Count > 0 ? rows[^1] : null;
            if (last is not null && MathF.Abs(last.Y - item.Y) <= YTolerance)
            {
                last.Items.Add(item);
                last.Y = last.Items.SumF32(m => m.Y) / last.Items.Count;
            }
            else
            {
                rows.Add(new Row { Y = item.Y, Items = [item] });
            }
        }

        foreach (var row in rows)
        {
            row.Items.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.X, b.X));
        }

        return rows;
    }

    /// <summary>True when a side of a candidate gutter carries enough words to be prose.</summary>
    private static bool SideIsProse(List<TextItem> items)
    {
        var text = string.Join(" ", items.Select(i => i.Text.Trim()));
        var alphabetic = text.Count(char.IsLetter);
        var cjk = text.Count(TextUtils.IsCjkChar);
        var words = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;

        return (words >= 3 || cjk >= 10) && alphabetic >= 10;
    }

    /// <summary>
    /// The widest gutter within a row that sits near the page centre and has
    /// prose on both sides, or null when the row shows no column split.
    /// </summary>
    private static float? AlignedRowSplit(Row row, float xMin, float xMax)
    {
        if (row.Items.Count < 2)
        {
            return null;
        }

        var pageWidth = xMax - xMin;
        var centerLow = xMin + (pageWidth * 0.25f);
        var centerHigh = xMin + (pageWidth * 0.75f);

        float? bestSplit = null;
        var bestGap = float.NegativeInfinity;

        for (var i = 0; i + 1 < row.Items.Count; i++)
        {
            var leftEnd = row.Items[i].X + TextUtils.EffectiveWidth(row.Items[i]);
            var rightStart = row.Items[i + 1].X;
            var gap = rightStart - leftEnd;
            var splitX = (leftEnd + rightStart) / 2.0f;

            if (gap < MinRowGutter || splitX < centerLow || splitX > centerHigh)
            {
                continue;
            }

            var left = row.Items.Where(it => it.X + (TextUtils.EffectiveWidth(it) / 2.0f) < splitX).ToList();
            var right = row.Items.Where(it => it.X + (TextUtils.EffectiveWidth(it) / 2.0f) >= splitX).ToList();

            if (SideIsProse(left) && SideIsProse(right) && gap > bestGap)
            {
                bestGap = gap;
                bestSplit = splitX;
            }
        }

        return bestSplit;
    }

    /// <summary>
    /// Detects a two-column flow beneath a single hero image. The anchor must be
    /// one nearly square, nearly full-width figure: wide report banners and
    /// full-page artwork often sit above unrelated page furniture whose aligned
    /// labels would mimic prose columns.
    /// </summary>
    private static ColumnFlowBand? LocalFlowBelowFullWidthImage(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<ImageRegion> images,
        float xMin,
        float xMax)
    {
        var pageWidth = xMax - xMin;

        var fullWidthImages = images
            .Where(r => r.Width >= pageWidth * 0.65f && r.Height >= 60.0f)
            .ToList();

        if (fullWidthImages.Count != 1)
        {
            return null;
        }

        var anchor = fullWidthImages[0];
        var anchorWidth = anchor.Width;
        var anchorHeight = anchor.Height;

        if (anchorWidth < pageWidth * 0.85f
            || anchorHeight < anchorWidth * 0.85f
            || anchorHeight > anchorWidth * 1.2f)
        {
            return null;
        }

        var imageBottom = fullWidthImages.Max(r => r.Bottom);
        if (!float.IsFinite(imageBottom))
        {
            return null;
        }

        var below = items.Where(i => i.Y < imageBottom && i.Y >= imageBottom - 220.0f).ToList();

        var candidates = new List<(float Split, float Y)>();
        foreach (var row in GroupRows(below))
        {
            if (AlignedRowSplit(row, xMin, xMax) is { } split)
            {
                candidates.Add((split, row.Y));
            }
        }

        if (candidates.Count < MinAlignedRows)
        {
            return null;
        }

        // Candidate splits that agree within tolerance form one cluster; the
        // largest cluster is the real gutter.
        var clusters = new List<List<(float Split, float Y)>>();
        foreach (var candidate in candidates)
        {
            var cluster = clusters.FirstOrDefault(c =>
                MathF.Abs((c.SumF32(e => e.Split) / c.Count) - candidate.Split) <= SplitClusterTolerance);

            if (cluster is not null)
            {
                cluster.Add(candidate);
            }
            else
            {
                clusters.Add([candidate]);
            }
        }

        var dominant = clusters.OrderByDescending(c => c.Count).FirstOrDefault();
        if (dominant is null || dominant.Count < MinAlignedRows)
        {
            return null;
        }

        var splitX = dominant.SumF32(e => e.Split) / dominant.Count;
        var yTop = dominant.Max(e => e.Y) + 3.0f;

        // The columns must sit a caption's distance below the image — closer
        // and they are part of it, further and they are unrelated furniture.
        var imageGap = imageBottom - yTop;
        if (imageGap is < 60.0f or > 120.0f)
        {
            return null;
        }

        var yBottom = dominant.Min(e => e.Y) - 3.0f;
        if (yTop - yBottom > 130.0f)
        {
            return null;
        }

        Log.Debug(Module, () =>
            $"page {(items.Count > 0 ? items[0].Page : 0)}: full-width image flow images={images.Count} " +
            $"aligned_rows={dominant.Count} split={splitX:F1} page=[{xMin:F1}..{xMax:F1}] " +
            $"image_bottom={imageBottom:F1} y=[{yBottom:F1}..{yTop:F1}]");

        return new ColumnFlowBand(splitX, yBottom, yTop);
    }

    /// <summary>
    /// Detects a two-column flow evidenced by images confined to each column and
    /// stacked vertically, at a split the histogram already proposed.
    /// </summary>
    private static ColumnFlowBand? PairedColumnImages(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<ImageRegion> images,
        float splitX,
        float xMin,
        float xMax)
    {
        var pageWidth = xMax - xMin;
        if (splitX < xMin + (pageWidth * 0.4f) || splitX > xMin + (pageWidth * 0.6f))
        {
            return null;
        }

        var qualifying = images.Where(r =>
        {
            var confinedToOneColumn = r.Right <= splitX || r.Left >= splitX;
            return confinedToOneColumn && r.Width >= MinImageWidth && r.Height >= MinImageHeight;
        }).ToList();

        var wideImages = qualifying.Where(r => r.Width >= pageWidth * 0.35f).ToList();

        if (qualifying.Count < 3 || wideImages.Count < 3)
        {
            return null;
        }

        var hasLeft = qualifying.Any(r => (r.X0 + r.X1) / 2.0f < splitX);
        var hasRight = qualifying.Any(r => (r.X0 + r.X1) / 2.0f >= splitX);
        if (!hasLeft || !hasRight)
        {
            return null;
        }

        // A meaningful image-backed flow spans several vertical panels. Three
        // same-row header or logo images would otherwise satisfy the count and
        // send an ordinary asymmetric page through sequential column order.
        var imageYMin = wideImages.Min(r => r.Bottom);
        var imageYMax = wideImages.Max(r => r.Top);

        var hasVerticalStack = false;
        for (var i = 0; i < wideImages.Count && !hasVerticalStack; i++)
        {
            for (var j = i + 1; j < wideImages.Count; j++)
            {
                var left = wideImages[i];
                var right = wideImages[j];

                var sameSide = ((left.X0 + left.X1) / 2.0f < splitX) == ((right.X0 + right.X1) / 2.0f < splitX);
                var leftCenter = (left.Y0 + left.Y1) / 2.0f;
                var rightCenter = (right.Y0 + right.Y1) / 2.0f;

                float verticalGap;
                if (left.Top < right.Bottom)
                {
                    verticalGap = right.Bottom - left.Top;
                }
                else if (right.Top < left.Bottom)
                {
                    verticalGap = left.Bottom - right.Top;
                }
                else
                {
                    verticalGap = 0.0f;
                }

                if (sameSide
                    && MathF.Abs(leftCenter - rightCenter) >= MathF.Min(left.Height, right.Height) * 0.5f
                    && verticalGap <= MathF.Max(left.Height, right.Height) * 0.5f)
                {
                    hasVerticalStack = true;
                    break;
                }
            }
        }

        if (imageYMax - imageYMin < pageWidth * 0.45f || !hasVerticalStack)
        {
            return null;
        }

        var yTop = qualifying.Max(r => r.Top) + 3.0f;

        // Only column-confined text proves the flow's lower extent. A spanning
        // heading or caption below the columns must become the trailing
        // full-width node rather than stretching the band to the page foot.
        var confined = items
            .Where(i => i.Y <= yTop && (i.X + TextUtils.EffectiveWidth(i) <= splitX || i.X >= splitX))
            .ToList();

        if (confined.Count == 0)
        {
            return null;
        }

        var yBottom = confined.Min(i => i.Y) - 3.0f;

        int DistinctRows(bool right)
        {
            var ys = items
                .Where(i => i.Y <= yTop && (i.X + (TextUtils.EffectiveWidth(i) / 2.0f) >= splitX) == right)
                .Select(i => i.Y)
                .OrderBy(y => y, FloatTotalOrder.Instance)
                .ToList();

            var count = 0;
            float? last = null;
            foreach (var y in ys)
            {
                if (last is null || MathF.Abs(y - last.Value) > 3.0f)
                {
                    count++;
                    last = y;
                }
            }

            return count;
        }

        var leftRows = DistinctRows(false);
        var rightRows = DistinctRows(true);
        var lineBalance = (float)Math.Min(leftRows, rightRows) / Math.Max(Math.Max(leftRows, rightRows), 1);

        if (leftRows < 5 || rightRows < 5 || lineBalance >= 0.55f)
        {
            return null;
        }

        Log.Debug(Module, () =>
            $"page {(items.Count > 0 ? items[0].Page : 0)}: paired-image flow " +
            $"qualifying_images={qualifying.Count} rows={leftRows}/{rightRows} split={splitX:F1} " +
            $"page=[{xMin:F1}..{xMax:F1}] y=[{yBottom:F1}..{yTop:F1}]");

        return new ColumnFlowBand(splitX, yBottom, yTop);
    }

    /// <summary>
    /// Infers a local column band from image geometry, or returns null when the
    /// evidence does not support one.
    /// </summary>
    public static ColumnFlowBand? InferImageAnchoredFlow(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<ImageRegion> images,
        float? detectedSplit)
    {
        if (items.Count == 0 || images.Count == 0)
        {
            return null;
        }

        if (PageXBounds(items, images) is not var (xMin, xMax))
        {
            return null;
        }

        if (detectedSplit is { } split &&
            PairedColumnImages(items, images, split, xMin, xMax) is { } paired)
        {
            return paired;
        }

        return LocalFlowBelowFullWidthImage(items, images, xMin, xMax);
    }

    /// <summary>
    /// Partitions a page into the topological order above → left → right →
    /// below, which encodes the reading-order edges. Empty nodes are omitted,
    /// and right-to-left pages swap the two column nodes.
    /// </summary>
    public static List<RegionNode> BuildRegionGraph(List<TextItem> items, ColumnFlowBand band)
    {
        var above = new List<TextItem>();
        var left = new List<TextItem>();
        var right = new List<TextItem>();
        var below = new List<TextItem>();

        foreach (var item in items)
        {
            if (item.Y > band.YTop)
            {
                above.Add(item);
            }
            else if (item.Y < band.YBottom)
            {
                below.Add(item);
            }
            else if (item.X + (TextUtils.EffectiveWidth(item) / 2.0f) < band.SplitX)
            {
                left.Add(item);
            }
            else
            {
                right.Add(item);
            }
        }

        var rtl = TextUtils.IsRtlText(left.Concat(right).Select(i => i.Text));

        var ordered = new List<(RegionKind Kind, List<TextItem> Items)>
        {
            (RegionKind.FullWidth, above),
        };

        if (rtl)
        {
            ordered.Add((RegionKind.Column, right));
            ordered.Add((RegionKind.Column, left));
        }
        else
        {
            ordered.Add((RegionKind.Column, left));
            ordered.Add((RegionKind.Column, right));
        }

        ordered.Add((RegionKind.FullWidth, below));

        return [.. ordered
            .Where(node => node.Items.Count > 0)
            .Select(node => new RegionNode { Kind = node.Kind, Items = node.Items })];
    }
}
