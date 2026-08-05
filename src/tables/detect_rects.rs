//! Rectangle-based table detection using union-find clustering.

use std::collections::HashMap;

use log::debug;

use crate::types::{PdfRect, TextItem};

use super::Table;

const DOMINANT_PAGE_BACKGROUND_MIN_REPETITIONS: usize = 8;
const COMPETING_TABLE_MIN_ROWS: usize = 8;

/// Disjoint-set (union-find) with component sizes for clustering indices.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
            size: vec![1; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        let new_size = self.size[ra] + self.size[rb];
        if self.rank[ra] < self.rank[rb] {
            self.parent[ra] = rb;
            self.size[rb] = new_size;
        } else if self.rank[ra] > self.rank[rb] {
            self.parent[rb] = ra;
            self.size[ra] = new_size;
        } else {
            self.parent[rb] = ra;
            self.size[ra] = new_size;
            self.rank[ra] += 1;
        }
    }

    fn component_size(&mut self, x: usize) -> usize {
        let root = self.find(x);
        self.size[root]
    }
}

/// Check if two rects overlap after expanding each by `tol` on all sides.
pub(crate) fn rects_overlap(a: &(f32, f32, f32, f32), b: &(f32, f32, f32, f32), tol: f32) -> bool {
    // a and b are (x, y, w, h) where (x,y) is bottom-left corner
    let (ax, ay, aw, ah) = *a;
    let (bx, by, bw, bh) = *b;
    // Expand each rect by tol
    let a_left = ax - tol;
    let a_right = ax + aw + tol;
    let a_bottom = ay - tol;
    let a_top = ay + ah + tol;
    let b_left = bx - tol;
    let b_right = bx + bw + tol;
    let b_bottom = by - tol;
    let b_top = by + bh + tol;
    // AABB overlap: NOT (separated)
    !(a_right < b_left || b_right < a_left || a_top < b_bottom || b_top < a_bottom)
}

/// Maximum component size for rect clustering.  No real table has thousands
/// of cell rects — once a component exceeds this, it is a vector drawing or
/// page-spanning clipping path.  We skip overlap checks for rects already in
/// an oversized component, keeping the original O(n²) loop but making it
/// effectively O(n) for pathological pages.
const MAX_CLUSTER_RECTS: usize = 2000;

/// Cluster rects by spatial overlap using union-find.
/// Returns groups of rect indices; only groups with ≥ `min_size` rects are returned.
///
/// Skips overlap checks for rects whose component has already exceeded
/// [`MAX_CLUSTER_RECTS`], so pages with tens of thousands of vector-drawing
/// rects complete in milliseconds instead of minutes.
pub(crate) fn cluster_rects(
    rects: &[(f32, f32, f32, f32)],
    tolerance: f32,
    min_size: usize,
) -> Vec<Vec<usize>> {
    let n = rects.len();
    let mut uf = UnionFind::new(n);

    for i in 0..n {
        // If rect i is already in an oversized component, no point comparing
        // it against further rects — the component won't be used for table
        // detection anyway.
        if uf.component_size(i) >= MAX_CLUSTER_RECTS {
            continue;
        }
        for j in (i + 1)..n {
            if rects_overlap(&rects[i], &rects[j], tolerance) {
                uf.union(i, j);
                // Check if the merged component just exceeded the cap —
                // if so, no need to test more pairs for rect i.
                if uf.component_size(i) >= MAX_CLUSTER_RECTS {
                    break;
                }
            }
        }
    }

    // Group indices by root
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        groups.entry(uf.find(i)).or_default().push(i);
    }

    // Sort by root index for deterministic output order
    let mut result: Vec<(usize, Vec<usize>)> = groups
        .into_iter()
        .filter(|(_, g)| g.len() >= min_size)
        .collect();
    result.sort_by_key(|(root, _)| *root);
    result.into_iter().map(|(_, g)| g).collect()
}

/// Split a rect cluster at the widest X-gap when detection fails.
/// Returns sub-groups only if a gap >= `min_gap` exists and both sides have >= `min_group_size` rects.
#[allow(clippy::type_complexity)]
fn split_wide_cluster(
    rects: &[(f32, f32, f32, f32)],
    min_gap: f32,
    min_group_size: usize,
) -> Option<(Vec<(f32, f32, f32, f32)>, Vec<(f32, f32, f32, f32)>)> {
    if rects.len() < min_group_size * 2 {
        return None;
    }

    // Build sorted list of X-intervals (x_left, x_right) from each rect
    let mut intervals: Vec<(f32, f32)> = rects.iter().map(|&(x, _, w, _)| (x, x + w)).collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Merge overlapping intervals to find contiguous X-bands
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (start, end) in &intervals {
        if let Some(last) = merged.last_mut() {
            if *start <= last.1 + 1.0 {
                last.1 = last.1.max(*end);
                continue;
            }
        }
        merged.push((*start, *end));
    }

    if merged.len() < 2 {
        return None;
    }

    // Find the widest gap between consecutive merged intervals
    let mut best_gap = 0.0_f32;
    let mut best_split_x = 0.0_f32;
    for i in 1..merged.len() {
        let gap = merged[i].0 - merged[i - 1].1;
        if gap > best_gap {
            best_gap = gap;
            best_split_x = (merged[i - 1].1 + merged[i].0) / 2.0;
        }
    }

    if best_gap < min_gap {
        return None;
    }

    let left: Vec<_> = rects
        .iter()
        .filter(|&&(x, _, w, _)| x + w / 2.0 < best_split_x)
        .copied()
        .collect();
    let right: Vec<_> = rects
        .iter()
        .filter(|&&(x, _, w, _)| x + w / 2.0 >= best_split_x)
        .copied()
        .collect();

    if left.len() >= min_group_size && right.len() >= min_group_size {
        Some((left, right))
    } else {
        None
    }
}

/// A bounding box hint from cell-border rects that failed full grid validation.
///
/// When a rect cluster contains cell-sized borders but they don't form a valid
/// grid (e.g. only horizontal row borders with no vertical column dividers),
/// the bounding box of those cell-sized rects can still be used to scope
/// heuristic table detection, preventing unrelated items (graph labels, etc.)
/// from being merged into the table.
#[derive(Debug, Clone)]
pub struct RectHintRegion {
    /// Y coordinate of the top edge (highest value in PDF space)
    pub y_top: f32,
    /// Y coordinate of the bottom edge (lowest value in PDF space)
    pub y_bottom: f32,
    /// X coordinate of the left edge
    pub x_left: f32,
    /// X coordinate of the right edge
    pub x_right: f32,
    /// Raw rects from the cluster (x, y, w, h) for rect-guided table building
    pub cluster_rects: Vec<(f32, f32, f32, f32)>,
}

/// Detect tables from explicit rectangle (`re`) operators in the PDF.
///
/// Many PDFs draw cell borders using `re` (rectangle) operators.  Table pages
/// typically have 100-200+ rects while non-table pages have < 30.  This function
/// clusters spatially connected rectangles into groups, then identifies grids of
/// cell-sized rectangles within each cluster and assigns text items to cells.
///
/// Also returns hint regions: bounding boxes of cell-sized rects from clusters
/// that failed full grid validation.  These can be used to scope heuristic
/// detection and prevent unrelated items from being merged into tables.
/// Bounding boxes of chart-bar clusters on the page. Text inside these
/// regions (axis labels, data values, legends) belongs to a figure and must
/// not be gridded into a table by any detection strategy.
pub fn detect_chart_regions(
    items: &[TextItem],
    rects: &[PdfRect],
    page: u32,
) -> Vec<(f32, f32, f32, f32)> {
    // Match detect_tables_from_rects: image placeholders are not text and
    // would defeat the bar-content check.
    let items_owned: Vec<TextItem> = items
        .iter()
        .filter(|i| crate::extractor::is_text_layout_item(i))
        .cloned()
        .collect();
    let items = items_owned.as_slice();
    let page_rects: Vec<(f32, f32, f32, f32)> = rects
        .iter()
        .filter(|r| r.page == page)
        .map(|r| {
            let (x, w) = if r.width < 0.0 {
                (r.x + r.width, -r.width)
            } else {
                (r.x, r.width)
            };
            let (y, h) = if r.height < 0.0 {
                (r.y + r.height, -r.height)
            } else {
                (r.y, r.height)
            };
            (x, y, w, h)
        })
        // Origin-anchored page backgrounds/clipping paths are never chart
        // geometry, and letting one bridge into a bar cluster would inflate
        // the region to the whole page.
        .filter(|&(x, y, w, h)| w >= 5.0 && h >= 5.0 && !(x < 5.0 && y < 5.0))
        .collect();
    if page_rects.len() < 6 {
        return Vec::new();
    }
    let mut regions = Vec::new();
    for cluster in &cluster_rects(&page_rects, 3.0, 6) {
        let group: Vec<(f32, f32, f32, f32)> = cluster.iter().map(|&i| page_rects[i]).collect();
        if is_chart_bar_cluster(items, &group, page) {
            let bbox = group.iter().fold(
                (
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ),
                |(x0, y0, x1, y1), &(x, y, w, h)| {
                    (x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h))
                },
            );
            regions.push(bbox);
        }
    }
    regions
}

fn detect_direct_rect_table(
    items: &[TextItem],
    rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    detect_table_from_rect_group(items, rects, page)
        .or_else(|| detect_row_stripe_table(items, rects, page))
        .or_else(|| detect_stacked_box_table(items, rects, page))
}

pub fn detect_tables_from_rects(
    items: &[TextItem],
    rects: &[PdfRect],
    page: u32,
) -> (Vec<Table>, Vec<RectHintRegion>) {
    // Strip Image placeholders before column/row clustering — an image's bbox
    // would otherwise show up as a spurious column edge. See `is_text_layout_item`.
    let items_owned: Vec<TextItem> = items
        .iter()
        .filter(|i| crate::extractor::is_text_layout_item(i))
        .cloned()
        .collect();
    let items = items_owned.as_slice();

    // Filter rects on this page; normalize negative widths/heights; skip tiny rects.
    let mut page_rects: Vec<(f32, f32, f32, f32)> = Vec::new(); // (x, y, w, h) normalized
    for r in rects {
        if r.page != page {
            continue;
        }
        let (mut x, mut y, mut w, mut h) = (r.x, r.y, r.width, r.height);
        if w < 0.0 {
            x += w;
            w = -w;
        }
        if h < 0.0 {
            y += h;
            h = -h;
        }
        // Skip tiny rects (borders, dots, decorations)
        if w < 5.0 || h < 5.0 {
            continue;
        }
        page_rects.push((x, y, w, h));
    }

    // Remove rects that are much wider than typical cell rects — these are
    // page-spanning clipping paths or row-spanning background fills that
    // would add spurious X-edges and corrupt the grid.  We use the median
    // WIDTH (not area) because row-stripe tables have ALL rects at the same
    // full width, so their median width equals the full table width and none
    // get filtered.  Cell-grid tables have narrow cell rects, so full-width
    // background fills stand out clearly.
    if page_rects.len() >= 6 {
        let mut widths: Vec<f32> = page_rects.iter().map(|&(_, _, w, _)| w).collect();
        widths.sort_by(|a, b| a.total_cmp(b));
        let median_width = widths[widths.len() / 2];
        let width_threshold = median_width * 10.0;
        let before = page_rects.len();
        page_rects.retain(|&(_, _, w, _)| w <= width_threshold);
        if page_rects.len() < before {
            debug!(
                "page {}: removed {} oversized rects (median_w={:.0}, threshold={:.0})",
                page,
                before - page_rects.len(),
                median_width,
                width_threshold,
            );
        }

        // Deduplicate sub-rects: when a rect is fully contained within a
        // slightly larger rect (same column, interior Y range), the smaller
        // one is a cell-internal decoration (e.g. content-area shading
        // inside the full cell background).  Keeping both creates spurious
        // Y-edges that split visual rows into thin sub-rows.
        //
        // Only remove when the container is a similarly-sized cell (height
        // ratio < 4×), NOT when the container is a table-wide background
        // that dwarfs the sub-rect.  Origin-anchored page-background rects
        // also disqualify as containers — they normally exceed the 4× ratio,
        // but when the sub-rect is itself a tall table-frame the ratio can
        // fall under the gate, and dropping the frame collapses cluster
        // adjacency between adjacent column-cell groups.
        //
        // Skip this O(n²) dedup when there are too many rects — pages with
        // thousands of vector-drawing rects won't benefit from cell dedup.
        if page_rects.len() < MAX_CLUSTER_RECTS {
            let before = page_rects.len();
            let snapshot = page_rects.clone();
            page_rects.retain(|&(ax, ay, aw, ah)| {
                let tol = 2.0;
                !snapshot.iter().any(|&(bx, by, bw, bh)| {
                    let container_is_page_bg = bx < 5.0 && by < 5.0;
                    // b must strictly contain a (b is larger in area)
                    bw * bh > aw * ah * 1.2
                        && bh < ah * 4.0 // container must be similarly sized, not a table background
                        && !container_is_page_bg
                        && bx <= ax + tol
                        && (bx + bw) >= (ax + aw) - tol
                        && by <= ay + tol
                        && (by + bh) >= (ay + ah) - tol
                })
            });
            if page_rects.len() < before {
                debug!(
                    "page {}: removed {} contained sub-rects",
                    page,
                    before - page_rects.len(),
                );
            }
        }
    }

    debug!(
        "page {}: {} rects after size filter (from {} raw)",
        page,
        page_rects.len(),
        rects.iter().filter(|r| r.page == page).count(),
    );

    let mut tables = Vec::new();
    let mut hint_regions = Vec::new();
    let mut failed_clusters: Vec<Vec<(f32, f32, f32, f32)>> = Vec::new();

    // Full grid detection requires ≥ 6 rects
    if page_rects.len() >= 6 {
        // Identify origin-anchored page-background rects (clipping paths or
        // page fills) that would bridge separate table regions if included in
        // clustering.  Exclude them from adjacency but add them back to each
        // cluster they overlap, so grid detection still has their edges.
        let is_page_bg = {
            let mut heights: Vec<f32> = page_rects.iter().map(|&(_, _, _, h)| h).collect();
            heights.sort_by(|a, b| a.total_cmp(b));
            let median_height = heights[heights.len() / 2];
            let height_threshold = median_height * 20.0;
            let flags: Vec<bool> = page_rects
                .iter()
                .map(|&(x, y, _, h)| x < 5.0 && y < 5.0 && h > height_threshold)
                .collect();
            if flags.iter().any(|&b| b) {
                debug!(
                    "page {}: {} origin-anchored page-bg rects excluded from clustering",
                    page,
                    flags.iter().filter(|&&b| b).count(),
                );
            }
            flags
        };

        // Build filtered rect list for clustering (excluding page backgrounds)
        let non_bg_indices: Vec<usize> =
            (0..page_rects.len()).filter(|&i| !is_page_bg[i]).collect();
        let non_bg_rects: Vec<(f32, f32, f32, f32)> =
            non_bg_indices.iter().map(|&i| page_rects[i]).collect();
        let raw_clusters = cluster_rects(&non_bg_rects, 3.0, 6);

        // Map cluster indices back to page_rects indices
        let clusters: Vec<Vec<usize>> = raw_clusters
            .iter()
            .map(|cluster| cluster.iter().map(|&i| non_bg_indices[i]).collect())
            .collect();

        debug!("page {}: {} clusters with >= 6 rects", page, clusters.len());
        let mut merge_excluded_cluster_ids: Vec<usize> = Vec::new();
        for (cluster_id, cluster_indices) in clusters.iter().enumerate() {
            let group_rects: Vec<(f32, f32, f32, f32)> =
                cluster_indices.iter().map(|&i| page_rects[i]).collect();
            // Chart bars are neither table cells nor a hint region — gridding
            // a chart's axis labels scrambles the page. Skip the cluster
            // entirely so it can't reach any detector, the merged fallback,
            // or the hint fallback.
            if is_chart_bar_cluster(items, &group_rects, page) {
                // Repeated page fills can dominate the geometry and make a
                // real shaded-cell table look like a chart. Remove those fills,
                // re-cluster the remaining geometry, and evaluate valid table
                // candidates as a competing hypothesis before the chart
                // rejection wins.
                let normalized = without_dominant_page_backgrounds(&group_rects);
                let normalized_table = (normalized.len() < group_rects.len())
                    .then(|| {
                        cluster_rects(&normalized, 3.0, 6)
                            .iter()
                            .filter_map(|indices| {
                                let candidate: Vec<(f32, f32, f32, f32)> =
                                    indices.iter().map(|&i| normalized[i]).collect();
                                if is_chart_bar_cluster(items, &candidate, page) {
                                    None
                                } else {
                                    detect_table_from_rect_group(items, &candidate, page)
                                        .or_else(|| {
                                            detect_row_stripe_table_from_cell_rects(
                                                items, &candidate, page,
                                            )
                                        })
                                        // Small chart panels can still form
                                        // plausible grids from their labels.
                                        // Require sustained row evidence; the
                                        // motivating table has 17 rows.
                                        .filter(|table| {
                                            table.rows.len() >= COMPETING_TABLE_MIN_ROWS
                                        })
                                }
                            })
                            .max_by_key(|table| table.rows.len() * table.columns.len())
                    })
                    .flatten();
                if let Some(table) = normalized_table {
                    debug!(
                        "page {}: chart-like cluster normalized from {} to {} rects; accepted {}x{} table hypothesis",
                        page,
                        group_rects.len(),
                        normalized.len(),
                        table.rows.len(),
                        table.columns.len()
                    );
                    // The accepted hypothesis is based on normalized
                    // geometry. Keep the original chart-like cluster out of
                    // the merged fallback: reintroducing its repeated page
                    // fills can manufacture a wider candidate that replaces
                    // this valid narrow table below.
                    merge_excluded_cluster_ids.push(cluster_id);
                    tables.push(table);
                    continue;
                } else {
                    debug!(
                        "page {}: skipping chart-bar cluster ({} rects)",
                        page,
                        group_rects.len()
                    );
                    merge_excluded_cluster_ids.push(cluster_id);
                    continue;
                }
            }
            if let Some(table) = detect_direct_rect_table(items, &group_rects, page) {
                tables.push(table);
            } else if let Some((left, right)) = split_wide_cluster(&group_rects, 15.0, 6) {
                // Cluster was too wide — retry each half independently
                debug!(
                    "page {}: splitting cluster of {} rects into {} + {} at x-gap",
                    page,
                    group_rects.len(),
                    left.len(),
                    right.len()
                );
                let mut split_found = false;
                for sub in [&left, &right] {
                    if let Some(table) = detect_table_from_rect_group(items, sub, page) {
                        tables.push(table);
                        split_found = true;
                    } else if let Some(table) = detect_row_stripe_table(items, sub, page) {
                        tables.push(table);
                        split_found = true;
                    }
                }
                if !split_found {
                    failed_clusters.push(group_rects);
                }
            } else {
                failed_clusters.push(group_rects);
            }
        }

        // Merged-cluster fallback: when per-cluster attempts produce no tables
        // or only narrow false-positives (≤3 columns from individual column
        // clusters), merge all cluster rects and try row-stripe strategy with
        // text-based column detection.
        let only_narrow = !tables.is_empty() && tables.iter().all(|t| t.columns.len() <= 3);
        if tables.is_empty() || only_narrow {
            // Chart clusters stay out of the merge as well.
            let table_clusters: Vec<&Vec<usize>> = clusters
                .iter()
                .enumerate()
                .filter(|(id, _)| !merge_excluded_cluster_ids.contains(id))
                .map(|(_, c)| c)
                .collect();
            let total_clustered: usize = table_clusters.iter().map(|c| c.len()).sum();
            if table_clusters.len() >= 3 && total_clustered >= 50 {
                debug!(
                    "page {}: trying merged-cluster fallback ({} clusters, {} rects{})",
                    page,
                    table_clusters.len(),
                    total_clustered,
                    if only_narrow {
                        ", replacing narrow tables"
                    } else {
                        ""
                    }
                );
                let all_cluster_rects: Vec<(f32, f32, f32, f32)> = table_clusters
                    .iter()
                    .flat_map(|idxs| idxs.iter().map(|&i| page_rects[i]))
                    .collect();
                if let Some(table) = detect_merged_cluster_table(items, &all_cluster_rects, page) {
                    if only_narrow {
                        tables.clear();
                    }
                    tables.push(table);
                }
            }
        }

        // Cell-rect fallback: when per-cluster attempts all fail, try using
        // rect Y-edges for rows + text X-positions for columns on each failed
        // cluster.  Handles tables with cell-background rects that don't form
        // a clean grid (variable column widths, decoration fills).
        if tables.is_empty() {
            debug!(
                "page {}: cell-rect fallback: {} failed clusters",
                page,
                failed_clusters.len()
            );
            for fc_rects in &failed_clusters {
                if fc_rects.len() >= 6 {
                    if let Some(table) =
                        detect_row_stripe_table_from_cell_rects(items, fc_rects, page)
                    {
                        tables.push(table);
                    }
                }
            }
        }

        // Row-stripe fallback: when clustering produces no large clusters
        // (row stripes don't overlap so each is its own cluster of 1),
        // try all page rects directly as a row-stripe table.
        // Require ≥15 rects and ≥10 result rows to avoid decorative fill false positives.
        if tables.is_empty() && clusters.is_empty() && page_rects.len() >= 15 {
            if let Some(table) = detect_row_stripe_table(items, &page_rects, page) {
                if table.rows.len() >= 10 {
                    debug!(
                        "page {}: row-stripe fallback succeeded ({} rects, {} rows)",
                        page,
                        page_rects.len(),
                        table.rows.len()
                    );
                    tables.push(table);
                } else {
                    debug!(
                        "page {}: row-stripe fallback rejected: only {} rows",
                        page,
                        table.rows.len()
                    );
                }
            }
        }
    }

    // NOTE: 3-5 box stacks never reach detect_stacked_box_table — the main
    // loop requires >=6-rect clusters (and a >=6-rect page). This is a
    // deliberate precision gate: routing smaller clusters through the
    // detector was tried and regressed four pdf-evals documents (striped
    // bullet lists, wrapped regulation text, stats-table columns) while
    // improving nothing — with so few boxes the anti-prose guards have too
    // little signal to discriminate. See stacked_box_three_rows_below_
    // cluster_minimum for the pinned behavior.
    if tables.is_empty() {
        // When no tables detected but clusters exist, generate XY hint regions
        // from cluster bounding boxes to scope heuristic table detection.
        // This handles both large decorative-rect clusters (calendars, forms)
        // and small cell-border clusters on rect-sparse pages.
        let mut has_failed_cluster_hints = false;
        if page_rects.len() >= 6 {
            let clusters = cluster_rects(&page_rects, 3.0, 6);

            // Generate hints from large clusters (≥30 rects, decorative/calendar style)
            for cluster_indices in &clusters {
                let group_rects: Vec<(f32, f32, f32, f32)> =
                    cluster_indices.iter().map(|&i| page_rects[i]).collect();
                if group_rects.len() < 30 {
                    continue;
                }
                let x_left = group_rects.iter().map(|r| r.0).reduce(f32::min).unwrap();
                let x_right = group_rects
                    .iter()
                    .map(|r| r.0 + r.2)
                    .reduce(f32::max)
                    .unwrap();
                let y_bottom = group_rects.iter().map(|r| r.1).reduce(f32::min).unwrap();
                let y_top = group_rects
                    .iter()
                    .map(|r| r.1 + r.3)
                    .reduce(f32::max)
                    .unwrap();
                let w = x_right - x_left;
                let h = y_top - y_bottom;
                if (30.0..=400.0).contains(&w) && (10.0..=400.0).contains(&h) {
                    debug!(
                        "page {}: hint candidate from {} rects: x={:.1}..{:.1} y={:.1}..{:.1} ({:.0}×{:.0})",
                        page, group_rects.len(), x_left, x_right, y_bottom, y_top, w, h
                    );
                    hint_regions.push(RectHintRegion {
                        y_top,
                        y_bottom,
                        x_left,
                        x_right,
                        cluster_rects: group_rects.clone(),
                    });
                }
            }

            // Generate hints from failed clusters (≥6 rects that had valid bounding
            // boxes but insufficient grid structure — e.g. outer border or header
            // divider with 2x2 edges). These tell us WHERE a table is even though
            // the rects don't define column structure.
            for fc_rects in &failed_clusters {
                if fc_rects.len() < 6 {
                    continue;
                }
                let x_left = fc_rects.iter().map(|r| r.0).reduce(f32::min).unwrap();
                let x_right = fc_rects.iter().map(|r| r.0 + r.2).reduce(f32::max).unwrap();
                let y_bottom = fc_rects.iter().map(|r| r.1).reduce(f32::min).unwrap();
                let y_top = fc_rects.iter().map(|r| r.1 + r.3).reduce(f32::max).unwrap();
                let h = y_top - y_bottom;
                // Require reasonable height and text items inside the region
                let padding = 15.0;
                let items_inside = items
                    .iter()
                    .filter(|item| {
                        item.y >= y_bottom - padding
                            && item.y <= y_top + padding
                            && item.x >= x_left - padding
                            && item.x <= x_right + padding
                    })
                    .count();
                let w = x_right - x_left;
                // Require reasonable dimensions: height ≥100pt (≈5+ rows),
                // height ≤600pt (not full page).
                // Width check: ≤500pt normally, but allow wider for large
                // clusters (≥30 rects) that are clearly structured.
                let max_w = if fc_rects.len() >= 30 { 800.0 } else { 500.0 };
                if (100.0..=600.0).contains(&h) && w <= max_w && items_inside >= 6 {
                    debug!(
                        "page {}: failed-cluster hint from {} rects ({} items): x={:.1}..{:.1} y={:.1}..{:.1} ({:.0}×{:.0})",
                        page, fc_rects.len(), items_inside, x_left, x_right, y_bottom, y_top,
                        x_right - x_left, h
                    );
                    hint_regions.push(RectHintRegion {
                        y_top,
                        y_bottom,
                        x_left,
                        x_right,
                        cluster_rects: fc_rects.clone(),
                    });
                    has_failed_cluster_hints = true;
                }
            }

            // Deduplicate overlapping hints
            hint_regions = merge_overlapping_hints(hint_regions);
            // Require multiple hint regions to confirm a multi-zone layout
            // (calendars, forms). A single hint is likely a decorative cluster
            // that would interfere with full-page heuristic detection.
            // Exception: failed-cluster hints represent real table boundaries
            // confirmed by rect presence, so a single one is meaningful.
            if hint_regions.len() < 2 && !has_failed_cluster_hints {
                hint_regions.clear();
            }
            if !hint_regions.is_empty() {
                debug!(
                    "page {}: {} XY hint regions from failed clusters",
                    page,
                    hint_regions.len()
                );
            }
        }

        // On rect-sparse pages (≤ 6 rects), a few cell-border rects may define the
        // table region even though they can't form a full grid (e.g. only horizontal
        // row borders, no column dividers).  Extract a hint region so the heuristic
        // detector can be scoped to just that area.
        if hint_regions.is_empty() && page_rects.len() >= 4 && page_rects.len() <= 6 {
            let small_clusters = cluster_rects(&page_rects, 3.0, 4);
            for cluster_indices in &small_clusters {
                let group_rects: Vec<(f32, f32, f32, f32)> =
                    cluster_indices.iter().map(|&i| page_rects[i]).collect();
                if let Some(hint) = extract_hint_region(&group_rects) {
                    debug!(
                        "page {}: hint region y={:.1}..{:.1} x={:.1}..{:.1}",
                        page, hint.y_bottom, hint.y_top, hint.x_left, hint.x_right
                    );
                    hint_regions.push(hint);
                }
            }
        }
    }

    (tables, hint_regions)
}

