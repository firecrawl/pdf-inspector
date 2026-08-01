// Ported from reference/src/extractor/layout.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>A column region on a page, given by its horizontal extent.</summary>
internal sealed class ColumnRegion(float xMin, float xMax)
{
    public float XMin { get; } = xMin;

    public float XMax { get; } = xMax;

    public float Width => XMax - XMin;
}

/// <summary>
/// Column detection from a horizontal projection profile. An occupancy
/// histogram across the page width reveals gutters as valleys; each candidate
/// is then validated for vertical consistency so a chance gap does not read as
/// a column boundary.
/// </summary>
internal static class Columns
{
    private const string Module = "layout";

    private const float BinWidth = 2.0f;
    private const float MinGutterWidth = 8.0f;
    private const float MinVerticalSpanRatio = 0.30f;
    private const int MinItemsPerColumn = 10;
    private const float NoiseFraction = 0.15f;

    public static List<ColumnRegion> DetectColumns(IReadOnlyList<TextItem> items, uint page, bool pageHasTable)
    {
        // Image placeholders are excluded: an image's left edge would otherwise
        // count toward the projection profile.
        var pageItems = items.Where(i => i.Page == page && IsTextLayoutItem(i)).ToList();

        if (pageItems.Count == 0)
        {
            return [];
        }

        Log.Debug(Module, () => $"page {page}: detect_columns: {pageItems.Count} items");

        var xMin = pageItems.Min(i => i.X);
        var xMax = pageItems.Max(i => i.X + TextUtils.EffectiveWidth(i));
        var pageWidth = xMax - xMin;

        if (pageWidth < 200.0f || pageItems.Count < 20)
        {
            return [new ColumnRegion(xMin, xMax)];
        }

        // Items wider than 60% of the page are spanning content — titles and
        // full-width paragraphs — which would fill the gutter and hide a
        // partial-page column layout.
        var wideThreshold = pageWidth * 0.6f;
        var numBins = Math.Max((int)MathF.Ceiling(pageWidth / BinWidth), 1);
        var histogram = new uint[numBins];

        foreach (var item in pageItems)
        {
            var w = TextUtils.EffectiveWidth(item);
            if (w > wideThreshold)
            {
                continue;
            }

            var left = Math.Min((int)MathF.Floor((item.X - xMin) / BinWidth), numBins);
            var right = Math.Min((int)MathF.Ceiling((item.X + w - xMin) / BinWidth), numBins);

            for (var i = Math.Max(left, 0); i < right; i++)
            {
                histogram[i]++;
            }
        }

        var maxCount = histogram.Length > 0 ? histogram.Max() : 0u;
        var noiseThreshold = (uint)(maxCount * NoiseFraction);

        var valleys = FindAbsoluteValleys(histogram, numBins, noiseThreshold, pageWidth);
        var marginThreshold = pageWidth * 0.05f;

        // Justified text can leave gutter bins occupied, because item widths run
        // to the column edge. A relative-valley pass finds local minima that sit
        // well below the peaks on either side. Only dense pages qualify, and
        // pages with tables are skipped: table column gaps look like gutters,
        // and the table pipeline already handles their reading order.
        if (valleys.Count == 0)
        {
            if (pageItems.Count >= 100 && !pageHasTable)
            {
                var relative = FindRelativeValleys(histogram, numBins, BinWidth, pageWidth, marginThreshold);
                if (relative.Count > 0)
                {
                    var result = ValidateAndBuildColumns(
                        relative, pageItems, xMin, xMax, MinItemsPerColumn, MinVerticalSpanRatio, page,
                        centerAssign: true);

                    if (result.Count > 1)
                    {
                        // Tables, forms, and checklists have short scattered
                        // items that create false gutter signals, so a relative
                        // valley only commits when both sides read as prose.
                        if (ColumnsHaveProse(result, pageItems))
                        {
                            Log.Debug(Module, () =>
                                $"page {page}: relative valley detection found {result.Count} columns");
                            return result;
                        }

                        Log.Debug(Module, () =>
                            $"page {page}: relative valley rejected — columns lack prose density");
                    }
                }

                if (TryXyCutSplit(pageItems, xMin, xMax, page) is { } cut)
                {
                    return cut;
                }
            }

            return [new ColumnRegion(xMin, xMax)];
        }

        // Center-based assignment handles asymmetric layouts and sidebars better;
        // edge-based is the fallback when it produces a degenerate split.
        var centered = ValidateAndBuildColumns(
            valleys, pageItems, xMin, xMax, MinItemsPerColumn, MinVerticalSpanRatio, page, centerAssign: true);
        if (centered.Count > 1)
        {
            return centered;
        }

        var edged = ValidateAndBuildColumns(
            valleys, pageItems, xMin, xMax, MinItemsPerColumn, MinVerticalSpanRatio, page, centerAssign: false);
        if (edged.Count > 1)
        {
            return edged;
        }

        if (pageItems.Count >= 20 && !pageHasTable && TryXyCutSplit(pageItems, xMin, xMax, page) is { } fallback)
        {
            return fallback;
        }

        return [new ColumnRegion(xMin, xMax)];
    }

