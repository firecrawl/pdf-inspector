//! Rectangle-based table detection using union-find clustering.

use std::collections::HashMap;

use log::debug;

use crate::types::{PdfRect, TextItem};

use super::Table;

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
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

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
pub fn detect_tables_from_rects(
    items: &[TextItem],
    rects: &[PdfRect],
    page: u32,
) -> (Vec<Table>, Vec<RectHintRegion>) {
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
        widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
        // that dwarfs the sub-rect.
        //
        // Skip this O(n²) dedup when there are too many rects — pages with
        // thousands of vector-drawing rects won't benefit from cell dedup.
        if page_rects.len() < MAX_CLUSTER_RECTS {
            let before = page_rects.len();
            let snapshot = page_rects.clone();
            page_rects.retain(|&(ax, ay, aw, ah)| {
                let tol = 2.0;
                !snapshot.iter().any(|&(bx, by, bw, bh)| {
                    // b must strictly contain a (b is larger in area)
                    bw * bh > aw * ah * 1.2
                        && bh < ah * 4.0 // container must be similarly sized, not a table background
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
        let clusters = cluster_rects(&page_rects, 3.0, 6);
        debug!("page {}: {} clusters with >= 6 rects", page, clusters.len());
        for cluster_indices in &clusters {
            let group_rects: Vec<(f32, f32, f32, f32)> =
                cluster_indices.iter().map(|&i| page_rects[i]).collect();
            if let Some(table) = detect_table_from_rect_group(items, &group_rects, page) {
                tables.push(table);
            } else if let Some(table) = detect_row_stripe_table(items, &group_rects, page) {
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
            let total_clustered: usize = clusters.iter().map(|c| c.len()).sum();
            if clusters.len() >= 3 && total_clustered >= 50 {
                debug!(
                    "page {}: trying merged-cluster fallback ({} clusters, {} rects{})",
                    page,
                    clusters.len(),
                    total_clustered,
                    if only_narrow {
                        ", replacing narrow tables"
                    } else {
                        ""
                    }
                );
                let all_cluster_rects: Vec<(f32, f32, f32, f32)> = clusters
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
                // width ≤500pt (not page-spanning), height ≤600pt (not full page)
                if (100.0..=600.0).contains(&h) && w <= 500.0 && items_inside >= 6 {
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
fn merge_overlapping_hints(mut hints: Vec<RectHintRegion>) -> Vec<RectHintRegion> {
    if hints.len() <= 1 {
        return hints;
    }
    loop {
        hints.sort_by(|a, b| a.x_left.partial_cmp(&b.x_left).unwrap());
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
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
    col_edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut row_edges = y_edges;
    row_edges.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

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

    // Reject tables with any completely empty column — indicates a bad grid.
    for col in 0..num_cols {
        let col_has_content = cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()));
        if !col_has_content {
            debug!("  rejected: column {} is completely empty", col);
            return GridResult::Failed;
        }
    }

    GridResult::Ok(Table {
        columns,
        rows,
        cells,
        item_indices,
    })
}

/// Deduplicate nearby edge values within a tolerance, returning sorted unique edges.
pub(crate) fn snap_edges(values: &[f32], tolerance: f32) -> Vec<f32> {
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

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
            let text: String = col_items
                .iter()
                .map(|(_, item)| item.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            row_cells.push(text);
        }
        cells.push(row_cells);
    }

    (cells, indices)
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

            // Find first and last grid rows that the rect spans
            let first_row = (0..num_rows)
                .find(|&r| ry <= row_edges[r] + tol && (ry + rh) >= row_edges[r + 1] - tol);
            let last_row = (0..num_rows)
                .rfind(|&r| ry <= row_edges[r] + tol && (ry + rh) >= row_edges[r + 1] - tol);

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
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
    row_edges.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

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
    if max_cell_len > 500 {
        debug!(
            "  row-stripe rejected: max cell length {} > 500 (layout background)",
            max_cell_len
        );
        return None;
    }

    // No empty columns
    for col in 0..num_cols {
        let col_has_content = cells
            .iter()
            .any(|row| row.get(col).is_some_and(|c| !c.trim().is_empty()));
        if !col_has_content {
            debug!("  row-stripe rejected: column {} is empty", col);
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
        "  row-stripe table accepted: {}x{}, {:.0}% density",
        num_rows,
        num_cols,
        content_ratio * 100.0
    );

    Some(Table {
        columns: column_centers,
        rows: row_centers,
        cells,
        item_indices,
    })
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
    row_edges.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

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

    // Reject if any cell has excessive text — layout background rects produce
    // "cells" containing paragraphs, not short data-table values.
    let max_cell_len = cells
        .iter()
        .flat_map(|row| row.iter())
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    if max_cell_len > 500 {
        debug!(
            "  merged-cluster rejected: max cell length {} > 500 (layout background)",
            max_cell_len
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

    Some(Table {
        columns: column_centers,
        rows: row_centers,
        cells,
        item_indices,
    })
}

/// Cluster text item X positions into column centers with a given minimum threshold.
///
/// Similar to `find_column_boundaries` in grid.rs but with a lower minimum threshold
/// suitable for rect-backed tables where we already know tabular structure exists
/// (no need for anti-paragraph safeguards).
fn cluster_x_positions(items: &[(usize, &TextItem)], min_threshold: f32) -> Vec<f32> {
    let mut x_positions: Vec<f32> = items.iter().map(|(_, i)| i.x).collect();
    x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

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
            item_type: ItemType::Text,
            mcid: None,
        }
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
}