/// Merge nearby hint regions that share a Y band.
///
/// Two hints merge when they have substantial Y overlap (>50%) AND their X ranges
/// overlap or are close (gap < 50pt).  This handles calendar-style layouts where a
/// month zone's decorative rects split into 2-3 adjacent clusters with small X gaps.
/// Runs iteratively until no more merges occur.
/// Detect a single-column table drawn as a vertical stack of boxes, each
/// holding one short line of text (framework/step lists on slide-style
/// pages). The normal grid path rejects these — one column means only two
/// x-edges — so the rows would otherwise flow into surrounding prose as a
/// run-on paragraph.
fn detect_stacked_box_table(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    // Candidate row boxes: single-text-line height, substantial width.
    let cands: Vec<(f32, f32, f32, f32)> = group_rects
        .iter()
        .copied()
        .filter(|&(_, _, w, h)| w >= 100.0 && (8.0..=80.0).contains(&h))
        .collect();
    // The row boxes form the largest family of same-width, x-aligned rects
    // (backgrounds and decor have their own geometry and stay out).
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    for &anchor in &cands {
        let family: Vec<(f32, f32, f32, f32)> = cands
            .iter()
            .copied()
            .filter(|&(x, _, w, h)| {
                (x - anchor.0).abs() <= 12.0
                    && (w - anchor.2).abs() <= anchor.2 * 0.15
                    && (h - anchor.3).abs() <= anchor.3 * 0.3
            })
            .collect();
        if family.len() > boxes.len() {
            boxes = family;
        }
    }
    if boxes.len() < 3 {
        return None;
    }
    // Boxes flanked at the same y-level — by other rects or by text outside
    // the family's x-range — are one column of a wider structure. Leave
    // those to the grid/cell-rect paths instead of collapsing to one column.
    let flanked = boxes
        .iter()
        .filter(|&&(bx, by, bw, bh)| {
            let rect_sibling = group_rects.iter().any(|&(ox, oy, ow, oh)| {
                let y_overlap = (by + bh).min(oy + oh) - by.max(oy);
                oh >= 8.0
                    && y_overlap > bh * 0.5
                    && (ox + ow <= bx + 2.0 || ox >= bx + bw - 2.0)
                    && ow >= 30.0
            });
            let text_sibling = items.iter().any(|it| {
                let cx = it.x + it.width / 2.0;
                it.page == page
                    && it.y >= by - 2.0
                    && it.y <= by + bh + 2.0
                    && (cx < bx - 5.0 || cx > bx + bw + 5.0)
                    && it.width >= 10.0
            });
            rect_sibling || text_sibling
        })
        .count();
    if flanked * 3 >= boxes.len() {
        debug!(
            "  stacked-box rejected: {}/{} boxes flanked by rects or text",
            flanked,
            boxes.len()
        );
        return None;
    }
    boxes.sort_by(|a, b| b.1.total_cmp(&a.1)); // top to bottom (descending y)

    // Merge duplicates (border + fill pairs draw the same box twice), then
    // require a clean vertical stack: no overlaps beyond a small tolerance.
    boxes.dedup_by(|a, b| (a.1 - b.1).abs() <= 3.0 && (a.3 - b.3).abs() <= 6.0);
    if boxes.len() < 3 {
        return None;
    }
    for w in boxes.windows(2) {
        let (upper, lower) = (w[0], w[1]);
        let upper_bottom = upper.1;
        let lower_top = lower.1 + lower.3;
        if lower_top > upper_bottom + 4.0 {
            return None; // vertical overlap — not a stack
        }
        if upper_bottom - lower_top > upper.3.max(lower.3) {
            return None; // gap larger than a row — unrelated boxes
        }
    }

    // Assign items to boxes; every box needs text and cells must stay short
    // (prose paragraphs inside stacked frames are page decor, not a table).
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(boxes.len());
    let mut item_indices: Vec<usize> = Vec::new();
    let mut multi_run_boxes = 0usize;
    for &(bx, by, bw, bh) in &boxes {
        let mut in_box: Vec<(usize, &TextItem)> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.page == page
                    && it.y >= by - 2.0
                    && it.y <= by + bh + 2.0
                    && it.x + it.width / 2.0 >= bx
                    && it.x + it.width / 2.0 <= bx + bw
            })
            .collect();
        if in_box.is_empty() {
            return None;
        }
        in_box.sort_by(|a, b| {
            b.1.y
                .partial_cmp(&a.1.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.1.x
                        .partial_cmp(&b.1.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        // Count horizontally separated text runs inside the box. A single
        // list row flows as one run; two-plus runs across most boxes means
        // multi-column content (striped prose or a real grid) that must not
        // collapse into a one-column table. Same-baseline only: boxed
        // display/diagram rows legitimately scatter segments at mixed
        // baselines, and those must stay one row.
        let mut runs = 1usize;
        for pair in in_box.windows(2) {
            let (prev, item) = (pair[0].1, pair[1].1);
            if (prev.y - item.y).abs() <= 2.0 && item.x - (prev.x + prev.width) > 15.0 {
                runs += 1;
            }
        }
        if runs >= 2 {
            multi_run_boxes += 1;
        }
        let text = in_box
            .iter()
            .map(|(_, it)| it.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() || text.chars().count() > 120 {
            return None;
        }
        item_indices.extend(in_box.iter().map(|(i, _)| *i));
        cells.push(vec![text]);
    }
    if multi_run_boxes * 2 >= boxes.len() {
        debug!(
            "  stacked-box rejected: {}/{} boxes hold multiple text runs",
            multi_run_boxes,
            boxes.len()
        );
        return None;
    }

    // Reject prose behind per-line stripe rects: sentence fragments flowing
    // across rows read as long, function-word-dense cells, while genuine
    // list-table rows are short labels/titles.
    const PROSE_WORDS: &[&str] = &[
        "a", "an", "the", "of", "to", "is", "was", "are", "were", "be", "been", "in", "on", "at",
        "with", "for", "by", "as", "and", "or", "but", "this", "that", "these", "those", "from",
        "into", "has", "have", "had", "not", "it", "its", "their", "such", "shall", "which",
    ];
    let total_chars: usize = cells.iter().map(|r| r[0].chars().count()).sum();
    let mean_chars = total_chars / cells.len().max(1);
    let prose_cells = cells
        .iter()
        .filter(|r| {
            r[0].to_ascii_lowercase()
                .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
                .any(|w| PROSE_WORDS.contains(&w))
        })
        .count();
    if mean_chars > 60 && prose_cells * 5 >= cells.len() * 2 {
        debug!(
            "  stacked-box rejected: prose rows (mean {} chars, prose words {}/{})",
            mean_chars,
            prose_cells,
            cells.len()
        );
        return None;
    }
    // Sentences wrapping across stripe rects: a row ending with a comma, or
    // a row without terminal punctuation followed by a row starting
    // lowercase, is mid-sentence flow — not list rows. Genuine label/title
    // rows produce none of these, so even a small share is disqualifying.
    let continuations = cells
        .windows(2)
        .filter(|pair| {
            let prev = pair[0][0].trim_end();
            let next = pair[1][0].trim_start();
            let prev_open = !prev.ends_with(['.', ':', ';', '!', '?', ')', '"', '%']);
            let next_lower = next.chars().next().is_some_and(|c| c.is_lowercase());
            prev.ends_with(',') || (prev_open && next_lower)
        })
        .count();
    if cells.len() >= 2 && (continuations >= 2 || continuations * 4 >= cells.len() - 1) {
        debug!(
            "  stacked-box rejected: {}/{} row pairs continue a sentence",
            continuations,
            cells.len() - 1
        );
        return None;
    }
    // Numbered/lettered list items behind decorative stripes stay lists:
    // "1) content..." / "(ii) content..." / "a. content...".
    let list_marker = |t: &str| {
        let t = t.trim_start().strip_prefix('(').unwrap_or(t.trim_start());
        let marker_len = t.chars().take_while(|c| c.is_ascii_alphanumeric()).count();
        (1..=3).contains(&marker_len)
            && t.chars()
                .nth(marker_len)
                .is_some_and(|c| c == ')' || c == '.')
    };
    let list_rows = cells.iter().filter(|r| list_marker(&r[0])).count();
    if list_rows * 2 >= cells.len() {
        debug!(
            "  stacked-box rejected: {}/{} rows are numbered list items",
            list_rows,
            cells.len()
        );
        return None;
    }

    debug!(
        "page {}: stacked-box table: {} single-column rows",
        page,
        cells.len()
    );
    let columns = vec![boxes[0].0 + boxes[0].2 / 2.0];
    let rows: Vec<f32> = boxes.iter().map(|b| b.1 + b.3 / 2.0).collect();
    Some(Table::new(columns, rows, cells, item_indices))
}

fn merge_overlapping_hints(mut hints: Vec<RectHintRegion>) -> Vec<RectHintRegion> {
    if hints.len() <= 1 {
        return hints;
    }
    loop {
        hints.sort_by(|a, b| a.x_left.total_cmp(&b.x_left));
        let mut merged: Vec<RectHintRegion> = Vec::new();
        let mut any_merged = false;
        for hint in &hints {
            let mut did_merge = false;
            for existing in merged.iter_mut() {
                // Check Y overlap (>50% of smaller span)
                let y_overlap =
                    existing.y_top.min(hint.y_top) - existing.y_bottom.max(hint.y_bottom);
                let y_min_span =
                    (existing.y_top - existing.y_bottom).min(hint.y_top - hint.y_bottom);
                if y_overlap <= y_min_span * 0.5 {
                    continue;
                }
                // Check X: overlapping or adjacent (gap < 50pt)
                let x_gap = existing.x_left.max(hint.x_left) - existing.x_right.min(hint.x_right);
                if x_gap < 50.0 {
                    // Don't merge if result would exceed max hint width (400pt)
                    let merged_left = existing.x_left.min(hint.x_left);
                    let merged_right = existing.x_right.max(hint.x_right);
                    if merged_right - merged_left > 400.0 {
                        continue;
                    }
                    existing.x_left = merged_left;
                    existing.x_right = merged_right;
                    existing.y_bottom = existing.y_bottom.min(hint.y_bottom);
                    existing.y_top = existing.y_top.max(hint.y_top);
                    existing
                        .cluster_rects
                        .extend_from_slice(&hint.cluster_rects);
                    did_merge = true;
                    any_merged = true;
                    break;
                }
            }
            if !did_merge {
                merged.push(hint.clone());
            }
        }
        hints = merged;
        if !any_merged {
            break;
        }
    }
    hints
}

/// Extract a hint region from a rect cluster that failed grid validation.
///
/// Only produces hints from small clusters (≤ 8 rects) where a few cell-border
/// rects define a table's row boundaries.  Large clusters (form-style decorative
/// rects) are not suitable for hint regions since they typically span the whole page.
///
/// Filters out oversized "bounding box" rects (height > 4× the median height),
/// then computes the Y bounding box of the remaining cell-sized rects.
fn extract_hint_region(group_rects: &[(f32, f32, f32, f32)]) -> Option<RectHintRegion> {
    // Only produce hints from small clusters — large clusters that fail grid
    // validation are likely form-style decorative rects, not table cell borders.
    if group_rects.len() < 2 || group_rects.len() > 8 {
        return None;
    }

    // Compute median height to identify cell-sized rects
    let mut heights: Vec<f32> = group_rects.iter().map(|&(_, _, _, h)| h).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let median_h = heights[heights.len() / 2];

    // Keep only cell-sized rects (height ≤ 4× median)
    let cell_rects: Vec<&(f32, f32, f32, f32)> = group_rects
        .iter()
        .filter(|(_, _, _, h)| *h <= median_h * 4.0)
        .collect();

    if cell_rects.len() < 2 {
        return None;
    }

    // Compute bounding box of cell-sized rects
    let y_bottom = cell_rects.iter().map(|(_, y, _, _)| *y).reduce(f32::min)?;
    let y_top = cell_rects
        .iter()
        .map(|(_, y, _, h)| *y + *h)
        .reduce(f32::max)?;
    let x_left = cell_rects.iter().map(|(x, _, _, _)| *x).reduce(f32::min)?;
    let x_right = cell_rects
        .iter()
        .map(|(x, _, w, _)| *x + *w)
        .reduce(f32::max)?;

    // The region must have meaningful height but not span an unreasonable area
    let region_height = y_top - y_bottom;
    if !(10.0..=300.0).contains(&region_height) {
        return None;
    }

    Some(RectHintRegion {
        y_top,
        y_bottom,
        x_left,
        x_right,
        cluster_rects: Vec::new(),
    })
}

/// Detect a single table from a cluster of spatially connected rects.
///
/// Contains the grid-detection logic: snap edges, fill-ratio check,
/// assign items to grid, content density validation.
pub(crate) fn detect_table_from_rect_group(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    // First, try normal detection with all rects.
    let no_skip: Vec<bool> = vec![false; group_rects.len()];
    match try_build_grid(items, group_rects, page, &no_skip, false) {
        GridResult::Ok(table) => return Some(table),
        GridResult::FewNonEmptyRows => {
            // propagate_merged_cells likely collapsed text into row 0
            // due to a full-page background rect — retry below.
        }
        GridResult::Failed => return None,
    }

    // Check if the group contains page-origin background rects (starting
    // near (0,0), spanning nearly the full group).  If so, retry with those
    // rects excluded from X-edge extraction and propagate_merged_cells.
    // This handles PDFs where a full-page background fill adds spurious
    // margin columns and collapses all rows.
    let origin_tol = 5.0;
    let group_x_min = group_rects
        .iter()
        .map(|r| r.0)
        .fold(f32::INFINITY, f32::min);
    let group_x_max = group_rects
        .iter()
        .map(|r| r.0 + r.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let group_y_min = group_rects
        .iter()
        .map(|r| r.1)
        .fold(f32::INFINITY, f32::min);
    let group_y_max = group_rects
        .iter()
        .map(|r| r.1 + r.3)
        .fold(f32::NEG_INFINITY, f32::max);
    let group_w = group_x_max - group_x_min;
    let group_h = group_y_max - group_y_min;

    let is_page_bg: Vec<bool> = group_rects
        .iter()
        .map(|&(x, y, w, h)| {
            x < origin_tol && y < origin_tol && w >= group_w * 0.95 && h >= group_h * 0.9
        })
        .collect();

    // Only retry for groups with enough Y-edges to form a large grid.
    // Full-page backgrounds are problematic for dense tables (many rows)
    // but not for small grids where the retry would accept false positives.
    let y_edge_count = {
        let mut ys: Vec<f32> = Vec::new();
        for &(_, y, _, h) in group_rects {
            ys.push(y);
            ys.push(y + h);
        }
        snap_edges(&ys, 6.0).len()
    };

    if is_page_bg.iter().any(|&b| b) && y_edge_count >= 12 {
        debug!("  retrying without page-background rects");
        if let GridResult::Ok(table) = try_build_grid(items, group_rects, page, &is_page_bg, true) {
            return Some(table);
        }
    }

    None
}

/// Result from `try_build_grid` — distinguishes "few non-empty rows"
/// (fixable by excluding page-background rects) from other failures.
enum GridResult {
    Ok(Table),
    /// Grid was structurally valid but too few rows had content —
    /// likely caused by `propagate_merged_cells` collapsing text.
    FewNonEmptyRows,
    /// Grid failed for structural reasons (bad dimensions, low fill, etc.)
    Failed,
}

/// Core grid-building logic.  `skip_rects[i]` marks rects to exclude from
/// X-edge extraction and propagate_merged_cells (but they're still used for
/// fill-ratio checking).  When `strict` is true, apply higher thresholds
/// for non-empty rows and content density to avoid false positives.
fn try_build_grid(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
    skip_rects: &[bool],
    strict: bool,
) -> GridResult {
    // Extract unique X and Y edges from all rects.
    // Skip X edges from marked rects (page backgrounds add page-boundary
    // edges that create empty margin columns).
    let mut x_edges: Vec<f32> = Vec::new();
    let mut y_edges: Vec<f32> = Vec::new();
    for (i, &(x, y, w, h)) in group_rects.iter().enumerate() {
        if !skip_rects[i] {
            x_edges.push(x);
            x_edges.push(x + w);
        }
        y_edges.push(y);
        y_edges.push(y + h);
    }

    let x_edges = snap_edges(&x_edges, 6.0);
    let y_edges = snap_edges(&y_edges, 6.0);

    debug!(
        "  edges: {} x, {} y — grid {}x{}",
        x_edges.len(),
        y_edges.len(),
        y_edges.len().saturating_sub(1),
        x_edges.len().saturating_sub(1),
    );

    if x_edges.len() < 3 || y_edges.len() < 4 {
        debug!(
            "  rejected: {} x-edges, {} y-edges (need >=3, >=4)",
            x_edges.len(),
            y_edges.len()
        );
        return GridResult::Failed;
    }

    // Sort column edges left-to-right, row edges top-to-bottom (highest Y first for PDF)
    let mut col_edges = x_edges;
    col_edges.sort_by(|a, b| a.total_cmp(b));
    let mut row_edges = y_edges;
    row_edges.sort_by(|a, b| b.total_cmp(a));

    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;

    if num_cols < 2 || num_rows < 2 {
        return GridResult::Failed;
    }

    // Reject grids that are too large — form-style PDFs with scattered field
    // boxes produce huge sparse grids.  Statistical lookup tables (e.g. MWU,
    // chi-square) can legitimately have 20+ columns, so allow up to 25.
    if num_cols > 25 {
        debug!("  rejected: {} columns > 25", num_cols);
        return GridResult::Failed;
    }

    // Verify that cell-sized rects actually fill the grid
    // Count how many grid cells have a matching rect
    let mut filled_cells = 0u32;
    for row in 0..num_rows {
        let y_top = row_edges[row];
        let y_bot = row_edges[row + 1];
        for col in 0..num_cols {
            let x_left = col_edges[col];
            let x_right = col_edges[col + 1];
            // Check if any rect approximately covers this cell
            let cell_covered = group_rects.iter().any(|&(rx, ry, rw, rh)| {
                let tol = 6.0;
                rx <= x_left + tol
                    && (rx + rw) >= x_right - tol
                    && ry <= y_top + tol
                    && (ry + rh) >= y_bot - tol
            });
            if cell_covered {
                filled_cells += 1;
            }
        }
    }

    let total_cells = (num_cols * num_rows) as f32;
    let fill_ratio = filled_cells as f32 / total_cells;

    debug!(
        "  grid: {}x{} = {} cells, {} filled, ratio={:.2}",
        num_rows, num_cols, total_cells as u32, filled_cells, fill_ratio
    );

    // Require at least 30% of cells to be backed by rects
    if fill_ratio < 0.3 {
        debug!("  rejected: fill ratio {:.2} < 0.30", fill_ratio);
        return GridResult::Failed;
    }

    // Build table: assign text items to cells
    let (mut cells, item_indices) = assign_items_to_grid(items, &col_edges, &row_edges, page);

    // Consolidate vertically-merged cells: rects spanning multiple grid rows
    // should have their text collected into the first sub-row.
    // Skip for wide tables (>10 columns) where spanning rects are typically
    // background fills rather than true merged cells (e.g. statistical lookup
    // tables with row-grouping shading).
    if num_cols <= 10 {
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, group_rects, skip_rects);
    }

    // Compute column centers and row centers for the Table struct
    let columns: Vec<f32> = (0..num_cols)
        .map(|c| (col_edges[c] + col_edges[c + 1]) / 2.0)
        .collect();
    let rows: Vec<f32> = (0..num_rows)
        .map(|r| (row_edges[r] + row_edges[r + 1]) / 2.0)
        .collect();

    // Skip if no text was assigned
    if item_indices.is_empty() {
        debug!("  rejected: no text items assigned to grid");
        return GridResult::Failed;
    }

    // Skip tables with too few rows of content.
    // In strict mode (retry without page backgrounds), require at least 50%
    // of rows to have content to avoid false positives.
    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
        .count();
    let min_rows = if strict { num_rows / 2 } else { 2 };
    if non_empty_rows < min_rows {
        debug!(
            "  rejected: only {} non-empty rows (need {})",
            non_empty_rows, min_rows
        );
        return GridResult::FewNonEmptyRows;
    }

    // Content density check: reject tables where most cells are empty.
    // In strict mode, require 40% instead of 25%.
    let non_empty_cells = cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    let content_ratio = non_empty_cells as f32 / total_cells;
    let min_content = if strict { 0.40 } else { 0.25 };
    if content_ratio < min_content {
        debug!(
            "  rejected: content ratio {:.2} < {:.2} ({} non-empty / {} total)",
            content_ratio, min_content, non_empty_cells, total_cells as u32
        );
        return GridResult::Failed;
    }

    // In strict mode, reject tables where any single cell has very long text —
    // this indicates a paragraph was incorrectly captured in the grid.
    if strict {
        let max_cell_len = cells
            .iter()
            .flat_map(|row| row.iter())
            .map(|c| c.len())
            .max()
            .unwrap_or(0);
        if max_cell_len > 200 {
            debug!(
                "  rejected: max cell length {} > 200 (likely paragraph text)",
                max_cell_len
            );
            return GridResult::Failed;
        }
    }

    // Trim empty outer columns (rect edges beyond text), reject if any
    // interior column is empty — that indicates a bad grid.
    let first_non_empty = (0..num_cols).find(|&col| {
        cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()))
    });
    let last_non_empty = (0..num_cols).rev().find(|&col| {
        cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()))
    });
    let (first_col, last_col) = match (first_non_empty, last_non_empty) {
        (Some(f), Some(l)) if l > f => (f, l),
        _ => {
            debug!("  rejected: no content columns");
            return GridResult::Failed;
        }
    };
    // Check interior columns
    for col in first_col..=last_col {
        let col_has_content = cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()));
        if !col_has_content {
            debug!("  rejected: interior column {} is completely empty", col);
            return GridResult::Failed;
        }
    }
    // Trim outer empty columns
    let (columns, cells) = if first_col > 0 || last_col < num_cols - 1 {
        let trimmed_cols: Vec<f32> = columns[first_col..=last_col].to_vec();
        let trimmed_cells: Vec<Vec<String>> = cells
            .iter()
            .map(|row| row[first_col..=last_col].to_vec())
            .collect();
        debug!(
            "  trimmed {} empty outer columns ({}..={})",
            (num_cols - 1 - last_col + first_col),
            first_col,
            last_col
        );
        (trimmed_cols, trimmed_cells)
    } else {
        (columns, cells)
    };

    GridResult::Ok(Table::new(columns, rows, cells, item_indices))
}

