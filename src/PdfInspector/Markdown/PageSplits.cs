// Ported from reference/src/markdown/mod.rs
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>An x band on a page, used to scope table detection and line grouping.</summary>
internal readonly record struct XBand(float Low, float High);

/// <summary>
/// Page-partitioning heuristics: side-by-side layout detection, chart-page prose
/// columns, and the band filters that scope rects and lines to a zone.
/// </summary>
internal static class PageSplits
{
    private const string Module = "markdown";

    /// <summary>
    /// Detects a side-by-side layout — two tables placed left and right — by
    /// finding a significant x-position gap.
    /// </summary>
    /// <remarks>
    /// A candidate gap must be at least 30pt and fall in the middle 60% of the
    /// page's x range. Items are counted by centre position for an accurate
    /// balance check, each side needing at least 20%. The candidate with the
    /// fewest bounding-box crossings wins, and must stay under 5% of all items. To
    /// reject a single wide table with several column gaps, only pages with one
    /// cluster of balanced candidates, within 50pt, are accepted.
    /// </remarks>
    public static List<XBand> SplitSideBySide(IReadOnlyList<TextItem> items)
    {
        if (items.Count < 40)
        {
            return [];
        }

        var xs = items.Select(i => i.X).OrderBy(x => x, FloatTotalOrder.Instance).ToList();

        var xMin = xs[0];
        var xMax = xs[^1];
        var xRange = xMax - xMin;
        var centerLo = xMin + (xRange * 0.2f);
        var centerHi = xMin + (xRange * 0.8f);

        var candidates = new List<float>();
        for (var i = 1; i < xs.Count; i++)
        {
            var gap = xs[i] - xs[i - 1];
            var splitX = (xs[i - 1] + xs[i]) / 2.0f;
            if (gap >= 30.0f && i >= 20 && xs.Count - i >= 20 && splitX >= centerLo && splitX <= centerHi)
            {
                candidates.Add(splitX);
            }
        }

        if (candidates.Count == 0)
        {
            return [];
        }

        // Only a balanced split counts — each side must hold at least a fifth of
        // the items by centre position, which is more accurate than counting left
        // edges.
        var minSide = items.Count / 5;
        var bestSplit = 0.0f;
        var bestCrossing = int.MaxValue;

        foreach (var splitX in candidates)
        {
            var leftCount = items.Count(i => i.X + (i.Width / 2.0f) < splitX);
            var rightCount = items.Count - leftCount;
            if (Math.Min(leftCount, rightCount) < minSide)
            {
                continue;
            }

            var crossing = items.Count(item => item.X < splitX && item.X + item.Width > splitX);
            if (crossing < bestCrossing)
            {
                bestCrossing = crossing;
                bestSplit = splitX;
            }
        }

        if (bestCrossing == int.MaxValue)
        {
            return [];
        }

        // Crossing items must stay under 5% of the total, which still allows
        // spanning headers and labels.
        if (bestCrossing > Math.Max(items.Count / 20, 2))
        {
            return [];
        }

        // Several balanced candidates far apart mean a multi-column single table.
        // Candidates within 50pt count as the same split point; a genuine
        // side-by-side layout has exactly one cluster near the inter-table gap.
        var balancedPositions = candidates
            .Where(sx =>
            {
                var lc = items.Count(i => i.X + (i.Width / 2.0f) < sx);
                return Math.Min(lc, items.Count - lc) >= minSide;
            })
            .OrderBy(x => x, FloatTotalOrder.Instance)
            .ToList();

        var clusteredPositions = new List<float>();
        foreach (var position in balancedPositions)
        {
            if (clusteredPositions.Count == 0 || MathF.Abs(position - clusteredPositions[^1]) >= 50.0f)
            {
                clusteredPositions.Add(position);
            }
        }

        if (clusteredPositions.Count > 1)
        {
            return [];
        }

        // Do not split when the left side is text labels and the right side is
        // numeric data at matching Y positions: that is one table of labels plus
        // numbers, not two independent regions. All three signals are required.
        var leftItems = items.Where(i => i.X + (i.Width / 2.0f) < bestSplit).ToList();
        var rightItems = items.Where(i => i.X + (i.Width / 2.0f) >= bestSplit).ToList();

        if (leftItems.Count > 0 && rightItems.Count > 0)
        {
            var leftNumericRatio = leftItems.Count(IsNumericItem) / (float)leftItems.Count;
            var rightNumericRatio = rightItems.Count(IsNumericItem) / (float)rightItems.Count;

            if (leftNumericRatio < 0.30f && rightNumericRatio >= 0.70f)
            {
                const float YTol = 5.0f;
                var yMatches = rightItems.Count(ri => leftItems.Any(li => MathF.Abs(li.Y - ri.Y) < YTol));
                if (yMatches / (float)rightItems.Count >= 0.5f)
                {
                    return [];
                }
            }
        }

        return [new XBand(xMin, bestSplit), new XBand(bestSplit, xMax)];
    }