    /// <summary>
    /// Items that should take part in layout heuristics. Image placeholders
    /// carry no glyphs and would skew column and row clustering; links and form
    /// fields are text-like and do participate.
    /// </summary>
    public static bool IsTextLayoutItem(TextItem item) => item.Kind != ItemKind.Image;

    private static List<(int Start, int End)> FindAbsoluteValleys(
        uint[] histogram,
        int numBins,
        uint noiseThreshold,
        float pageWidth)
    {
        var valleys = new List<(int Start, int End)>();
        int? valleyStart = null;

        for (var i = 0; i < numBins; i++)
        {
            if (histogram[i] <= noiseThreshold)
            {
                valleyStart ??= i;
            }
            else if (valleyStart is { } start)
            {
                valleys.Add((start, i));
                valleyStart = null;
            }
        }

        if (valleyStart is { } trailing)
        {
            valleys.Add((trailing, numBins));
        }

        // A valley must be wide enough and away from the page margins.
        var marginThreshold = pageWidth * 0.05f;
        return [.. valleys.Where(v =>
        {
            var widthPts = (v.End - v.Start) * BinWidth;
            if (widthPts < MinGutterWidth)
            {
                return false;
            }

            var centerPts = (v.Start + v.End) / 2.0f * BinWidth;
            return centerPts > marginThreshold && centerPts < pageWidth - marginThreshold;
        })];
    }