/// Deduplicate nearby edge values within a tolerance, returning sorted unique edges.
pub(crate) fn snap_edges(values: &[f32], tolerance: f32) -> Vec<f32> {
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));

    let mut snapped: Vec<f32> = Vec::new();
    for &v in &sorted {
        if let Some(last) = snapped.last() {
            if (v - *last).abs() <= tolerance {
                continue; // Skip — too close to previous edge
            }
        }
        snapped.push(v);
    }
    snapped
}

/// Assign text items to grid cells defined by column/row edges.
///
/// Returns `(cells, item_indices)` where `cells[row][col]` is the cell text
/// and `item_indices` lists the original item indices that were consumed.
pub(crate) fn assign_items_to_grid(
    items: &[TextItem],
    col_edges: &[f32],
    row_edges: &[f32],
    page: u32,
) -> (Vec<Vec<String>>, Vec<usize>) {
    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;

    // Collect items per cell for proper sorting before joining
    let mut cell_items: Vec<Vec<Vec<(usize, &TextItem)>>> =
        vec![vec![Vec::new(); num_cols]; num_rows];
    let mut indices = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        if item.page != page {
            continue;
        }
        // Use item center for assignment
        let cx = item.x + item.width / 2.0;
        let cy = item.y;

        // Find column: cx must be between col_edges[c] and col_edges[c+1]
        let col = (0..num_cols).find(|&c| cx >= col_edges[c] - 2.0 && cx <= col_edges[c + 1] + 2.0);
        // Find row: cy must be between row_edges[r+1] (bottom) and row_edges[r] (top)
        let row = (0..num_rows).find(|&r| cy >= row_edges[r + 1] - 2.0 && cy <= row_edges[r] + 2.0);

        if let (Some(c), Some(r)) = (col, row) {
            cell_items[r][c].push((idx, item));
            indices.push(idx);
        }
    }

    // Build cell strings: sort items within each cell by Y descending then X ascending
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(num_rows);
    for row_items in &mut cell_items {
        let mut row_cells = Vec::with_capacity(num_cols);
        for col_items in row_items.iter_mut() {
            col_items.sort_by(|a, b| {
                b.1.y
                    .partial_cmp(&a.1.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        a.1.x
                            .partial_cmp(&b.1.x)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            let text = col_items
                .iter()
                .map(|(_, item)| item.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let text = remove_inner_delimiter_spaces(&text);
            row_cells.push(text);
        }
        cells.push(row_cells);
    }

    (cells, indices)
}

fn remove_inner_delimiter_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());

    for (i, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            let after_open =
                result.ends_with('(') || result.ends_with('[') || result.ends_with('{');
            let before_close = chars
                .get(i + 1)
                .is_some_and(|next| matches!(next, ')' | ']' | '}'));
            if after_open || before_close {
                continue;
            }
        }
        result.push(ch);
    }

    result
}

/// Consolidate text in vertically-merged cells.
///
/// When a single rect spans multiple grid rows (e.g. a "Classification" label
/// covering several price sub-rows), text ends up in only one sub-row while the
/// others have an empty cell.  This function detects such spans and moves all
/// text into the first sub-row, clearing the rest so that downstream
/// continuation-merge in `clean_table_cells` collapses sub-rows correctly.
fn propagate_merged_cells(
    cells: &mut [Vec<String>],
    col_edges: &[f32],
    row_edges: &[f32],
    group_rects: &[(f32, f32, f32, f32)],
    skip_rects: &[bool],
) {
    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;
    let tol = 6.0;

    for col in 0..num_cols {
        for (rect_idx, rect) in group_rects.iter().enumerate() {
            let (rx, ry, rw, rh) = *rect;

            // Skip rects flagged as page backgrounds — they span all rows
            // and would collapse all text into the first row.
            if skip_rects[rect_idx] {
                continue;
            }

            // Rect must cover this column
            if rx > col_edges[col] + tol || (rx + rw) < col_edges[col + 1] - tol {
                continue;
            }

            // Find first and last grid rows that the rect spans.
            //
            // Require a rect to actually overlap the row by more than `tol`
            // to count as a span. A "rect bottom ≤ row top + tol AND rect
            // top ≥ row bottom − tol" check gives false positives at shared
            // row boundaries — a rect whose top equals row N's bottom lies
            // entirely below the row but still passes the tolerance-slack
            // check, cascading body text from unrelated rows into one
            // merged cell.
            let spans = |r: usize| {
                let row_top = row_edges[r];
                let row_bot = row_edges[r + 1];
                let overlap = (row_top.min(ry + rh) - row_bot.max(ry)).max(0.0);
                overlap > tol
            };
            let first_row = (0..num_rows).find(|&r| spans(r));
            let last_row = (0..num_rows).rfind(|&r| spans(r));

            let (first, last) = match (first_row, last_row) {
                (Some(f), Some(l)) if l > f => (f, l),
                _ => continue, // Single row or no match — skip
            };

            // Collect all text from sub-rows within the merged range
            let mut combined = String::new();
            for row in cells.iter().take(last + 1).skip(first) {
                let text = row[col].trim();
                if !text.is_empty() {
                    if !combined.is_empty() {
                        combined.push(' ');
                    }
                    combined.push_str(text);
                }
            }

            // Place combined text in the first sub-row, clear the rest
            cells[first][col] = combined;
            for row in cells.iter_mut().take(last + 1).skip(first + 1) {
                row[col] = String::new();
            }
        }
    }
}

/// Check if rects form a row-stripe pattern (full-width horizontal bands).
///
/// Row-stripe shading uses rects that all share similar X position and width,
/// spanning the full table width. This produces only ~2 unique X-edges, which
/// makes normal grid detection fail (1-column grid).
fn is_row_stripe_pattern(rects: &[(f32, f32, f32, f32)]) -> bool {
    if rects.len() < 3 {
        return false;
    }

    let mut widths: Vec<f32> = rects.iter().map(|&(_, _, w, _)| w).collect();
    widths.sort_by(|a, b| a.total_cmp(b));
    let median_width = widths[widths.len() / 2];

    // Must be page-spanning (>200pt)
    if median_width <= 200.0 {
        return false;
    }

    // >75% of rects should have width within 10% of median
    let within_tolerance = rects
        .iter()
        .filter(|&&(_, _, w, _)| (w - median_width).abs() <= median_width * 0.10)
        .count();

    within_tolerance as f32 / rects.len() as f32 > 0.75
}

/// Detect a table from row-stripe rects by using rect Y-edges for rows
/// and text X-position clustering for columns.
fn detect_row_stripe_table(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    if !is_row_stripe_pattern(group_rects) {
        return None;
    }

    debug!(
        "  trying row-stripe detection ({} rects)",
        group_rects.len()
    );

    // Extract Y-edges from rects
    let mut y_edges: Vec<f32> = Vec::new();
    for &(_, y, _, h) in group_rects {
        y_edges.push(y);
        y_edges.push(y + h);
    }
    let y_edges = snap_edges(&y_edges, 6.0);

    if y_edges.len() < 4 {
        debug!("  row-stripe rejected: only {} y-edges", y_edges.len());
        return None;
    }

    // Sort row edges top-to-bottom (highest Y first for PDF)
    let mut row_edges = y_edges;
    row_edges.sort_by(|a, b| b.total_cmp(a));

    // Compute the bounding box of the stripe region for filtering items
    let y_top = row_edges[0];
    let y_bottom = *row_edges.last().unwrap();
    let x_left = group_rects
        .iter()
        .map(|&(x, _, _, _)| x)
        .reduce(f32::min)
        .unwrap();
    let x_right = group_rects
        .iter()
        .map(|&(x, _, w, _)| x + w)
        .reduce(f32::max)
        .unwrap();

    // Gather page items within the stripe region
    let page_items: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.page == page
                && item.y >= y_bottom - 2.0
                && item.y <= y_top + 2.0
                && item.x >= x_left - 5.0
                && item.x + item.width <= x_right + 5.0
        })
        .collect();

    if page_items.is_empty() {
        return None;
    }

    // Derive column boundaries from text X-position clustering.
    // Use a lower threshold than find_column_boundaries (which clamps at 25pt min)
    // since we already know this is a table from the rects and narrow columns
    // (e.g. row-number + date at 21pt gap) should stay separate.
    let columns = cluster_x_positions(&page_items, 15.0);

    if columns.len() < 2 {
        debug!(
            "  row-stripe rejected: only {} columns from text clustering",
            columns.len()
        );
        return None;
    }

    // Convert column centers to column edges (midpoints between adjacent, plus outer edges)
    let mut col_edges: Vec<f32> = Vec::with_capacity(columns.len() + 1);

    // Left edge: minimum item X minus small padding
    let min_x = page_items
        .iter()
        .map(|(_, i)| i.x)
        .reduce(f32::min)
        .unwrap();
    col_edges.push(min_x - 5.0);

    // Midpoints between adjacent column centers
    for pair in columns.windows(2) {
        col_edges.push((pair[0] + pair[1]) / 2.0);
    }

    // Right edge: maximum item right edge plus small padding
    let max_x_right = page_items
        .iter()
        .map(|(_, i)| i.x + i.width)
        .reduce(f32::max)
        .unwrap();
    col_edges.push(max_x_right + 5.0);

    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;

    debug!(
        "  row-stripe grid: {}x{} ({} col edges, {} row edges)",
        num_rows,
        num_cols,
        col_edges.len(),
        row_edges.len()
    );

    // Assign items to grid
    let (cells, item_indices) = assign_items_to_grid(items, &col_edges, &row_edges, page);

    if item_indices.is_empty() {
        debug!("  row-stripe rejected: no items assigned");
        return None;
    }

    // Validate: >=2 non-empty rows
    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
        .count();
    if non_empty_rows < 2 {
        debug!(
            "  row-stripe rejected: only {} non-empty rows",
            non_empty_rows
        );
        return None;
    }

    // Content density: >=25%
    let total_cells = (num_cols * num_rows) as f32;
    let non_empty_cells = cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    let content_ratio = non_empty_cells as f32 / total_cells;
    if content_ratio < 0.40 {
        debug!(
            "  row-stripe rejected: content ratio {:.2} < 0.40",
            content_ratio
        );
        return None;
    }

    // Reject if any cell has excessive text — layout background rects (sidebar,
    // header, section bands) produce "cells" that contain paragraphs of body text.
    // Real alternating-row-stripe data tables have short cell content.
    let max_cell_len = cells
        .iter()
        .flat_map(|row| row.iter())
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    // Allow longer cells for multi-column tables (descriptions in one column
    // are common). Narrow grids with giant cells are usually layout
    // backgrounds — but only when the row count is also small. A 4+-row
    // key/value table with one descriptive column reads as a real table
    // on every other gate, so don't reject it on cell length alone.
    let max_allowed = if num_cols >= 3 { 2000 } else { 500 };
    if max_cell_len > max_allowed && non_empty_rows < 4 {
        debug!(
            "  row-stripe rejected: max cell length {} > {} (layout background, {} rows)",
            max_cell_len, max_allowed, non_empty_rows
        );
        return None;
    }

    // Trim empty outer columns, reject if interior columns are empty
    let first_col = (0..num_cols).find(|&col| {
        cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()))
    });
    let last_col = (0..num_cols).rev().find(|&col| {
        cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()))
    });
    let (first_col, last_col) = match (first_col, last_col) {
        (Some(f), Some(l)) if l > f => (f, l),
        _ => return None,
    };
    for col in first_col..=last_col {
        let col_has_content = cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()));
        if !col_has_content {
            debug!("  row-stripe rejected: interior column {} is empty", col);
            return None;
        }
    }
    let (col_edges, cells) = if first_col > 0 || last_col < num_cols - 1 {
        let new_edges: Vec<f32> = col_edges[first_col..=last_col + 1].to_vec();
        let new_cells: Vec<Vec<String>> = cells
            .iter()
            .map(|row| row[first_col..=last_col].to_vec())
            .collect();
        (new_edges, new_cells)
    } else {
        (col_edges, cells)
    };
    let num_cols = col_edges.len() - 1;
    if row_stripe_is_sparse_prose_outline(&cells) {
        debug!("  row-stripe rejected: sparse outline/prose continuation shape");
        return None;
    }
    if has_dominant_prose_cell(&cells) {
        debug!("  row-stripe rejected: dominant prose cell (chart/figure region over body text)");
        return None;
    }

    let column_centers: Vec<f32> = (0..num_cols)
        .map(|c| (col_edges[c] + col_edges[c + 1]) / 2.0)
        .collect();
    let row_centers: Vec<f32> = (0..num_rows)
        .map(|r| (row_edges[r] + row_edges[r + 1]) / 2.0)
        .collect();

    debug!(
        "  row-stripe table accepted: {}x{}, {:.0}% density",
        num_rows,
        num_cols,
        content_ratio * 100.0
    );

    Some(Table::new(column_centers, row_centers, cells, item_indices))
}

/// Detect a grid that swallowed body text instead of tabular data.
///
/// Charts (bar graphs, axis gridlines) emit fields of drawing rects that can
/// pass the row-stripe shape test; the resulting "table" then captures the
/// page's prose. The signature: one cell holds an entire paragraph — ≥60 words
/// AND at least a third of all words in the table.
///
/// There is deliberately no row-count exemption. A small table whose single
/// long cell dominates its word count is indistinguishable by content from a
/// phantom grid over body text, and across the regression corpora every such
/// grid observed has been swallowed prose, never a real note table. The costs
/// are also asymmetric: rejecting a real table degrades it to readable prose,
/// while accepting a phantom scrambles the page into Y-interleaved cells.
/// Larger legitimate tables are safe because the one-third-of-total threshold
/// scales with table size.
fn has_dominant_prose_cell(cells: &[Vec<String>]) -> bool {
    let mut total_words = 0usize;
    let mut max_cell_words = 0usize;
    for row in cells {
        for cell in row {
            let words = cell.split_whitespace().count();
            total_words += words;
            max_cell_words = max_cell_words.max(words);
        }
    }
    max_cell_words >= 60 && max_cell_words * 3 >= total_words
}

fn row_stripe_is_sparse_prose_outline(cells: &[Vec<String>]) -> bool {
    let Some(num_cols) = cells.first().map(|row| row.len()) else {
        return false;
    };
    if num_cols != 2 || cells.len() < 4 {
        return false;
    }

    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .count();
    if non_empty_rows < 4 {
        return false;
    }

    let mut col_counts = [0usize; 2];
    for row in cells {
        for (idx, cell) in row.iter().enumerate() {
            if !cell.trim().is_empty() {
                col_counts[idx] += 1;
            }
        }
    }

    let (sparse_col, dense_col) = if col_counts[0] <= col_counts[1] {
        (0usize, 1usize)
    } else {
        (1usize, 0usize)
    };
    let sparse_count = col_counts[sparse_col];
    let dense_count = col_counts[dense_col];
    if sparse_count * 2 >= non_empty_rows || dense_count * 3 < non_empty_rows * 2 {
        return false;
    }

    let blank_sparse_dense_rows = cells
        .iter()
        .filter(|row| row[sparse_col].trim().is_empty() && !row[dense_col].trim().is_empty())
        .count();
    if blank_sparse_dense_rows * 2 < non_empty_rows {
        return false;
    }

    let long_dense_cells = cells
        .iter()
        .filter(|row| row[dense_col].split_whitespace().count() >= 6)
        .count();
    long_dense_cells * 2 >= dense_count
}

