// Ported from reference/src/tables/detect_rects.rs
using PdfInspector.Extractor;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// A bounding box derived from cell-border rectangles that failed full grid
/// validation. Even without a usable grid the box still tells the heuristic
/// detector where a table sits, keeping unrelated items (chart labels and the
/// like) out of it.
/// </summary>
internal sealed class RectHintRegion
{
    /// <summary>Y of the top edge — the highest value, since PDF space grows upwards.</summary>
    public float YTop { get; set; }

    /// <summary>Y of the bottom edge.</summary>
    public float YBottom { get; set; }

    /// <summary>X of the left edge.</summary>
    public float XLeft { get; set; }

    /// <summary>X of the right edge.</summary>
    public float XRight { get; set; }

    /// <summary>The cluster's raw rectangles, for rect-guided table building.</summary>
    public List<RectBox> ClusterRects { get; set; } = [];
}

/// <summary>
/// The rectangle-driven table strategies that sit on top of the grid builder in
/// <see cref="RectGrid"/>: row stripes, stacked boxes, merged clusters, chart
/// rejection, and the hint regions a failed cluster leaves behind.
/// </summary>
internal static class RectTables
{
    private const string Module = "tables";

    /// <summary>
    /// A handful of coincident origin frames may be real structure, so repeated
    /// page fills only count as normalization evidence once they dominate.
    /// </summary>
    private const int DominantPageBackgroundMinRepetitions = 8;

    /// <summary>
    /// Small chart panels can still form plausible grids from their labels, so a
    /// hypothesis competing with a chart rejection needs sustained row evidence.
    /// </summary>
    private const int CompetingTableMinRows = 8;

    /// <summary>Function words whose presence marks a cell as prose rather than data.</summary>
    private static readonly string[] StackedBoxProseWords =
    [
        "a", "an", "the", "of", "to", "is", "was", "are", "were", "be", "been", "in", "on", "at",
        "with", "for", "by", "as", "and", "or", "but", "this", "that", "these", "those", "from",
        "into", "has", "have", "had", "not", "it", "its", "their", "such", "shall", "which",
    ];

    /// <summary>The same test as <see cref="StackedBoxProseWords"/>, widened with pronouns for framed prose.</summary>
    private static readonly string[] CellRectProseWords =
    [
        "a", "an", "the", "of", "to", "is", "was", "are", "were", "be", "been", "in", "on",
        "at", "with", "for", "by", "as", "and", "or", "but", "this", "that", "these", "those",
        "from", "into", "has", "have", "had", "not", "don't", "doesn't", "it's", "its", "it",
        "i", "me", "my", "we", "our", "us", "you", "your", "they", "them", "their", "he",
        "she", "his", "her",
    ];

    /// <summary>
    /// Bounding boxes of the page's chart-bar clusters. Text inside one (axis
    /// labels, data values, legends) belongs to a figure and must not be gridded
    /// into a table by any strategy.
    /// </summary>
    public static List<RectBox> DetectChartRegions(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        uint page)
    {
        // Match DetectTablesFromRects: image placeholders are not text and would
        // defeat the bar-content check.
        var textItems = items.Where(Columns.IsTextLayoutItem).ToList();

        var pageRects = new List<RectBox>();
        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            var box = Normalize(r);

            // Origin-anchored page backgrounds and clipping paths are never chart
            // geometry, and letting one bridge into a bar cluster would inflate the
            // region to the whole page.
            if (box.W >= 5.0f && box.H >= 5.0f && !(box.X < 5.0f && box.Y < 5.0f))
            {
                pageRects.Add(box);
            }
        }

        if (pageRects.Count < 6)
        {
            return [];
        }

        var regions = new List<RectBox>();
        foreach (var cluster in RectGrid.ClusterRects(pageRects, 3.0f, 6))
        {
            var group = cluster.Select(i => pageRects[i]).ToList();
            if (IsChartBarCluster(textItems, group, page))
            {
                regions.Add(BoundingBox(group));
            }
        }