    /// <summary>
    /// Finds local minima that drop well below the peaks on either side, which
    /// reveals a gutter even when justified text keeps its bins occupied.
    /// </summary>
    private static List<(int Start, int End)> FindRelativeValleys(
        uint[] histogram,
        int numBins,
        float binWidth,
        float pageWidth,
        float marginThreshold)
    {
        const int MinGutterBins = 2;
        const float ContrastThreshold = 0.60f;
        const int PeakWindow = 25;
        const float MinPeakHeight = 20.0f;

        if (numBins < 10)
        {
            return [];
        }

        // A five-bin moving average damps the noise that would otherwise
        // produce spurious minima.
        var smoothed = new float[numBins];
        const int HalfWindow = 2;
        for (var i = 0; i < numBins; i++)
        {
            var lo = Math.Max(i - HalfWindow, 0);
            var hi = Math.Min(i + HalfWindow + 1, numBins);
            uint sum = 0;
            for (var j = lo; j < hi; j++)
            {
                sum += histogram[j];
            }

            smoothed[i] = (float)sum / (hi - lo);
        }

        var candidates = new List<(int Bin, float Value, float Contrast)>();

        for (var i = PeakWindow; i < Math.Max(numBins - PeakWindow, PeakWindow); i++)
        {
            var val = smoothed[i];
            if (val < 1.0f)
            {
                continue;
            }

            var localLo = Math.Max(i - 3, 0);
            var localHi = Math.Min(i + 4, numBins);
            var isLocalMin = true;
            for (var j = localLo; j < localHi; j++)
            {
                if (smoothed[j] < val - 0.5f)
                {
                    isLocalMin = false;
                    break;
                }
            }

            if (!isLocalMin)
            {
                continue;
            }

            var leftPeak = 0.0f;
            for (var j = Math.Max(i - PeakWindow, 0); j < i; j++)
            {
                leftPeak = MathF.Max(leftPeak, smoothed[j]);
            }

            var rightPeak = 0.0f;
            for (var j = i + 1; j < Math.Min(i + 1 + PeakWindow, numBins); j++)
            {
                rightPeak = MathF.Max(rightPeak, smoothed[j]);
            }

            if (leftPeak < MinPeakHeight || rightPeak < MinPeakHeight)
            {
                continue;
            }

            // Both peaks must be substantial, which keeps a margin drop-off in a
            // single-column ragged layout from reading as a gutter.
            var peakBalance = MathF.Min(leftPeak, rightPeak) / MathF.Max(leftPeak, rightPeak);
            if (peakBalance < 0.40f)
            {
                continue;
            }

            var contrast = val / MathF.Min(leftPeak, rightPeak);
            if (contrast >= ContrastThreshold)
            {
                continue;
            }

            var centerPts = i * binWidth;
            if (centerPts > marginThreshold && centerPts < pageWidth - marginThreshold)
            {
                candidates.Add((i, val, contrast));
            }
        }

        if (candidates.Count == 0)
        {
            return [];
        }

        // Adjacent candidates form one valley; the deepest point represents it.
        var valleys = new List<(int Start, int End)>();
        var bestBin = candidates[0].Bin;
        var bestContrast = candidates[0].Contrast;

        for (var i = 0; i + 1 < candidates.Count; i++)
        {
            var prevBin = candidates[i].Bin;
            var (nextBin, _, nextContrast) = candidates[i + 1];

            if (nextBin - prevBin <= 5)
            {
                if (nextContrast < bestContrast)
                {
                    bestBin = nextBin;
                    bestContrast = nextContrast;
                }
            }
            else
            {
                valleys.Add((Math.Max(bestBin - MinGutterBins, 0), Math.Min(bestBin + MinGutterBins + 1, numBins)));
                bestBin = nextBin;
                bestContrast = nextContrast;
            }
        }

        valleys.Add((Math.Max(bestBin - MinGutterBins, 0), Math.Min(bestBin + MinGutterBins + 1, numBins)));

        // Layouts with three or more columns have clear gutters that absolute
        // detection already handles, so this fallback keeps only its best valley.
        if (valleys.Count > 1)
        {
            var bestIndex = 0;
            var bestValue = float.MaxValue;

            for (var vi = 0; vi < valleys.Count; vi++)
            {
                var mid = (valleys[vi].Start + valleys[vi].End) / 2;
                float? nearest = null;

                foreach (var (bin, _, contrast) in candidates)
                {
                    if (Math.Abs(bin - mid) <= 5)
                    {
                        nearest = nearest is null ? contrast : MathF.Min(nearest.Value, contrast);
                    }
                }

                if (nearest is { } c && c < bestValue)
                {
                    bestValue = c;
                    bestIndex = vi;
                }
            }

            return [valleys[bestIndex]];
        }

        return valleys;
    }

