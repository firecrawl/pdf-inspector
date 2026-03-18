//! Column detection, line grouping, and reading-order layout.

use std::collections::HashMap;

use crate::text_utils::{effective_width, sort_line_items};
use crate::types::{TextItem, TextLine};
use log::debug;

/// Represents a column region on a page
#[derive(Debug, Clone)]
pub(crate) struct ColumnRegion {
    pub(crate) x_min: f32,
    pub(crate) x_max: f32,
}

/// Detect column boundaries on a page using a horizontal projection profile.
///
/// Builds an occupancy histogram across the page width and finds empty valleys
/// (gutters) where no text exists. Validates valleys with vertical consistency
/// checks to avoid false positives.
pub(crate) fn detect_columns(items: &[TextItem], page: u32) -> Vec<ColumnRegion> {
    const BIN_WIDTH: f32 = 2.0;
    const MIN_GUTTER_WIDTH: f32 = 8.0;
    const MIN_VERTICAL_SPAN_RATIO: f32 = 0.30;
    const MIN_ITEMS_PER_COLUMN: usize = 10;
    const NOISE_FRACTION: f32 = 0.15;

    // Get items for this page
    let page_items: Vec<&TextItem> = items.iter().filter(|i| i.page == page).collect();

    if page_items.is_empty() {
        return vec![];
    }

    // Find page bounds
    let x_min = page_items.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let x_max = page_items
        .iter()
        .map(|i| i.x + effective_width(i))
        .fold(f32::NEG_INFINITY, f32::max);

    let page_width = x_max - x_min;
    if page_width < 200.0 {
        return vec![ColumnRegion { x_min, x_max }];
    }

    if page_items.len() < 20 {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Build occupancy histogram.
    // Exclude items wider than 60% of page width — these are spanning items
    // (titles, full-width paragraphs) that would fill the gutter and prevent
    // detection of partial-page column layouts (e.g. two-column abstracts on
    // a page that also has single-column introduction text).
    let wide_threshold = page_width * 0.6;
    let num_bins = ((page_width / BIN_WIDTH).ceil() as usize).max(1);
    let mut histogram = vec![0u32; num_bins];

    for item in &page_items {
        let w = effective_width(item);
        if w > wide_threshold {
            continue;
        }
        let left = ((item.x - x_min) / BIN_WIDTH).floor() as usize;
        let right = (((item.x + w) - x_min) / BIN_WIDTH).ceil() as usize;
        let left = left.min(num_bins);
        let right = right.min(num_bins);
        for count in histogram.iter_mut().take(right).skip(left) {
            *count += 1;
        }
    }

    // Find the noise threshold: bins with count <= max_count * NOISE_FRACTION are "empty"
    let max_count = *histogram.iter().max().unwrap_or(&0);
    let noise_threshold = (max_count as f32 * NOISE_FRACTION) as u32;

    // Find empty valleys (consecutive runs of low-count bins)
    // Each valley is stored as (start_bin, end_bin)
    let mut valleys: Vec<(usize, usize)> = Vec::new();
    let mut valley_start: Option<usize> = None;

    for (i, &count) in histogram.iter().enumerate() {
        if count <= noise_threshold {
            if valley_start.is_none() {
                valley_start = Some(i);
            }
        } else if let Some(start) = valley_start {
            valleys.push((start, i));
            valley_start = None;
        }
    }
    // Close any valley that extends to the end
    if let Some(start) = valley_start {
        valleys.push((start, num_bins));
    }

    // Filter valleys: must be wide enough and not at page margins
    let margin_threshold = page_width * 0.05;
    let valleys: Vec<(usize, usize)> = valleys
        .into_iter()
        .filter(|&(start, end)| {
            let width_pts = (end - start) as f32 * BIN_WIDTH;
            if width_pts < MIN_GUTTER_WIDTH {
                return false;
            }
            // Valley center must not be within 5% of page edges
            let center_pts = ((start + end) as f32 / 2.0) * BIN_WIDTH;
            center_pts > margin_threshold && center_pts < (page_width - margin_threshold)
        })
        .collect();

    if valleys.is_empty() {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Compute Y range of the page
    let y_min = page_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let y_max = page_items
        .iter()
        .map(|i| i.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_range = y_max - y_min;

    // Validate each valley with vertical consistency
    // Each entry: (start_bin, end_bin, left_count, right_count)
    let mut valid_valleys: Vec<(usize, usize, usize, usize)> = Vec::new();
    for &(start, end) in &valleys {
        let gutter_left = x_min + start as f32 * BIN_WIDTH;
        let gutter_right = x_min + end as f32 * BIN_WIDTH;
        let gutter_center = (gutter_left + gutter_right) / 2.0;

        // Collect items on each side of the gutter
        let left_items: Vec<&&TextItem> = page_items
            .iter()
            .filter(|i| i.x + effective_width(i) <= gutter_center)
            .collect();
        let right_items: Vec<&&TextItem> =
            page_items.iter().filter(|i| i.x >= gutter_center).collect();

        if left_items.len() < MIN_ITEMS_PER_COLUMN || right_items.len() < MIN_ITEMS_PER_COLUMN {
            continue;
        }

        // Check vertical overlap
        if y_range > 0.0 {
            let left_y_min = left_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
            let left_y_max = left_items
                .iter()
                .map(|i| i.y)
                .fold(f32::NEG_INFINITY, f32::max);
            let right_y_min = right_items
                .iter()
                .map(|i| i.y)
                .fold(f32::INFINITY, f32::min);
            let right_y_max = right_items
                .iter()
                .map(|i| i.y)
                .fold(f32::NEG_INFINITY, f32::max);

            let overlap_min = left_y_min.max(right_y_min);
            let overlap_max = left_y_max.min(right_y_max);
            let overlap = (overlap_max - overlap_min).max(0.0);

            if overlap / y_range < MIN_VERTICAL_SPAN_RATIO {
                continue;
            }
        }

        valid_valleys.push((start, end, left_items.len(), right_items.len()));
    }

    if valid_valleys.is_empty() {
        return vec![ColumnRegion { x_min, x_max }];
    }

    debug!(
        "page {}: {} columns detected (boundaries: {:?})",
        page,
        valid_valleys.len() + 1,
        valid_valleys
            .iter()
            .map(|(s, e, _, _)| x_min + ((*s + *e) as f32 / 2.0) * BIN_WIDTH)
            .collect::<Vec<_>>()
    );

    // Limit to at most 3 gutters (4 columns).
    // Score = width_in_bins * min(left_count, right_count)
    // This prefers gutters that separate substantial content on both sides,
    // rather than just the physically widest gaps (which may be intra-column).
    if valid_valleys.len() > 3 {
        valid_valleys.sort_by(|a, b| {
            let score_a = (a.1 - a.0) as f32 * (a.2.min(a.3) as f32);
            let score_b = (b.1 - b.0) as f32 * (b.2.min(b.3) as f32);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        valid_valleys.truncate(3);
        // Re-sort by position (left to right)
        valid_valleys.sort_by_key(|v| v.0);
    }

    // Build column regions from gutter boundaries
    let mut columns = Vec::new();
    let mut col_start = x_min;
    for &(start, end, _, _) in &valid_valleys {
        let gutter_center = x_min + ((start + end) as f32 / 2.0) * BIN_WIDTH;
        columns.push(ColumnRegion {
            x_min: col_start,
            x_max: gutter_center,
        });
        col_start = gutter_center;
    }
    columns.push(ColumnRegion {
        x_min: col_start,
        x_max,
    });

    columns
}

/// Determines if a text item spans across multiple column regions (e.g. full-width headers/titles).
fn spans_multiple_columns(item: &TextItem, columns: &[ColumnRegion]) -> bool {
    let w = effective_width(item);
    let item_right = item.x + w;
    let overlap_count = columns
        .iter()
        .filter(|col| {
            let overlap_start = item.x.max(col.x_min);
            let overlap_end = item_right.min(col.x_max);
            let overlap = (overlap_end - overlap_start).max(0.0);
            overlap > (col.x_max - col.x_min) * 0.10 || overlap > 20.0
        })
        .count();
    overlap_count >= 2
}

/// Check if a text item is likely a page number
fn is_page_number(item: &TextItem) -> bool {
    let text = item.text.trim();

    // Must be 1-4 digits only
    if text.is_empty() || text.len() > 4 {
        return false;
    }
    if !text.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    // Must be at top or bottom of page.
    // US Letter = 792pt, A4 = 841pt. Page numbers are typically in the
    // top ~5% or bottom ~12% of the page.
    item.y > 720.0 || item.y < 100.0
}

/// Group text items into lines, with multi-column support
/// Detect newspaper-style columns: independent text flows that should be read
/// sequentially (all of col1, then col2) rather than Y-interleaved.
pub(crate) fn is_newspaper_layout(
    per_column_lines: &[Vec<TextLine>],
    columns: &[ColumnRegion],
) -> bool {
    if per_column_lines.len() < 2 {
        return false;
    }

    // Each column must independently have substantial content
    let min_lines = per_column_lines.iter().map(|c| c.len()).min().unwrap_or(0);
    let max_lines = per_column_lines.iter().map(|c| c.len()).max().unwrap_or(0);

    if min_lines < 5 {
        return false;
    }

    if min_lines < 15 {
        // Sidebar detection: a narrow annotation column beside a wide body column.
        // Guards:
        //   - Only 2 columns (sidebars are body+sidebar, not 3+ columns)
        //   - width_ratio < 0.50: sidebar is much narrower than body
        //   - line_balance < 0.35: sidebar has significantly fewer lines
        //   - max_lines >= 20: body column has substantial prose content
        //   - narrower column has fewer lines (not a dense reference column)
        if columns.len() == 2 && per_column_lines.len() == 2 {
            let w0 = columns[0].x_max - columns[0].x_min;
            let w1 = columns[1].x_max - columns[1].x_min;
            let width_ratio = w0.min(w1) / w0.max(w1);
            let line_balance = if max_lines > 0 {
                min_lines as f32 / max_lines as f32
            } else {
                1.0
            };
            let narrow_width = w0.min(w1);
            if width_ratio < 0.50 && line_balance < 0.35 && max_lines >= 20 && narrow_width >= 160.0
            {
                let narrower_idx = if w0 < w1 { 0 } else { 1 };
                let fewest_idx = if per_column_lines[0].len() <= per_column_lines[1].len() {
                    0
                } else {
                    1
                };
                if narrower_idx == fewest_idx {
                    // Sparse density check: sidebar annotations are spread thinly
                    // across the page height while regular two-column text is dense.
                    // Compare average Y-gap between successive lines in each column.
                    let narrow = &per_column_lines[narrower_idx];
                    let wide = &per_column_lines[1 - narrower_idx];
                    let avg_gap = |lines: &[TextLine]| -> f32 {
                        if lines.len() < 2 {
                            return 0.0;
                        }
                        let mut ys: Vec<f32> = lines.iter().map(|l| l.y).collect();
                        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let span = ys.last().unwrap() - ys.first().unwrap();
                        span / (lines.len() as f32 - 1.0)
                    };
                    let narrow_gap = avg_gap(narrow);
                    let wide_gap = avg_gap(wide);
                    // Sidebar annotations have >2.5x the average gap of body text
                    if wide_gap > 0.0 && narrow_gap / wide_gap >= 2.5 {
                        return true;
                    }
                }
            }
        }
        return false;
    }

    // Dense balanced columns (similar line counts) are newspaper regardless of Y-alignment.
    // By this point table items are already removed, so two dense balanced columns
    // of remaining text are independent prose flows.
    let balance_ratio = min_lines as f32 / max_lines as f32;
    if balance_ratio > 0.7 {
        return true;
    }

    // For unbalanced columns, fall back to Y-collision check
    let y_tol = 5.0; // was 3.0 — handles government gazette typesetting variance
    let (smallest_idx, _) = per_column_lines
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| c.len())
        .unwrap();

    let smallest = &per_column_lines[smallest_idx];
    let mut collisions = 0u32;
    for line in smallest {
        for (ci, col) in per_column_lines.iter().enumerate() {
            if ci == smallest_idx {
                continue;
            }
            if col.iter().any(|ol| (ol.y - line.y).abs() < y_tol) {
                collisions += 1;
                break;
            }
        }
    }

    let ratio = collisions as f32 / smallest.len() as f32;
    ratio > 0.5
}

/// Split column lines into a core cluster and stragglers.
/// The core is the largest group of consecutive lines separated by normal
/// line spacing. Lines in other groups (header remnants, per-word items from
/// full-width lines) are returned as stragglers.
fn split_column_stragglers(lines: Vec<TextLine>) -> (Vec<TextLine>, Vec<TextLine>) {
    if lines.len() < 3 {
        return (lines, Vec::new());
    }

    // Lines are sorted Y descending (top-first). Compute gaps.
    let mut gaps: Vec<f32> = Vec::new();
    for i in 0..lines.len() - 1 {
        gaps.push(lines[i].y - lines[i + 1].y);
    }

    // Median gap = typical line spacing
    let mut sorted_gaps = gaps.clone();
    sorted_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_gap = sorted_gaps[sorted_gaps.len() / 2];

    // A gap > 3× median (min 30pt) indicates a break between content clusters
    let threshold = (median_gap * 3.0).max(30.0);

    // Find all split points
    let mut split_indices: Vec<usize> = Vec::new();
    for (i, &gap) in gaps.iter().enumerate() {
        if gap > threshold {
            split_indices.push(i);
        }
    }

    if split_indices.is_empty() {
        return (lines, Vec::new());
    }

    // Build segments: (start_line_idx, end_line_idx_exclusive)
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for &si in &split_indices {
        segments.push((start, si + 1));
        start = si + 1;
    }
    segments.push((start, lines.len()));

    // Find the largest segment (the core cluster)
    let (core_seg, _) = segments
        .iter()
        .enumerate()
        .max_by_key(|(_, (s, e))| e - s)
        .unwrap();

    let (cs, ce) = segments[core_seg];
    let mut core = Vec::with_capacity(ce - cs);
    let mut stragglers = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if i >= cs && i < ce {
            core.push(line);
        } else {
            stragglers.push(line);
        }
    }

    (core, stragglers)
}

pub fn group_into_lines(items: Vec<TextItem>) -> Vec<TextLine> {
    group_into_lines_with_thresholds(items, &HashMap::new())
}

/// Group text items into lines, using pre-computed per-page adaptive thresholds
/// from Canva-style letter-spacing detection. Falls back to computing the
/// threshold from item gaps when no pre-computed value is available.
pub(crate) fn group_into_lines_with_thresholds(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Filter out page numbers (standalone numbers at top/bottom of page)
    let items: Vec<TextItem> = items
        .into_iter()
        .filter(|item| !is_page_number(item))
        .collect();

    // Get unique pages
    let mut pages: Vec<u32> = items.iter().map(|i| i.page).collect();
    pages.sort();
    pages.dedup();

    let mut all_lines = Vec::new();

    for page in pages {
        let page_items: Vec<TextItem> = items.iter().filter(|i| i.page == page).cloned().collect();

        // Use pre-computed threshold from fix_letterspaced_items if available
        // (computed before embedded-space removal, with full signal).
        // Non-Canva pages use the default 0.10 threshold.
        let adaptive_threshold = page_thresholds.get(&page).copied().unwrap_or(0.10);

        // Detect columns for this page
        let columns = detect_columns(&page_items, page);

        if columns.len() <= 1 {
            // Single column - use simple sorting
            let lines = group_single_column(page_items, adaptive_threshold);
            all_lines.extend(lines);
        } else {
            // Multi-column - separate spanning items from column items
            let mut spanning_items: Vec<TextItem> = Vec::new();
            let mut column_items: Vec<TextItem> = Vec::new();

            for item in &page_items {
                if spans_multiple_columns(item, &columns) {
                    spanning_items.push(item.clone());
                } else {
                    column_items.push(item.clone());
                }
            }

            // Process each column's items independently, preserving column identity.
            // Assign each item to the column with greatest horizontal overlap
            // (instead of center-point) to avoid gutter mis-assignment.
            let mut col_buckets: Vec<Vec<TextItem>> = vec![Vec::new(); columns.len()];
            for item in &column_items {
                let item_left = item.x;
                let item_right = item.x + effective_width(item);
                let mut best_col = 0;
                let mut best_overlap = f32::NEG_INFINITY;
                for (ci, col) in columns.iter().enumerate() {
                    let overlap = (item_right.min(col.x_max) - item_left.max(col.x_min)).max(0.0);
                    if overlap > best_overlap {
                        best_overlap = overlap;
                        best_col = ci;
                    }
                }
                col_buckets[best_col].push(item.clone());
            }

            debug!(
                "page {}: {} columns, {} spanning items",
                page,
                columns.len(),
                spanning_items.len()
            );
            for (ci, col) in columns.iter().enumerate() {
                debug!(
                    "  col {}: x=[{:.0}..{:.0}] {} items",
                    ci,
                    col.x_min,
                    col.x_max,
                    col_buckets[ci].len()
                );
            }
            if log::log_enabled!(log::Level::Trace) {
                for (ci, bucket) in col_buckets.iter().enumerate() {
                    for item in bucket {
                        log::trace!(
                            "  col {} <- x={:7.1} y={:7.1} {:?}",
                            ci,
                            item.x,
                            item.y,
                            if item.text.len() > 60 {
                                &item.text[..60]
                            } else {
                                &item.text
                            }
                        );
                    }
                }
            }

            let mut per_column_lines: Vec<Vec<TextLine>> = Vec::new();
            for col_items in col_buckets {
                let lines = group_single_column(col_items, adaptive_threshold);
                per_column_lines.push(lines);
            }

            // Process spanning items as their own group
            let spanning_lines = group_single_column(spanning_items, adaptive_threshold);

            let is_newspaper = is_newspaper_layout(&per_column_lines, &columns);
            debug!(
                "page {}: layout={}",
                page,
                if is_newspaper { "newspaper" } else { "tabular" }
            );

            if is_newspaper {
                // Newspaper: columns are independent text flows.
                // 1. Split each column into its densest cluster (core) and stragglers
                // 2. Use core columns to determine the above/below threshold
                // 3. Emit: above items → core columns sequentially → below items
                let mut core_columns: Vec<Vec<TextLine>> = Vec::new();
                let mut col_stragglers: Vec<Vec<TextLine>> = Vec::new();
                for col in per_column_lines {
                    let (core, stragglers) = split_column_stragglers(col);
                    core_columns.push(core);
                    col_stragglers.push(stragglers);
                }

                // col_top = min of max Y across core columns
                let col_top = core_columns
                    .iter()
                    .filter(|c| !c.is_empty())
                    .map(|c| c.iter().map(|l| l.y).fold(f32::NEG_INFINITY, f32::max))
                    .fold(f32::INFINITY, f32::min);
                let margin = 5.0;

                let mut above: Vec<TextLine> = Vec::new();
                let mut below_spanning: Vec<TextLine> = Vec::new();

                // Spanning items: above or below the column region
                for line in spanning_lines {
                    if line.y > col_top + margin {
                        above.push(line);
                    } else {
                        below_spanning.push(line);
                    }
                }

                // Column stragglers above col_top go to "above";
                // below col_top they stay with their column to avoid
                // re-interleaving when sorted by Y.
                let mut col_below: Vec<Vec<TextLine>> = vec![Vec::new(); core_columns.len()];
                for (ci, stragglers) in col_stragglers.into_iter().enumerate() {
                    for line in stragglers {
                        if line.y > col_top + margin {
                            above.push(line);
                        } else {
                            col_below[ci].push(line);
                        }
                    }
                }

                above.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));
                below_spanning
                    .sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));

                all_lines.extend(above);
                for col in core_columns {
                    all_lines.extend(col);
                }
                for cb in col_below {
                    all_lines.extend(cb);
                }
                all_lines.extend(below_spanning);
            } else {
                // Tabular: Y-interleaved merge — rows at the same Y from
                // different columns form a single logical line.
                let mut all_page_lines: Vec<TextLine> = Vec::new();
                all_page_lines.extend(spanning_lines);
                for col_lines in per_column_lines {
                    all_page_lines.extend(col_lines);
                }

                // Sort by Y descending (top-first), then by X for same-Y lines
                all_page_lines.sort_by(|a, b| {
                    b.y.partial_cmp(&a.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(
                            a.items
                                .first()
                                .map(|i| i.x)
                                .unwrap_or(0.0)
                                .partial_cmp(&b.items.first().map(|i| i.x).unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal),
                        )
                });

                // Merge lines at the same Y (within tolerance) into single lines
                let y_tol = 3.0;
                let mut merged: Vec<TextLine> = Vec::new();
                for line in all_page_lines {
                    if let Some(last) = merged.last_mut() {
                        if last.page == line.page && (last.y - line.y).abs() < y_tol {
                            last.items.extend(line.items);
                            sort_line_items(&mut last.items);
                            continue;
                        }
                    }
                    merged.push(line);
                }

                all_lines.extend(merged);
            }
        }
    }

    all_lines
}