    /// <summary>True when most of the item's characters are numeric or currency punctuation.</summary>
    private static bool IsNumericItem(TextItem item)
    {
        var text = item.Text.Trim();
        if (text.Length == 0)
        {
            return false;
        }

        var dataChars = text.Count(c => char.IsAsciiDigit(c) || ",.-+%€$£¥()".Contains(c, StringComparison.Ordinal));
        return dataChars / (float)TextUtils.CharCount(text) >= 0.6f;
    }

    /// <summary>
    /// Detects two short prose columns on a chart page from repeated left anchors.
    /// Chart masking can leave fewer than 20 lines per column, and justified text
    /// can shrink the physical gutter below the projection detector's 8pt minimum;
    /// repeated prose left edges stay reliable in that case. The caller scopes this
    /// signal to pages with a confirmed chart region.
    /// </summary>
    public static float? ChartPageProseColumnSplit(IReadOnlyList<TextItem> items)
    {
        const float XTolerance = 12.0f;
        const int MinLinesPerColumn = 6;
        const float MinAnchorSeparation = 120.0f;
        const float MinVerticalSpan = 60.0f;

        var prose = items
            .Where(item =>
            {
                var words = item.Text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;
                var chars = Math.Max(TextUtils.CharCount(item.Text), 1);
                var alphabetic = item.Text.Count(char.IsLetter);
                return words >= 4 && item.Width >= 80.0f && alphabetic * 2 >= chars;
            })
            .OrderBy(i => i.X, FloatTotalOrder.Instance)
            .ToList();

        if (prose.Count < MinLinesPerColumn * 2)
        {
            return null;
        }

        var clusters = new List<(float Anchor, List<TextItem> Members)>();
        foreach (var item in prose)
        {
            var found = false;
            for (var i = 0; i < clusters.Count; i++)
            {
                if (MathF.Abs(item.X - clusters[i].Anchor) > XTolerance)
                {
                    continue;
                }

                clusters[i].Members.Add(item);
                clusters[i] = (clusters[i].Members.Sum(m => m.X) / clusters[i].Members.Count, clusters[i].Members);
                found = true;
                break;
            }

            if (!found)
            {
                clusters.Add((item.X, [item]));
            }
        }

        var dominant = clusters
            .Where(c => c.Members.Count >= MinLinesPerColumn)
            .OrderBy(c => c.Anchor, FloatTotalOrder.Instance)
            .ToList();

        if (dominant.Count != 2 || dominant[1].Anchor - dominant[0].Anchor < MinAnchorSeparation)
        {
            return null;
        }

        static (float Min, float Max) VerticalRange(List<TextItem> members) =>
            (members.Min(i => i.Y), members.Max(i => i.Y));

        var leftY = VerticalRange(dominant[0].Members);
        var rightY = VerticalRange(dominant[1].Members);
        if (leftY.Max - leftY.Min < MinVerticalSpan || rightY.Max - rightY.Min < MinVerticalSpan)
        {
            return null;
        }

        var overlap = MathF.Max(MathF.Min(leftY.Max, rightY.Max) - MathF.Max(leftY.Min, rightY.Min), 0.0f);
        var shorterSpan = MathF.Min(leftY.Max - leftY.Min, rightY.Max - rightY.Min);
        return overlap < shorterSpan * 0.4f ? null : (dominant[0].Anchor + dominant[1].Anchor) / 2.0f;
    }

    /// <summary>
    /// True when a chart crosses the inferred prose gutter far enough to act as a
    /// page-wide separator. A chart confined to one column must stay in that
    /// column's local reading order rather than reordering the whole page.
    /// </summary>
    public static bool ChartSpansProseSplit(ChartRegion region, float splitX)
    {
        const float MinChartWidthPerSide = 40.0f;
        return splitX - region.Left >= MinChartWidthPerSide && region.Right - splitX >= MinChartWidthPerSide;
    }