    /// <summary>
    /// True when a side of a gutter is predominantly standalone list markers. A
    /// column of bullets at the left margin creates a spurious valley between
    /// the marker and its content, which would split every list item in two.
    /// </summary>
    private static bool IsListMarkerColumn(List<TextItem> items)
    {
        char[] listMarkers = ['•', '●', '○', '◦', '▪', '▫', '◆', '◇', '■', '□'];

        if (items.Count == 0)
        {
            return false;
        }

        var markerCount = items.Count(i =>
        {
            var t = i.Text.Trim();
            return t.Length == 1 && listMarkers.Contains(t[0]);
        });

        // A few stray page numbers or footnote references should not defeat the check.
        return (float)markerCount / items.Count >= 0.8f;
    }

    /// <summary>
    /// Validates valley candidates against vertical consistency and builds the
    /// resulting column regions.
    /// </summary>
    /// <param name="centerAssign">
    /// When true, items are assigned by their midpoint rather than their right
    /// edge, which helps where justified text runs past the gutter.
    /// </param>
    private static List<ColumnRegion> ValidateAndBuildColumns(
        List<(int Start, int End)> valleys,
        List<TextItem> pageItems,
        float xMin,
        float xMax,
        int minItems,
        float minVerticalSpan,
        uint page,
        bool centerAssign)
    {
        // The vertical range comes from column-eligible items only — the same
        // ones the histogram counted. Letting spanning items stretch it would
        // sink the overlap ratio for columns that legitimately occupy just part
        // of the page, such as two-column text below a figure.
        var xSpan = pageItems.Max(i => i.X + TextUtils.EffectiveWidth(i)) - pageItems.Min(i => i.X);
        var narrow = pageItems.Where(i => TextUtils.EffectiveWidth(i) <= xSpan * 0.6f).ToList();
        var spanItems = narrow.Count == 0 ? [] : narrow;

        var yMin = spanItems.Count > 0 ? spanItems.Min(i => i.Y) : float.PositiveInfinity;
        var yMax = spanItems.Count > 0 ? spanItems.Max(i => i.Y) : float.NegativeInfinity;
        var yRange = yMax - yMin;

        var validValleys = new List<(int Start, int End, int LeftCount, int RightCount)>();

        foreach (var (start, end) in valleys)
        {
            var gutterLeft = xMin + (start * BinWidth);
            var gutterRight = xMin + (end * BinWidth);
            var gutterCenter = (gutterLeft + gutterRight) / 2.0f;

            var leftItems = pageItems.Where(i => centerAssign
                ? i.X + (TextUtils.EffectiveWidth(i) / 2.0f) <= gutterCenter
                : i.X + TextUtils.EffectiveWidth(i) <= gutterCenter).ToList();

            var rightItems = pageItems.Where(i => centerAssign
                ? i.X + (TextUtils.EffectiveWidth(i) / 2.0f) > gutterCenter
                : i.X >= gutterCenter).ToList();

            // A symmetric layout needs the minimum on each side; an asymmetric
            // one (a sidebar) is accepted when the dominant side has the minimum
            // and the smaller side has at least three items.
            var (smaller, larger) = leftItems.Count <= rightItems.Count
                ? (leftItems.Count, rightItems.Count)
                : (rightItems.Count, leftItems.Count);

            if (larger < minItems || smaller < 3)
            {
                Log.Debug(Module, () => $"  valley rejected: counts smaller={smaller} larger={larger}");
                continue;
            }

            var smallerItems = leftItems.Count <= rightItems.Count ? leftItems : rightItems;
            if (IsListMarkerColumn(smallerItems))
            {
                Log.Debug(Module, "  valley rejected: list-marker column");
                continue;
            }

            if (yRange > 0.0f)
            {
                var leftYMin = leftItems.Min(i => i.Y);
                var leftYMax = leftItems.Max(i => i.Y);
                var rightYMin = rightItems.Min(i => i.Y);
                var rightYMax = rightItems.Max(i => i.Y);

                var overlapMin = MathF.Max(leftYMin, rightYMin);
                var overlapMax = MathF.Min(leftYMax, rightYMax);
                var overlap = MathF.Max(overlapMax - overlapMin, 0.0f);

                if (overlap / yRange < minVerticalSpan)
                {
                    Log.Debug(Module, () =>
                        $"  valley rejected: overlap {overlap:F0}/{yRange:F0} = " +
                        $"{overlap / yRange:F2} < {minVerticalSpan:F2}");
                    continue;
                }
            }

            validValleys.Add((start, end, leftItems.Count, rightItems.Count));
        }

        if (validValleys.Count == 0)
        {
            Log.Debug(Module, () => $"page {page}: {valleys.Count} valleys found but none passed validation");
            return [new ColumnRegion(xMin, xMax)];
        }

        Log.Debug(Module, () => $"page {page}: {validValleys.Count + 1} columns detected");

        // At most three gutters, so four columns. The score favours a wide
        // gutter with substantial content on both sides.
        if (validValleys.Count > 3)
        {
            validValleys.Sort((a, b) =>
            {
                var scoreA = (a.End - a.Start) * (float)Math.Min(a.LeftCount, a.RightCount);
                var scoreB = (b.End - b.Start) * (float)Math.Min(b.LeftCount, b.RightCount);
                return scoreB.CompareTo(scoreA);
            });

            validValleys = [.. validValleys.Take(3)];
            validValleys.Sort((a, b) => a.Start.CompareTo(b.Start));
        }

        var columns = new List<ColumnRegion>();
        var colStart = xMin;

        foreach (var (start, end, _, _) in validValleys)
        {
            var gutterCenter = xMin + ((start + end) / 2.0f * BinWidth);
            columns.Add(new ColumnRegion(colStart, gutterCenter));
            colStart = gutterCenter;
        }

        columns.Add(new ColumnRegion(colStart, xMax));
        return columns;
    }