/// Remove repeated page-scale fills from a chart-like cluster so the actual
/// cell/bar geometry can be evaluated independently. A small number of
/// coincident origin frames may be meaningful table structure, so repetition
/// only becomes normalization evidence when it dominates the cluster.
fn without_dominant_page_backgrounds(rects: &[(f32, f32, f32, f32)]) -> Vec<(f32, f32, f32, f32)> {
    let x_max = rects
        .iter()
        .map(|&(x, _, width, _)| x + width)
        .fold(0.0_f32, f32::max);
    let y_max = rects
        .iter()
        .map(|&(_, y, _, height)| y + height)
        .fold(0.0_f32, f32::max);
    let is_page_scale = |&(x, y, width, height): &(f32, f32, f32, f32)| {
        x < 5.0 && y < 5.0 && width >= x_max * 0.9 && height >= y_max * 0.9
    };

    if rects.iter().filter(|rect| is_page_scale(rect)).count()
        < DOMINANT_PAGE_BACKGROUND_MIN_REPETITIONS
    {
        return rects.to_vec();
    }

    rects
        .iter()
        .filter(|rect| !is_page_scale(rect))
        .copied()
        .collect()
}

/// Repeated rows of touching cell rectangles are stronger table evidence
/// than the bar-length variation used by the chart detector.
///
/// Ruled tables with wrapped labels naturally have variable row heights, and
/// numeric-heavy cells can otherwise resemble horizontal or vertical bars.
/// Require several rows to repeat a shared edge schema before overriding the
/// chart hypothesis so sparse plots and independent bars remain unaffected.
fn is_repeated_cell_grid(group_rects: &[(f32, f32, f32, f32)]) -> bool {
    type RowGroup = (f32, f32, Vec<(f32, f32)>);

    const ROW_EDGE_TOLERANCE: f32 = 3.0;
    const MIN_GRID_ROWS: usize = 4;
    const MIN_CELLS_PER_ROW: usize = 3;

    if group_rects.len() < MIN_GRID_ROWS * MIN_CELLS_PER_ROW {
        return false;
    }

    let mut row_groups: Vec<RowGroup> = Vec::new();
    for &(x, y, width, height) in group_rects {
        if width < 5.0 || height < 5.0 {
            continue;
        }
        let top = y + height;
        if let Some((_, _, cells)) = row_groups.iter_mut().find(|(bottom, row_top, _)| {
            (y - *bottom).abs() <= ROW_EDGE_TOLERANCE
                && (top - *row_top).abs() <= ROW_EDGE_TOLERANCE
        }) {
            cells.push((x, x + width));
        } else {
            row_groups.push((y, top, vec![(x, x + width)]));
        }
    }

    let mut row_schemas = Vec::new();
    for (_, _, mut cells) in row_groups {
        if cells.len() < MIN_CELLS_PER_ROW {
            continue;
        }
        let mut widths: Vec<f32> = cells.iter().map(|&(left, right)| right - left).collect();
        widths.sort_by(f32::total_cmp);
        let median_width = widths[widths.len() / 2];
        cells.retain(|&(left, right)| right - left <= median_width * 2.5);
        cells.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        });
        cells.dedup_by(|left, right| {
            (left.0 - right.0).abs() <= ROW_EDGE_TOLERANCE
                && (left.1 - right.1).abs() <= ROW_EDGE_TOLERANCE
        });
        if cells.len() < MIN_CELLS_PER_ROW
            || cells
                .windows(2)
                .any(|pair| pair[1].0 > pair[0].1 + ROW_EDGE_TOLERANCE)
        {
            continue;
        }
        let edges: Vec<f32> = cells
            .iter()
            .flat_map(|&(left, right)| [left, right])
            .collect();
        let schema = snap_edges(&edges, ROW_EDGE_TOLERANCE);
        if schema.len() > MIN_CELLS_PER_ROW {
            row_schemas.push(schema);
        }
    }
    if row_schemas.len() < MIN_GRID_ROWS {
        return false;
    }

    let reference = row_schemas
        .iter()
        .max_by_key(|schema| schema.len())
        .expect("grid rows are non-empty");
    row_schemas
        .iter()
        .filter(|schema| {
            let comparable_edges = reference.len().min(schema.len());
            let matched_edges = schema
                .iter()
                .filter(|edge| {
                    reference
                        .iter()
                        .any(|reference_edge| (*edge - *reference_edge).abs() <= ROW_EDGE_TOLERANCE)
                })
                .count();
            matched_edges > MIN_CELLS_PER_ROW && matched_edges * 4 >= comparable_edges * 3
        })
        .count()
        >= MIN_GRID_ROWS
}

fn repeated_cell_grid_overrides_bar_hypothesis(group_rects: &[(f32, f32, f32, f32)]) -> bool {
    is_repeated_cell_grid(group_rects)
        && without_dominant_page_backgrounds(group_rects).len() == group_rects.len()
}

/// Detect horizontal segmented stacks from aligned rows of touching rects.
///
/// Category rows must have visible gutters and data-varying internal segment
/// boundaries, unlike the stable boundaries of a ruled table.
struct SegmentedBarGeometry {
    bounds: (f32, f32, f32, f32),
    row_bands: Vec<(f32, f32)>,
}

fn segmented_stacked_bar_geometry(
    group_rects: &[(f32, f32, f32, f32)],
) -> Option<SegmentedBarGeometry> {
    type BarRow = (f32, f32, Vec<(f32, f32)>);

    const EDGE_TOLERANCE: f32 = 3.0;
    const MIN_ROWS: usize = 4;
    const MIN_SEGMENTS: usize = 3;

    let mut rows: Vec<BarRow> = Vec::new();
    for &(x, y, width, height) in group_rects {
        if width < 5.0 || height < 5.0 {
            continue;
        }
        let top = y + height;
        if let Some((_, _, segments)) = rows.iter_mut().find(|(bottom, row_top, _)| {
            (y - *bottom).abs() <= EDGE_TOLERANCE && (top - *row_top).abs() <= EDGE_TOLERANCE
        }) {
            segments.push((x, x + width));
        } else {
            rows.push((y, top, vec![(x, x + width)]));
        }
    }

    rows.retain_mut(|(_, _, segments)| {
        segments.sort_by(|left, right| left.0.total_cmp(&right.0));
        segments.len() >= MIN_SEGMENTS
            && segments
                .windows(2)
                .all(|pair| (pair[1].0 - pair[0].1).abs() <= EDGE_TOLERANCE)
    });
    if rows.len() < MIN_ROWS {
        return None;
    }
    rows.sort_by(|left, right| left.0.total_cmp(&right.0));

    // Table rows normally share borders. Horizontal stacked bars instead
    // leave a visible gutter between category rows.
    if rows.windows(2).any(|pair| {
        let shorter_height = (pair[0].1 - pair[0].0).min(pair[1].1 - pair[1].0);
        pair[1].0 - pair[0].1 < (shorter_height * 0.25).max(2.0)
    }) {
        return None;
    }

    // At least two rows must move an internal segment boundary. Stable
    // boundaries across every row are stronger evidence for a ruled table.
    let reference_edges: Vec<f32> = rows[0]
        .2
        .iter()
        .take(rows[0].2.len() - 1)
        .map(|segment| segment.1)
        .collect();
    let drifting_rows = rows
        .iter()
        .skip(1)
        .filter(|(_, _, segments)| {
            let edges: Vec<f32> = segments
                .iter()
                .take(segments.len() - 1)
                .map(|segment| segment.1)
                .collect();
            edges.len() == reference_edges.len()
                && edges
                    .iter()
                    .zip(&reference_edges)
                    .any(|(edge, reference)| (edge - reference).abs() > EDGE_TOLERANCE)
        })
        .count();
    if drifting_rows < 2 {
        return None;
    }

    let left = rows
        .iter()
        .flat_map(|row| &row.2)
        .map(|segment| segment.0)
        .reduce(f32::min)?;
    let right = rows
        .iter()
        .flat_map(|row| &row.2)
        .map(|segment| segment.1)
        .reduce(f32::max)?;
    let bottom = rows.iter().map(|row| row.0).reduce(f32::min)?;
    let top = rows.iter().map(|row| row.1).reduce(f32::max)?;
    let row_bands = rows.iter().map(|row| (row.0, row.1)).collect();
    Some(SegmentedBarGeometry {
        bounds: (left, bottom, right, top),
        row_bands,
    })
}

/// Category labels beside multiple bar rows are independent chart evidence:
/// numeric table text stays inside its cells, regardless of whether the table
/// has an outer border or extra padding.
fn has_external_segmented_bar_labels(
    items: &[TextItem],
    page: u32,
    geometry: &SegmentedBarGeometry,
) -> bool {
    const LABEL_EDGE_TOLERANCE: f32 = 3.0;
    const LABEL_CLAIM_PAD: f32 = 20.0;

    let (content_left, _, content_right, _) = geometry.bounds;
    let labeled_rows = geometry
        .row_bands
        .iter()
        .filter(|&&(row_bottom, row_top)| {
            items.iter().any(|item| {
                if item.page != page || item.text.trim().is_empty() {
                    return false;
                }
                let item_left = item.x.min(item.x + item.width);
                let item_right = item.x.max(item.x + item.width);
                let item_center_x = (item_left + item_right) / 2.0;
                let item_center_y = item.y + item.height / 2.0;
                let beside_stack = (item_center_x <= content_left + LABEL_EDGE_TOLERANCE
                    && item_center_x >= content_left - LABEL_CLAIM_PAD
                    && item_left < content_left)
                    || (item_center_x >= content_right - LABEL_EDGE_TOLERANCE
                        && item_center_x <= content_right + LABEL_CLAIM_PAD
                        && item_right > content_right);
                beside_stack
                    && item_center_y >= row_bottom - LABEL_EDGE_TOLERANCE
                    && item_center_y <= row_top + LABEL_EDGE_TOLERANCE
            })
        })
        .count();

    labeled_rows >= 2 && labeled_rows * 2 >= geometry.row_bands.len()
}

/// Recognize filled vertical or horizontal bars whose geometry and labels are
/// data-driven rather than uniform table cells.
fn has_chart_bar_signature(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> bool {
    let numeric_or_empty = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        let inside: Vec<&TextItem> = items
            .iter()
            .filter(|it| {
                let cx = it.x + it.width / 2.0;
                it.page == page && cx >= rx && cx <= rx + rw && it.y >= ry && it.y <= ry + rh
            })
            .collect();
        // Any number of numeric data labels is chart-like; a single run of
        // word text inside means a table cell.
        inside.iter().all(|it| {
            let t = it.text.trim();
            let data = t
                .chars()
                .filter(|c| c.is_ascii_digit() || ",.%-".contains(*c))
                .count();
            t.is_empty() || data * 2 >= t.chars().count()
        })
    };

    // Bars: the dominant equal-width family, arranged in >=2 spaced columns
    // (inter-column gap >= half a bar width — table cell rects touch), with
    // data-driven height variation (checkbox/cell grids are uniform).
    // Mirrored predicate catches horizontal bar charts.
    let bar_family = |pos: fn(&(f32, f32, f32, f32)) -> f32,
                      breadth: fn(&(f32, f32, f32, f32)) -> f32,
                      length: fn(&(f32, f32, f32, f32)) -> f32,
                      along: fn(&(f32, f32, f32, f32)) -> f32| {
        group_rects.iter().any(|anchor| {
            let bw = breadth(anchor);
            if bw <= 0.0 {
                return false;
            }
            let family: Vec<&(f32, f32, f32, f32)> = group_rects
                .iter()
                .filter(|r| {
                    (breadth(r) - bw).abs() <= (bw * 0.1).max(2.0)
                        && length(r) > 0.0
                        && length(r) < bw * 20.0
                })
                .collect();
            if family.len() < 4 {
                return false;
            }
            // Distinct positions along the axis (bar columns).
            let mut positions: Vec<f32> = Vec::new();
            for r in &family {
                let p = pos(r);
                if !positions.iter().any(|&q| (q - p).abs() <= 2.0) {
                    positions.push(p);
                }
            }
            if positions.len() < 2 {
                return false;
            }
            positions.sort_by(|a, b| a.total_cmp(b));
            let min_gap = positions
                .windows(2)
                .map(|w| w[1] - w[0] - bw)
                .fold(f32::INFINITY, f32::min);
            if min_gap < bw * 0.5 {
                return false;
            }
            // Data-driven variation along the bar direction.
            let len_min = family
                .iter()
                .map(|r| length(r))
                .fold(f32::INFINITY, f32::min);
            let len_max = family
                .iter()
                .map(|r| length(r))
                .fold(f32::NEG_INFINITY, f32::max);
            if len_max < len_min * 1.3 {
                return false;
            }
            // Grid rows disguise as bars: a table's cell rects have same-y,
            // same-height partners in other columns (uniform row heights).
            // Chart segments start where the previous datum ended, so their
            // extents rarely pair up across positions.
            let matched = family
                .iter()
                .filter(|r| {
                    family.iter().any(|s| {
                        (pos(s) - pos(r)).abs() > 2.0
                            && (along(s) - along(r)).abs() <= 3.0
                            && (length(s) - length(r)).abs() <= 3.0
                    })
                })
                .count();
            if matched * 5 >= family.len() * 3 {
                return false;
            }
            family.iter().filter(|r| numeric_or_empty(***r)).count() * 3 >= family.len() * 2
        })
    };

    // vertical bars: position/breadth = x/width, length = height, along = y
    bar_family(|r| r.0, |r| r.2, |r| r.3, |r| r.1)
        // horizontal bars: position/breadth = y/height, length = width, along = x
        || bar_family(|r| r.1, |r| r.3, |r| r.2, |r| r.0)
}