    /// <summary>
    /// True when a table-shaped rect cluster ends at an interior band boundary and
    /// its rows visibly continue past it. Tables often rule only their leading
    /// columns, so the text gap before the borderless columns masquerades as a
    /// page gutter; a real second layout column would instead be dense prose that
    /// does not track the table's rows.
    /// </summary>
    public static bool RectClusterSpansBandBoundary(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        uint page,
        IReadOnlyList<XBand> bands)
    {
        if (bands.Count < 2)
        {
            return false;
        }

        var pageRects = new List<RectBox>();
        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            var (x, w) = r.Width < 0.0f ? (r.X + r.Width, -r.Width) : (r.X, r.Width);
            var (y, h) = r.Height < 0.0f ? (r.Y + r.Height, -r.Height) : (r.Y, r.Height);
            pageRects.Add(new RectBox(x, y, w, h));
        }

        if (pageRects.Count < 6)
        {
            return false;
        }

        var clusters = RectGrid.ClusterRects(pageRects, 3.0f, 6);

        for (var bandIdx = 0; bandIdx + 1 < bands.Count; bandIdx++)
        {
            var b = bands[bandIdx].High;

            // Y ranges of clusters that individually show the split cuts a table:
            // either ruled on both sides of the boundary, or ending at it with
            // cell-like text row-aligned beyond.
            var tableYRanges = new List<(float Lo, float Hi)>();

            foreach (var cluster in clusters)
            {
                var x0 = float.PositiveInfinity;
                var y0 = float.PositiveInfinity;
                var x1 = float.NegativeInfinity;
                var y1 = float.NegativeInfinity;
                foreach (var i in cluster)
                {
                    var rect = pageRects[i];
                    x0 = MathF.Min(x0, rect.Left);
                    y0 = MathF.Min(y0, rect.Bottom);
                    x1 = MathF.Max(x1, rect.Right);
                    y1 = MathF.Max(y1, rect.Top);
                }

                var spans = x0 < b - 20.0f && x1 > b + 20.0f;
                var endsAt = x1 >= b - 60.0f && x1 <= b + 10.0f && x0 <= b;
                if (!spans && !endsAt)
                {
                    continue;
                }

                // Distinct row baselines of the items inside the cluster's box.
                var rowYs = new List<float>();
                foreach (var it in items)
                {
                    var cx = it.X + (it.Width / 2.0f);
                    if (it.Page == page
                        && cx > x0
                        && cx < x1
                        && it.Y >= y0 - 2.0f
                        && it.Y <= y1 + 2.0f
                        && !rowYs.Any(y => MathF.Abs(y - it.Y) <= 2.0f))
                    {
                        rowYs.Add(it.Y);
                    }
                }

                if (rowYs.Count < 2)
                {
                    continue;
                }

                var farAlignedRows = rowYs.Count(y => items.Any(it =>
                    it.Page == page
                    && it.X + (it.Width / 2.0f) > b
                    && it.Width <= 150.0f
                    && MathF.Abs(it.Y - y) <= 2.0f));

                if (farAlignedRows >= 2 && farAlignedRows * 2 >= rowYs.Count)
                {
                    tableYRanges.Add((y0, y1));
                }
            }

            if (tableYRanges.Count == 0)
            {
                continue;
            }

            // The split is only wrong when the table rows account for most of the
            // far side. A figure legitimately spanning two text columns leaves most
            // far-side text — the column's prose — outside its Y range.
            var far = items.Where(it => it.Page == page && it.X + (it.Width / 2.0f) > b).ToList();
            if (far.Count == 0)
            {
                continue;
            }

            var inside = far.Count(it => tableYRanges.Any(r => it.Y >= r.Lo - 2.0f && it.Y <= r.Hi + 2.0f));
            if (inside * 10 >= far.Count * 6)
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>
    /// Derives a side-by-side split from rect hint regions. When
    /// <see cref="SplitSideBySide"/> finds no gap — because the text gap is too
    /// small — hint regions from large rect clusters can still reveal a left/right
    /// zone layout, as calendar months and form sections do.
    /// </summary>
    public static List<XBand> SplitFromHintRegions(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        uint page)
    {
        var pageRects = new List<RectBox>();
        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            var (x, w) = r.Width < 0.0f ? (r.X + r.Width, -r.Width) : (r.X, r.Width);
            var (y, h) = r.Height < 0.0f ? (r.Y + r.Height, -r.Height) : (r.Y, r.Height);
            if (w < 5.0f || h < 5.0f)
            {
                continue;
            }

            pageRects.Add(new RectBox(x, y, w, h));
        }

        if (pageRects.Count < 60)
        {
            return [];
        }

        // The same width-outlier filter the rect detector applies.
        var widths = pageRects.Select(r => r.W).OrderBy(w => w, FloatTotalOrder.Instance).ToList();
        var medianWidth = widths[widths.Count / 2];
        pageRects.RemoveAll(r => r.W > medianWidth * 10.0f);

        var clusters = RectGrid.ClusterRects(pageRects, 3.0f, 6);
        if (clusters.Count < 4)
        {
            return [];
        }

        var hints = new List<(float XLeft, float XRight, float YBottom, float YTop)>();
        foreach (var clusterIndices in clusters)
        {
            var group = clusterIndices.Select(i => pageRects[i]).ToList();
            if (group.Count < 30)
            {
                continue;
            }

            var xLeft = group.Min(r => r.Left);
            var xRight = group.Max(r => r.Right);
            var yBottom = group.Min(r => r.Bottom);
            var yTop = group.Max(r => r.Top);
            var w = xRight - xLeft;
            var h = yTop - yBottom;
            if (w is >= 30.0f and <= 400.0f && h is >= 10.0f and <= 400.0f)
            {
                hints.Add((xLeft, xRight, yBottom, yTop));
            }
        }

        if (hints.Count < 4)
        {
            return [];
        }

        var xMin = items.Count > 0 ? items.Min(i => i.X) : 0.0f;
        var xMax = items.Count > 0 ? items.Max(i => i.X + i.Width) : 800.0f;
        var pageXMid = (xMin + xMax) / 2.0f;

        // Hints at the same Y band should fall into distinct x halves; count the
        // pairs that do.
        var pairCount = 0;
        for (var i = 0; i < hints.Count; i++)
        {
            for (var j = i + 1; j < hints.Count; j++)
            {
                var a = hints[i];
                var b = hints[j];
                var yOverlap = MathF.Min(a.YTop, b.YTop) - MathF.Max(a.YBottom, b.YBottom);
                var yMinSpan = MathF.Min(a.YTop - a.YBottom, b.YTop - b.YBottom);
                if (yOverlap <= yMinSpan * 0.5f)
                {
                    continue;
                }

                var aCenter = (a.XLeft + a.XRight) / 2.0f;
                var bCenter = (b.XLeft + b.XRight) / 2.0f;
                if (aCenter < pageXMid != (bCenter < pageXMid))
                {
                    pairCount++;
                }
            }
        }

        // Three left/right pairs confirm the layout.
        if (pairCount < 3)
        {
            return [];
        }

        var leftHints = hints.Where(h => (h.XLeft + h.XRight) / 2.0f < pageXMid).ToList();
        var rightHints = hints.Where(h => (h.XLeft + h.XRight) / 2.0f >= pageXMid).ToList();
        if (leftHints.Count == 0 || rightHints.Count == 0)
        {
            return [];
        }

        // The split sits midway between the rightmost left-zone hint and the
        // leftmost right-zone hint.
        var splitX = (leftHints.Max(h => h.XRight) + rightHints.Min(h => h.XLeft)) / 2.0f;
        Log.Debug(Module, () => $"page {page}: hint-derived side-by-side split at x={splitX:F1}");

        return [new XBand(xMin, splitX), new XBand(splitX, xMax)];
    }