    /// <summary>
    /// A simplified single-level XY cut: find the widest horizontal gap between
    /// item edges and split there when both sides carry enough vertically
    /// overlapping content. This catches asymmetric layouts whose narrow column
    /// has too few items to register in the occupancy profile.
    /// </summary>
    private static List<ColumnRegion>? TryXyCutSplit(
        List<TextItem> pageItems,
        float pageXMin,
        float pageXMax,
        uint page)
    {
        const float MinGap = 15.0f;
        const int MinItemsMajor = 10;
        const int MinItemsMinor = 3;

        var pageWidth = pageXMax - pageXMin;
        if (pageWidth < 200.0f || pageItems.Count < 2)
        {
            return null;
        }

        var sortedByLeft = pageItems
            .Select(i => (Left: i.X, Right: i.X + TextUtils.EffectiveWidth(i)))
            .OrderBy(e => e.Left, FloatTotalOrder.Instance)
            .ToList();

        var bestGap = 0.0f;
        var bestSplit = 0.0f;
        var maxRightSoFar = float.NegativeInfinity;

        for (var i = 0; i + 1 < sortedByLeft.Count; i++)
        {
            maxRightSoFar = MathF.Max(maxRightSoFar, sortedByLeft[i].Right);

            var gap = sortedByLeft[i + 1].Left - maxRightSoFar;
            if (gap > bestGap)
            {
                bestGap = gap;
                bestSplit = (maxRightSoFar + sortedByLeft[i + 1].Left) / 2.0f;
            }
        }

        if (bestGap < MinGap)
        {
            return null;
        }

        var margin = pageWidth * 0.10f;
        if (bestSplit - pageXMin < margin || pageXMax - bestSplit < margin)
        {
            return null;
        }

        var leftItems = pageItems.Where(i => i.X + (TextUtils.EffectiveWidth(i) / 2.0f) <= bestSplit).ToList();
        var rightItems = pageItems.Where(i => i.X + (TextUtils.EffectiveWidth(i) / 2.0f) > bestSplit).ToList();

        var (minor, major) = leftItems.Count <= rightItems.Count
            ? (leftItems.Count, rightItems.Count)
            : (rightItems.Count, leftItems.Count);

        if (major < MinItemsMajor || minor < MinItemsMinor)
        {
            return null;
        }

        // Both sides must span a meaningful vertical range for the split to be
        // a column boundary rather than a stacked layout.
        var lYMin = leftItems.Min(i => i.Y);
        var lYMax = leftItems.Max(i => i.Y);
        var rYMin = rightItems.Min(i => i.Y);
        var rYMax = rightItems.Max(i => i.Y);

        var overlap = MathF.Max(MathF.Min(lYMax, rYMax) - MathF.Max(lYMin, rYMin), 0.0f);
        var yRange = MathF.Max(MathF.Max(lYMax, rYMax) - MathF.Min(lYMin, rYMin), 1.0f);

        if (overlap / yRange < 0.20f)
        {
            return null;
        }

        Log.Debug(Module, () =>
            $"page {page}: XY-cut split at x={bestSplit:F1} (gap={bestGap:F1}pt, " +
            $"left={leftItems.Count}, right={rightItems.Count})");

        return [new ColumnRegion(pageXMin, bestSplit), new ColumnRegion(bestSplit, pageXMax)];
    }