        return regions;
    }

    /// <summary>Normalises a raw rectangle so its width and height are positive.</summary>
    private static RectBox Normalize(PdfRect r)
    {
        var (x, w) = r.Width < 0.0f ? (r.X + r.Width, -r.Width) : (r.X, r.Width);
        var (y, h) = r.Height < 0.0f ? (r.Y + r.Height, -r.Height) : (r.Y, r.Height);
        return new RectBox(x, y, w, h);
    }

    /// <summary>The bounding box of a non-empty rectangle group.</summary>
    private static RectBox BoundingBox(IReadOnlyList<RectBox> group)
    {
        var x0 = float.PositiveInfinity;
        var y0 = float.PositiveInfinity;
        var x1 = float.NegativeInfinity;
        var y1 = float.NegativeInfinity;

        foreach (var r in group)
        {
            x0 = MathF.Min(x0, r.Left);
            y0 = MathF.Min(y0, r.Bottom);
            x1 = MathF.Max(x1, r.Right);
            y1 = MathF.Max(y1, r.Top);
        }

        return new RectBox(x0, y0, x1 - x0, y1 - y0);
    }

    /// <summary>Runs the three per-cluster strategies in priority order.</summary>
    private static Table? DetectDirectRectTable(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> rects,
        uint page) =>
        RectGrid.DetectTableFromRectGroup(items, rects, page)
            ?? DetectRowStripeTable(items, rects, page)
            ?? DetectStackedBoxTable(items, rects, page);

    /// <summary>
    /// Detects tables from the explicit <c>re</c> rectangles a PDF draws for cell
    /// borders. Table pages typically carry 100–200+ rectangles where ordinary
    /// pages carry fewer than 30. Spatially connected rectangles are clustered,
    /// grids of cell-sized rectangles identified within each cluster, and text
    /// items assigned to cells.
    /// </summary>
    /// <returns>
    /// The detected tables, plus hint regions from clusters that failed grid
    /// validation — those scope heuristic detection so unrelated items stay out.
    /// </returns>
    public static (List<Table> Tables, List<RectHintRegion> Hints) DetectTablesFromRects(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        uint page)
    {
        // Strip image placeholders before clustering — an image's bbox would
        // otherwise show up as a spurious column edge.
        var textItems = items.Where(Columns.IsTextLayoutItem).ToList();

        var pageRects = new List<RectBox>();
        var rawOnPage = 0;
        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            rawOnPage++;
            var box = Normalize(r);

            // Skip tiny rects: borders, dots, decorations.
            if (box.W < 5.0f || box.H < 5.0f)
            {
                continue;
            }

            pageRects.Add(box);
        }

        // Remove rects much wider than a typical cell — page-spanning clipping
        // paths or row-spanning background fills that would add spurious X-edges
        // and corrupt the grid. The median WIDTH (not area) is the right yardstick
        // because row-stripe tables have every rect at the same full width, so
        // their median width equals the table width and nothing gets filtered.
        // Cell-grid tables have narrow cell rects, so full-width fills stand out.
        if (pageRects.Count >= 6)
        {
            var widths = pageRects.Select(r => r.W).OrderBy(w => w, FloatTotalOrder.Instance).ToList();
            var medianWidth = widths[widths.Count / 2];
            var widthThreshold = medianWidth * 10.0f;
            var before = pageRects.Count;
            pageRects.RemoveAll(r => r.W > widthThreshold);
            if (pageRects.Count < before)
            {
                Log.Debug(Module, () =>
                    $"page {page}: removed {before - pageRects.Count} oversized rects " +
                    $"(median_w={medianWidth:F0}, threshold={widthThreshold:F0})");
            }

            // Deduplicate sub-rects: a rect fully contained in a slightly larger
            // one (same column, interior Y range) is a cell-internal decoration —
            // content-area shading inside the full cell background, say. Keeping
            // both creates spurious Y-edges that split visual rows into thin
            // sub-rows.
            //
            // Only remove when the container is a similarly sized cell (height
            // ratio < 4×), never when it is a table-wide background dwarfing the
            // sub-rect. Origin-anchored page backgrounds also disqualify as
            // containers: they normally exceed the 4× ratio, but when the sub-rect
            // is itself a tall table frame the ratio can slip under the gate, and
            // dropping the frame collapses cluster adjacency between adjacent
            // column-cell groups.
            //
            // Skip this O(n²) pass on rect-heavy pages; thousands of vector-drawing
            // rects gain nothing from cell dedup.
            if (pageRects.Count < RectGrid.MaxClusterRects)
            {
                var beforeDedup = pageRects.Count;
                var snapshot = pageRects.ToList();
                pageRects.RemoveAll(a =>
                {
                    const float Tol = 2.0f;
                    return snapshot.Any(b =>
                    {
                        var containerIsPageBg = b.X < 5.0f && b.Y < 5.0f;
                        return b.Area > a.Area * 1.2f
                            && b.H < a.H * 4.0f
                            && !containerIsPageBg
                            && b.Left <= a.Left + Tol
                            && b.Right >= a.Right - Tol
                            && b.Bottom <= a.Bottom + Tol
                            && b.Top >= a.Top - Tol;
                    });
                });
                if (pageRects.Count < beforeDedup)
                {
                    Log.Debug(Module, () =>
                        $"page {page}: removed {beforeDedup - pageRects.Count} contained sub-rects");
                }
            }
        }

        Log.Debug(Module, () =>
            $"page {page}: {pageRects.Count} rects after size filter (from {rawOnPage} raw)");

        var tables = new List<Table>();
        var hintRegions = new List<RectHintRegion>();
        var failedClusters = new List<List<RectBox>>();

        // Full grid detection requires at least 6 rects.
        if (pageRects.Count >= 6)
        {
            // Identify origin-anchored page-background rects (clipping paths or
            // page fills) that would bridge separate table regions if clustered.
            // They are excluded from adjacency but added back to each cluster they
            // overlap, so grid detection still sees their edges.
            var heights = pageRects.Select(r => r.H).OrderBy(h => h, FloatTotalOrder.Instance).ToList();
            var medianHeight = heights[heights.Count / 2];
            var heightThreshold = medianHeight * 20.0f;
            var isPageBg = pageRects
                .Select(r => r.X < 5.0f && r.Y < 5.0f && r.H > heightThreshold)
                .ToList();
            if (isPageBg.Any(b => b))
            {
                Log.Debug(Module, () =>
                    $"page {page}: {isPageBg.Count(b => b)} origin-anchored page-bg rects excluded from clustering");
            }

            var nonBgIndices = Enumerable.Range(0, pageRects.Count).Where(i => !isPageBg[i]).ToList();
            var nonBgRects = nonBgIndices.Select(i => pageRects[i]).ToList();
            var rawClusters = RectGrid.ClusterRects(nonBgRects, 3.0f, 6);

            // Map cluster indices back onto pageRects.
            var clusters = rawClusters
                .Select(cluster => cluster.Select(i => nonBgIndices[i]).ToList())
                .ToList();

            Log.Debug(Module, () => $"page {page}: {clusters.Count} clusters with >= 6 rects");

            var mergeExcludedClusterIds = new HashSet<int>();
            for (var clusterId = 0; clusterId < clusters.Count; clusterId++)
            {
                var groupRects = clusters[clusterId].Select(i => pageRects[i]).ToList();

                // Chart bars are neither table cells nor a hint region — gridding a
                // chart's axis labels scrambles the page. Skip the cluster entirely
                // so it reaches no detector, no merged fallback, and no hint.
                if (IsChartBarCluster(textItems, groupRects, page))
                {
                    // Repeated page fills can dominate the geometry and make a real
                    // shaded-cell table look like a chart. Remove those fills,
                    // re-cluster what remains, and weigh any valid table candidate
                    // as a competing hypothesis before the chart rejection wins.
                    var normalized = WithoutDominantPageBackgrounds(groupRects);
                    Table? normalizedTable = null;
                    if (normalized.Count < groupRects.Count)
                    {
                        foreach (var indices in RectGrid.ClusterRects(normalized, 3.0f, 6))
                        {
                            var candidate = indices.Select(i => normalized[i]).ToList();
                            if (IsChartBarCluster(textItems, candidate, page))
                            {
                                continue;
                            }

                            var found = RectGrid.DetectTableFromRectGroup(textItems, candidate, page)
                                ?? DetectRowStripeTableFromCellRects(textItems, candidate, page);
                            if (found is null || found.Rows.Count < CompetingTableMinRows)
                            {
                                continue;
                            }

                            if (normalizedTable is null
                                || (found.Rows.Count * found.Columns.Count)
                                    > (normalizedTable.Rows.Count * normalizedTable.Columns.Count))
                            {
                                normalizedTable = found;
                            }
                        }
                    }

                    if (normalizedTable is not null)
                    {
                        var accepted = normalizedTable;
                        var groupCount = groupRects.Count;
                        Log.Debug(Module, () =>
                            $"page {page}: chart-like cluster normalized from {groupCount} to {normalized.Count} rects; " +
                            $"accepted {accepted.Rows.Count}x{accepted.Columns.Count} table hypothesis");

                        // The accepted hypothesis rests on normalized geometry. Keep
                        // the original chart-like cluster out of the merged fallback:
                        // reintroducing its repeated page fills can manufacture a
                        // wider candidate that replaces this valid narrow table.
                        mergeExcludedClusterIds.Add(clusterId);
                        tables.Add(accepted);
                        continue;
                    }

                    Log.Debug(Module, () =>
                        $"page {page}: skipping chart-bar cluster ({groupRects.Count} rects)");
                    mergeExcludedClusterIds.Add(clusterId);
                    continue;
                }

                var table = DetectDirectRectTable(textItems, groupRects, page);
                if (table is not null)
                {
                    tables.Add(table);
                    continue;
                }

                var split = RectGrid.SplitWideCluster(groupRects, 15.0f, 6);
                if (split is null)
                {
                    failedClusters.Add(groupRects);
                    continue;
                }

                // The cluster was too wide — retry each half independently.
                var (left, right) = split.Value;
                Log.Debug(Module, () =>
                    $"page {page}: splitting cluster of {groupRects.Count} rects into {left.Count} + {right.Count} at x-gap");

                var splitFound = false;
                foreach (var sub in new[] { left, right })
                {
                    var subTable = RectGrid.DetectTableFromRectGroup(textItems, sub, page)
                        ?? DetectRowStripeTable(textItems, sub, page);
                    if (subTable is not null)
                    {
                        tables.Add(subTable);
                        splitFound = true;
                    }
                }

                if (!splitFound)
                {
                    failedClusters.Add(groupRects);
                }
            }

            // Merged-cluster fallback: when per-cluster attempts produce nothing, or
            // only narrow false positives (≤3 columns from individual column
            // clusters), merge every cluster's rects and try the row-stripe strategy
            // with text-based column detection.
            var onlyNarrow = tables.Count > 0 && tables.All(t => t.Columns.Count <= 3);
            if (tables.Count == 0 || onlyNarrow)
            {
                // Chart clusters stay out of the merge as well.
                var tableClusters = clusters
                    .Where((_, id) => !mergeExcludedClusterIds.Contains(id))
                    .ToList();
                var totalClustered = tableClusters.Sum(c => c.Count);
                if (tableClusters.Count >= 3 && totalClustered >= 50)
                {
                    Log.Debug(Module, () =>
                        $"page {page}: trying merged-cluster fallback ({tableClusters.Count} clusters, " +
                        $"{totalClustered} rects{(onlyNarrow ? ", replacing narrow tables" : string.Empty)})");

                    var allClusterRects = tableClusters
                        .SelectMany(idxs => idxs.Select(i => pageRects[i]))
                        .ToList();
                    var merged = DetectMergedClusterTable(textItems, allClusterRects, page);
                    if (merged is not null)
                    {
                        if (onlyNarrow)
                        {
                            tables.Clear();
                        }

                        tables.Add(merged);
                    }
                }
            }

            // Cell-rect fallback: when every per-cluster attempt failed, try rect
            // Y-edges for rows plus text X-positions for columns on each failed
            // cluster. This covers tables whose cell-background rects never form a
            // clean grid (variable column widths, decoration fills).
            if (tables.Count == 0)
            {
                Log.Debug(Module, () =>
                    $"page {page}: cell-rect fallback: {failedClusters.Count} failed clusters");
                foreach (var fcRects in failedClusters)
                {
                    if (fcRects.Count < 6)
                    {
                        continue;
                    }

                    var table = DetectRowStripeTableFromCellRects(textItems, fcRects, page);
                    if (table is not null)
                    {
                        tables.Add(table);
                    }
                }
            }

            // Row-stripe fallback: when clustering produced no large clusters — row
            // stripes do not overlap, so each is its own cluster of one — try all the
            // page's rects directly. At least 15 rects and 10 result rows are needed
            // to keep decorative fills from passing.
            if (tables.Count == 0 && clusters.Count == 0 && pageRects.Count >= 15)
            {
                var table = DetectRowStripeTable(textItems, pageRects, page);
                if (table is not null)
                {
                    if (table.Rows.Count >= 10)
                    {
                        Log.Debug(Module, () =>
                            $"page {page}: row-stripe fallback succeeded ({pageRects.Count} rects, {table.Rows.Count} rows)");
                        tables.Add(table);
                    }
                    else
                    {
                        Log.Debug(Module, () =>
                            $"page {page}: row-stripe fallback rejected: only {table.Rows.Count} rows");
                    }
                }
            }
        }

        // Stacks of 3–5 boxes never reach DetectStackedBoxTable: the main loop
        // requires clusters of 6+ on a page of 6+. That is a deliberate precision
        // gate — routing smaller clusters through the detector was tried and
        // regressed four pdf-evals documents (striped bullet lists, wrapped
        // regulation text, stats-table columns) while improving nothing. With so
        // few boxes the anti-prose guards have too little signal to discriminate.
        if (tables.Count == 0)
        {
            // With no tables but clusters present, generate XY hint regions from
            // cluster bounding boxes to scope heuristic detection. This covers both
            // large decorative-rect clusters (calendars, forms) and small
            // cell-border clusters on rect-sparse pages.
            var hasFailedClusterHints = false;
            if (pageRects.Count >= 6)
            {
                var clusters = RectGrid.ClusterRects(pageRects, 3.0f, 6);

                // Hints from large clusters (30+ rects, decorative or calendar style).
                foreach (var clusterIndices in clusters)
                {
                    var groupRects = clusterIndices.Select(i => pageRects[i]).ToList();
                    if (groupRects.Count < 30)
                    {
                        continue;
                    }

                    var bbox = BoundingBox(groupRects);
                    var w = bbox.W;
                    var h = bbox.H;
                    if (w is >= 30.0f and <= 400.0f && h is >= 10.0f and <= 400.0f)
                    {
                        Log.Debug(Module, () =>
                            $"page {page}: hint candidate from {groupRects.Count} rects: " +
                            $"x={bbox.Left:F1}..{bbox.Right:F1} y={bbox.Bottom:F1}..{bbox.Top:F1} ({w:F0}×{h:F0})");
                        hintRegions.Add(new RectHintRegion
                        {
                            YTop = bbox.Top,
                            YBottom = bbox.Bottom,
                            XLeft = bbox.Left,
                            XRight = bbox.Right,
                            ClusterRects = [.. groupRects],
                        });
                    }
                }

                // Hints from failed clusters: 6+ rects with a valid bounding box but
                // not enough grid structure — an outer border or a header divider
                // with 2×2 edges. They say WHERE a table is even when the rects do
                // not define its columns.
                foreach (var fcRects in failedClusters)
                {
                    if (fcRects.Count < 6)
                    {
                        continue;
                    }

                    var bbox = BoundingBox(fcRects);
                    var h = bbox.H;
                    var w = bbox.W;

                    const float Padding = 15.0f;
                    var itemsInside = textItems.Count(item =>
                        item.Y >= bbox.Bottom - Padding
                        && item.Y <= bbox.Top + Padding
                        && item.X >= bbox.Left - Padding
                        && item.X <= bbox.Right + Padding);

                    // Height must be at least ~5 rows and less than a full page.
                    // Width caps at 500pt normally, wider for large clusters (30+
                    // rects) that are clearly structured.
                    var maxW = fcRects.Count >= 30 ? 800.0f : 500.0f;
                    if (h is >= 100.0f and <= 600.0f && w <= maxW && itemsInside >= 6)
                    {
                        Log.Debug(Module, () =>
                            $"page {page}: failed-cluster hint from {fcRects.Count} rects ({itemsInside} items): " +
                            $"x={bbox.Left:F1}..{bbox.Right:F1} y={bbox.Bottom:F1}..{bbox.Top:F1} ({w:F0}×{h:F0})");
                        hintRegions.Add(new RectHintRegion
                        {
                            YTop = bbox.Top,
                            YBottom = bbox.Bottom,
                            XLeft = bbox.Left,
                            XRight = bbox.Right,
                            ClusterRects = [.. fcRects],
                        });
                        hasFailedClusterHints = true;
                    }
                }

                hintRegions = MergeOverlappingHints(hintRegions);

                // Several hint regions confirm a multi-zone layout (calendars,
                // forms). A lone hint is more likely a decorative cluster that would
                // interfere with full-page heuristic detection. Failed-cluster hints
                // are the exception: rect presence confirms a real table boundary, so
                // one of those is meaningful on its own.
                if (hintRegions.Count < 2 && !hasFailedClusterHints)
                {
                    hintRegions.Clear();
                }

                if (hintRegions.Count > 0)
                {
                    Log.Debug(Module, () =>
                        $"page {page}: {hintRegions.Count} XY hint regions from failed clusters");
                }
            }

            // On rect-sparse pages (6 or fewer rects), a few cell-border rects may
            // still define the table region even though they cannot form a grid —
            // horizontal row borders with no column dividers, say. Extract a hint so
            // the heuristic detector can be scoped to that area.
            if (hintRegions.Count == 0 && pageRects.Count is >= 4 and <= 6)
            {
                foreach (var clusterIndices in RectGrid.ClusterRects(pageRects, 3.0f, 4))
                {
                    var groupRects = clusterIndices.Select(i => pageRects[i]).ToList();
                    var hint = ExtractHintRegion(groupRects);
                    if (hint is not null)
                    {
                        Log.Debug(Module, () =>
                            $"page {page}: hint region y={hint.YBottom:F1}..{hint.YTop:F1} x={hint.XLeft:F1}..{hint.XRight:F1}");
                        hintRegions.Add(hint);
                    }
                }
            }
        }

        return (tables, hintRegions);
    }

    /// <summary>
    /// Detects a single-column table drawn as a vertical stack of boxes, each
    /// holding one short line of text — the framework and step lists on
    /// slide-style pages. The grid path rejects these because one column yields
    /// only two x-edges, so without this the rows would flow into the surrounding
    /// prose as a run-on paragraph.
    /// </summary>
    private static Table? DetectStackedBoxTable(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page)
    {
        // Candidate row boxes: one text line tall, substantial width.
        var cands = groupRects
            .Where(r => r.W >= 100.0f && r.H is >= 8.0f and <= 80.0f)
            .ToList();

        // The row boxes form the largest family of same-width, x-aligned rects;
        // backgrounds and decor have their own geometry and stay out.
        var boxes = new List<RectBox>();
        foreach (var anchor in cands)
        {
            var family = cands
                .Where(r => MathF.Abs(r.X - anchor.X) <= 12.0f
                    && MathF.Abs(r.W - anchor.W) <= anchor.W * 0.15f
                    && MathF.Abs(r.H - anchor.H) <= anchor.H * 0.3f)
                .ToList();
            if (family.Count > boxes.Count)
            {
                boxes = family;
            }
        }

        if (boxes.Count < 3)
        {
            return null;
        }

        // A box flanked at its own y-level — by another rect, or by text outside
        // the family's x-range — is one column of a wider structure. Leave those to
        // the grid and cell-rect paths rather than collapsing them to one column.
        var flanked = boxes.Count(b =>
        {
            var rectSibling = groupRects.Any(o =>
            {
                var yOverlap = MathF.Min(b.Top, o.Top) - MathF.Max(b.Bottom, o.Bottom);
                return o.H >= 8.0f
                    && yOverlap > b.H * 0.5f
                    && (o.Right <= b.Left + 2.0f || o.Left >= b.Right - 2.0f)
                    && o.W >= 30.0f;
            });
            var textSibling = items.Any(it =>
            {
                var cx = it.X + (it.Width / 2.0f);
                return it.Page == page
                    && it.Y >= b.Bottom - 2.0f
                    && it.Y <= b.Top + 2.0f
                    && (cx < b.Left - 5.0f || cx > b.Right + 5.0f)
                    && it.Width >= 10.0f;
            });
            return rectSibling || textSibling;
        });

        if (flanked * 3 >= boxes.Count)
        {
            Log.Debug(Module, () =>
                $"  stacked-box rejected: {flanked}/{boxes.Count} boxes flanked by rects or text");
            return null;
        }

        // Top to bottom, i.e. descending y.
        boxes.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));

        // Merge duplicates — a border and fill pair draws the same box twice — then
        // require a clean vertical stack with no overlap beyond a small tolerance.
        var deduped = new List<RectBox>();
        foreach (var b in boxes)
        {
            if (deduped.Count > 0
                && MathF.Abs(deduped[^1].Y - b.Y) <= 3.0f
                && MathF.Abs(deduped[^1].H - b.H) <= 6.0f)
            {
                continue;
            }

            deduped.Add(b);
        }

        boxes = deduped;
        if (boxes.Count < 3)
        {
            return null;
        }

        for (var i = 0; i + 1 < boxes.Count; i++)
        {
            var upper = boxes[i];
            var lower = boxes[i + 1];
            var upperBottom = upper.Bottom;
            var lowerTop = lower.Top;
            if (lowerTop > upperBottom + 4.0f)
            {
                // Vertical overlap — not a stack.
                return null;
            }

            if (upperBottom - lowerTop > MathF.Max(upper.H, lower.H))
            {
                // A gap larger than a row means unrelated boxes.
                return null;
            }
        }

        // Assign items to boxes. Every box needs text, and cells must stay short:
        // prose paragraphs inside stacked frames are page decor, not a table.
        var cells = new List<List<string>>(boxes.Count);
        var itemIndices = new List<int>();
        var multiRunBoxes = 0;

        foreach (var b in boxes)
        {
            var inBox = items
                .Select((it, i) => (Index: i, Item: it))
                .Where(p => p.Item.Page == page
                    && p.Item.Y >= b.Bottom - 2.0f
                    && p.Item.Y <= b.Top + 2.0f
                    && p.Item.X + (p.Item.Width / 2.0f) >= b.Left
                    && p.Item.X + (p.Item.Width / 2.0f) <= b.Right)
                .ToList();

            if (inBox.Count == 0)
            {
                return null;
            }

            inBox.Sort((a, c) =>
            {
                var cmp = c.Item.Y.CompareTo(a.Item.Y);
                return cmp != 0 ? cmp : a.Item.X.CompareTo(c.Item.X);
            });

            // Count horizontally separated text runs inside the box. A single list
            // row flows as one run; two or more runs across most boxes means
            // multi-column content — striped prose or a real grid — that must not
            // collapse into a one-column table. Same-baseline only: boxed display
            // and diagram rows legitimately scatter segments across mixed baselines,
            // and those must stay one row.
            var runs = 1;
            for (var i = 0; i + 1 < inBox.Count; i++)
            {
                var prev = inBox[i].Item;
                var item = inBox[i + 1].Item;
                if (MathF.Abs(prev.Y - item.Y) <= 2.0f && item.X - (prev.X + prev.Width) > 15.0f)
                {
                    runs++;
                }
            }

            if (runs >= 2)
            {
                multiRunBoxes++;
            }

            var text = string.Join(
                ' ',
                inBox.Select(p => p.Item.Text.Trim()).Where(t => t.Length > 0));
            if (text.Length == 0 || TextUtils.CharCount(text) > 120)
            {
                return null;
            }

            itemIndices.AddRange(inBox.Select(p => p.Index));
            cells.Add([text]);
        }

        if (multiRunBoxes * 2 >= boxes.Count)
        {
            Log.Debug(Module, () =>
                $"  stacked-box rejected: {multiRunBoxes}/{boxes.Count} boxes hold multiple text runs");
            return null;
        }

        // Reject prose behind per-line stripe rects: sentence fragments flowing
        // across rows read as long, function-word-dense cells, while genuine
        // list-table rows are short labels and titles.
        var totalChars = cells.Sum(r => TextUtils.CharCount(r[0]));
        var meanChars = totalChars / Math.Max(cells.Count, 1);
        var proseCells = cells.Count(r => HasProseWord(r[0], StackedBoxProseWords));
        if (meanChars > 60 && proseCells * 5 >= cells.Count * 2)
        {
            Log.Debug(Module, () =>
                $"  stacked-box rejected: prose rows (mean {meanChars} chars, prose words {proseCells}/{cells.Count})");
            return null;
        }

        // Sentences wrapping across stripe rects: a row ending in a comma, or a row
        // without terminal punctuation followed by one starting lowercase, is
        // mid-sentence flow rather than list rows. Genuine label and title rows
        // produce none of these, so even a small share disqualifies.
        var continuations = 0;
        for (var i = 0; i + 1 < cells.Count; i++)
        {
            var prev = cells[i][0].TrimEnd();
            var next = cells[i + 1][0].TrimStart();
            var prevOpen = prev.Length == 0 || !".:;!?)\"%".Contains(prev[^1], StringComparison.Ordinal);
            var nextLower = next.Length > 0 && char.IsLower(next[0]);
            if ((prev.Length > 0 && prev[^1] == ',') || (prevOpen && nextLower))
            {
                continuations++;
            }
        }

        if (cells.Count >= 2 && (continuations >= 2 || continuations * 4 >= cells.Count - 1))
        {
            Log.Debug(Module, () =>
                $"  stacked-box rejected: {continuations}/{cells.Count - 1} row pairs continue a sentence");
            return null;
        }

        // Numbered and lettered list items behind decorative stripes stay lists:
        // "1) content...", "(ii) content...", "a. content...".
        var listRows = cells.Count(r => IsListMarker(r[0]));
        if (listRows * 2 >= cells.Count)
        {
            Log.Debug(Module, () =>
                $"  stacked-box rejected: {listRows}/{cells.Count} rows are numbered list items");
            return null;
        }

        Log.Debug(Module, () => $"page {page}: stacked-box table: {cells.Count} single-column rows");

        var columns = new List<float> { boxes[0].X + (boxes[0].W / 2.0f) };
        var rows = boxes.Select(b => b.Y + (b.H / 2.0f)).ToList();
        return Table.Create(columns, rows, cells, itemIndices);
    }

    /// <summary>True when the text opens with a short "1)", "(ii)" or "a." style marker.</summary>
    private static bool IsListMarker(string text)
    {
        var t = text.TrimStart();
        if (t.StartsWith('('))
        {
            t = t[1..];
        }

        var markerLen = 0;
        while (markerLen < t.Length && char.IsAsciiLetterOrDigit(t[markerLen]))
        {
            markerLen++;
        }

        return markerLen is >= 1 and <= 3
            && markerLen < t.Length
            && t[markerLen] is ')' or '.';
    }

    /// <summary>True when any ASCII word in the text, apostrophes included, is a function word.</summary>
    private static bool HasProseWord(string text, string[] words)
    {
        var lower = text.ToLowerInvariant();
        var start = 0;
        for (var i = 0; i <= lower.Length; i++)
        {
            var isSeparator = i == lower.Length || (!char.IsAsciiLetter(lower[i]) && lower[i] != '\'');
            if (!isSeparator)
            {
                continue;
            }

            var word = lower[start..i];
            if (words.Contains(word))
            {
                return true;
            }

            start = i + 1;
        }

        return false;
    }

    /// <summary>
    /// Merges nearby hint regions that share a Y band: substantial Y overlap
    /// (&gt;50%) plus X ranges that overlap or sit within 50pt. This handles
    /// calendar-style layouts where a month zone's decorative rects split into two
    /// or three adjacent clusters with small X gaps. Runs until nothing merges.
    /// </summary>
    private static List<RectHintRegion> MergeOverlappingHints(List<RectHintRegion> hints)
    {
        if (hints.Count <= 1)
        {
            return hints;
        }

        while (true)
        {
            hints.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.XLeft, b.XLeft));
            var merged = new List<RectHintRegion>();
            var anyMerged = false;

            foreach (var hint in hints)
            {
                var didMerge = false;
                foreach (var existing in merged)
                {
                    // Y overlap must exceed half the smaller span.
                    var yOverlap = MathF.Min(existing.YTop, hint.YTop) - MathF.Max(existing.YBottom, hint.YBottom);
                    var yMinSpan = MathF.Min(existing.YTop - existing.YBottom, hint.YTop - hint.YBottom);
                    if (yOverlap <= yMinSpan * 0.5f)
                    {
                        continue;
                    }

                    // X must overlap or be adjacent within 50pt.
                    var xGap = MathF.Max(existing.XLeft, hint.XLeft) - MathF.Min(existing.XRight, hint.XRight);
                    if (xGap >= 50.0f)
                    {
                        continue;
                    }

                    // Never merge past the 400pt maximum hint width.
                    var mergedLeft = MathF.Min(existing.XLeft, hint.XLeft);
                    var mergedRight = MathF.Max(existing.XRight, hint.XRight);
                    if (mergedRight - mergedLeft > 400.0f)
                    {
                        continue;
                    }

                    existing.XLeft = mergedLeft;
                    existing.XRight = mergedRight;
                    existing.YBottom = MathF.Min(existing.YBottom, hint.YBottom);
                    existing.YTop = MathF.Max(existing.YTop, hint.YTop);
                    existing.ClusterRects.AddRange(hint.ClusterRects);
                    didMerge = true;
                    anyMerged = true;
                    break;
                }

                if (!didMerge)
                {
                    merged.Add(new RectHintRegion
                    {
                        YTop = hint.YTop,
                        YBottom = hint.YBottom,
                        XLeft = hint.XLeft,
                        XRight = hint.XRight,
                        ClusterRects = [.. hint.ClusterRects],
                    });
                }
            }

            hints = merged;
            if (!anyMerged)
            {
                return hints;
            }
        }
    }

    /// <summary>
    /// Extracts a hint region from a rect cluster that failed grid validation.
    /// Only small clusters (8 rects or fewer) qualify, where a few cell-border
    /// rects mark a table's row boundaries; large clusters that fail validation
    /// are form-style decoration and typically span the whole page. Oversized
    /// bounding-box rects — more than 4× the median height — are dropped before
    /// the box is computed.
    /// </summary>
    private static RectHintRegion? ExtractHintRegion(IReadOnlyList<RectBox> groupRects)
    {
        if (groupRects.Count is < 2 or > 8)
        {
            return null;
        }

        var heights = groupRects.Select(r => r.H).OrderBy(h => h, FloatTotalOrder.Instance).ToList();
        var medianH = heights[heights.Count / 2];

        var cellRects = groupRects.Where(r => r.H <= medianH * 4.0f).ToList();
        if (cellRects.Count < 2)
        {
            return null;
        }

        var bbox = BoundingBox(cellRects);

        // The region needs meaningful height without spanning an unreasonable area.
        var regionHeight = bbox.H;
        if (regionHeight is < 10.0f or > 300.0f)
        {
            return null;
        }

        return new RectHintRegion
        {
            YTop = bbox.Top,
            YBottom = bbox.Bottom,
            XLeft = bbox.Left,
            XRight = bbox.Right,
            ClusterRects = [],
        };
    }

    /// <summary>Detects a table from alternating full-width row-stripe fills.</summary>
    private static Table? DetectRowStripeTable(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page)
    {
        if (!RectGrid.IsRowStripePattern(groupRects))
        {
            return null;
        }

        Log.Debug(Module, () => $"  trying row-stripe detection ({groupRects.Count} rects)");

        var yEdges = new List<float>();
        foreach (var r in groupRects)
        {
            yEdges.Add(r.Bottom);
            yEdges.Add(r.Top);
        }

        var rowEdges = RectGrid.SnapEdges(yEdges, 6.0f);
        if (rowEdges.Count < 4)
        {
            Log.Debug(Module, () => $"  row-stripe rejected: only {rowEdges.Count} y-edges");
            return null;
        }

        // Top to bottom: highest Y first, as PDF space grows upwards.
        rowEdges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        var yTop = rowEdges[0];
        var yBottom = rowEdges[^1];
        var xLeft = groupRects.Min(r => r.Left);
        var xRight = groupRects.Max(r => r.Right);

        var pageItems = ItemsInBand(items, page, yBottom, yTop, xLeft, xRight);
        if (pageItems.Count == 0)
        {
            return null;
        }

        // Columns come from clustering text X positions. The threshold is lower
        // than FindColumnBoundaries' 25pt floor: the rects already prove this is a
        // table, so narrow columns — a row number and a date 21pt apart — should
        // stay separate.
        var columns = ClusterXPositions(pageItems, 15.0f);
        if (columns.Count < 2)
        {
            Log.Debug(Module, () =>
                $"  row-stripe rejected: only {columns.Count} columns from text clustering");
            return null;
        }

        var colEdges = ColumnCentersToEdges(columns, pageItems);
        var numCols = colEdges.Count - 1;
        var numRows = rowEdges.Count - 1;

        Log.Debug(Module, () =>
            $"  row-stripe grid: {numRows}x{numCols} ({colEdges.Count} col edges, {rowEdges.Count} row edges)");

        var (cells, itemIndices) = RectGrid.AssignItemsToGrid(items, colEdges, rowEdges, page);
        if (itemIndices.Count == 0)
        {
            Log.Debug(Module, "  row-stripe rejected: no items assigned");
            return null;
        }

        var nonEmptyRows = cells.Count(row => row.Any(c => c.Trim().Length > 0));
        if (nonEmptyRows < 2)
        {
            Log.Debug(Module, () => $"  row-stripe rejected: only {nonEmptyRows} non-empty rows");
            return null;
        }

        var totalCells = (float)(numCols * numRows);
        var nonEmptyCells = cells.Sum(row => row.Count(c => c.Trim().Length > 0));
        var contentRatio = nonEmptyCells / totalCells;
        if (contentRatio < 0.40f)
        {
            Log.Debug(Module, () => $"  row-stripe rejected: content ratio {contentRatio:F2} < 0.40");
            return null;
        }

        // Reject any cell with excessive text. Layout background rects — sidebars,
        // headers, section bands — produce "cells" holding paragraphs of body text,
        // while real alternating-stripe data tables keep cell content short.
        // Multi-column tables get more slack, since a description column is common;
        // narrow grids with giant cells are usually layout backgrounds, but only
        // when the row count is small too. A 4-row key/value table with one
        // descriptive column reads as a real table on every other gate.
        var maxCellLen = cells.SelectMany(row => row).Select(TextUtils.ByteLength).DefaultIfEmpty(0).Max();
        var maxAllowed = numCols >= 3 ? 2000 : 500;
        if (maxCellLen > maxAllowed && nonEmptyRows < 4)
        {
            Log.Debug(Module, () =>
                $"  row-stripe rejected: max cell length {maxCellLen} > {maxAllowed} (layout background, {nonEmptyRows} rows)");
            return null;
        }

        // Trim empty outer columns; an empty interior column disqualifies.
        int? firstCol = null;
        int? lastCol = null;
        for (var col = 0; col < numCols; col++)
        {
            if (ColumnHasContent(cells, col))
            {
                firstCol ??= col;
                lastCol = col;
            }
        }

        if (firstCol is not { } fc || lastCol is not { } lc || lc <= fc)
        {
            return null;
        }

        for (var col = fc; col <= lc; col++)
        {
            if (!ColumnHasContent(cells, col))
            {
                var emptyCol = col;
                Log.Debug(Module, () => $"  row-stripe rejected: interior column {emptyCol} is empty");
                return null;
            }
        }

        if (fc > 0 || lc < numCols - 1)
        {
            colEdges = colEdges[fc..(lc + 2)];
            cells = cells.Select(row => row[fc..(lc + 1)]).ToList();
        }

        numCols = colEdges.Count - 1;

        if (RowStripeIsSparseProseOutline(cells))
        {
            Log.Debug(Module, "  row-stripe rejected: sparse outline/prose continuation shape");
            return null;
        }

        if (HasDominantProseCell(cells))
        {
            Log.Debug(Module, "  row-stripe rejected: dominant prose cell (chart/figure region over body text)");
            return null;
        }

        var columnCenters = EdgeCenters(colEdges, numCols);
        var rowCenters = EdgeCenters(rowEdges, numRows);

        Log.Debug(Module, () =>
            $"  row-stripe table accepted: {numRows}x{numCols}, {contentRatio * 100.0f:F0}% density");

        return Table.Create(columnCenters, rowCenters, cells, itemIndices);
    }

    /// <summary>True when any row carries text in the given column.</summary>
    private static bool ColumnHasContent(List<List<string>> cells, int col) =>
        cells.Any(row => col < row.Count && row[col].Trim().Length > 0);

    /// <summary>The midpoints of the first <paramref name="count"/> spans between edges.</summary>
    private static List<float> EdgeCenters(List<float> edges, int count)
    {
        var centers = new List<float>(count);
        for (var i = 0; i < count; i++)
        {
            centers.Add((edges[i] + edges[i + 1]) / 2.0f);
        }

        return centers;
    }

    /// <summary>Gathers page items that fall inside the given band, with the usual slack.</summary>
    private static List<(int Index, TextItem Item)> ItemsInBand(
        IReadOnlyList<TextItem> items,
        uint page,
        float yBottom,
        float yTop,
        float xLeft,
        float xRight)
    {
        var result = new List<(int, TextItem)>();
        for (var i = 0; i < items.Count; i++)
        {
            var item = items[i];
            if (item.Page == page
                && item.Y >= yBottom - 2.0f
                && item.Y <= yTop + 2.0f
                && item.X >= xLeft - 5.0f
                && item.X + item.Width <= xRight + 5.0f)
            {
                result.Add((i, item));
            }
        }

        return result;
    }

    /// <summary>
    /// Turns column centers into edges: the leftmost item start, the midpoints
    /// between adjacent centers, then the rightmost item end — each with a little
    /// padding.
    /// </summary>
    private static List<float> ColumnCentersToEdges(
        List<float> columns,
        List<(int Index, TextItem Item)> pageItems)
    {
        var edges = new List<float>(columns.Count + 1) { pageItems.Min(p => p.Item.X) - 5.0f };
        for (var i = 0; i + 1 < columns.Count; i++)
        {
            edges.Add((columns[i] + columns[i + 1]) / 2.0f);
        }

        edges.Add(pageItems.Max(p => p.Item.X + p.Item.Width) + 5.0f);
        return edges;
    }

    /// <summary>
    /// Detects a grid that swallowed body text instead of tabular data. Charts —
    /// bar graphs, axis gridlines — emit fields of drawing rects that can pass the
    /// row-stripe shape test, and the resulting "table" then captures the page's
    /// prose. The signature is one cell holding a whole paragraph: at least 60
    /// words and at least a third of every word in the table.
    /// </summary>
    /// <remarks>
    /// There is deliberately no row-count exemption. A small table whose single
    /// long cell dominates its word count is indistinguishable by content from a
    /// phantom grid over body text, and across the regression corpora every such
    /// grid has been swallowed prose, never a real note table. The costs are
    /// asymmetric too: rejecting a real table degrades it to readable prose, while
    /// accepting a phantom scrambles the page into Y-interleaved cells. Larger
    /// legitimate tables stay safe because the one-third threshold scales with
    /// table size.
    /// </remarks>
    private static bool HasDominantProseCell(List<List<string>> cells)
    {
        var totalWords = 0;
        var maxCellWords = 0;
        foreach (var row in cells)
        {
            foreach (var cell in row)
            {
                var words = cell.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;
                totalWords += words;
                maxCellWords = Math.Max(maxCellWords, words);
            }
        }

        return maxCellWords >= 60 && maxCellWords * 3 >= totalWords;
    }

    /// <summary>
    /// True for the two-column shape an outline of prose continuations produces: a
    /// sparse marker column beside a dense column of long text, with most rows
    /// leaving the marker blank.
    /// </summary>
    private static bool RowStripeIsSparseProseOutline(List<List<string>> cells)
    {
        if (cells.Count == 0)
        {
            return false;
        }

        var numCols = cells[0].Count;
        if (numCols != 2 || cells.Count < 4)
        {
            return false;
        }

        var nonEmptyRows = cells.Count(row => row.Any(cell => cell.Trim().Length > 0));
        if (nonEmptyRows < 4)
        {
            return false;
        }

        var colCounts = new int[2];
        foreach (var row in cells)
        {
            for (var idx = 0; idx < row.Count && idx < 2; idx++)
            {
                if (row[idx].Trim().Length > 0)
                {
                    colCounts[idx]++;
                }
            }
        }

        var (sparseCol, denseCol) = colCounts[0] <= colCounts[1] ? (0, 1) : (1, 0);
        var sparseCount = colCounts[sparseCol];
        var denseCount = colCounts[denseCol];
        if (sparseCount * 2 >= nonEmptyRows || denseCount * 3 < nonEmptyRows * 2)
        {
            return false;
        }

        var blankSparseDenseRows = cells.Count(row =>
            row[sparseCol].Trim().Length == 0 && row[denseCol].Trim().Length > 0);
        if (blankSparseDenseRows * 2 < nonEmptyRows)
        {
            return false;
        }

        var longDenseCells = cells.Count(row =>
            row[denseCol].Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length >= 6);
        return longDenseCells * 2 >= denseCount;
    }

    /// <summary>
    /// Removes repeated page-scale fills from a chart-like cluster so the actual
    /// cell and bar geometry can be judged on its own.
    /// </summary>
    private static List<RectBox> WithoutDominantPageBackgrounds(IReadOnlyList<RectBox> rects)
    {
        var xMax = 0.0f;
        var yMax = 0.0f;
        foreach (var r in rects)
        {
            xMax = MathF.Max(xMax, r.Right);
            yMax = MathF.Max(yMax, r.Top);
        }

        bool IsPageScale(RectBox r) =>
            r.X < 5.0f && r.Y < 5.0f && r.W >= xMax * 0.9f && r.H >= yMax * 0.9f;

        return rects.Count(IsPageScale) < DominantPageBackgroundMinRepetitions
            ? [.. rects]
            : rects.Where(r => !IsPageScale(r)).ToList();
    }

    /// <summary>
    /// The chart-bar signature: three or more rects sharing an aligned bottom edge
    /// (the axis), similar in width (bars) but strongly varying in height
    /// (data-driven), each holding at most a single numeric data label. Bar charts
    /// drawn as filled rects would otherwise read as cell rects and grid their
    /// axis labels into a phantom table. The check runs mirrored to catch
    /// horizontal bar charts too.
    /// </summary>
    private static bool IsChartBarCluster(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page)
    {
        bool NumericOrEmpty(RectBox r)
        {
            // Any number of numeric data labels is chart-like; a single run of word
            // text inside means a table cell.
            return items
                .Where(it =>
                {
                    var cx = it.X + (it.Width / 2.0f);
                    return it.Page == page && cx >= r.Left && cx <= r.Right && it.Y >= r.Bottom && it.Y <= r.Top;
                })
                .All(it =>
                {
                    var t = it.Text.Trim();
                    var data = t.Count(c => char.IsAsciiDigit(c) || ",.%-".Contains(c, StringComparison.Ordinal));
                    return t.Length == 0 || data * 2 >= TextUtils.CharCount(t);
                });
        }

        // Bars: the dominant equal-breadth family, arranged in two or more spaced
        // columns — an inter-column gap of at least half a bar breadth, since table
        // cell rects touch — with data-driven length variation, where checkbox and
        // cell grids stay uniform. Running the predicate mirrored catches
        // horizontal bar charts.
        bool BarFamily(
            Func<RectBox, float> pos,
            Func<RectBox, float> breadth,
            Func<RectBox, float> length,
            Func<RectBox, float> along)
        {
            foreach (var anchor in groupRects)
            {
                var bw = breadth(anchor);
                if (bw <= 0.0f)
                {
                    continue;
                }

                var family = groupRects
                    .Where(r => MathF.Abs(breadth(r) - bw) <= MathF.Max(bw * 0.1f, 2.0f)
                        && length(r) > 0.0f
                        && length(r) < bw * 20.0f)
                    .ToList();
                if (family.Count < 4)
                {
                    continue;
                }

                // Distinct positions along the axis are the bar columns.
                var positions = new List<float>();
                foreach (var r in family)
                {
                    var p = pos(r);
                    if (!positions.Any(q => MathF.Abs(q - p) <= 2.0f))
                    {
                        positions.Add(p);
                    }
                }

                if (positions.Count < 2)
                {
                    continue;
                }

                positions.Sort(FloatTotalOrder.Instance);
                var minGap = float.PositiveInfinity;
                for (var i = 0; i + 1 < positions.Count; i++)
                {
                    minGap = MathF.Min(minGap, positions[i + 1] - positions[i] - bw);
                }

                if (minGap < bw * 0.5f)
                {
                    continue;
                }

                // Data-driven variation along the bar direction.
                var lenMin = family.Min(length);
                var lenMax = family.Max(length);
                if (lenMax < lenMin * 1.3f)
                {
                    continue;
                }

                // Grid rows disguise themselves as bars: a table's cell rects have
                // same-position, same-length partners in other columns, since row
                // heights are uniform. Chart segments start where the previous datum
                // ended, so their extents rarely pair up across positions.
                var matched = family.Count(r => family.Any(s =>
                    MathF.Abs(pos(s) - pos(r)) > 2.0f
                    && MathF.Abs(along(s) - along(r)) <= 3.0f
                    && MathF.Abs(length(s) - length(r)) <= 3.0f));
                if (matched * 5 >= family.Count * 3)
                {
                    continue;
                }

                if (family.Count(NumericOrEmpty) * 3 >= family.Count * 2)
                {
                    return true;
                }
            }

            return false;
        }

        // Vertical bars: position and breadth are x and width, length is height,
        // along is y. Horizontal bars mirror that.
        return BarFamily(r => r.X, r => r.W, r => r.H, r => r.Y)
            || BarFamily(r => r.Y, r => r.H, r => r.W, r => r.X);
    }

    /// <summary>
    /// Detects a table from cell-background rects that failed grid detection,
    /// using rect Y-edges for rows and text X-position clustering for columns.
    /// This covers tables whose cell backgrounds never form a clean X-edge grid:
    /// variable column widths, decorative fills.
    /// </summary>
    private static Table? DetectRowStripeTableFromCellRects(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> groupRects,
        uint page)
    {
        if (groupRects.Count < 6)
        {
            return null;
        }

        var yVals = new List<float>();
        foreach (var r in groupRects)
        {
            yVals.Add(r.Bottom);
            yVals.Add(r.Top);
        }

        var yEdges = RectGrid.SnapEdges(yVals, 6.0f);

        List<float> rowEdges;
        if (yEdges.Count >= 4)
        {
            rowEdges = yEdges;
            rowEdges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));
        }
        else
        {
            // Rect Y-edges give no row structure: scope items by the rect bounding
            // box and derive rows from text Y-positions instead.
            var yMin = yEdges.Count > 0 ? yEdges[0] : 0.0f;
            var yMax = yEdges.Count > 0 ? yEdges[^1] : 0.0f;
            var xMin = groupRects.Count > 0 ? groupRects.Min(r => r.Left) : 0.0f;
            var xMax = groupRects.Count > 0 ? groupRects.Max(r => r.Right) : 0.0f;

            var regionItems = items
                .Where(i => i.Page == page
                    && i.Y >= yMin - 5.0f
                    && i.Y <= yMax + 5.0f
                    && i.X >= xMin - 5.0f
                    && i.X <= xMax + 5.0f)
                .ToList();
            if (regionItems.Count < 4)
            {
                return null;
            }

            // Cluster Y positions with the median font height as the threshold.
            var hs = regionItems.Select(i => i.Height).OrderBy(h => h, FloatTotalOrder.Instance).ToList();
            var medianItemH = hs[hs.Count / 2];
            var ys = regionItems.Select(i => i.Y).OrderByDescending(y => y, FloatTotalOrder.Instance).ToList();

            var edges = new List<float>();
            var threshold = medianItemH * 0.8f;
            var clusterSum = ys[0];
            var clusterCount = 1.0f;
            foreach (var y in ys.Skip(1))
            {
                if (MathF.Abs((clusterSum / clusterCount) - y) > threshold)
                {
                    var c = clusterSum / clusterCount;
                    edges.Add(c + (medianItemH * 0.5f));
                    edges.Add(c - (medianItemH * 0.5f));
                    clusterSum = y;
                    clusterCount = 1.0f;
                }
                else
                {
                    clusterSum += y;
                    clusterCount += 1.0f;
                }
            }

            var last = clusterSum / clusterCount;
            edges.Add(last + (medianItemH * 0.5f));
            edges.Add(last - (medianItemH * 0.5f));

            edges = RectGrid.SnapEdges(edges, 3.0f);
            edges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));
            if (edges.Count < 4)
            {
                return null;
            }

            rowEdges = edges;
        }

        // Bounding box from the rects that are not full-page.
        var heights = groupRects.Select(r => r.H).OrderBy(h => h, FloatTotalOrder.Instance).ToList();
        var medianH = heights[heights.Count / 2];
        var contentRects = groupRects.Where(r => r.H < medianH * 10.0f).ToList();
        if (contentRects.Count == 0)
        {
            return null;
        }

        var xLeft = contentRects.Min(r => r.Left);
        var xRight = contentRects.Max(r => r.Right);
        var yTop = rowEdges[0];
        var yBottom = rowEdges[^1];

        var pageItems = ItemsInBand(items, page, yBottom, yTop, xLeft, xRight);
        if (pageItems.Count == 0)
        {
            return null;
        }

        // Columns come from text X-position clustering, but rect X-edges win when
        // they already give a tighter scaffold. Some PDFs draw only the row-index
        // cells in the body plus a full header row; that is not dense enough for
        // TryBuildGrid, yet the header rects still define the real columns. Text
        // starts inside wide cells would otherwise split the table into spurious
        // sub-columns.
        var columns = ClusterXPositions(pageItems, 15.0f);
        List<float>? textColEdges = columns.Count >= 2 ? ColumnCentersToEdges(columns, pageItems) : null;

        List<float>? rectColEdges = null;
        {
            var xValues = new List<float>(contentRects.Count * 2);
            foreach (var r in contentRects)
            {
                xValues.Add(r.Left);
                xValues.Add(r.Right);
            }

            var edges = RectGrid.SnapEdges(xValues, 6.0f);
            edges.Sort(FloatTotalOrder.Instance);
            if (edges.Count is >= 3 and <= 26)
            {
                rectColEdges = edges;
            }
        }

        // For wired-grid tables whose header text is centered or right-aligned but
        // whose data is left-aligned, ClusterXPositions can drop the header-only
        // x-cluster in its singleton-filter pass and merge adjacent data clusters
        // whose gap is below threshold, losing a column. Rect borders are ground
        // truth there — but only when every rect column actually holds text.
        // Decorative or background rects (prose laid out in a frame, cell fills with
        // extra borders) can yield more rect columns than the text supports, and
        // preferring rects then splits a logical column into spurious sub-columns.
        var rectColsMatchText = false;
        if (rectColEdges is { Count: >= 4 } rectEdgesCheck)
        {
            var numRectCols = rectEdgesCheck.Count - 1;
            var colItemCounts = new int[numRectCols];
            foreach (var (_, item) in pageItems)
            {
                var cx = item.X + (item.Width / 2.0f);
                for (var c = 0; c < numRectCols; c++)
                {
                    if (cx >= rectEdgesCheck[c] - 2.0f && cx <= rectEdgesCheck[c + 1] + 2.0f)
                    {
                        colItemCounts[c]++;
                        break;
                    }
                }
            }

            // Every rect column must hold multiple text items. A rect column with no
            // or only one item is decorative, or the rect grid found a spurious
            // column the data does not need; the text-cluster preference is safer.
            rectColsMatchText = colItemCounts.All(n => n >= 2);
        }

        List<float> colEdges;
        bool columnsFromText;
        if (rectColEdges is not null && rectColsMatchText)
        {
            var textCols = textColEdges is null ? -1 : textColEdges.Count - 1;
            Log.Debug(Module, () =>
                $"  cell-rect using {rectColEdges.Count - 1} rect-derived columns " +
                $"(text clusters: {textCols}; rect cols well-distributed)");
            colEdges = rectColEdges;
            columnsFromText = false;
        }
        else if (rectColEdges is not null && textColEdges is not null && rectColEdges.Count <= textColEdges.Count)
        {
            Log.Debug(Module, () =>
                $"  cell-rect using {rectColEdges.Count - 1} rect-derived columns over {textColEdges.Count - 1} text clusters");
            colEdges = rectColEdges;
            columnsFromText = false;
        }
        else if (textColEdges is not null)
        {
            colEdges = textColEdges;
            columnsFromText = true;
        }
        else if (rectColEdges is not null)
        {
            colEdges = rectColEdges;
            columnsFromText = false;
        }
        else
        {
            Log.Debug(Module, () =>
                $"  cell-rect rejected: only {columns.Count} columns from text clustering");
            return null;
        }

        if (colEdges.Count < 3)
        {
            return null;
        }

        var numCols = colEdges.Count - 1;

        Log.Debug(Module, () =>
            $"  cell-rect table: {rowEdges.Count - 1}x{numCols} from {groupRects.Count} rects, {pageItems.Count} items");

        var (cells, itemIndices) = RectGrid.AssignItemsToGrid(items, colEdges, rowEdges, page);
        if (itemIndices.Count == 0)
        {
            return null;
        }

        var (collapsedCells, collapsedRowEdges, collapsedRows) =
            CollapseMultilineDescriptionRows(cells, rowEdges, colEdges);
        var hasWrappedDescriptionRows = collapsedRows > 0;
        cells = collapsedCells;
        rowEdges = collapsedRowEdges;
        if (collapsedRows > 0)
        {
            Log.Debug(Module, () => $"  cell-rect collapsed {collapsedRows} wrapped description rows");
        }

        var nonEmptyRows = cells.Count(row => row.Any(c => c.Trim().Length > 0));
        if (nonEmptyRows < 2)
        {
            Log.Debug(Module, () => $"  cell-rect rejected: only {nonEmptyRows} non-empty rows");
            return null;
        }

        var numRows = cells.Count;
        var totalCells = (float)(numCols * numRows);
        var nonEmptyCells = cells.Sum(row => row.Count(c => c.Trim().Length > 0));
        var density = totalCells > 0.0f ? nonEmptyCells / totalCells : 0.0f;
        if (density < 0.25f)
        {
            Log.Debug(Module, () => $"  cell-rect rejected: density {density * 100.0f:F0}% < 25%");
            return null;
        }

        // Reject paragraph-length cells: typically layout backgrounds — sidebars,
        // banners — where a single big rectangle holds a wall of prose. Spare
        // multi-row key/value tables whose value column is a multi-bullet
        // description pass every other gate and should not die on cell length.
        var maxCellLen = cells.SelectMany(row => row).Select(TextUtils.ByteLength).DefaultIfEmpty(0).Max();
        if (maxCellLen > 500 && nonEmptyRows < 4)
        {
            Log.Debug(Module, () =>
                $"  cell-rect rejected: max cell length {maxCellLen} > 500 ({nonEmptyRows} rows, layout background)");
            return null;
        }

        // Reject wildly disproportionate grids, e.g. 68x6 from decorative rects.
        if (numRows > 20 && numCols < 4)
        {
            Log.Debug(Module, () => $"  cell-rect rejected: disproportionate grid {numRows}x{numCols}");
            return null;
        }

        // Reject "tables" that are really prose in a framed region. Columns here
        // come from text X-position clustering, and when prose wraps inside a
        // bounding-box rect — chat-transcript figures, two-column legal text in
        // forms — the word-boundary gaps cluster into spurious columns and the
        // resulting cells hold sentence fragments riddled with function words.
        //
        // This applies at any column count of 2 or more. The 2-column case is the
        // bite: a paragraph wrapped into two justified columns produces the same
        // surface signal as a real label/value table in the well-distributed check
        // (both columns populated), so telling them apart needs content evidence.
        if (numCols >= 2)
        {
            var proseCells = 0;
            var counted = 0;
            var totalChars = 0;
            foreach (var row in cells)
            {
                foreach (var cell in row)
                {
                    var t = cell.Trim();
                    if (t.Length == 0)
                    {
                        continue;
                    }

                    counted++;
                    totalChars += TextUtils.CharCount(t);
                    if (HasProseWord(t, CellRectProseWords))
                    {
                        proseCells++;
                    }
                }
            }

            if (counted > 0 && proseCells * 5 >= counted)
            {
                // (a) Long-cell content, which overrides the well-distributed
                // relaxation. Prose-in-a-frame averages ~70–100 chars per non-empty
                // cell (sentence fragments); real data tables sit under 30, rarely up
                // to ~55 for descriptive four-column tables. The 65-char threshold
                // separates them cleanly on the observed fixtures — accessory_building
                // prose at 74, upstage data at 53, greencomp at 20. The 2-column
                // prose case populates both columns and so passes well-distributed,
                // which makes mean cell length the discriminator.
                const int ProseMeanCharThreshold = 65;
                var meanChars = totalChars / counted;
                if (meanChars > ProseMeanCharThreshold && !hasWrappedDescriptionRows)
                {
                    Log.Debug(Module, () =>
                        $"  cell-rect rejected: prose-in-frame, mean non-empty cell {meanChars} chars > " +
                        $"{ProseMeanCharThreshold} (prose words {proseCells}/{counted})");
                    return null;
                }

                if (meanChars > ProseMeanCharThreshold)
                {
                    Log.Debug(Module, () =>
                        $"  cell-rect prose check relaxed: wrapped description rows, mean {meanChars} chars " +
                        $"(prose words {proseCells}/{counted})");
                }

                // (b) Two text-derived columns are not enough vector evidence once the
                // content looks prose-like. Real 2-column rect tables still pass, since
                // their column scaffold comes from drawn cell geometry.
                if (columnsFromText && numCols == 2)
                {
                    Log.Debug(Module, () =>
                        $"  cell-rect rejected: prose-in-frame with text-derived 2-col scaffold " +
                        $"(mean {meanChars} chars, prose words {proseCells}/{counted})");
                    return null;
                }

                // (c) Well-distributed columns: at least 75% hold two or more
                // non-empty cells. That catches prose-paragraph-as-many-columns while
                // admitting real label/value/description/benefit tables.
                var filledCols = 0;
                for (var c = 0; c < numCols; c++)
                {
                    var col = c;
                    if (cells.Count(row => col < row.Count && row[col].Trim().Length > 0) >= 2)
                    {
                        filledCols++;
                    }
                }

                if (filledCols * 4 < numCols * 3)
                {
                    Log.Debug(Module, () =>
                        $"  cell-rect rejected: {proseCells}/{counted} cells contain prose function words — " +
                        $"likely prose ({filledCols}/{numCols} cols filled, mean {meanChars} chars)");
                    return null;
                }

                Log.Debug(Module, () =>
                    $"  cell-rect prose check relaxed: {filledCols}/{numCols} cols filled, mean {meanChars} chars — " +
                    "table-with-description-col");
            }
        }

        var columnCenters = EdgeCenters(colEdges, numCols);
        var rowCenters = EdgeCenters(rowEdges, numRows);

        Log.Debug(Module, () =>
            $"  cell-rect table accepted: {numRows}x{numCols}, {nonEmptyCells / totalCells * 100.0f:F0}% density");

        return Table.Create(columnCenters, rowCenters, cells, itemIndices);
    }

    /// <summary>
    /// Merges wrapped description-line bands back into their visual data rows.
    /// Some Word and PDF exports draw enough rectangle geometry to prove a table
    /// exists but expose Y bands per wrapped text line instead of per cell row. In
    /// the common mapping-table shape a narrow row-label column precedes one wide
    /// description column, and wrapped continuation bands carry content only in
    /// that wide column. Only that high-confidence shape is merged, so framed
    /// prose still falls through to the prose guards.
    /// </summary>
    private static (List<List<string>> Cells, List<float> RowEdges, int WrappedRows)
        CollapseMultilineDescriptionRows(
            List<List<string>> cells,
            List<float> rowEdges,
            List<float> colEdges)
    {
        var numRows = cells.Count;
        var numCols = Math.Max(colEdges.Count - 1, 0);
        if (numRows < 3 || numCols < 3 || rowEdges.Count != numRows + 1)
        {
            return (cells, rowEdges, 0);
        }

        var tableWidth = colEdges[numCols] - colEdges[0];
        if (tableWidth <= 0.0f)
        {
            return (cells, rowEdges, 0);
        }

        var descriptionCol = 0;
        var descriptionWidth = float.NegativeInfinity;
        for (var c = 0; c < numCols; c++)
        {
            var width = colEdges[c + 1] - colEdges[c];
            if (FloatTotalOrder.Instance.Compare(width, descriptionWidth) >= 0)
            {
                descriptionCol = c;
                descriptionWidth = width;
            }
        }

        // A preceding row-label column is required. Without it — a prose frame split
        // into text-start columns, say — "one populated wide column" is not enough
        // evidence to find visual row starts safely.
        if (descriptionCol == 0 || descriptionWidth < tableWidth * 0.35f)
        {
            return (cells, rowEdges, 0);
        }

        bool RowHasLeftLabel(List<string> row) =>
            row.Take(descriptionCol).Any(cell => cell.Trim().Length > 0);

        var labeledRows = cells.Count(RowHasLeftLabel);
        if (labeledRows < 2)
        {
            return (cells, rowEdges, 0);
        }

        var mergedRows = 0;
        var wrappedDescriptionRows = 0;
        var newCells = new List<List<string>>(numRows);
        var newEdges = new List<float>(rowEdges.Count) { rowEdges[0] };

        for (var rowIdx = 0; rowIdx < cells.Count; rowIdx++)
        {
            var row = cells[rowIdx];
            var descText = descriptionCol < row.Count ? row[descriptionCol].Trim() : string.Empty;
            var leftLabel = RowHasLeftLabel(row);
            var nonDescNonEmpty = row.Where((cell, col) => col != descriptionCol && cell.Trim().Length > 0).Count();

            // Wrapped continuation bands hold only description-column text; the
            // preceding label column is empty because the visual row's label cell
            // spans the whole wrapped block.
            var isDescriptionContinuation = rowIdx > 0
                && descText.Length > 0
                && !leftLabel
                && nonDescNonEmpty == 0
                && newCells.Count > 0;

            // Header cells are often split as "Controls" / "Version" in the first
            // column while the other header labels sit on the first band.
            var onlyFirstCol = row.Where((cell, col) => col != 0 && cell.Trim().Length > 0).Take(1).Count() == 0;
            var isHeaderContinuation = rowIdx > 0
                && onlyFirstCol
                && row.Count > 0
                && row[0].Trim().Length > 0
                && TextUtils.CharCount(row[0]) <= 24
                && newCells.Count > 0
                && newCells[^1].Count(c => c.Trim().Length > 0) >= 2;

            if (isDescriptionContinuation || isHeaderContinuation)
            {
                if (newCells.Count > 0)
                {
                    var prev = newCells[^1];
                    for (var col = 0; col < row.Count && col < prev.Count; col++)
                    {
                        var text = row[col].Trim();
                        if (text.Length == 0)
                        {
                            continue;
                        }

                        prev[col] = prev[col].Trim().Length > 0 ? prev[col] + " " + text : prev[col] + text;
                    }
                }

                mergedRows++;
                if (isDescriptionContinuation)
                {
                    wrappedDescriptionRows++;
                }
            }
            else
            {
                if (newCells.Count > 0)
                {
                    newEdges.Add(rowEdges[rowIdx]);
                }

                newCells.Add(row);
            }
        }

        newEdges.Add(rowEdges[^1]);

        return mergedRows == 0 || newCells.Count < 2 || newEdges.Count != newCells.Count + 1
            ? (newCells, rowEdges, 0)
            : (newCells, newEdges, wrappedDescriptionRows);
    }

    /// <summary>
    /// Detects a table by merging every cluster's rects into one group. This
    /// handles clip-path PDFs where each column's cell rects form a separate
    /// cluster with no spatial overlap between columns: rect Y-edges give the rows
    /// and text X-position clustering gives the columns, as in
    /// <see cref="DetectRowStripeTable"/> but without the width-uniformity check.
    /// </summary>
    private static Table? DetectMergedClusterTable(
        IReadOnlyList<TextItem> items,
        IReadOnlyList<RectBox> allRects,
        uint page)
    {
        var yVals = new List<float>();
        foreach (var r in allRects)
        {
            yVals.Add(r.Bottom);
            yVals.Add(r.Top);
        }

        var rowEdges = RectGrid.SnapEdges(yVals, 6.0f);
        if (rowEdges.Count < 4)
        {
            Log.Debug(Module, () => $"  merged-cluster rejected: only {rowEdges.Count} y-edges");
            return null;
        }

        rowEdges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        var yTop = rowEdges[0];
        var yBottom = rowEdges[^1];
        var xLeft = allRects.Min(r => r.Left);
        var xRight = allRects.Max(r => r.Right);

        var pageItems = ItemsInBand(items, page, yBottom, yTop, xLeft, xRight);
        if (pageItems.Count == 0)
        {
            return null;
        }

        var columns = ClusterXPositions(pageItems, 15.0f);
        if (columns.Count < 2)
        {
            Log.Debug(Module, () =>
                $"  merged-cluster rejected: only {columns.Count} columns from text clustering");
            return null;
        }

        var colEdges = ColumnCentersToEdges(columns, pageItems);
        var numCols = colEdges.Count - 1;
        var numRows = rowEdges.Count - 1;

        Log.Debug(Module, () =>
            $"  merged-cluster grid: {numRows}x{numCols} ({colEdges.Count} col edges, {rowEdges.Count} row edges)");

        var (cells, itemIndices) = RectGrid.AssignItemsToGrid(items, colEdges, rowEdges, page);
        if (itemIndices.Count == 0)
        {
            Log.Debug(Module, "  merged-cluster rejected: no items assigned");
            return null;
        }

        var nonEmptyRows = cells.Count(row => row.Any(c => c.Trim().Length > 0));
        if (nonEmptyRows < 2)
        {
            Log.Debug(Module, () => $"  merged-cluster rejected: only {nonEmptyRows} non-empty rows");
            return null;
        }

        var totalCells = (float)(numCols * numRows);
        var nonEmptyCells = cells.Sum(row => row.Count(c => c.Trim().Length > 0));
        var contentRatio = nonEmptyCells / totalCells;
        if (contentRatio < 0.40f)
        {
            Log.Debug(Module, () => $"  merged-cluster rejected: content ratio {contentRatio:F2} < 0.40");
            return null;
        }

        // Reject any cell with excessive text: layout background rects produce
        // "cells" holding paragraphs, not short data-table values. Multi-row
        // key/value tables can legitimately have one long descriptive column, so
        // only narrow-row layouts are rejected here.
        var maxCellLen = cells.SelectMany(row => row).Select(TextUtils.ByteLength).DefaultIfEmpty(0).Max();
        if (maxCellLen > 500 && nonEmptyRows < 4)
        {
            Log.Debug(Module, () =>
                $"  merged-cluster rejected: max cell length {maxCellLen} > 500 ({nonEmptyRows} rows, layout background)");
            return null;
        }

        if (HasDominantProseCell(cells))
        {
            Log.Debug(Module, "  merged-cluster rejected: dominant prose cell (chart/figure region over body text)");
            return null;
        }

        for (var col = 0; col < numCols; col++)
        {
            if (!ColumnHasContent(cells, col))
            {
                var emptyCol = col;
                Log.Debug(Module, () => $"  merged-cluster rejected: column {emptyCol} is empty");
                return null;
            }
        }

        var columnCenters = EdgeCenters(colEdges, numCols);
        var rowCenters = EdgeCenters(rowEdges, numRows);

        Log.Debug(Module, () =>
            $"  merged-cluster table accepted: {numRows}x{numCols}, {contentRatio * 100.0f:F0}% density");

        return Table.Create(columnCenters, rowCenters, cells, itemIndices);
    }

    /// <summary>
    /// Clusters text item X positions into column centers, given a minimum
    /// threshold. This mirrors <c>Grid.FindColumnBoundaries</c> but with a lower
    /// floor, which suits rect-backed tables where the structure is already proven
    /// and the anti-paragraph safeguards are unnecessary.
    /// </summary>
    private static List<float> ClusterXPositions(
        List<(int Index, TextItem Item)> items,
        float minThreshold)
    {
        // Column edges come from where text STARTS. An item whose left edge hugs
        // the previous item's right edge on the same line is a continuation run — a
        // style boundary, a script change, an underline split — and feeding its
        // x-start in here fabricates a phantom column mid-cell.
        var sorted = items
            .Select(p => p.Item)
            .OrderBy(i => i.Y, FloatTotalOrder.Instance)
            .ThenBy(i => i.X, FloatTotalOrder.Instance)
            .ToList();

        var xPositions = new List<float>(sorted.Count);
        for (var idx = 0; idx < sorted.Count; idx++)
        {
            var item = sorted[idx];
            var isContinuation = false;
            if (idx > 0)
            {
                var prev = sorted[idx - 1];

                // Style and underline splits leave runs that TOUCH, with a gap near
                // zero; real cell boundaries keep a visible gap even in the tightest
                // tables, and 2pt separates the two without eating dense-table
                // columns. The negative side is bounded as well: text overhanging
                // from an adjacent cell overlaps by far more than italic kerning ever
                // does, and must still start its own column.
                var gap = item.X - (prev.X + prev.Width);
                isContinuation = MathF.Abs(prev.Y - item.Y) <= 2.0f
                    && gap < 2.0f
                    && gap > -4.0f
                    && item.X >= prev.X;
            }

            if (!isContinuation)
            {
                xPositions.Add(item.X);
            }
        }

        xPositions.Sort(FloatTotalOrder.Instance);
        if (xPositions.Count == 0)
        {
            return [];
        }

        var xRange = xPositions[^1] - xPositions[0];
        var avgGap = xPositions.Count > 1 ? xRange / (xPositions.Count - 1) : 60.0f;
        var clusterThreshold = Math.Clamp(avgGap, minThreshold, 50.0f);

        var columns = new List<float>();
        var clusterItems = new List<float> { xPositions[0] };

        foreach (var x in xPositions.Skip(1))
        {
            var clusterCenter = clusterItems.SumF32() / clusterItems.Count;
            if (x - clusterCenter > clusterThreshold)
            {
                columns.Add(clusterCenter);
                clusterItems = [x];
            }
            else
            {
                clusterItems.Add(x);
            }
        }

        if (clusterItems.Count > 0)
        {
            columns.Add(clusterItems.SumF32() / clusterItems.Count);
        }

        // Every column needs multiple items behind it.
        var minItemsPerCol = Math.Max(items.Count / Math.Max(columns.Count, 1) / 4, 2);
        return columns
            .Where(colX => items.Count(p => MathF.Abs(p.Item.X - colX) < clusterThreshold) >= minItemsPerCol)
            .ToList();
    }
}