fn is_chart_bar_cluster(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> bool {
    let has_bar_signature = has_chart_bar_signature(items, group_rects, page);

    // A segmented horizontal chart can share most of its edges across rows.
    // Row-aligned category labels outside the stack distinguish it from a
    // numeric table without depending on whether either shape has a frame.
    if has_bar_signature {
        if let Some(geometry) = segmented_stacked_bar_geometry(group_rects) {
            if has_external_segmented_bar_labels(items, page, &geometry) {
                return true;
            }
        }
    }
    if repeated_cell_grid_overrides_bar_hypothesis(group_rects) {
        return false;
    }

    has_bar_signature
}

fn detect_row_stripe_table_from_cell_rects(
    items: &[TextItem],
    group_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    if group_rects.len() < 6 {
        return None;
    }

    // Extract Y-edges from rects
    let mut y_edges: Vec<f32> = Vec::new();
    for &(_, y, _, h) in group_rects {
        y_edges.push(y);
        y_edges.push(y + h);
    }
    let y_edges = snap_edges(&y_edges, 6.0);

    // If rect Y-edges are insufficient for row structure, use the rect
    // bounding box to scope items and derive rows from text Y-positions.
    let row_edges = if y_edges.len() >= 4 {
        let mut edges = y_edges;
        edges.sort_by(|a, b| b.total_cmp(a));
        edges
    } else {
        // Fall back: gather items in the rect region and cluster by Y
        let y_min = y_edges.first().copied().unwrap_or(0.0);
        let y_max = y_edges.last().copied().unwrap_or(0.0);
        let x_min = group_rects
            .iter()
            .map(|r| r.0)
            .reduce(f32::min)
            .unwrap_or(0.0);
        let x_max = group_rects
            .iter()
            .map(|r| r.0 + r.2)
            .reduce(f32::max)
            .unwrap_or(0.0);
        let region_items: Vec<&TextItem> = items
            .iter()
            .filter(|i| {
                i.page == page
                    && i.y >= y_min - 5.0
                    && i.y <= y_max + 5.0
                    && i.x >= x_min - 5.0
                    && i.x <= x_max + 5.0
            })
            .collect();
        if region_items.len() < 4 {
            return None;
        }
        // Cluster Y positions using median font height as threshold
        let median_h = {
            let mut hs: Vec<f32> = region_items.iter().map(|i| i.height).collect();
            hs.sort_by(|a, b| a.total_cmp(b));
            hs[hs.len() / 2]
        };
        let mut ys: Vec<f32> = region_items.iter().map(|i| i.y).collect();
        ys.sort_by(|a, b| b.total_cmp(a));
        let mut edges = Vec::new();
        let threshold = median_h * 0.8;
        let mut cluster_start = ys[0];
        let mut cluster_sum = ys[0];
        let mut cluster_count = 1.0f32;
        for &y in &ys[1..] {
            if (cluster_sum / cluster_count - y).abs() > threshold {
                let center = cluster_sum / cluster_count;
                edges.push(center + median_h * 0.5);
                edges.push(center - median_h * 0.5);
                cluster_start = y;
                cluster_sum = y;
                cluster_count = 1.0;
            } else {
                cluster_sum += y;
                cluster_count += 1.0;
            }
        }
        let center = cluster_sum / cluster_count;
        edges.push(center + median_h * 0.5);
        edges.push(center - median_h * 0.5);
        let _ = cluster_start; // suppress unused warning
        edges = snap_edges(&edges, 3.0);
        edges.sort_by(|a, b| b.total_cmp(a));
        if edges.len() < 4 {
            return None;
        }
        edges
    };

    // Compute bounding box from non-full-page rects
    let median_h = {
        let mut heights: Vec<f32> = group_rects.iter().map(|&(_, _, _, h)| h).collect();
        heights.sort_by(|a, b| a.total_cmp(b));
        heights[heights.len() / 2]
    };
    let content_rects: Vec<_> = group_rects
        .iter()
        .filter(|&&(_, _, _, h)| h < median_h * 10.0)
        .collect();
    if content_rects.is_empty() {
        return None;
    }

    let x_left = content_rects
        .iter()
        .map(|&&(x, _, _, _)| x)
        .reduce(f32::min)?;
    let x_right = content_rects
        .iter()
        .map(|&&(x, _, w, _)| x + w)
        .reduce(f32::max)?;
    let y_top = row_edges[0];
    let y_bottom = *row_edges.last()?;

    // Gather items within the rect region
    let page_items: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.page == page
                && item.y >= y_bottom - 2.0
                && item.y <= y_top + 2.0
                && item.x >= x_left - 5.0
                && item.x + item.width <= x_right + 5.0
        })
        .collect();

    if page_items.is_empty() {
        return None;
    }

    // Derive columns from text X-position clustering, but prefer rect
    // X-edges when they already provide a tighter scaffold.  Some PDFs draw
    // only the row-index cells in the body plus a full header row; that is
    // not dense enough for `try_build_grid`, but the header rects still define
    // the real columns.  Text starts inside wide cells can otherwise split the
    // table into spurious sub-columns.
    let columns = cluster_x_positions(&page_items, 15.0);
    let text_col_edges = if columns.len() >= 2 {
        let mut edges: Vec<f32> = Vec::with_capacity(columns.len() + 1);
        let min_x = page_items.iter().map(|(_, i)| i.x).reduce(f32::min)?;
        edges.push(min_x - 5.0);
        for pair in columns.windows(2) {
            edges.push((pair[0] + pair[1]) / 2.0);
        }
        let max_x_right = page_items
            .iter()
            .map(|(_, i)| i.x + i.width)
            .reduce(f32::max)?;
        edges.push(max_x_right + 5.0);
        Some(edges)
    } else {
        None
    };

    let rect_col_edges = {
        let mut x_vals = Vec::with_capacity(content_rects.len() * 2);
        for &&(x, _, w, _) in &content_rects {
            x_vals.push(x);
            x_vals.push(x + w);
        }
        let mut edges = snap_edges(&x_vals, 6.0);
        edges.sort_by(|a, b| a.total_cmp(b));
        if (3..=26).contains(&edges.len()) {
            Some(edges)
        } else {
            None
        }
    };

    // For wired-grid tables whose header text is centered/right-aligned but
    // whose data is left-aligned, cluster_x_positions can drop the header-only
    // x-cluster in its singleton-filter pass and merge adjacent data clusters
    // when the gap is below threshold, losing a column. Rect borders are
    // ground truth in that case — but only when each rect column actually
    // holds text. Decorative or background rects (prose laid out in a frame,
    // cell-fill rects with extra borders) can produce more rect-derived
    // columns than the text supports; preferring rects there would split a
    // logical column into spurious sub-columns.
    let rect_cols_match_text = match (&rect_col_edges, &text_col_edges) {
        (Some(rect_edges), _) if rect_edges.len() >= 4 => {
            let num_rect_cols = rect_edges.len() - 1;
            let mut col_item_counts = vec![0usize; num_rect_cols];
            for (_, item) in &page_items {
                let cx = item.x + item.width / 2.0;
                for c in 0..num_rect_cols {
                    if cx >= rect_edges[c] - 2.0 && cx <= rect_edges[c + 1] + 2.0 {
                        col_item_counts[c] += 1;
                        break;
                    }
                }
            }
            // Require every rect column to hold multiple text items. A rect
            // column with no (or only one) item is decorative or the rect grid
            // is detecting a spurious column the data does not need; in those
            // cases the old text-cluster preference is the safer fallback.
            col_item_counts.iter().all(|&n| n >= 2)
        }
        _ => false,
    };

    let (col_edges, columns_from_text) = match (rect_col_edges, text_col_edges) {
        (Some(rect_edges), text_edges_opt) if rect_cols_match_text => {
            debug!(
                "  cell-rect using {} rect-derived columns (text clusters: {}; rect cols well-distributed)",
                rect_edges.len() - 1,
                text_edges_opt
                    .as_ref()
                    .map(|e| (e.len() - 1) as i32)
                    .unwrap_or(-1)
            );
            (rect_edges, false)
        }
        (Some(rect_edges), Some(text_edges)) if rect_edges.len() <= text_edges.len() => {
            debug!(
                "  cell-rect using {} rect-derived columns over {} text clusters",
                rect_edges.len() - 1,
                text_edges.len() - 1
            );
            (rect_edges, false)
        }
        (_, Some(text_edges)) => (text_edges, true),
        (Some(rect_edges), None) => (rect_edges, false),
        (None, None) => {
            debug!(
                "  cell-rect rejected: only {} columns from text clustering",
                columns.len()
            );
            return None;
        }
    };

    if col_edges.len() < 3 {
        return None;
    }

    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;

    debug!(
        "  cell-rect table: {}x{} from {} rects, {} items",
        num_rows,
        num_cols,
        group_rects.len(),
        page_items.len()
    );

    let (mut cells, item_indices) = assign_items_to_grid(items, &col_edges, &row_edges, page);

    if item_indices.is_empty() {
        return None;
    }

    let mut row_edges = row_edges;
    let (collapsed_cells, collapsed_row_edges, collapsed_rows) =
        collapse_multiline_description_rows(cells, row_edges, &col_edges);
    let has_wrapped_description_rows = collapsed_rows > 0;
    cells = collapsed_cells;
    row_edges = collapsed_row_edges;
    if collapsed_rows > 0 {
        debug!(
            "  cell-rect collapsed {} wrapped description rows",
            collapsed_rows
        );
    }

    // Validate: >=2 non-empty rows, >=25% density
    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
        .count();
    if non_empty_rows < 2 {
        debug!(
            "  cell-rect rejected: only {} non-empty rows",
            non_empty_rows
        );
        return None;
    }

    let num_rows = cells.len();
    let total_cells = (num_cols * num_rows) as f32;
    let non_empty_cells = cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    let density = if total_cells > 0.0 {
        non_empty_cells as f32 / total_cells
    } else {
        0.0
    };
    if density < 0.25 {
        debug!(
            "  cell-rect rejected: density {:.0}% < 25%",
            density * 100.0
        );
        return None;
    }

    // Reject tables with paragraph-length cells — typically layout
    // backgrounds (sidebars, banners) where a single big rectangle
    // contains a wall of prose.  Spare multi-row key/value tables where
    // the value column is a multi-bullet description: those pass every
    // other gate and shouldn't get killed on cell length alone.
    let max_cell_len = cells
        .iter()
        .flat_map(|row| row.iter())
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    if max_cell_len > 500 && non_empty_rows < 4 {
        debug!(
            "  cell-rect rejected: max cell length {} > 500 ({} rows, layout background)",
            max_cell_len, non_empty_rows
        );
        return None;
    }

    // Reject wildly disproportionate grids (e.g. 68x6 from decorative rects)
    if num_rows > 20 && num_cols < 4 {
        debug!(
            "  cell-rect rejected: disproportionate grid {}x{}",
            num_rows, num_cols
        );
        return None;
    }

    // Reject "tables" that are actually prose in a framed region.
    // Columns here come from text X-position clustering; when prose wraps
    // inside a bounding-box rect (e.g. chat-transcript figures, two-column
    // legal-text blocks in forms) the word-boundary gaps cluster into
    // spurious columns, and the resulting cells hold sentence fragments
    // riddled with common English function words.
    //
    // Apply at any column count >= 2. The 2-col case is the bite — a
    // paragraph wrapped into 2 justified columns produces the same
    // surface signal as a real "label / value" table in the
    // well-distributed-cols check (both cols populated), so we need a
    // content-based signal to tell them apart.
    //
    // Layered checks combine after the 20%-of-cells prose-word
    // trigger fires:
    //   (a) Long-cell content: prose-in-a-frame averages ~70-100 chars
    //       per non-empty cell (sentence fragments); real data tables
    //       are typically <30 chars, occasionally up to ~55 for
    //       descriptive 4-col tables. The 65-char threshold cleanly
    //       separates them on observed fixtures (accessory_building
    //       prose=74 chars, upstage data=53, greencomp=20). This
    //       overrides the well-distributed relaxation — long cells
    //       are the strongest prose signal even when both cols are
    //       populated.
    //   (b) Two-column text-only scaffold: when both columns were inferred
    //       from text starts rather than rect edges, prose fragments can look
    //       perfectly balanced. Require rect evidence for this relaxed shape.
    //   (c) Well-distributed columns: ≥75% of cols hold ≥2 non-empty
    //       cells. Catches the prose-paragraph-as-many-cols shape
    //       while admitting real "label / value / description /
    //       benefit"-style tables.
    if num_cols >= 2 {
        const PROSE_WORDS: &[&str] = &[
            "a", "an", "the", "of", "to", "is", "was", "are", "were", "be", "been", "in", "on",
            "at", "with", "for", "by", "as", "and", "or", "but", "this", "that", "these", "those",
            "from", "into", "has", "have", "had", "not", "don't", "doesn't", "it's", "its", "it",
            "i", "me", "my", "we", "our", "us", "you", "your", "they", "them", "their", "he",
            "she", "his", "her",
        ];
        let mut prose_cells = 0usize;
        let mut counted = 0usize;
        let mut total_chars = 0usize;
        for row in &cells {
            for cell in row {
                let t = cell.trim();
                if t.is_empty() {
                    continue;
                }
                counted += 1;
                total_chars += t.chars().count();
                let lower = t.to_ascii_lowercase();
                let has_prose_word = lower
                    .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
                    .any(|w| PROSE_WORDS.contains(&w));
                if has_prose_word {
                    prose_cells += 1;
                }
            }
        }
        if counted > 0 && prose_cells * 5 >= counted {
            // (a) Long-cell content: overrides the well-distributed
            // relaxation. The 2-col prose-in-a-frame case populates
            // both cols (passes well-distributed) but every cell
            // holds a sentence fragment, so mean cell length is the
            // discriminator.
            const PROSE_MEAN_CHAR_THRESHOLD: usize = 65;
            let mean_chars = total_chars / counted;
            if mean_chars > PROSE_MEAN_CHAR_THRESHOLD && !has_wrapped_description_rows {
                debug!(
                    "  cell-rect rejected: prose-in-frame, mean non-empty cell {} chars > {} (prose words {}/{})",
                    mean_chars, PROSE_MEAN_CHAR_THRESHOLD, prose_cells, counted
                );
                return None;
            } else if mean_chars > PROSE_MEAN_CHAR_THRESHOLD {
                debug!(
                    "  cell-rect prose check relaxed: wrapped description rows, mean {} chars (prose words {}/{})",
                    mean_chars, prose_cells, counted
                );
            }

            // (b) Two text-derived columns are not enough vector evidence once
            // the content looks prose-like. Real 2-col rect tables still pass
            // when the column scaffold comes from drawn cell geometry.
            if columns_from_text && num_cols == 2 {
                debug!(
                    "  cell-rect rejected: prose-in-frame with text-derived 2-col scaffold (mean {} chars, prose words {}/{})",
                    mean_chars, prose_cells, counted
                );
                return None;
            }

            // (c) Well-distributed columns.
            let filled_cols = (0..num_cols)
                .filter(|&c| {
                    cells
                        .iter()
                        .filter(|row| {
                            !row.get(c)
                                .map(String::as_str)
                                .unwrap_or("")
                                .trim()
                                .is_empty()
                        })
                        .count()
                        >= 2
                })
                .count();
            let well_distributed = filled_cols * 4 >= num_cols * 3;
            if !well_distributed {
                debug!(
                    "  cell-rect rejected: {}/{} cells contain prose function words — likely prose ({}/{} cols filled, mean {} chars)",
                    prose_cells, counted, filled_cols, num_cols, mean_chars
                );
                return None;
            }
            debug!(
                "  cell-rect prose check relaxed: {}/{} cols filled, mean {} chars — table-with-description-col",
                filled_cols, num_cols, mean_chars
            );
        }
    }

    let column_centers: Vec<f32> = (0..num_cols)
        .map(|c| (col_edges[c] + col_edges[c + 1]) / 2.0)
        .collect();
    let row_centers: Vec<f32> = (0..num_rows)
        .map(|r| (row_edges[r] + row_edges[r + 1]) / 2.0)
        .collect();

    debug!(
        "  cell-rect table accepted: {}x{}, {:.0}% density",
        num_rows,
        num_cols,
        non_empty_cells as f32 / total_cells * 100.0
    );

    Some(Table::new(column_centers, row_centers, cells, item_indices))
}

/// Merge wrapped description-line bands back into their visual data rows.
///
/// Some Word/PDF exports draw enough rectangle geometry to prove a table exists
/// but expose Y bands per wrapped text line instead of per cell row. In the
/// common mapping-table shape, a narrow row-label column precedes one wide
/// description column, and wrapped continuation bands have content only in that
/// wide column. Merge only that high-confidence shape so framed prose still
/// falls through the existing prose guards.
fn collapse_multiline_description_rows(
    cells: Vec<Vec<String>>,
    row_edges: Vec<f32>,
    col_edges: &[f32],
) -> (Vec<Vec<String>>, Vec<f32>, usize) {
    let num_rows = cells.len();
    let num_cols = col_edges.len().saturating_sub(1);
    if num_rows < 3 || num_cols < 3 || row_edges.len() != num_rows + 1 {
        return (cells, row_edges, 0);
    }

    let table_width = col_edges[num_cols] - col_edges[0];
    if table_width <= 0.0 {
        return (cells, row_edges, 0);
    }

    let Some((description_col, description_width)) = (0..num_cols)
        .map(|c| (c, col_edges[c + 1] - col_edges[c]))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return (cells, row_edges, 0);
    };

    // Require a preceding row-label column. Without it (e.g. a prose frame
    // split into text-start columns), "one populated wide column" is not enough
    // evidence to find visual row starts safely.
    if description_col == 0 || description_width < table_width * 0.35 {
        return (cells, row_edges, 0);
    }

    let row_has_left_label = |row: &[String]| {
        row.iter()
            .take(description_col)
            .any(|cell| !cell.trim().is_empty())
    };
    let labeled_rows = cells.iter().filter(|row| row_has_left_label(row)).count();
    if labeled_rows < 2 {
        return (cells, row_edges, 0);
    }

    let mut merged_rows = 0usize;
    let mut wrapped_description_rows = 0usize;
    let mut new_cells: Vec<Vec<String>> = Vec::with_capacity(num_rows);
    let mut new_edges = Vec::with_capacity(row_edges.len());
    new_edges.push(row_edges[0]);

    for (row_idx, row) in cells.into_iter().enumerate() {
        let desc_text = row
            .get(description_col)
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        let left_label = row_has_left_label(&row);
        let non_desc_non_empty = row
            .iter()
            .enumerate()
            .filter(|(col, cell)| *col != description_col && !cell.trim().is_empty())
            .count();

        // Wrapped continuation bands contain only description-column text.
        // The preceding label/marker column is empty because the visual row's
        // label cell spans the whole wrapped block.
        let is_description_continuation = row_idx > 0
            && !desc_text.is_empty()
            && !left_label
            && non_desc_non_empty == 0
            && !new_cells.is_empty();

        // Header cells are often split as "Controls" / "Version" in the first
        // column while the other header labels sit on the first band.
        let only_first_col = row
            .iter()
            .enumerate()
            .all(|(col, cell)| col == 0 || cell.trim().is_empty());
        let is_header_continuation = row_idx > 0
            && only_first_col
            && row
                .first()
                .is_some_and(|cell| !cell.trim().is_empty() && cell.chars().count() <= 24)
            && !new_cells.is_empty()
            && new_cells
                .last()
                .is_some_and(|prev| prev.iter().filter(|c| !c.trim().is_empty()).count() >= 2);

        if is_description_continuation || is_header_continuation {
            if let Some(prev) = new_cells.last_mut() {
                for (col, cell) in row.iter().enumerate() {
                    let text = cell.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if !prev[col].trim().is_empty() {
                        prev[col].push(' ');
                    }
                    prev[col].push_str(text);
                }
            }
            merged_rows += 1;
            if is_description_continuation {
                wrapped_description_rows += 1;
            }
        } else {
            if !new_cells.is_empty() {
                new_edges.push(row_edges[row_idx]);
            }
            new_cells.push(row);
        }
    }

    new_edges.push(*row_edges.last().unwrap());

    if merged_rows == 0 || new_cells.len() < 2 || new_edges.len() != new_cells.len() + 1 {
        return (new_cells, row_edges, 0);
    }

    (new_cells, new_edges, wrapped_description_rows)
}

/// Detect a table by merging all cluster rects into one group.
///
/// This handles clip-path PDFs where each column's cell rects form a separate
/// cluster (no spatial overlap between columns). Uses rect Y-edges for rows
/// and text X-position clustering for columns, similar to `detect_row_stripe_table`
/// but without the width-uniformity check.
fn detect_merged_cluster_table(
    items: &[TextItem],
    all_rects: &[(f32, f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    // Extract Y-edges from all rects
    let mut y_vals: Vec<f32> = Vec::new();
    for &(_, y, _, h) in all_rects {
        y_vals.push(y);
        y_vals.push(y + h);
    }
    let y_edges = snap_edges(&y_vals, 6.0);

    if y_edges.len() < 4 {
        debug!("  merged-cluster rejected: only {} y-edges", y_edges.len());
        return None;
    }

    let mut row_edges = y_edges;
    row_edges.sort_by(|a, b| b.total_cmp(a));

    // Bounding box of all rects
    let y_top = row_edges[0];
    let y_bottom = *row_edges.last().unwrap();
    let x_left = all_rects
        .iter()
        .map(|&(x, _, _, _)| x)
        .reduce(f32::min)
        .unwrap();
    let x_right = all_rects
        .iter()
        .map(|&(x, _, w, _)| x + w)
        .reduce(f32::max)
        .unwrap();

    // Gather page items within the bounding box
    let page_items: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.page == page
                && item.y >= y_bottom - 2.0
                && item.y <= y_top + 2.0
                && item.x >= x_left - 5.0
                && item.x + item.width <= x_right + 5.0
        })
        .collect();

    if page_items.is_empty() {
        return None;
    }

    // Derive columns from text X-position clustering
    let columns = cluster_x_positions(&page_items, 15.0);

    if columns.len() < 2 {
        debug!(
            "  merged-cluster rejected: only {} columns from text clustering",
            columns.len()
        );
        return None;
    }

    // Convert column centers to edges
    let mut col_edges: Vec<f32> = Vec::with_capacity(columns.len() + 1);
    let min_x = page_items
        .iter()
        .map(|(_, i)| i.x)
        .reduce(f32::min)
        .unwrap();
    col_edges.push(min_x - 5.0);
    for pair in columns.windows(2) {
        col_edges.push((pair[0] + pair[1]) / 2.0);
    }
    let max_x_right = page_items
        .iter()
        .map(|(_, i)| i.x + i.width)
        .reduce(f32::max)
        .unwrap();
    col_edges.push(max_x_right + 5.0);

    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges.len() - 1;

    debug!(
        "  merged-cluster grid: {}x{} ({} col edges, {} row edges)",
        num_rows,
        num_cols,
        col_edges.len(),
        row_edges.len()
    );

    // Assign items to grid
    let (cells, item_indices) = assign_items_to_grid(items, &col_edges, &row_edges, page);

    if item_indices.is_empty() {
        debug!("  merged-cluster rejected: no items assigned");
        return None;
    }

    // Validate: >=2 non-empty rows
    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
        .count();
    if non_empty_rows < 2 {
        debug!(
            "  merged-cluster rejected: only {} non-empty rows",
            non_empty_rows
        );
        return None;
    }

    // Content density: >=40%
    let total_cells = (num_cols * num_rows) as f32;
    let non_empty_cells = cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    let content_ratio = non_empty_cells as f32 / total_cells;
    if content_ratio < 0.40 {
        debug!(
            "  merged-cluster rejected: content ratio {:.2} < 0.40",
            content_ratio
        );
        return None;
    }

    // Reject if any cell has excessive text — layout background rects
    // produce "cells" containing paragraphs, not short data-table values.
    // Multi-row key/value tables can legitimately have one column of
    // long descriptive text, so only reject narrow-row layouts here.
    let max_cell_len = cells
        .iter()
        .flat_map(|row| row.iter())
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    if max_cell_len > 500 && non_empty_rows < 4 {
        debug!(
            "  merged-cluster rejected: max cell length {} > 500 ({} rows, layout background)",
            max_cell_len, non_empty_rows
        );
        return None;
    }
    if has_dominant_prose_cell(&cells) {
        debug!(
            "  merged-cluster rejected: dominant prose cell (chart/figure region over body text)"
        );
        return None;
    }

    // No empty columns
    for col in 0..num_cols {
        let col_has_content = cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()));
        if !col_has_content {
            debug!("  merged-cluster rejected: column {} is empty", col);
            return None;
        }
    }

    let column_centers: Vec<f32> = (0..num_cols)
        .map(|c| (col_edges[c] + col_edges[c + 1]) / 2.0)
        .collect();
    let row_centers: Vec<f32> = (0..num_rows)
        .map(|r| (row_edges[r] + row_edges[r + 1]) / 2.0)
        .collect();

    debug!(
        "  merged-cluster table accepted: {}x{}, {:.0}% density",
        num_rows,
        num_cols,
        content_ratio * 100.0
    );

    Some(Table::new(column_centers, row_centers, cells, item_indices))
}