    /// <summary>
    /// True when every proposed column holds paragraph-like content. Two-column
    /// prose produces lines that fill most of the column width; tables, forms,
    /// and checklists produce short scattered items that do not.
    /// </summary>
    private static bool ColumnsHaveProse(List<ColumnRegion> columns, List<TextItem> items)
    {
        const float YTol = 3.0f;
        const float LineFillThreshold = 0.45f;
        const float MinProseRatio = 0.40f;
        const int MinLines = 8;
        const float MinColWidth = 120.0f;
        const float MaxAvgItemsPerLine = 3.5f;

        foreach (var col in columns)
        {
            var colWidth = col.Width;
            if (colWidth < MinColWidth)
            {
                return false;
            }

            var colItems = items.Where(i =>
            {
                var center = i.X + (TextUtils.EffectiveWidth(i) / 2.0f);
                return center >= col.XMin && center <= col.XMax;
            }).ToList();

            if (colItems.Count < MinLines)
            {
                return false;
            }

            // Top of page first, since PDF coordinates run upward.
            colItems.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));

            var fullLines = 0;
            var totalLines = 0;
            var totalItemsInLines = 0;
            var lineItems = new List<TextItem>();
            var lineY = float.NaN;

            void FlushLine()
            {
                if (lineItems.Count == 0)
                {
                    return;
                }

                totalLines++;
                totalItemsInLines += lineItems.Count;

                var left = lineItems.Min(i => MathF.Max(i.X, col.XMin));
                var right = lineItems.Max(i => MathF.Min(i.X + TextUtils.EffectiveWidth(i), col.XMax));
                var span = MathF.Max(right - left, 0.0f);

                if (span >= colWidth * LineFillThreshold)
                {
                    fullLines++;
                }
            }

            foreach (var item in colItems)
            {
                if (lineItems.Count == 0 || MathF.Abs(lineY - item.Y) < YTol)
                {
                    if (lineItems.Count == 0)
                    {
                        lineY = item.Y;
                    }

                    lineItems.Add(item);
                }
                else
                {
                    FlushLine();
                    lineItems.Clear();
                    lineY = item.Y;
                    lineItems.Add(item);
                }
            }

            FlushLine();

            if (totalLines < MinLines)
            {
                return false;
            }

            var ratio = (float)fullLines / totalLines;
            var avgItems = (float)totalItemsInLines / totalLines;

            Log.Debug(Module, () =>
                $"columns_have_prose: col [{col.XMin:F0}..{col.XMax:F0}] lines={totalLines} " +
                $"full={fullLines} ratio={ratio:F2} avg_items={avgItems:F1}");

            if (ratio < MinProseRatio)
            {
                return false;
            }