/// Determine if Y-sorting should be used instead of stream order.
/// Returns true if the stream order appears chaotic (items jump around in Y position).
fn should_use_y_sorting(items: &[TextItem]) -> bool {
    if items.len() < 5 {
        return false; // Not enough items to judge
    }

    // Sample Y positions from stream order
    let y_positions: Vec<f32> = items.iter().map(|i| i.y).collect();

    // Count "order violations" - cases where Y increases (going up) when it should decrease
    // In proper reading order, Y should generally decrease (top to bottom)
    let mut large_jumps_up = 0;
    let mut large_jumps_down = 0;
    let jump_threshold = 50.0; // Significant Y jump

    for window in y_positions.windows(2) {
        let delta = window[1] - window[0];
        if delta > jump_threshold {
            large_jumps_up += 1; // Y increased significantly (jumped up on page)
        } else if delta < -jump_threshold {
            large_jumps_down += 1; // Y decreased significantly (normal reading direction)
        }
    }

    // If there are many upward jumps relative to downward jumps, order is chaotic
    // A well-ordered document should have mostly downward progression
    let total_jumps = large_jumps_up + large_jumps_down;
    if total_jumps < 3 {
        return false; // Not enough jumps to judge
    }

    // If more than 40% of large jumps are upward, use Y-sorting
    let chaos_ratio = large_jumps_up as f32 / total_jumps as f32;
    chaos_ratio > 0.4
}