/// Cluster text item X positions into column centers with a given minimum threshold.
///
/// Similar to `find_column_boundaries` in grid.rs but with a lower minimum threshold
/// suitable for rect-backed tables where we already know tabular structure exists
/// (no need for anti-paragraph safeguards).
fn cluster_x_positions(items: &[(usize, &TextItem)], min_threshold: f32) -> Vec<f32> {
    // Column edges come from where text STARTS. An item whose left edge hugs
    // the previous item's right edge on the same line is a continuation run
    // (style boundary, script change, underline split) — feeding its x-start
    // in here fabricates a phantom column mid-cell.
    let mut sorted: Vec<&TextItem> = items.iter().map(|&(_, i)| i).collect();
    sorted.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let mut x_positions: Vec<f32> = Vec::with_capacity(sorted.len());
    for (idx, item) in sorted.iter().enumerate() {
        let is_continuation = idx > 0 && {
            let prev = sorted[idx - 1];
            // Style/underline splits leave runs that TOUCH (gap ~0); real
            // cell boundaries in even the tightest tables keep a visible
            // gap. 2pt separates the two without eating dense-table columns.
            // The negative side is bounded too: text overhanging from an
            // adjacent cell overlaps by far more than italic kerning ever
            // does, and must still start its own column.
            let gap = item.x - (prev.x + prev.width);
            (prev.y - item.y).abs() <= 2.0 && gap < 2.0 && gap > -4.0 && item.x >= prev.x
        };
        if !is_continuation {
            x_positions.push(item.x);
        }
    }
    x_positions.sort_by(|a, b| a.total_cmp(b));

    if x_positions.is_empty() {
        return vec![];
    }

    let x_range = x_positions.last().unwrap() - x_positions.first().unwrap();
    let avg_gap = if x_positions.len() > 1 {
        x_range / (x_positions.len() - 1) as f32
    } else {
        60.0
    };
    let cluster_threshold = avg_gap.clamp(min_threshold, 50.0);

    let mut columns = Vec::new();
    let mut cluster_items: Vec<f32> = vec![x_positions[0]];

    for &x in &x_positions[1..] {
        let cluster_center = cluster_items.iter().sum::<f32>() / cluster_items.len() as f32;
        if x - cluster_center > cluster_threshold {
            columns.push(cluster_center);
            cluster_items = vec![x];
        } else {
            cluster_items.push(x);
        }
    }
    if !cluster_items.is_empty() {
        columns.push(cluster_items.iter().sum::<f32>() / cluster_items.len() as f32);
    }

    // Filter: each column needs multiple items
    let min_items_per_col = (items.len() / columns.len().max(1) / 4).max(2);
    columns
        .into_iter()
        .filter(|&col_x| {
            items
                .iter()
                .filter(|(_, i)| (i.x - col_x).abs() < cluster_threshold)
                .count()
                >= min_items_per_col
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn make_item(text: &str, x: f32, y: f32, font_size: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * font_size * 0.5,
            height: font_size,
            font: "TestFont".to_string(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    // --- is_chart_bar_cluster / detect_chart_regions ---

    /// Stacked bar chart: frame + 3 columns of equal-width segments with
    /// data-driven heights, holding numeric labels.
    fn chart_rects() -> Vec<PdfRect> {
        let mut rects = vec![PdfRect {
            x: 126.0,
            y: 548.0,
            width: 396.0,
            height: 216.0,
            page: 1,
        }];
        let bars = [
            (208.0, 618.0, 59.0),
            (208.0, 661.0, 39.0),
            (208.0, 696.0, 37.0),
            (313.0, 618.0, 67.0),
            (313.0, 670.0, 49.0),
            (313.0, 691.0, 42.0),
            (419.0, 618.0, 73.0),
            (419.0, 684.0, 37.0),
            (419.0, 708.0, 25.0),
        ];
        for (x, y, h) in bars {
            rects.push(PdfRect {
                x,
                y,
                width: 46.0,
                height: h,
                page: 1,
            });
        }
        rects
    }

    #[test]
    fn chart_bars_produce_region_not_table() {
        let items: Vec<TextItem> = [
            ("38", 228.0, 638.0),
            ("30", 228.0, 676.0),
            ("46", 333.0, 643.0),
            ("17", 333.0, 679.0),
            ("57", 438.0, 650.0),
            ("20", 438.0, 694.0),
        ]
        .iter()
        .map(|&(t, x, y)| make_item(t, x, y, 9.0))
        .collect();
        let rects = chart_rects();
        let regions = detect_chart_regions(&items, &rects, 1);
        assert_eq!(regions.len(), 1, "expected one chart region");
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        assert!(tables.is_empty(), "chart bars must not become a table");
        assert!(hints.is_empty(), "chart bars must not become a hint region");
    }

    #[test]
    fn dominant_page_backgrounds_are_normalized_only_after_repetition() {
        let page_fill = (0.0, 0.0, 600.0, 800.0);
        let cell = (100.0, 500.0, 120.0, 20.0);

        let mut dominant = vec![page_fill; DOMINANT_PAGE_BACKGROUND_MIN_REPETITIONS];
        dominant.push(cell);
        assert_eq!(without_dominant_page_backgrounds(&dominant), vec![cell]);

        let mut incidental = vec![page_fill; DOMINANT_PAGE_BACKGROUND_MIN_REPETITIONS - 1];
        incidental.push(cell);
        assert_eq!(without_dominant_page_backgrounds(&incidental), incidental);
    }

    #[test]
    fn uniform_cell_grid_is_not_a_chart() {
        // Touching, uniform-height cell rects (a real table) must not match:
        // no inter-column gap and no bar-length variation.
        let mut rects = Vec::new();
        for row in 0..4 {
            for col in 0..3 {
                rects.push(PdfRect {
                    x: 100.0 + col as f32 * 80.0,
                    y: 600.0 - row as f32 * 20.0,
                    width: 80.0,
                    height: 20.0,
                    page: 1,
                });
            }
        }
        let items: Vec<TextItem> = (0..4)
            .flat_map(|r| {
                (0..3).map(move |c| (100.0 + c as f32 * 80.0 + 10.0, 605.0 - r as f32 * 20.0))
            })
            .map(|(x, y)| make_item("42", x, y, 9.0))
            .collect();
        assert!(detect_chart_regions(&items, &rects, 1).is_empty());
    }

    #[test]
    fn variable_height_ruled_grid_overrides_bar_hypothesis() {
        let edge_sets = [
            [80.0, 140.0, 200.0, 260.0, 320.0, 380.0, 440.0, 500.0, 560.0],
            [80.0, 140.0, 210.0, 260.0, 320.0, 380.0, 450.0, 500.0, 560.0],
        ];
        let heights = [20.0, 34.0, 26.0, 42.0, 20.0, 34.0];
        let edge_variants = [0, 0, 0, 0, 1, 1];
        let mut rects = Vec::new();
        let mut y = 650.0;
        for (row, height) in heights.into_iter().enumerate() {
            let edges = edge_sets[edge_variants[row]];
            rects.extend(
                edges
                    .windows(2)
                    .map(|edge| (edge[0], y, edge[1] - edge[0], height)),
            );
            y -= height;
        }

        assert!(is_repeated_cell_grid(&rects));
        assert!(has_chart_bar_signature(&[], &rects, 1));
        assert!(repeated_cell_grid_overrides_bar_hypothesis(&rects));
        assert!(segmented_stacked_bar_geometry(&rects).is_none());
        assert!(!is_chart_bar_cluster(&[], &rects, 1));

        let mut with_page_fills =
            vec![(0.0, 0.0, 600.0, 800.0); DOMINANT_PAGE_BACKGROUND_MIN_REPETITIONS];
        with_page_fills.extend(rects);
        assert!(!repeated_cell_grid_overrides_bar_hypothesis(
            &with_page_fills
        ));
    }

    #[test]
    fn touching_segments_with_spaced_rows_remain_a_chart() {
        let row_edges = [
            [100.0, 140.0, 180.0, 220.0, 260.0],
            [100.0, 140.0, 180.0, 228.0, 260.0],
            [100.0, 140.0, 180.0, 214.0, 260.0],
            [100.0, 140.0, 180.0, 232.0, 260.0],
        ];
        let mut raw_rects = vec![(90.0, 530.0, 190.0, 100.0)];
        for (row, edges) in row_edges.into_iter().enumerate() {
            let y = 540.0 + row as f32 * 20.0;
            raw_rects.extend(
                edges
                    .windows(2)
                    .map(|edge| (edge[0], y, edge[1] - edge[0], 12.0)),
            );
        }
        let items: Vec<TextItem> = (0..4)
            .map(|row| make_item("Category", 62.0, 541.0 + row as f32 * 20.0, 9.0))
            .collect();

        assert!(is_repeated_cell_grid(&raw_rects));
        assert!(has_chart_bar_signature(&items, &raw_rects, 1));
        let geometry = segmented_stacked_bar_geometry(&raw_rects).expect("segmented stack");
        assert!(has_external_segmented_bar_labels(&items, 1, &geometry));
        assert!(is_chart_bar_cluster(&items, &raw_rects, 1));

        let numeric_items: Vec<TextItem> = (0..4)
            .map(|row| make_item("2024", 80.0, 541.0 + row as f32 * 18.0, 9.0))
            .collect();
        assert!(has_external_segmented_bar_labels(
            &numeric_items,
            1,
            &geometry
        ));
        assert!(is_chart_bar_cluster(&numeric_items, &raw_rects, 1));

        let edge_adjacent_items: Vec<TextItem> = (0..4)
            .map(|row| make_item("2024", 92.0, 541.0 + row as f32 * 18.0, 9.0))
            .collect();
        assert!(has_external_segmented_bar_labels(
            &edge_adjacent_items,
            1,
            &geometry
        ));
        assert!(is_chart_bar_cluster(&edge_adjacent_items, &raw_rects, 1));

        let far_items: Vec<TextItem> = (0..4)
            .map(|row| make_item("Category", 20.0, 541.0 + row as f32 * 18.0, 9.0))
            .collect();
        assert!(!has_external_segmented_bar_labels(&far_items, 1, &geometry));
        assert!(!is_chart_bar_cluster(&far_items, &raw_rects, 1));

        let rects: Vec<PdfRect> = raw_rects
            .into_iter()
            .map(|(x, y, width, height)| PdfRect {
                x,
                y,
                width,
                height,
                page: 1,
            })
            .collect();
        assert_eq!(detect_chart_regions(&items, &rects, 1).len(), 1);
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        assert!(tables.is_empty());
        assert!(hints.is_empty());
    }

    #[test]
    fn padded_numeric_grid_frame_remains_a_table() {
        let row_edges = [
            [100.0, 140.0, 180.0, 220.0, 260.0],
            [100.0, 140.0, 180.0, 228.0, 260.0],
            [100.0, 140.0, 180.0, 214.0, 260.0],
            [100.0, 140.0, 180.0, 232.0, 260.0],
        ];
        let mut raw_rects = vec![(96.0, 536.0, 168.0, 80.0)];
        let mut items = Vec::new();
        for (row, edges) in row_edges.into_iter().enumerate() {
            let y = 540.0 + row as f32 * 20.0;
            for edge in edges.windows(2) {
                raw_rects.push((edge[0], y, edge[1] - edge[0], 12.0));
                items.push(make_item("42", edge[0] + 8.0, y + 1.0, 9.0));
            }
        }

        assert!(is_repeated_cell_grid(&raw_rects));
        assert!(has_chart_bar_signature(&items, &raw_rects, 1));
        let geometry = segmented_stacked_bar_geometry(&raw_rects).expect("segmented rows");
        assert!(!has_external_segmented_bar_labels(&items, 1, &geometry));
        assert!(!is_chart_bar_cluster(&items, &raw_rects, 1));

        let flush_items: Vec<TextItem> = (0..4)
            .map(|row| make_item("1", 100.0, 541.0 + row as f32 * 20.0, 9.0))
            .collect();
        assert!(!has_external_segmented_bar_labels(
            &flush_items,
            1,
            &geometry
        ));
        assert!(!is_chart_bar_cluster(&flush_items, &raw_rects, 1));

        let rects: Vec<PdfRect> = raw_rects
            .into_iter()
            .map(|(x, y, width, height)| PdfRect {
                x,
                y,
                width,
                height,
                page: 1,
            })
            .collect();
        assert!(detect_chart_regions(&items, &rects, 1).is_empty());
        assert!(!detect_tables_from_rects(&items, &rects, 1).0.is_empty());
    }

    #[test]
    fn frameless_segmented_chart_with_category_labels_remains_a_chart() {
        let row_edges = [
            [100.0, 140.0, 180.0, 220.0, 260.0],
            [100.0, 140.0, 180.0, 228.0, 260.0],
            [100.0, 140.0, 180.0, 214.0, 260.0],
            [100.0, 140.0, 180.0, 232.0, 260.0],
        ];
        let mut raw_rects = Vec::new();
        let mut items = Vec::new();
        for (row, edges) in row_edges.into_iter().enumerate() {
            let y = 540.0 + row as f32 * 18.0;
            raw_rects.extend(
                edges
                    .windows(2)
                    .map(|edge| (edge[0], y, edge[1] - edge[0], 12.0)),
            );
            items.push(make_item("Category", 62.0, y + 1.0, 9.0));
        }

        let geometry = segmented_stacked_bar_geometry(&raw_rects).expect("segmented stack");
        assert!(has_external_segmented_bar_labels(&items, 1, &geometry));
        assert!(is_chart_bar_cluster(&items, &raw_rects, 1));

        let rects: Vec<PdfRect> = raw_rects
            .into_iter()
            .map(|(x, y, width, height)| PdfRect {
                x,
                y,
                width,
                height,
                page: 1,
            })
            .collect();
        assert_eq!(detect_chart_regions(&items, &rects, 1).len(), 1);
    }

    // --- detect_stacked_box_table ---

    /// N stacked boxes at x=100, w=300, h=22, top-to-bottom from y=600.
    fn stacked_boxes(n: usize) -> Vec<(f32, f32, f32, f32)> {
        (0..n)
            .map(|i| (100.0, 600.0 - i as f32 * 22.0, 300.0, 22.0))
            .collect()
    }

    #[test]
    fn stacked_box_list_becomes_single_column_table() {
        let rects = stacked_boxes(5);
        let items: Vec<TextItem> = (0..5)
            .map(|i| make_item("#1: Recycling Basics", 120.0, 605.0 - i as f32 * 22.0, 10.0))
            .collect();
        let table = detect_stacked_box_table(&items, &rects, 1).expect("stacked-box table");
        assert_eq!(table.cells.len(), 5);
        assert_eq!(table.cells[0].len(), 1);
    }

    #[test]
    fn stacked_box_rejects_wrapped_sentences() {
        // Line stripes behind flowing prose: rows continue mid-sentence.
        let rects = stacked_boxes(4);
        let texts = [
            "the provisions of this section apply to",
            "companies subject to tax under those",
            "sections, except that the copy of the",
            "annual statement must be retained.",
        ];
        let items: Vec<TextItem> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| make_item(t, 120.0, 605.0 - i as f32 * 22.0, 10.0))
            .collect();
        assert!(detect_stacked_box_table(&items, &rects, 1).is_none());
    }

    #[test]
    fn stacked_box_rejects_flanking_text() {
        // A ruled label column with plain-text data columns beside it is one
        // column of a wider table, not a single-column list.
        let rects = stacked_boxes(4);
        let mut items = Vec::new();
        for i in 0..4 {
            let y = 605.0 - i as f32 * 22.0;
            items.push(make_item("Section 1.382", 120.0, y, 10.0));
            items.push(make_item("removed text", 450.0, y, 10.0)); // beside the box
        }
        assert!(detect_stacked_box_table(&items, &rects, 1).is_none());
    }

    #[test]
    fn stacked_box_rejects_two_column_content() {
        // Boxes holding two separated runs are striped multi-column content.
        let rects = stacked_boxes(4);
        let mut items = Vec::new();
        for i in 0..4 {
            let y = 605.0 - i as f32 * 22.0;
            let mut left = make_item("left words", 110.0, y, 10.0);
            left.width = 60.0;
            let mut right = make_item("right words", 250.0, y, 10.0);
            right.width = 60.0;
            items.push(left);
            items.push(right);
        }
        assert!(detect_stacked_box_table(&items, &rects, 1).is_none());
    }

    #[test]
    fn stacked_box_rejects_mixed_height_stripes() {
        // Mixed 13/27pt stripes (redline markup) — height uniformity splits
        // the family and the gap check rejects the remainder.
        let mut rects = Vec::new();
        let mut y = 600.0;
        for i in 0..8 {
            let h = if i % 3 == 0 { 27.0 } else { 13.5 };
            y -= h;
            rects.push((100.0, y, 300.0, h));
        }
        let items: Vec<TextItem> = (0..8)
            .map(|i| make_item("PART 602 OMB CONTROL", 120.0, 590.0 - i as f32 * 18.0, 10.0))
            .collect();
        assert!(detect_stacked_box_table(&items, &rects, 1).is_none());
    }

    // --- has_dominant_prose_cell ---

    fn cells_of(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn dominant_prose_cell_rejects_swallowed_paragraph() {
        // Two cells hold paragraphs (the shape every observed phantom grid
        // has: swallowed body text spans multiple cells), rest are chart labels
        let para = ["word"; 70].join(" ");
        let para2 = ["word"; 35].join(" ");
        let cells = cells_of(&[
            &[para.as_str(), "81", "76"],
            &[para2.as_str(), "56", "9"],
            &["2019", "2020", ""],
        ]);
        assert!(has_dominant_prose_cell(&cells));
    }

    #[test]
    fn dominant_prose_cell_rejects_small_table_dominated_by_one_cell() {
        // Boundary case, documented as INTENDED: a small grid whose single
        // long cell dominates the word count is rejected even at 4+ rows.
        // By content alone this shape is indistinguishable from a phantom
        // grid over body text, and every observed instance in the regression
        // corpora was swallowed prose (chart/figure regions), not a real
        // note table. Rejection degrades gracefully — the text is still
        // extracted as prose — while accepting a phantom scrambles reading
        // order.
        let note = ["word"; 70].join(" ");
        let cells = cells_of(&[
            &["Purpose", note.as_str()],
            &["Owner", "Facilities team"],
            &["Date", "2024-06-01"],
            &["Status", "Active"],
        ]);
        assert!(has_dominant_prose_cell(&cells));
    }

    #[test]
    fn dominant_prose_cell_allows_description_column() {
        // Long-ish description cells, but text is spread across the table
        let desc = ["word"; 25].join(" ");
        let cells = cells_of(&[
            &["Item A", desc.as_str(), "100"],
            &["Item B", desc.as_str(), "200"],
            &["Item C", desc.as_str(), "300"],
            &["Item D", desc.as_str(), "400"],
        ]);
        assert!(!has_dominant_prose_cell(&cells));
    }

    #[test]
    fn dominant_prose_cell_allows_short_tables() {
        let cells = cells_of(&[&["Name", "Value"], &["Total", "42"]]);
        assert!(!has_dominant_prose_cell(&cells));
    }

    #[test]
    fn dominant_prose_cell_allows_data_table_with_long_note() {
        // A real 4+ row table with one verbose remark cell: the note is ≥60
        // words but the table's other content carries more than 2× its word
        // count, so concentration stays below the 1/3 threshold. The
        // denominator scales with table size — this is what keeps large
        // legitimate tables safe where a bare length cap would not.
        let note = ["word"; 60].join(" ");
        let row_text = ["data"; 12].join(" ");
        let mut rows: Vec<Vec<String>> = (0..11)
            .map(|i| {
                vec![
                    format!("Item {i}"),
                    row_text.clone(),
                    format!("{}", i * 100),
                ]
            })
            .collect();
        rows.push(vec!["Note".into(), note, String::new()]);
        assert!(!has_dominant_prose_cell(&rows));
    }

    // --- rects_overlap ---

    #[test]
    fn test_rects_overlap_overlapping() {
        let a = (0.0, 0.0, 10.0, 10.0);
        let b = (5.0, 5.0, 10.0, 10.0);
        assert!(rects_overlap(&a, &b, 0.0));
    }

    #[test]
    fn test_rects_overlap_touching() {
        let a = (0.0, 0.0, 10.0, 10.0);
        let b = (10.0, 0.0, 10.0, 10.0);
        // Touching at edge — with 0 tolerance, the right edge of a == left edge of b
        assert!(rects_overlap(&a, &b, 0.0));
    }

    #[test]
    fn test_rects_overlap_separated() {
        let a = (0.0, 0.0, 10.0, 10.0);
        let b = (20.0, 20.0, 10.0, 10.0);
        assert!(!rects_overlap(&a, &b, 0.0));
    }

    #[test]
    fn test_rects_overlap_contained() {
        let a = (0.0, 0.0, 20.0, 20.0);
        let b = (5.0, 5.0, 5.0, 5.0);
        assert!(rects_overlap(&a, &b, 0.0));
    }

    #[test]
    fn test_rects_overlap_identical() {
        let a = (10.0, 10.0, 50.0, 50.0);
        assert!(rects_overlap(&a, &a, 0.0));
    }

    #[test]
    fn test_rects_overlap_tolerance_expansion() {
        let a = (0.0, 0.0, 10.0, 10.0);
        let b = (15.0, 0.0, 10.0, 10.0);
        // Gap of 5 — with tol=0 they don't overlap
        assert!(!rects_overlap(&a, &b, 0.0));
        // With tol=3, each expands by 3 → they overlap
        assert!(rects_overlap(&a, &b, 3.0));
    }

    // --- cluster_rects ---

    #[test]
    fn test_cluster_rects_empty() {
        let rects: Vec<(f32, f32, f32, f32)> = vec![];
        assert!(cluster_rects(&rects, 3.0, 1).is_empty());
    }

    #[test]
    fn test_cluster_rects_single_rect() {
        let rects = vec![(0.0, 0.0, 10.0, 10.0)];
        // min_size=1 → should return the single rect
        let groups = cluster_rects(&rects, 3.0, 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0]);
    }

    #[test]
    fn test_cluster_rects_all_disconnected() {
        let rects = vec![
            (0.0, 0.0, 10.0, 10.0),
            (100.0, 100.0, 10.0, 10.0),
            (200.0, 200.0, 10.0, 10.0),
        ];
        // All separated, min_size=2 → no groups
        let groups = cluster_rects(&rects, 0.0, 2);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_cluster_rects_chain_overlap() {
        // A overlaps B, B overlaps C → all in one group
        let rects = vec![
            (0.0, 0.0, 10.0, 10.0),
            (8.0, 0.0, 10.0, 10.0),
            (16.0, 0.0, 10.0, 10.0),
        ];
        let groups = cluster_rects(&rects, 0.0, 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn test_cluster_rects_all_connected() {
        let rects = vec![
            (0.0, 0.0, 20.0, 20.0),
            (5.0, 5.0, 20.0, 20.0),
            (10.0, 10.0, 20.0, 20.0),
        ];
        let groups = cluster_rects(&rects, 0.0, 1);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn test_cluster_rects_min_size_filter() {
        // Two separate pairs + one lone rect
        let rects = vec![
            (0.0, 0.0, 10.0, 10.0),
            (5.0, 0.0, 10.0, 10.0),
            (100.0, 100.0, 10.0, 10.0),
        ];
        // min_size=2 → only the overlapping pair returned
        let groups = cluster_rects(&rects, 0.0, 2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    // --- snap_edges ---

    #[test]
    fn test_snap_edges_empty() {
        assert!(snap_edges(&[], 6.0).is_empty());
    }

    #[test]
    fn test_snap_edges_single_value() {
        assert_eq!(snap_edges(&[42.0], 6.0), vec![42.0]);
    }

    #[test]
    fn test_snap_edges_within_tolerance_deduped() {
        let edges = snap_edges(&[10.0, 12.0, 14.0, 30.0], 6.0);
        // 10, 12, 14 are all within 6 of the first → deduplicated
        assert_eq!(edges.len(), 2);
        assert!((edges[0] - 10.0).abs() < 0.01);
        assert!((edges[1] - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_snap_edges_outside_tolerance_kept() {
        let edges = snap_edges(&[10.0, 20.0, 30.0], 5.0);
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn test_snap_edges_unsorted_input() {
        let edges = snap_edges(&[30.0, 10.0, 20.0], 5.0);
        // Should be sorted
        assert_eq!(edges, vec![10.0, 20.0, 30.0]);
    }

    // --- assign_items_to_grid ---

    #[test]
    fn test_assign_items_basic() {
        let items = vec![
            make_item("A", 15.0, 85.0, 10.0),
            make_item("B", 55.0, 85.0, 10.0),
            make_item("C", 15.0, 55.0, 10.0),
            make_item("D", 55.0, 55.0, 10.0),
        ];
        // 2x2 grid: cols at [10, 50, 90], rows at [90, 70, 50] (top-to-bottom)
        let col_edges = vec![10.0, 50.0, 90.0];
        let row_edges = vec![90.0, 70.0, 40.0];
        let (cells, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0][0], "A");
        assert_eq!(cells[0][1], "B");
        assert_eq!(cells[1][0], "C");
        assert_eq!(cells[1][1], "D");
        assert_eq!(indices.len(), 4);
    }

    #[test]
    fn test_assign_items_outside_grid() {
        let items = vec![make_item("Outside", 500.0, 500.0, 10.0)];
        let col_edges = vec![10.0, 50.0, 90.0];
        let row_edges = vec![90.0, 70.0, 50.0];
        let (_, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert!(indices.is_empty());
    }

    #[test]
    fn test_assign_items_wrong_page_filtered() {
        let mut item = make_item("A", 15.0, 85.0, 10.0);
        item.page = 2;
        let items = vec![item];
        let col_edges = vec![10.0, 50.0, 90.0];
        let row_edges = vec![90.0, 70.0, 50.0];
        let (_, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert!(indices.is_empty());
    }

    #[test]
    fn test_assign_items_multiple_same_cell() {
        let items = vec![
            make_item("Hello", 15.0, 85.0, 10.0),
            make_item("World", 20.0, 80.0, 10.0),
        ];
        let col_edges = vec![10.0, 50.0];
        let row_edges = vec![90.0, 70.0];
        let (cells, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(indices.len(), 2);
        assert!(cells[0][0].contains("Hello"));
        assert!(cells[0][0].contains("World"));
    }

    #[test]
    fn test_assign_items_parenthetical_no_inner_spaces() {
        let items = vec![
            make_item("The first sentence", 15.0, 85.0, 10.0),
            make_item("(", 90.0, 85.0, 10.0),
            make_item("twice", 95.0, 85.0, 10.0),
            make_item(")", 120.0, 85.0, 10.0),
        ];
        let col_edges = vec![10.0, 150.0];
        let row_edges = vec![90.0, 70.0];
        let (cells, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(indices.len(), 4);
        assert_eq!(cells[0][0], "The first sentence (twice)");
    }

    #[test]
    fn test_assign_items_boundary_tolerance() {
        // Item right at edge with ±2pt tolerance
        let items = vec![make_item("Edge", 9.0, 89.0, 10.0)];
        let col_edges = vec![10.0, 50.0];
        let row_edges = vec![90.0, 70.0];
        let (_, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(indices.len(), 1);
    }

    #[test]
    fn test_assign_items_empty_grid() {
        let items = vec![make_item("A", 15.0, 85.0, 10.0)];
        let col_edges = vec![10.0]; // Only 1 edge → 0 columns
        let row_edges = vec![90.0]; // Only 1 edge → 0 rows
        let (cells, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert!(cells.is_empty());
        assert!(indices.is_empty());
    }

    #[test]
    fn test_assign_items_all_assigned() {
        let items = vec![
            make_item("A", 15.0, 85.0, 10.0),
            make_item("B", 55.0, 85.0, 10.0),
        ];
        let col_edges = vec![10.0, 50.0, 90.0];
        let row_edges = vec![90.0, 70.0];
        let (_, indices) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(indices.len(), 2);
    }

    #[test]
    fn test_assign_items_sorted_y_desc_x_asc() {
        // Two items in same cell — should sort by Y desc, X asc
        let items = vec![
            make_item("Bottom", 15.0, 75.0, 10.0),
            make_item("Top", 15.0, 85.0, 10.0),
        ];
        let col_edges = vec![10.0, 50.0];
        let row_edges = vec![90.0, 70.0];
        let (cells, _) = assign_items_to_grid(&items, &col_edges, &row_edges, 1);
        assert_eq!(cells[0][0], "Top Bottom");
    }

    // --- is_row_stripe_pattern ---

    #[test]
    fn test_is_row_stripe_pattern_too_few_rects() {
        let rects = vec![(0.0, 0.0, 300.0, 20.0), (0.0, 25.0, 300.0, 20.0)];
        assert!(!is_row_stripe_pattern(&rects));
    }

    #[test]
    fn test_is_row_stripe_pattern_narrow_rects() {
        let rects = vec![
            (0.0, 0.0, 50.0, 20.0),
            (0.0, 25.0, 50.0, 20.0),
            (0.0, 50.0, 50.0, 20.0),
        ];
        assert!(!is_row_stripe_pattern(&rects));
    }

    #[test]
    fn test_is_row_stripe_pattern_uniform_wide() {
        let rects = vec![
            (10.0, 0.0, 500.0, 20.0),
            (10.0, 25.0, 500.0, 20.0),
            (10.0, 50.0, 500.0, 20.0),
            (10.0, 75.0, 500.0, 20.0),
        ];
        assert!(is_row_stripe_pattern(&rects));
    }

    #[test]
    fn test_is_row_stripe_pattern_mixed_widths() {
        let rects = vec![
            (10.0, 0.0, 500.0, 20.0),
            (10.0, 25.0, 100.0, 20.0), // Very different width
            (10.0, 50.0, 500.0, 20.0),
            (10.0, 75.0, 50.0, 20.0), // Very different width
        ];
        assert!(!is_row_stripe_pattern(&rects));
    }

    #[test]
    fn test_is_row_stripe_pattern_75_percent_boundary() {
        // 3 of 4 (75%) within tolerance → should pass (> 0.75)
        let rects = vec![
            (10.0, 0.0, 500.0, 20.0),
            (10.0, 25.0, 505.0, 20.0),
            (10.0, 50.0, 495.0, 20.0),
            (10.0, 75.0, 100.0, 20.0), // outlier
        ];
        // 3/4 = 0.75 — NOT > 0.75, so false
        assert!(!is_row_stripe_pattern(&rects));
    }

    #[test]
    fn test_row_stripe_rejects_layout_background_long_cells() {
        // Simulate a newsletter page with wide background rects (sidebar, header, body)
        // that look like row stripes but contain paragraphs of body text.
        let rects = vec![
            (10.0, 700.0, 550.0, 50.0),  // header band
            (10.0, 640.0, 550.0, 50.0),  // nav band
            (10.0, 200.0, 550.0, 430.0), // body background
        ];
        let items = vec![
            make_item("General News", 20.0, 650.0, 10.0),
            make_item("People News", 20.0, 710.0, 10.0),
            // Simulate a long body text (>500 chars) in the main content area
            make_item(&"A".repeat(600), 200.0, 650.0, 10.0),
        ];
        let result = detect_row_stripe_table(&items, &rects, 1);
        assert!(
            result.is_none(),
            "layout background rects should not be detected as a table"
        );
    }

    #[test]
    fn test_row_stripe_accepts_multi_row_key_value_long_cells() {
        // Multi-row 2-column key/value table where one value cell holds
        // a paragraph (>500 chars).  The old `max_cell_len > 500` check
        // rejected this shape as a "layout background"; with the
        // multi-row guard, it should be accepted.
        let mut rects = Vec::new();
        let row_h = 25.0_f32;
        let y_top = 700.0_f32;
        for i in 0..8 {
            let y = y_top - (i as f32) * row_h;
            rects.push((40.0, y, 510.0, row_h));
        }
        let mut items = Vec::new();
        for i in 0..8 {
            let row_center_y = y_top - (i as f32) * row_h + row_h / 2.0;
            // Left column: short label
            items.push(make_item(&format!("Field {}", i), 45.0, row_center_y, 10.0));
            // Right column: short value, except the last row which is a paragraph
            let value = if i == 7 {
                "X".repeat(800)
            } else {
                "value".to_string()
            };
            items.push(make_item(&value, 300.0, row_center_y, 10.0));
        }
        let result = detect_row_stripe_table(&items, &rects, 1);
        assert!(
            result.is_some(),
            "multi-row key/value table with one long cell should be accepted"
        );
        let t = result.unwrap();
        assert!(
            t.cells.len() >= 4,
            "expected ≥4 rows, got {}",
            t.cells.len()
        );
        assert_eq!(t.cells[0].len(), 2, "expected 2 columns");
    }

    // --- propagate_merged_cells ---

    #[test]
    fn test_propagate_merged_cells_spanning_rect() {
        // A rect spanning 2 rows in column 0
        let col_edges = vec![0.0, 50.0, 100.0];
        let row_edges = vec![100.0, 80.0, 60.0]; // 2 rows
        let mut cells = vec![
            vec!["Top".to_string(), "A".to_string()],
            vec!["Bottom".to_string(), "B".to_string()],
        ];
        // Rect spanning both rows in col 0
        let group_rects = vec![(0.0, 60.0, 50.0, 40.0)];
        let skip = vec![false];
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells[0][0], "Top Bottom");
        assert!(cells[1][0].is_empty());
    }

    #[test]
    fn test_propagate_merged_cells_single_row_rect_noop() {
        // Use well-separated rows so the rect doesn't bleed into adjacent row
        // via the 6pt tolerance in propagate_merged_cells.
        let col_edges = vec![0.0, 50.0, 100.0];
        let row_edges = vec![200.0, 100.0, 0.0];
        let mut cells = vec![
            vec!["A".to_string(), "B".to_string()],
            vec!["C".to_string(), "D".to_string()],
        ];
        // Rect clearly inside row 0 only (y=110..190, row 0 is 100..200)
        // ry=110 > row_edges[1]+tol = 106, so it doesn't span into row 1
        let group_rects = vec![(0.0, 110.0, 50.0, 80.0)];
        let skip = vec![false];
        let cells_before = cells.clone();
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells, cells_before);
    }

    #[test]
    fn test_propagate_merged_cells_skip_rects_respected() {
        let col_edges = vec![0.0, 50.0, 100.0];
        let row_edges = vec![100.0, 80.0, 60.0];
        let mut cells = vec![
            vec!["A".to_string(), "B".to_string()],
            vec!["C".to_string(), "D".to_string()],
        ];
        let group_rects = vec![(0.0, 60.0, 50.0, 40.0)];
        let skip = vec![true]; // Skip this rect
        let cells_before = cells.clone();
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells, cells_before);
    }

    #[test]
    fn test_propagate_merged_cells_text_in_multiple_sub_rows() {
        let col_edges = vec![0.0, 50.0];
        let row_edges = vec![100.0, 80.0, 60.0, 40.0]; // 3 rows
        let mut cells = vec![
            vec!["Line1".to_string()],
            vec!["Line2".to_string()],
            vec!["Line3".to_string()],
        ];
        // Rect spanning all 3 rows
        let group_rects = vec![(0.0, 40.0, 50.0, 60.0)];
        let skip = vec![false];
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells[0][0], "Line1 Line2 Line3");
        assert!(cells[1][0].is_empty());
        assert!(cells[2][0].is_empty());
    }

    #[test]
    fn test_propagate_merged_cells_full_width_spanning() {
        let col_edges = vec![0.0, 50.0, 100.0];
        let row_edges = vec![100.0, 80.0, 60.0];
        let mut cells = vec![
            vec!["A".to_string(), "X".to_string()],
            vec!["B".to_string(), "Y".to_string()],
        ];
        // Rect spanning both rows but only column 1
        let group_rects = vec![(50.0, 60.0, 50.0, 40.0)];
        let skip = vec![false];
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells[0][1], "X Y");
        assert!(cells[1][1].is_empty());
        // Column 0 should be unchanged
        assert_eq!(cells[0][0], "A");
        assert_eq!(cells[1][0], "B");
    }

    #[test]
    fn test_propagate_merged_cells_rect_tangent_to_row_boundary() {
        // Regression: a rect whose top exactly equals a row's bottom lies
        // entirely outside that row, so it must not be considered to span
        // it. With the old overlap-based predicate this cascaded into body
        // text from unrelated rows being merged into a single header cell
        // (mythos system card CB task-based evaluations table).
        //
        // Layout: two rows 0..80 and 80..160 (bottom → top in PDF coords),
        // rect occupies only the lower row (y=0..80). Its top equals the
        // upper row's bottom; it must not span the upper row.
        let col_edges = vec![0.0, 50.0];
        let row_edges = vec![160.0, 80.0, 0.0]; // top → bot
        let mut cells = vec![vec!["Upper".to_string()], vec!["Lower".to_string()]];
        let group_rects = vec![(0.0, 0.0, 50.0, 80.0)]; // rect at y=0..80
        let skip = vec![false];
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        assert_eq!(cells[0][0], "Upper", "upper row must not be merged");
        assert_eq!(cells[1][0], "Lower", "lower row must not be touched");
    }

    #[test]
    fn test_propagate_merged_cells_empty_cells_preserved() {
        let col_edges = vec![0.0, 50.0];
        let row_edges = vec![100.0, 80.0, 60.0];
        let mut cells = vec![vec!["Text".to_string()], vec!["".to_string()]];
        // Rect spanning both rows
        let group_rects = vec![(0.0, 60.0, 50.0, 40.0)];
        let skip = vec![false];
        propagate_merged_cells(&mut cells, &col_edges, &row_edges, &group_rects, &skip);
        // Only "Text" in first row (empty cell contributes nothing)
        assert_eq!(cells[0][0], "Text");
        assert!(cells[1][0].is_empty());
    }

    // --- detect_table_from_rect_group / try_build_grid ---

    // Helper: create a 3-row × 2-col grid of rects with 10pt gaps between rows.
    // Gaps prevent propagate_merged_cells from collapsing adjacent rows
    // (shared-edge rects bleed via the 6pt tolerance).
    // Y layout: row0 y=60..80, row1 y=30..50, row2 y=0..20
    fn make_grid_rects() -> Vec<(f32, f32, f32, f32)> {
        vec![
            (10.0, 60.0, 40.0, 20.0), // row0, col0
            (50.0, 60.0, 40.0, 20.0), // row0, col1
            (10.0, 30.0, 40.0, 20.0), // row1, col0
            (50.0, 30.0, 40.0, 20.0), // row1, col1
            (10.0, 0.0, 40.0, 20.0),  // row2, col0
            (50.0, 0.0, 40.0, 20.0),  // row2, col1
        ]
    }

    #[test]
    fn test_try_build_grid_basic_valid() {
        let items = vec![
            make_item("H1", 15.0, 70.0, 10.0),
            make_item("H2", 55.0, 70.0, 10.0),
            make_item("D1", 15.0, 40.0, 10.0),
            make_item("D2", 55.0, 40.0, 10.0),
            make_item("E1", 15.0, 10.0, 10.0),
            make_item("E2", 55.0, 10.0, 10.0),
        ];
        let group_rects = make_grid_rects();
        let skip = vec![false; 6];
        match try_build_grid(&items, &group_rects, 1, &skip, false) {
            GridResult::Ok(table) => {
                assert!(table.columns.len() >= 2);
                assert!(table.rows.len() >= 2);
            }
            other => panic!(
                "Expected Ok, got {:?}",
                match other {
                    GridResult::FewNonEmptyRows => "FewNonEmptyRows",
                    GridResult::Failed => "Failed",
                    GridResult::Ok(_) => unreachable!(),
                }
            ),
        }
    }

    #[test]
    fn test_try_build_grid_too_few_edges() {
        // Only 2 rects → not enough edges for a grid
        let items = vec![make_item("A", 15.0, 85.0, 10.0)];
        let group_rects = vec![(10.0, 70.0, 40.0, 20.0), (10.0, 50.0, 40.0, 20.0)];
        let skip = vec![false; 2];
        match try_build_grid(&items, &group_rects, 1, &skip, false) {
            GridResult::Failed => {}
            _ => panic!("Expected Failed"),
        }
    }

    #[test]
    fn test_try_build_grid_strict_rejects_long_text() {
        let long_text = "a".repeat(250);
        let mut long_item = make_item(&long_text, 15.0, 70.0, 10.0);
        // Override width so the item center stays inside the grid cell
        long_item.width = 20.0;
        let items = vec![
            long_item,
            make_item("H2", 55.0, 70.0, 10.0),
            make_item("D1", 15.0, 40.0, 10.0),
            make_item("D2", 55.0, 40.0, 10.0),
            make_item("E1", 15.0, 10.0, 10.0),
            make_item("E2", 55.0, 10.0, 10.0),
        ];
        let group_rects = make_grid_rects();
        let skip = vec![false; 6];
        match try_build_grid(&items, &group_rects, 1, &skip, true) {
            GridResult::Failed => {}
            _ => panic!("Expected Failed due to long text in strict mode"),
        }
    }

    #[test]
    fn test_try_build_grid_empty_column_rejected() {
        // All items in column 0 only — column 1 is empty
        let items = vec![
            make_item("A", 15.0, 70.0, 10.0),
            make_item("B", 15.0, 40.0, 10.0),
            make_item("C", 15.0, 10.0, 10.0),
        ];
        let group_rects = make_grid_rects();
        let skip = vec![false; 6];
        match try_build_grid(&items, &group_rects, 1, &skip, false) {
            GridResult::Failed => {}
            _ => panic!("Expected Failed due to empty column"),
        }
    }

    #[test]
    fn test_try_build_grid_no_items() {
        let items: Vec<TextItem> = vec![];
        let group_rects = make_grid_rects();
        let skip = vec![false; 6];
        match try_build_grid(&items, &group_rects, 1, &skip, false) {
            GridResult::Failed => {}
            _ => panic!("Expected Failed with no items"),
        }
    }

    #[test]
    fn test_detect_table_from_rect_group_valid() {
        let items = vec![
            make_item("H1", 15.0, 70.0, 10.0),
            make_item("H2", 55.0, 70.0, 10.0),
            make_item("D1", 15.0, 40.0, 10.0),
            make_item("D2", 55.0, 40.0, 10.0),
            make_item("E1", 15.0, 10.0, 10.0),
            make_item("E2", 55.0, 10.0, 10.0),
        ];
        let group_rects = make_grid_rects();
        let result = detect_table_from_rect_group(&items, &group_rects, 1);
        assert!(result.is_some());
    }

    // --- extract_hint_region ---

    #[test]
    fn test_extract_hint_region_valid_small_cluster() {
        let rects = vec![
            (10.0, 100.0, 200.0, 30.0),
            (10.0, 140.0, 200.0, 30.0),
            (10.0, 180.0, 200.0, 30.0),
        ];
        let hint = extract_hint_region(&rects);
        assert!(hint.is_some());
        let hint = hint.unwrap();
        assert!(hint.y_top > hint.y_bottom);
    }

    #[test]
    fn test_extract_hint_region_too_few_rects() {
        let rects = vec![(10.0, 100.0, 200.0, 30.0)];
        assert!(extract_hint_region(&rects).is_none());
    }

    #[test]
    fn test_extract_hint_region_too_many_rects() {
        let rects: Vec<(f32, f32, f32, f32)> = (0..10)
            .map(|i| (10.0, 100.0 + i as f32 * 30.0, 200.0, 25.0))
            .collect();
        assert!(extract_hint_region(&rects).is_none());
    }

    // --- split_wide_cluster ---

    #[test]
    fn split_at_wide_gap() {
        // Left zone: x=10..50, Right zone: x=80..120 → gap of 30pt
        let mut rects = Vec::new();
        for i in 0..8 {
            rects.push((10.0, i as f32 * 20.0, 40.0, 15.0)); // left
            rects.push((80.0, i as f32 * 20.0, 40.0, 15.0)); // right
        }
        let result = split_wide_cluster(&rects, 15.0, 6);
        assert!(result.is_some());
        let (left, right) = result.unwrap();
        assert!(left.iter().all(|&(x, _, _, _)| x < 60.0));
        assert!(right.iter().all(|&(x, _, _, _)| x >= 60.0));
    }

    #[test]
    fn no_split_narrow_gap() {
        // Left zone: x=10..50, Right zone: x=55..95 → gap of only 5pt
        let mut rects = Vec::new();
        for i in 0..8 {
            rects.push((10.0, i as f32 * 20.0, 40.0, 15.0));
            rects.push((55.0, i as f32 * 20.0, 40.0, 15.0));
        }
        assert!(split_wide_cluster(&rects, 15.0, 6).is_none());
    }

    #[test]
    fn no_split_small_subgroup() {
        // Left zone: 2 rects, Right zone: 8 rects → left too small (< 6)
        let mut rects = Vec::new();
        for i in 0..2 {
            rects.push((10.0, i as f32 * 20.0, 40.0, 15.0));
        }
        for i in 0..8 {
            rects.push((80.0, i as f32 * 20.0, 40.0, 15.0));
        }
        // Also fails min total: 10 < 12 (min_group_size * 2 = 12)
        assert!(split_wide_cluster(&rects, 15.0, 6).is_none());
    }

    #[test]
    fn split_preserves_all_rects() {
        let mut rects = Vec::new();
        for i in 0..10 {
            rects.push((10.0, i as f32 * 20.0, 40.0, 15.0));
            rects.push((80.0, i as f32 * 20.0, 40.0, 15.0));
        }
        let (left, right) = split_wide_cluster(&rects, 15.0, 6).unwrap();
        assert_eq!(left.len() + right.len(), rects.len());
    }

    #[test]
    fn no_split_single_band() {
        // All rects overlap in X → single merged interval, no gap
        let rects: Vec<(f32, f32, f32, f32)> = (0..12)
            .map(|i| (10.0 + i as f32 * 5.0, i as f32 * 20.0, 40.0, 15.0))
            .collect();
        assert!(split_wide_cluster(&rects, 15.0, 6).is_none());
    }

    // --- XY hint regions from failed clusters ---

    #[test]
    fn hint_from_failed_large_clusters() {
        // Two separate clusters of 36 rects (6×6) each, placed side by side
        // with a large gap so they form two distinct clusters.
        // Requires ≥2 qualifying clusters to produce hints (multi-zone layout).
        let mut page_rects: Vec<(f32, f32, f32, f32)> = Vec::new();
        // Cluster 1: x=50..120, y=100..170
        for row in 0..6 {
            for col in 0..6 {
                page_rects.push((
                    50.0 + col as f32 * 12.0,
                    100.0 + row as f32 * 12.0,
                    10.0,
                    10.0,
                ));
            }
        }
        // Cluster 2: x=250..320, y=100..170 (130pt gap from cluster 1)
        for row in 0..6 {
            for col in 0..6 {
                page_rects.push((
                    250.0 + col as f32 * 12.0,
                    100.0 + row as f32 * 12.0,
                    10.0,
                    10.0,
                ));
            }
        }
        let items: Vec<TextItem> = vec![];
        let rects: Vec<crate::types::PdfRect> = page_rects
            .iter()
            .map(|&(x, y, w, h)| crate::types::PdfRect {
                x,
                y,
                width: w,
                height: h,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        assert!(tables.is_empty());
        assert_eq!(hints.len(), 2);
        // Cluster 1: x=50..120, y=100..170
        assert!((hints[0].x_left - 50.0).abs() < 1.0);
        assert!((hints[0].x_right - 120.0).abs() < 1.0);
        assert!((hints[0].y_bottom - 100.0).abs() < 1.0);
        assert!((hints[0].y_top - 170.0).abs() < 1.0);
        // Cluster 2: x=250..320, y=100..170
        assert!((hints[1].x_left - 250.0).abs() < 1.0);
        assert!((hints[1].x_right - 320.0).abs() < 1.0);
    }

    #[test]
    fn no_hint_single_large_cluster() {
        // Single cluster of 36 rects — not enough (need ≥2 zones)
        let mut page_rects: Vec<(f32, f32, f32, f32)> = Vec::new();
        for row in 0..6 {
            for col in 0..6 {
                page_rects.push((
                    50.0 + col as f32 * 12.0,
                    100.0 + row as f32 * 12.0,
                    10.0,
                    10.0,
                ));
            }
        }
        let items: Vec<TextItem> = vec![];
        let rects: Vec<crate::types::PdfRect> = page_rects
            .iter()
            .map(|&(x, y, w, h)| crate::types::PdfRect {
                x,
                y,
                width: w,
                height: h,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        assert!(tables.is_empty());
        assert!(hints.is_empty());
    }

    #[test]
    fn no_hint_too_few_rects() {
        // 5 rects (< 10 threshold for large-cluster hints, also < 6 for clustering)
        let rects: Vec<crate::types::PdfRect> = (0..5)
            .map(|i| crate::types::PdfRect {
                x: 50.0 + i as f32 * 30.0,
                y: 100.0,
                width: 20.0,
                height: 20.0,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&[], &rects, 1);
        assert!(tables.is_empty());
        // 5 rects: not enough for ≥6 clustering, and rect-sparse path needs 4-6
        // but clusters of ≥4 won't form with disconnected rects (30pt gap > 3pt tol)
        assert!(hints.is_empty());
    }

    #[test]
    fn no_hint_page_spanning_width() {
        // Rects spanning > 400pt width → no hint
        let mut page_rects = Vec::new();
        for i in 0..12 {
            page_rects.push(crate::types::PdfRect {
                x: i as f32 * 40.0,
                y: 100.0,
                width: 38.0,
                height: 10.0,
                page: 1,
            });
        }
        let (tables, hints) = detect_tables_from_rects(&[], &page_rects, 1);
        assert!(tables.is_empty());
        assert!(hints.is_empty());
    }

    // --- merge_overlapping_hints ---

    #[test]
    fn merge_overlapping_hints_dedup() {
        let hints = vec![
            RectHintRegion {
                x_left: 50.0,
                x_right: 250.0,
                y_bottom: 100.0,
                y_top: 200.0,
                cluster_rects: Vec::new(),
            },
            RectHintRegion {
                x_left: 60.0,
                x_right: 260.0,
                y_bottom: 110.0,
                y_top: 210.0,
                cluster_rects: Vec::new(),
            },
        ];
        let merged = merge_overlapping_hints(hints);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].x_left - 50.0).abs() < 0.01);
        assert!((merged[0].x_right - 260.0).abs() < 0.01);
        assert!((merged[0].y_bottom - 100.0).abs() < 0.01);
        assert!((merged[0].y_top - 210.0).abs() < 0.01);
    }

    #[test]
    fn merge_overlapping_hints_disjoint() {
        let hints = vec![
            RectHintRegion {
                x_left: 50.0,
                x_right: 200.0,
                y_bottom: 100.0,
                y_top: 200.0,
                cluster_rects: Vec::new(),
            },
            RectHintRegion {
                x_left: 350.0,
                x_right: 500.0,
                y_bottom: 100.0,
                y_top: 200.0,
                cluster_rects: Vec::new(),
            },
        ];
        let merged = merge_overlapping_hints(hints);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_hints_blocked_by_max_width() {
        // Two hints in the same Y band with small X gap (8pt) but combined
        // width > 400pt. Simulates left/right calendar month zones that
        // should NOT merge.
        let hints = vec![
            RectHintRegion {
                x_left: 20.0,
                x_right: 340.0,
                y_bottom: 100.0,
                y_top: 170.0,
                cluster_rects: Vec::new(),
            },
            RectHintRegion {
                x_left: 348.0,
                x_right: 668.0,
                y_bottom: 100.0,
                y_top: 170.0,
                cluster_rects: Vec::new(),
            },
        ];
        let merged = merge_overlapping_hints(hints);
        // Should remain separate: merged width would be 648pt > 400pt
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_hints_adjacent_fragments() {
        // Two fragments of the same zone with small gap, combined width < 400pt.
        // Should merge.
        let hints = vec![
            RectHintRegion {
                x_left: 20.0,
                x_right: 266.0,
                y_bottom: 100.0,
                y_top: 170.0,
                cluster_rects: Vec::new(),
            },
            RectHintRegion {
                x_left: 276.0,
                x_right: 340.0,
                y_bottom: 100.0,
                y_top: 170.0,
                cluster_rects: Vec::new(),
            },
        ];
        let merged = merge_overlapping_hints(hints);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].x_left - 20.0).abs() < 0.01);
        assert!((merged[0].x_right - 340.0).abs() < 0.01);
    }

    #[test]
    fn stacked_box_three_rows_below_cluster_minimum() {
        // Pins a deliberate precision gate: a 3-box stack stays below the
        // main loop's 6-rect cluster minimum and is NOT detected end-to-end.
        // Routing smaller clusters through detect_stacked_box_table was
        // tried and regressed four pdf-evals documents (striped bullet
        // lists, wrapped regulation text, stats-table columns) with no
        // corpus gains — too few boxes for the anti-prose guards to work.
        // If this ever becomes worth revisiting, the guards need stronger
        // signals first; flipping this assertion is the entry point.
        let mut rects: Vec<PdfRect> = (0..3)
            .map(|i| PdfRect {
                x: 100.0,
                y: 600.0 - i as f32 * 22.0,
                width: 300.0,
                height: 22.0,
                page: 1,
            })
            .collect();
        // Unrelated scattered rects push the page past the 6-rect page gate
        // so the run reaches clustering, while the 3-box stack itself stays
        // below the 6-rect cluster minimum.
        for i in 0..4 {
            rects.push(PdfRect {
                x: 100.0 + i as f32 * 120.0,
                y: 100.0,
                width: 40.0,
                height: 15.0,
                page: 1,
            });
        }
        let items: Vec<TextItem> = ["Step One: Plan", "Step Two: Build", "Step Three: Ship"]
            .iter()
            .enumerate()
            .map(|(i, t)| make_item(t, 120.0, 605.0 - i as f32 * 22.0, 10.0))
            .collect();
        let (tables, _) = detect_tables_from_rects(&items, &rects, 1);
        assert!(
            tables.is_empty(),
            "3-box stacks are intentionally below the detection floor"
        );
    }

    #[test]
    fn failed_cluster_generates_hint_with_items() {
        // A cluster of rects forming an outer border (2 x-edges after snapping)
        // that fails grid detection should produce a hint when items are inside.
        // Use overlapping rects with the same left/right edges but varied heights
        // so row-stripe detection also fails.
        let page_rects: Vec<(f32, f32, f32, f32)> = vec![
            (50.0, 100.0, 400.0, 200.0), // outer border
            (52.0, 102.0, 396.0, 196.0), // inner border (within snap tolerance)
            (51.0, 101.0, 398.0, 198.0), // another border variant
            (50.0, 100.0, 400.0, 10.0),  // top divider (thin)
            (50.0, 290.0, 400.0, 10.0),  // bottom divider (thin)
            (50.0, 195.0, 400.0, 10.0),  // middle divider
        ];
        // Create text items inside the bounding box (≥6 items)
        let mut items: Vec<TextItem> = Vec::new();
        for row in 0..4 {
            for col in 0..3 {
                items.push(TextItem {
                    text: format!("cell{}_{}", row, col),
                    x: 60.0 + col as f32 * 120.0,
                    y: 120.0 + row as f32 * 40.0,
                    width: 50.0,
                    height: 10.0,
                    font: String::new(),
                    font_size: 10.0,
                    page: 1,
                    is_bold: false,
                    is_italic: false,
                    is_underline: false,
                    is_strikeout: false,
                    item_type: crate::types::ItemType::Text,
                    mcid: None,
                });
            }
        }
        let rects: Vec<crate::types::PdfRect> = page_rects
            .iter()
            .map(|&(x, y, w, h)| crate::types::PdfRect {
                x,
                y,
                width: w,
                height: h,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        // Grid detection should fail (2 x-edges after snapping: ~50 and ~450)
        // If detection fails, we should get a failed-cluster hint
        if tables.is_empty() {
            assert_eq!(hints.len(), 1, "failed cluster should produce one hint");
            assert!(!hints[0].cluster_rects.is_empty());
        }
        // If tables were detected, that's also acceptable
    }

    #[test]
    fn text_derived_two_col_prose_is_not_cell_rect_table() {
        let page = 1;
        let mut rects = Vec::new();
        for row in 0..8 {
            rects.push(PdfRect {
                x: 50.0,
                y: 100.0 + row as f32 * 20.0,
                width: 180.0,
                height: 18.0,
                page,
            });
        }

        let mut items = Vec::new();
        let left = [
            "the annual plan was revised",
            "and the team noted changes",
            "this section explains limits",
            "with additional notes below",
            "the policy was reviewed",
            "and results are summarized",
            "this appendix describes scope",
            "with examples for reference",
        ];
        let right = [
            "for each area in the review",
            "as part of the assessment",
            "that were applied in context",
            "to support the conclusion",
            "for use by the committee",
            "as shown in the narrative",
            "that remain under discussion",
            "to clarify the method",
        ];
        for row in 0..8 {
            let y = 104.0 + row as f32 * 20.0;
            let mut left_item = make_item(left[row], 60.0, y, 9.0);
            left_item.width = 50.0;
            items.push(left_item);
            let mut right_item = make_item(right[row], 150.0, y, 9.0);
            right_item.width = 50.0;
            items.push(right_item);
        }

        let (tables, _hints) = detect_tables_from_rects(&items, &rects, page);
        assert!(
            tables.is_empty(),
            "text-derived two-column prose must not be accepted as a rect table; got {:?}",
            tables
                .iter()
                .map(|t| (t.rows.len(), t.columns.len()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiline_indented_description_rows_collapse_to_visual_rows() {
        let page = 1;
        let col_edges = [0.0, 60.0, 420.0, 460.0, 500.0, 540.0];
        let row_edges = [
            340.0, 320.0, 300.0, 270.0, 250.0, 230.0, 200.0, 180.0, 160.0,
        ];

        let mut rects = Vec::new();
        for row in 0..row_edges.len() - 1 {
            let y_top = row_edges[row];
            let y_bot = row_edges[row + 1];
            for col in 0..col_edges.len() - 1 {
                rects.push((
                    col_edges[col],
                    y_bot,
                    col_edges[col + 1] - col_edges[col],
                    y_top - y_bot,
                ));
            }
        }

        let mut items = vec![
            make_item("Controls", 8.0, 330.0, 9.0),
            make_item("Control", 70.0, 330.0, 9.0),
            make_item("IG 1", 428.0, 330.0, 9.0),
            make_item("IG 2", 468.0, 330.0, 9.0),
            make_item("IG 3", 508.0, 330.0, 9.0),
            make_item("Version", 8.0, 310.0, 9.0),
            make_item("v8", 20.0, 285.0, 9.0),
            make_item(
                "4.5 Implement and Manage a Firewall on End-User Devices",
                70.0,
                285.0,
                9.0,
            ),
            make_item("*", 438.0, 285.0, 9.0),
            make_item("*", 478.0, 285.0, 9.0),
            make_item("*", 518.0, 285.0, 9.0),
            make_item("v7", 20.0, 215.0, 9.0),
            make_item(
                "9.4 Apply Host-based Firewalls or Port-Filtering",
                70.0,
                215.0,
                9.0,
            ),
            make_item("*", 478.0, 215.0, 9.0),
            make_item("*", 518.0, 215.0, 9.0),
        ];
        items.push(make_item(
            "Implement and manage a host-based firewall or port-filtering tool",
            84.0,
            260.0,
            8.0,
        ));
        items.push(make_item(
            "on end-user devices with a default-deny rule",
            84.0,
            240.0,
            8.0,
        ));
        items.push(make_item(
            "Apply host-based firewalls or port filtering tools on end systems",
            84.0,
            190.0,
            8.0,
        ));
        items.push(make_item(
            "and deny unauthorized network communication",
            84.0,
            170.0,
            8.0,
        ));

        let table = detect_row_stripe_table_from_cell_rects(&items, &rects, page)
            .expect("expected multiline description table");
        assert_eq!(table.columns.len(), 5);
        assert_eq!(
            table.rows.len(),
            3,
            "wrapped lines should collapse to header plus two data rows"
        );
        assert_eq!(table.cells[0][0], "Controls Version");
        assert!(table.cells[1][1].contains("host-based firewall"));
        assert!(table.cells[1][1].contains("default-deny rule"));
        assert!(table.cells[2][1].contains("deny unauthorized"));
    }

    /// Wire-bordered 4-column table whose header text is centered/right-aligned
    /// inside each cell while the data is left-aligned: cluster_x_positions
    /// merges adjacent columns (data Item→EAN gap is below threshold) and
    /// drops the header-only x-clusters in the filter pass, leaving only 3
    /// text-derived columns. Rect borders are 4 columns of ground truth.
    /// Before the fix the cell-rect path preferred text edges when they were
    /// the smaller set — losing a column. After the fix, 3+ rect columns
    /// always win.
    #[test]
    fn wired_header_data_misaligned_keeps_all_columns_from_rects() {
        let page = 1;
        // 4 cols: Item | EAN | Nombre | Cant
        let col_xs = [380.0_f32, 410.0, 470.0, 660.0, 700.0];
        // Header + 9 data rows at 15pt tall each (y descending).
        let row_ys: Vec<f32> = (0..=10).map(|r| 400.0 - 15.0 * r as f32).collect();

        let mut rects: Vec<(f32, f32, f32, f32)> = Vec::new();
        for r in 0..10 {
            let y_top = row_ys[r];
            let y_bot = row_ys[r + 1];
            for c in 0..4 {
                rects.push((col_xs[c], y_bot, col_xs[c + 1] - col_xs[c], y_top - y_bot));
            }
        }

        let mut items: Vec<TextItem> = Vec::new();
        // Header row (y ≈ 392.5): headers sit further to the right than data
        // because they are centered/right-aligned in the cells.
        items.push(make_item("Item", 389.0, 392.5, 9.0));
        items.push(make_item("EAN", 432.0, 392.5, 9.0));
        items.push(make_item("Nombre", 552.0, 392.5, 9.0));
        items.push(make_item("Cant", 672.0, 392.5, 9.0));

        let names = [
            "Arnes Frontal",
            "Arnes Motor",
            "Arnes Piso",
            "Arnes Techo",
            "Arnes Puerta",
            "Arnes Tablero",
            "Arnes Trasero",
            "Arnes Lateral",
            "Arnes Sensor",
        ];
        for r in 0..9 {
            let y = 377.5 - 15.0 * r as f32;
            items.push(make_item(&(r + 1).to_string(), 396.0, y, 9.0));
            items.push(make_item("7701023403016", 410.0, y, 9.0));
            items.push(make_item(names[r], 480.0, y, 9.0));
            items.push(make_item("1", 680.0, y, 9.0));
        }

        let table = detect_row_stripe_table_from_cell_rects(&items, &rects, page)
            .expect("wired 4-column table with header/data x-misalignment must detect");
        assert_eq!(
            table.columns.len(),
            4,
            "expected 4 columns from rect borders; cells: {:?}",
            table.cells
        );
        for c in 0..4 {
            let any_populated = table.cells.iter().any(|row| !row[c].trim().is_empty());
            assert!(
                any_populated,
                "column {} empty across all rows; cells: {:?}",
                c, table.cells
            );
        }
        // Header row populated in all 4 cells.
        let header = &table.cells[0];
        assert_eq!(header[0].trim(), "Item");
        assert_eq!(header[1].trim(), "EAN");
        assert_eq!(header[2].trim(), "Nombre");
        assert_eq!(header[3].trim(), "Cant");
        // First data row: Item="1", EAN, name, count="1" — no Item↔EAN merge.
        let data1 = &table.cells[1];
        assert_eq!(data1[0].trim(), "1");
        assert_eq!(data1[1].trim(), "7701023403016");
        assert!(data1[2].trim().contains("Arnes"));
        assert_eq!(data1[3].trim(), "1");
    }

    #[test]
    fn failed_cluster_no_hint_without_items() {
        // Rects with no text items inside → no failed-cluster hint generated.
        // Use >6 rects to avoid the rect-sparse path (4-6 rects).
        let page_rects: Vec<(f32, f32, f32, f32)> = vec![
            (50.0, 100.0, 400.0, 200.0),
            (52.0, 102.0, 396.0, 196.0),
            (51.0, 101.0, 398.0, 198.0),
            (50.0, 100.0, 400.0, 10.0),
            (50.0, 290.0, 400.0, 10.0),
            (50.0, 195.0, 400.0, 10.0),
            (50.0, 150.0, 400.0, 10.0),
            (50.0, 250.0, 400.0, 10.0),
        ];
        let rects: Vec<crate::types::PdfRect> = page_rects
            .iter()
            .map(|&(x, y, w, h)| crate::types::PdfRect {
                x,
                y,
                width: w,
                height: h,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&[], &rects, 1);
        // No items → no table, no hint (items_inside check fails)
        if tables.is_empty() {
            assert!(hints.is_empty(), "no items inside → no hint");
        }
    }

    #[test]
    fn failed_cluster_no_hint_narrow_height() {
        // Cluster with only 20pt height (header band) should not produce hint
        // even with items inside (height < 100pt threshold)
        let page_rects: Vec<(f32, f32, f32, f32)> = vec![
            (50.0, 650.0, 50.0, 20.0),
            (100.0, 650.0, 50.0, 20.0),
            (150.0, 650.0, 50.0, 20.0),
            (200.0, 650.0, 50.0, 20.0),
            (250.0, 650.0, 50.0, 20.0),
            (300.0, 650.0, 50.0, 20.0),
            (350.0, 650.0, 50.0, 20.0),
            (400.0, 650.0, 50.0, 20.0),
        ];
        let mut items: Vec<TextItem> = Vec::new();
        for col in 0..8 {
            items.push(TextItem {
                text: format!("hdr{}", col),
                x: 55.0 + col as f32 * 50.0,
                y: 655.0,
                width: 40.0,
                height: 10.0,
                font: String::new(),
                font_size: 10.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: crate::types::ItemType::Text,
                mcid: None,
            });
        }
        let rects: Vec<crate::types::PdfRect> = page_rects
            .iter()
            .map(|&(x, y, w, h)| crate::types::PdfRect {
                x,
                y,
                width: w,
                height: h,
                page: 1,
            })
            .collect();
        let (tables, hints) = detect_tables_from_rects(&items, &rects, 1);
        assert!(tables.is_empty());
        assert!(
            hints.is_empty(),
            "narrow header band (20pt) should not produce hint"
        );
    }

    // --- page-bg clustering exclusion ---

    #[test]
    fn page_bg_rects_do_not_bridge_separate_clusters() {
        // Simulate page 27 scenario: two groups of row stripes at different Y
        // ranges, connected by full-page background rects at (0,0).
        // Without exclusion, all rects cluster into one group.
        // With exclusion, two separate clusters form.
        let mut rects = Vec::new();
        let page = 1;

        // Group 1: 7 row stripes at Y=444..537 (Reference Group table)
        for i in 0..7 {
            let y = 444.0 + i as f32 * 15.5;
            rects.push(PdfRect {
                x: 44.0,
                y,
                width: 505.0,
                height: 15.5,
                page,
            });
        }

        // Group 2: 4 row stripes at Y=176..238 (smaller table)
        for i in 0..4 {
            let y = 176.0 + i as f32 * 15.5;
            rects.push(PdfRect {
                x: 44.0,
                y,
                width: 505.0,
                height: 15.5,
                page,
            });
        }

        // 3 full-page background rects at origin
        for _ in 0..3 {
            rects.push(PdfRect {
                x: 0.0,
                y: 0.0,
                width: 594.0,
                height: 774.0,
                page,
            });
        }

        // Items in group 1 region for row-stripe detection
        let mut items = Vec::new();
        for i in 0..7 {
            let y = 449.0 + i as f32 * 15.5;
            items.push(make_item("Company Name", 50.0, y, 9.0));
            items.push(make_item("P", 320.0, y, 9.0));
            items.push(make_item("P", 450.0, y, 9.0));
        }

        let (tables, _hints) = detect_tables_from_rects(&items, &rects, page);
        // Should detect the group 1 table (7 row stripes) without being
        // confused by group 2 stripes bridged via page-bg rects.
        assert!(
            !tables.is_empty(),
            "should detect table from row stripes when page-bg rects are excluded from clustering"
        );
        // The table should have rows from group 1 only, not spanning to group 2
        let table = &tables[0];
        assert!(
            table.rows.len() <= 8,
            "table should have at most ~7 rows from group 1, got {}",
            table.rows.len()
        );
    }
}