            // Tables and forms carry many small items per line, one per cell,
            // while prose has few — one per word run or phrase.
            if (avgItems > MaxAvgItemsPerLine)
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>
    /// Marks items belonging to lines that run across the detected columns —
    /// titles and section headers. A line qualifies when its span exceeds the
    /// widest column by a third and no inter-item gap sits at a gutter, since a
    /// gap at a gutter means these are separate columns at the same height.
    /// </summary>
    public static bool[] IdentifySpanningLines(IReadOnlyList<TextItem> items, List<ColumnRegion> columns)
    {
        var mask = new bool[items.Count];

        if (items.Count < 3 || columns.Count < 2)
        {
            return mask;
        }

        var maxColWidth = columns.Max(c => c.Width);
        var spanThreshold = maxColWidth * 1.3f;

        var gutters = new List<float>();
        for (var i = 0; i + 1 < columns.Count; i++)
        {
            gutters.Add(columns[i].XMax);
        }

        const float GutterTol = 15.0f;
        const float YTol = 5.0f;

        var indexed = items
            .Select((item, index) => (Index: index, item.Y))
            .OrderByDescending(e => e.Y, FloatTotalOrder.Instance)
            .ToList();

        var groups = new List<List<int>>();
        var currentGroup = new List<int>();
        var currentY = float.NaN;

        foreach (var (index, y) in indexed)
        {
            if (currentGroup.Count == 0 || MathF.Abs(currentY - y) < YTol)
            {
                if (currentGroup.Count == 0)
                {
                    currentY = y;
                }

                currentGroup.Add(index);
            }
            else
            {
                groups.Add(currentGroup);
                currentGroup = [index];
                currentY = y;
            }
        }

        if (currentGroup.Count > 0)
        {
            groups.Add(currentGroup);
        }

        foreach (var group in groups)
        {
            if (group.Count < 2)
            {
                continue;
            }

            var sortedByX = group.OrderBy(i => items[i].X, FloatTotalOrder.Instance).ToList();

            var lineLeft = items[sortedByX[0]].X;
            var last = sortedByX[^1];
            var lineRight = items[last].X + TextUtils.EffectiveWidth(items[last]);

            if (lineRight - lineLeft <= spanThreshold)
            {
                continue;
            }

            var hasGutterGap = false;
            for (var i = 0; i + 1 < sortedByX.Count; i++)
            {
                var leftEnd = items[sortedByX[i]].X + TextUtils.EffectiveWidth(items[sortedByX[i]]);
                var rightStart = items[sortedByX[i + 1]].X;

                if (rightStart - leftEnd < 5.0f)
                {
                    continue;
                }

                if (gutters.Any(g => g > leftEnd - GutterTol && g < rightStart + GutterTol))
                {
                    hasGutterGap = true;
                    break;
                }
            }

            if (!hasGutterGap)
            {
                foreach (var index in sortedByX)
                {
                    mask[index] = true;
                }
            }
        }

        return mask;
    }

    /// <summary>True when an item straddles two or more columns, as a full-width header does.</summary>
    public static bool SpansMultipleColumns(TextItem item, List<ColumnRegion> columns)
    {
        var itemRight = item.X + TextUtils.EffectiveWidth(item);

        var overlapCount = columns.Count(col =>
        {
            var overlap = MathF.Max(MathF.Min(itemRight, col.XMax) - MathF.Max(item.X, col.XMin), 0.0f);
            return overlap > col.Width * 0.10f || overlap > 20.0f;
        });

        return overlapCount >= 2;
    }

    /// <summary>True when an item looks like a page number: a short run of digits near a page edge.</summary>
    public static bool IsPageNumber(TextItem item)
    {
        var text = item.Text.Trim();

        if (text.Length == 0 || text.Length > 4 || !text.All(char.IsAsciiDigit))
        {
            return false;
        }

        // US Letter is 792pt and A4 841pt; page numbers sit in the top few
        // percent or the bottom eighth.
        return item.Y > 720.0f || item.Y < 100.0f;
    }
}