/// Group items from a single column into lines
/// Uses heuristics to decide between PDF stream order and Y-position sorting.
fn group_single_column(items: Vec<TextItem>, adaptive_threshold: f32) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Decide whether to use stream order or Y-sorting
    let use_y_sorting = should_use_y_sorting(&items);

    let items = if use_y_sorting {
        // Sort by Y descending (top to bottom in PDF coords)
        let mut sorted = items;
        sorted.sort_by(|a, b| {
            b.y.partial_cmp(&a.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        sorted
    } else {
        items
    };

    // Group items into lines
    let mut lines: Vec<TextLine> = Vec::new();
    let y_tolerance = 3.0;

    for item in items {
        // Only check the most recent line for merging
        let should_merge = lines.last().is_some_and(|last_line| {
            if last_line.page != item.page {
                return false;
            }
            let y_diff = (last_line.y - item.y).abs();
            if y_diff >= y_tolerance {
                return false;
            }
            // Check if this looks like a new line despite similar Y:
            // If items are at the same X position (left margin) but different Y,
            // they're vertically stacked lines, not the same line
            let has_y_change = y_diff > 0.5;
            if has_y_change {
                if let Some(first_item) = last_line.items.first() {
                    let at_same_x = (item.x - first_item.x).abs() < 5.0;
                    // If at same X (left margin) with Y change, it's likely a new line
                    if at_same_x {
                        return false;
                    }
                    // If new item starts significantly to the left with Y change,
                    // it's a new line (not just out-of-order items on same line)
                    if let Some(last_item) = last_line.items.last() {
                        if item.x < last_item.x - 10.0 {
                            return false;
                        }
                    }
                }
            }
            true
        });

        if should_merge {
            // Add to the most recent line
            lines.last_mut().unwrap().items.push(item);
        } else {
            // Create new line
            let y = item.y;
            let page = item.page;
            lines.push(TextLine {
                items: vec![item],
                y,
                page,
                adaptive_threshold,
            });
        }
    }

    // Sort items within each line by X position (direction-aware)
    for line in &mut lines {
        sort_line_items(&mut line.items);
    }

    debug!("group_single_column: {} lines", lines.len());

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::ItemType;

    /// Helper: create a TextItem at given position with given width text.
    fn make_item(page: u32, x: f32, y: f32, text: &str) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * 6.0, // ~6pt per char
            height: 12.0,
            font_size: 12.0,
            font: String::new(),
            page,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    /// Generate dense items in a horizontal zone across many Y positions.
    /// Items are placed with overlapping coverage so no intra-zone valleys appear.
    fn fill_zone(page: u32, x_start: f32, x_end: f32, y_start: f32, y_end: f32) -> Vec<TextItem> {
        let mut items = Vec::new();
        let item_width = 60.0; // "SomeText__" = 10 chars * 6pt
        let step = 55.0; // overlap slightly to avoid intra-zone histogram gaps
        let mut y = y_start;
        while y >= y_end {
            let mut x = x_start;
            while x + item_width <= x_end {
                items.push(make_item(page, x, y, "SomeText__"));
                x += step;
            }
            y -= 14.0;
        }
        items
    }

    #[test]
    fn three_zone_layout_detected() {
        // Left months (x=15..330), right months (x=345..660), sidebar (x=675..800)
        // Each zone is >100pt wide so min_col_width won't reject any.
        let mut items = Vec::new();
        items.extend(fill_zone(1, 15.0, 330.0, 750.0, 50.0));
        items.extend(fill_zone(1, 345.0, 660.0, 750.0, 50.0));
        items.extend(fill_zone(1, 675.0, 800.0, 750.0, 50.0));

        let cols = detect_columns(&items, 1);
        assert_eq!(cols.len(), 3, "Expected 3 columns, got {}", cols.len());

        // Gutter 1 should be in the gap between left and middle zones
        let g1 = cols[0].x_max;
        assert!(
            (290.0..=350.0).contains(&g1),
            "First gutter at {g1}, expected between left and middle zones"
        );

        // Gutter 2 should be in the gap between middle and right zones
        let g2 = cols[1].x_max;
        assert!(
            (620.0..=680.0).contains(&g2),
            "Second gutter at {g2}, expected between middle and right zones"
        );
    }

    #[test]
    fn two_column_regression_guard() {
        // Standard 2-column layout with clear gutter at center
        let mut items = Vec::new();
        items.extend(fill_zone(1, 30.0, 280.0, 750.0, 50.0));
        items.extend(fill_zone(1, 320.0, 570.0, 750.0, 50.0));

        let cols = detect_columns(&items, 1);
        assert_eq!(cols.len(), 2, "Expected 2 columns, got {}", cols.len());

        let gutter = cols[0].x_max;
        assert!(
            (280.0..=320.0).contains(&gutter),
            "Gutter at {gutter}, expected ~300"
        );
    }

    #[test]
    fn score_prefers_balanced_gutter_over_wide_gap() {
        // 5 valid valleys: 2 are wide but split sparse content, 2 are narrower
        // but separate dense zones. The dense-zone gutters should win.
        let mut items = Vec::new();
        // Dense left zone
        items.extend(fill_zone(1, 15.0, 200.0, 750.0, 50.0));
        // Dense middle zone
        items.extend(fill_zone(1, 220.0, 400.0, 750.0, 50.0));
        // Dense right zone
        items.extend(fill_zone(1, 420.0, 600.0, 750.0, 50.0));
        // Sparse far-right zone (few items)
        for y_off in 0..12 {
            items.push(make_item(
                1,
                700.0,
                750.0 - y_off as f32 * 50.0,
                "Sparse____",
            ));
        }

        let cols = detect_columns(&items, 1);
        // Should detect the gutters between the 3 dense zones, not the wide gap
        // before the sparse zone
        assert!(
            cols.len() >= 3,
            "Expected >=3 columns for dense zones, got {}",
            cols.len()
        );
    }

    /// Helper: build a Vec<TextLine> with `n` lines at given X, starting at Y=700.
    fn make_lines(n: usize, x: f32) -> Vec<TextLine> {
        (0..n)
            .map(|i| {
                let y = 700.0 - i as f32 * 14.0;
                let item = make_item(1, x, y, "SomeText__");
                TextLine {
                    y,
                    page: 1,
                    adaptive_threshold: 0.10,
                    items: vec![item],
                }
            })
            .collect()
    }

    #[test]
    fn sidebar_layout_detected_as_newspaper() {
        // Wide body column (x 0..400) with 40 lines,
        // narrow sidebar (x 420..590, width 170) with 12 lines.
        // width_ratio = 170/400 = 0.425, line_balance = 12/40 = 0.30 → sidebar → newspaper
        // Sidebar lines have ~3x gap of body lines (sparse annotations).
        let body = make_lines(40, 50.0);
        let sidebar: Vec<TextLine> = (0..12)
            .map(|i| {
                let y = 693.0 - i as f32 * 45.0; // sparse annotations: ~3x body gap
                let item = make_item(1, 440.0, y, "SomeText__");
                TextLine {
                    y,
                    page: 1,
                    adaptive_threshold: 0.10,
                    items: vec![item],
                }
            })
            .collect();
        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 400.0,
            },
            ColumnRegion {
                x_min: 420.0,
                x_max: 590.0,
            },
        ];
        assert!(
            is_newspaper_layout(&[body, sidebar], &cols),
            "Wide body + narrow sidebar should be detected as newspaper"
        );
    }

    #[test]
    fn borderless_table_not_misclassified() {
        // Two columns of similar width and equal line counts → borderless table, not newspaper.
        // width_ratio = 250/300 = 0.83 (> 0.50), so sidebar guard fails → false.
        let col1 = make_lines(10, 50.0);
        let col2 = make_lines(10, 350.0);
        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 550.0,
            },
        ];
        assert!(
            !is_newspaper_layout(&[col1, col2], &cols),
            "Equal-width equal-row columns should NOT be newspaper (borderless table)"
        );
    }
}