    /// <summary>
    /// Filters rects to those mostly contained within an x band, excluding those
    /// that extend well beyond it — a page-wide background stripe spanning both
    /// side-by-side tables, say. A large rect needs 70% of its width inside the
    /// band; a small one only needs to overlap.
    /// </summary>
    public static List<PdfRect> FilterRectsToBand(
        IReadOnlyList<PdfRect> rects,
        uint page,
        float xLo,
        float xHi)
    {
        var bandWidth = xHi - xLo;
        var result = new List<PdfRect>();

        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            var rxMin = r.Width >= 0.0f ? r.X : r.X + r.Width;
            var rxMax = r.Width >= 0.0f ? r.X + r.Width : r.X;
            var rw = rxMax - rxMin;

            var overlap = MathF.Min(rxMax, xHi) - MathF.Max(rxMin, xLo);
            if (overlap <= 0.0f)
            {
                continue;
            }

            if (rw < bandWidth * 0.7f || overlap >= rw * 0.7f)
            {
                result.Add(r);
            }
        }

        return result;
    }

    /// <summary>Filters PDF line segments to those overlapping an x band.</summary>
    public static List<PdfLine> FilterLinesToBand(
        IReadOnlyList<PdfLine> lines,
        uint page,
        float xLo,
        float xHi) =>
        lines
            .Where(l => l.Page == page && MathF.Max(l.X1, l.X2) > xLo && MathF.Min(l.X1, l.X2) < xHi)
            .ToList();
}
