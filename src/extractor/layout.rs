//! Column detection, line grouping, and reading-order layout.

use std::collections::{HashMap, HashSet};

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
pub(crate) fn detect_columns(
    items: &[TextItem],
    page: u32,
    page_has_table: bool,
) -> Vec<ColumnRegion> {
    const BIN_WIDTH: f32 = 2.0;
    const MIN_GUTTER_WIDTH: f32 = 8.0;
    const MIN_VERTICAL_SPAN_RATIO: f32 = 0.30;
    const MIN_ITEMS_PER_COLUMN: usize = 10;
    const NOISE_FRACTION: f32 = 0.15;

    // Get items for this page. Strip Image placeholders — an image's left edge
    // would otherwise count toward the column projection profile.
    let page_items: Vec<&TextItem> = items
        .iter()
        .filter(|i| i.page == page && crate::extractor::is_text_layout_item(i))
        .collect();

    if page_items.is_empty() {
        return vec![];
    }
    debug!("page {}: detect_columns: {} items", page, page_items.len());

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

    // Fallback: if no absolute valleys found, try relative valley detection.
    // Justified text can leave gutter bins non-empty because item widths extend
    // to the column edge. Look for local minima that are significantly lower
    // than the peaks on either side.
    // Only attempt this for dense pages (>=100 items) — sparse pages with shallow
    // histogram dips are likely not multi-column.
    // Skip on pages with detected tables — table column gaps look like gutters
    // in the histogram but the table pipeline already handles reading order.
    if valleys.is_empty() && page_items.len() >= 100 && !page_has_table {
        let rel_valleys = find_relative_valleys(
            &histogram,
            num_bins,
            x_min,
            BIN_WIDTH,
            page_width,
            margin_threshold,
        );
        if !rel_valleys.is_empty() {
            let result = validate_and_build_columns(
                &rel_valleys,
                &page_items,
                x_min,
                BIN_WIDTH,
                x_max,
                MIN_ITEMS_PER_COLUMN,
                MIN_VERTICAL_SPAN_RATIO,
                page,
                true, // center-based assignment for relative valleys
            );
            if result.len() > 1 {
                // Validate that both sides contain paragraph-like content.
                // Tables, forms, and checklists have short scattered items
                // that create false gutter signals. Only commit to relative
                // valley columns when both sides look like flowing prose.
                if columns_have_prose(&result, &page_items) {
                    debug!(
                        "page {}: relative valley detection found {} columns",
                        page,
                        result.len()
                    );
                    return result;
                } else {
                    debug!(
                        "page {}: relative valley rejected — columns lack prose density",
                        page,
                    );
                }
            }
        }
        // Try XY-cut fallback before giving up
        if let Some(columns) = try_xy_cut_split(&page_items, x_min, x_max, page) {
            return columns;
        }
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Try center-based assignment first (handles asymmetric layouts / sidebars
    // better than edge-based). Fall back to edge-based if center produces
    // a degenerate split (one side empty).
    let result = validate_and_build_columns(
        &valleys,
        &page_items,
        x_min,
        BIN_WIDTH,
        x_max,
        MIN_ITEMS_PER_COLUMN,
        MIN_VERTICAL_SPAN_RATIO,
        page,
        true, // center-based assignment
    );
    if result.len() > 1 {
        return result;
    }
    let result = validate_and_build_columns(
        &valleys,
        &page_items,
        x_min,
        BIN_WIDTH,
        x_max,
        MIN_ITEMS_PER_COLUMN,
        MIN_VERTICAL_SPAN_RATIO,
        page,
        false, // edge-based fallback
    );
    if result.len() > 1 {
        return result;
    }

    // Fallback: XY-cut style gap detection.  When the histogram finds no
    // clear valleys (common with asymmetric/sidebar layouts), look for the
    // largest horizontal gap between item edges.  This is a simplified
    // single-level XY-cut inspired by opendataloader's XY-Cut++ algorithm.
    if page_items.len() >= 20 && !page_has_table {
        if let Some(columns) = try_xy_cut_split(&page_items, x_min, x_max, page) {
            return columns;
        }
    }

    vec![ColumnRegion { x_min, x_max }]
}

/// Simplified single-level XY-cut: find the largest horizontal gap between
/// item right-edges and left-edges.  If the gap is wide enough and both sides
/// have sufficient items with vertical overlap, split into two columns.
///
/// Inspired by opendataloader's XY-Cut++ algorithm but without full recursion.
/// Handles asymmetric layouts (sidebars) that the histogram misses because
/// the narrow column has too few items to register in the occupancy profile.
fn try_xy_cut_split(
    page_items: &[&TextItem],
    page_x_min: f32,
    page_x_max: f32,
    page: u32,
) -> Option<Vec<ColumnRegion>> {
    const MIN_GAP: f32 = 15.0; // minimum gap to consider a split
    const MIN_ITEMS_MAJOR: usize = 10; // major column must have ≥10 items
    const MIN_ITEMS_MINOR: usize = 3; // minor column (sidebar) must have ≥3

    let page_width = page_x_max - page_x_min;
    if page_width < 200.0 {
        return None;
    }

    // Collect all item edges: (right_edge, left_edge) pairs sorted by right_edge
    // The gap between one item's right edge and the next item's left edge
    // reveals column gutters.
    let mut edges: Vec<(f32, f32)> = page_items
        .iter()
        .map(|i| (i.x, i.x + effective_width(i)))
        .collect();
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Find the largest gap between consecutive items (by left edge).
    // Use a sweep: sort left edges, find max gap between sorted right edges
    // of items to the left and left edges of items to the right.
    let mut left_edges: Vec<f32> = page_items.iter().map(|i| i.x).collect();
    left_edges.sort_by(|a, b| a.total_cmp(b));

    // Build prefix max of right edges (for items sorted by left edge)
    let mut sorted_by_left: Vec<(f32, f32)> = page_items
        .iter()
        .map(|i| (i.x, i.x + effective_width(i)))
        .collect();
    sorted_by_left.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut best_gap = 0.0f32;
    let mut best_split = 0.0f32;
    let mut max_right_so_far = f32::NEG_INFINITY;

    for i in 0..sorted_by_left.len() - 1 {
        let (_, right) = sorted_by_left[i];
        max_right_so_far = max_right_so_far.max(right);

        let (next_left, _) = sorted_by_left[i + 1];
        let gap = next_left - max_right_so_far;
        if gap > best_gap {
            best_gap = gap;
            best_split = (max_right_so_far + next_left) / 2.0;
        }
    }

    if best_gap < MIN_GAP {
        return None;
    }

    // Don't split at page margins (within 10% of edges)
    let margin = page_width * 0.10;
    if best_split - page_x_min < margin || page_x_max - best_split < margin {
        return None;
    }

    // Count items on each side
    let left_count = page_items
        .iter()
        .filter(|i| i.x + effective_width(i) / 2.0 <= best_split)
        .count();
    let right_count = page_items
        .iter()
        .filter(|i| i.x + effective_width(i) / 2.0 > best_split)
        .count();

    let (minor, major) = if left_count <= right_count {
        (left_count, right_count)
    } else {
        (right_count, left_count)
    };

    if major < MIN_ITEMS_MAJOR || minor < MIN_ITEMS_MINOR {
        return None;
    }

    // Check vertical overlap — both sides should span a meaningful Y range
    let left_items: Vec<&&TextItem> = page_items
        .iter()
        .filter(|i| i.x + effective_width(i) / 2.0 <= best_split)
        .collect();
    let right_items: Vec<&&TextItem> = page_items
        .iter()
        .filter(|i| i.x + effective_width(i) / 2.0 > best_split)
        .collect();

    let l_y_min = left_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let l_y_max = left_items
        .iter()
        .map(|i| i.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let r_y_min = right_items
        .iter()
        .map(|i| i.y)
        .fold(f32::INFINITY, f32::min);
    let r_y_max = right_items
        .iter()
        .map(|i| i.y)
        .fold(f32::NEG_INFINITY, f32::max);

    let overlap_min = l_y_min.max(r_y_min);
    let overlap_max = l_y_max.min(r_y_max);
    let overlap = (overlap_max - overlap_min).max(0.0);
    let y_range = (l_y_max.max(r_y_max) - l_y_min.min(r_y_min)).max(1.0);

    if overlap / y_range < 0.20 {
        return None;
    }

    debug!(
        "page {}: XY-cut split at x={:.1} (gap={:.1}pt, left={}, right={})",
        page, best_split, best_gap, left_count, right_count
    );

    Some(vec![
        ColumnRegion {
            x_min: page_x_min,
            x_max: best_split,
        },
        ColumnRegion {
            x_min: best_split,
            x_max: page_x_max,
        },
    ])
}

/// Check whether each proposed column contains paragraph-like content.
///
/// Groups items per column into rough lines by Y-proximity, then measures
/// what fraction of those lines span a significant portion of the column
/// width. Two-column prose (justified or ragged-right) produces lines that
/// fill most of the column width. Tables, forms, and checklists produce
/// short scattered items that don't.
///
/// Returns true only when *every* column passes a minimum prose density.
fn columns_have_prose(columns: &[ColumnRegion], items: &[&TextItem]) -> bool {
    const Y_TOL: f32 = 3.0; // y-proximity to group items into the same line
    const LINE_FILL_THRESHOLD: f32 = 0.45; // line must span ≥45% of column width
    const MIN_PROSE_RATIO: f32 = 0.40; // ≥40% of lines must be "full"
    const MIN_LINES: usize = 8; // need enough lines to judge
    const MIN_COL_WIDTH: f32 = 120.0; // columns must be ≥120pt (not narrow sidebars/fragments)
    const MAX_AVG_ITEMS_PER_LINE: f32 = 3.5; // prose has 1-3 items/line; tables/forms have 4+

    for col in columns {
        let col_width = col.x_max - col.x_min;
        if col_width < MIN_COL_WIDTH {
            return false;
        }

        // Collect items whose center falls within this column
        let col_items: Vec<&TextItem> = items
            .iter()
            .filter(|i| {
                let center = i.x + effective_width(i) / 2.0;
                center >= col.x_min && center <= col.x_max
            })
            .copied()
            .collect();

        if col_items.len() < MIN_LINES {
            return false;
        }

        // Sort by Y descending (top of page = higher Y in PDF coords)
        let mut sorted: Vec<&TextItem> = col_items;
        sorted.sort_by(|a, b| b.y.total_cmp(&a.y));

        // Group into lines by Y-proximity and measure fill + item count
        let mut full_lines = 0usize;
        let mut total_lines = 0usize;
        let mut total_items_in_lines = 0usize;
        let mut line_items: Vec<&TextItem> = Vec::new();
        let mut line_y = f32::NAN;

        let flush_line = |line_items: &[&TextItem],
                          full: &mut usize,
                          total: &mut usize,
                          total_items: &mut usize| {
            if line_items.is_empty() {
                return;
            }
            *total += 1;
            *total_items += line_items.len();
            // Compute the span of text on this line within the column
            let left = line_items
                .iter()
                .map(|i| i.x.max(col.x_min))
                .fold(f32::INFINITY, f32::min);
            let right = line_items
                .iter()
                .map(|i| (i.x + effective_width(i)).min(col.x_max))
                .fold(f32::NEG_INFINITY, f32::max);
            let span = (right - left).max(0.0);
            if span >= col_width * LINE_FILL_THRESHOLD {
                *full += 1;
            }
        };

        for item in &sorted {
            if line_items.is_empty() || (line_y - item.y).abs() < Y_TOL {
                if line_items.is_empty() {
                    line_y = item.y;
                }
                line_items.push(item);
            } else {
                flush_line(
                    &line_items,
                    &mut full_lines,
                    &mut total_lines,
                    &mut total_items_in_lines,
                );
                line_items.clear();
                line_y = item.y;
                line_items.push(item);
            }
        }
        flush_line(
            &line_items,
            &mut full_lines,
            &mut total_lines,
            &mut total_items_in_lines,
        );

        if total_lines < MIN_LINES {
            return false;
        }

        let ratio = full_lines as f32 / total_lines as f32;
        let avg_items = total_items_in_lines as f32 / total_lines as f32;
        debug!(
            "columns_have_prose: col [{:.0}..{:.0}] lines={} full={} ratio={:.2} avg_items={:.1}",
            col.x_min, col.x_max, total_lines, full_lines, ratio, avg_items
        );
        if ratio < MIN_PROSE_RATIO {
            return false;
        }
        // Tables and forms tend to have many small items per line (one per cell),
        // while prose has few items per line (one per word-run or phrase).
        if avg_items > MAX_AVG_ITEMS_PER_LINE {
            return false;
        }
    }

    true
}

/// Find relative valleys (local minima) in the histogram.
///
/// When justified text fills gutters, the absolute noise threshold fails.
/// This finds local minima where the count drops significantly below
/// the peaks on either side — indicating a gutter even when not empty.
fn find_relative_valleys(
    histogram: &[u32],
    num_bins: usize,
    _x_min: f32,
    bin_width: f32,
    page_width: f32,
    margin_threshold: f32,
) -> Vec<(usize, usize)> {
    const MIN_GUTTER_BINS: usize = 2; // minimum 4pt gutter
    const CONTRAST_THRESHOLD: f32 = 0.60; // valley must be < 60% of surrounding peaks
    const PEAK_WINDOW: usize = 25; // look 50pt on each side for peaks
    const MIN_PEAK_HEIGHT: f32 = 20.0; // peaks must be ≥20 (dense text columns)

    if num_bins < 10 {
        return vec![];
    }

    // Smooth histogram with a 5-bin moving average to reduce noise
    let mut smoothed = vec![0.0f32; num_bins];
    let half_win = 2usize;
    for (i, s) in smoothed.iter_mut().enumerate().take(num_bins) {
        let lo = i.saturating_sub(half_win);
        let hi = (i + half_win + 1).min(num_bins);
        let sum: u32 = histogram[lo..hi].iter().sum();
        *s = sum as f32 / (hi - lo) as f32;
    }

    // Find local minima: positions where smoothed value is lower than
    // both sides within a search window
    let mut candidates: Vec<(usize, f32, f32)> = Vec::new(); // (bin, valley_val, contrast)

    for i in PEAK_WINDOW..num_bins.saturating_sub(PEAK_WINDOW) {
        let val = smoothed[i];
        if val < 1.0 {
            continue; // skip empty margins
        }

        // Check this is a local minimum within a small window
        let local_lo = i.saturating_sub(3);
        let local_hi = (i + 4).min(num_bins);
        let is_local_min = (local_lo..local_hi).all(|j| smoothed[j] >= val - 0.5);
        if !is_local_min {
            continue;
        }

        // Find peak values on each side
        let left_peak = smoothed[i.saturating_sub(PEAK_WINDOW)..i]
            .iter()
            .cloned()
            .fold(0.0f32, f32::max);
        let right_peak = smoothed[(i + 1)..(i + 1 + PEAK_WINDOW).min(num_bins)]
            .iter()
            .cloned()
            .fold(0.0f32, f32::max);

        if left_peak < MIN_PEAK_HEIGHT || right_peak < MIN_PEAK_HEIGHT {
            continue;
        }

        // Both peaks must be substantial — prevents detecting margin drop-offs
        // as gutters in single-column layouts with ragged text.
        let peak_balance = left_peak.min(right_peak) / left_peak.max(right_peak);
        if peak_balance < 0.40 {
            continue;
        }

        // Contrast: ratio of valley to the smaller of the two peaks
        let ref_peak = left_peak.min(right_peak);
        let contrast = val / ref_peak;

        if contrast < CONTRAST_THRESHOLD {
            // Check margin constraint
            let center_pts = i as f32 * bin_width;
            if center_pts > margin_threshold && center_pts < (page_width - margin_threshold) {
                candidates.push((i, val, contrast));
            }
        }
    }

    if candidates.is_empty() {
        return vec![];
    }

    // Group adjacent candidates into valley ranges and pick the deepest point
    let mut valleys: Vec<(usize, usize)> = Vec::new();
    let mut best_bin = candidates[0].0;
    let mut best_contrast = candidates[0].2;

    for window in candidates.windows(2) {
        let (prev_bin, _, _) = window[0];
        let (next_bin, _, next_contrast) = window[1];

        if next_bin - prev_bin <= 5 {
            // Same group
            if next_contrast < best_contrast {
                best_bin = next_bin;
                best_contrast = next_contrast;
            }
        } else {
            // End current group
            let half = MIN_GUTTER_BINS;
            valleys.push((
                best_bin.saturating_sub(half),
                (best_bin + half + 1).min(num_bins),
            ));
            best_bin = next_bin;
            best_contrast = next_contrast;
        }
    }
    // Close last group
    let half = MIN_GUTTER_BINS;
    valleys.push((
        best_bin.saturating_sub(half),
        (best_bin + half + 1).min(num_bins),
    ));

    // Limit to the single best valley (deepest contrast).
    // Multi-column layouts with 3+ columns typically have clear gutters that
    // the absolute valley detection handles. The relative fallback is designed
    // for 2-column layouts where justified text fills the gutter.
    if valleys.len() > 1 {
        // Keep only the valley with the best (lowest) contrast in the candidates
        let mut best_idx = 0;
        let mut best_c = f32::MAX;
        for (vi, v) in valleys.iter().enumerate() {
            let mid = (v.0 + v.1) / 2;
            // Find the candidate closest to this valley's midpoint
            if let Some(c) = candidates
                .iter()
                .filter(|(b, _, _)| (*b as isize - mid as isize).unsigned_abs() <= 5)
                .map(|(_, _, c)| *c)
                .reduce(f32::min)
            {
                if c < best_c {
                    best_c = c;
                    best_idx = vi;
                }
            }
        }
        return vec![valleys[best_idx]];
    }

    valleys
}

/// Detect whether a side of a gutter consists predominantly of list-marker
/// glyphs (•, ●, ○, ◦, ▪, ▫, ◆, ◇). A column of bullets on the left margin
/// creates a spurious histogram valley between the bullet and the content.
/// Treating it as a real column splits each list item's text across two
/// "columns," so we reject these candidates.
fn is_list_marker_column(items: &[&&TextItem]) -> bool {
    const LIST_MARKERS: &[char] = &['•', '●', '○', '◦', '▪', '▫', '◆', '◇', '■', '□'];
    if items.is_empty() {
        return false;
    }
    let marker_count = items
        .iter()
        .filter(|i| {
            let t = i.text.trim();
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => LIST_MARKERS.contains(&c),
                _ => false,
            }
        })
        .count();
    // Require ≥80% of items on this side to be standalone markers. A handful
    // of non-marker items (stray page numbers, footnote refs) shouldn't
    // defeat the check.
    marker_count as f32 / items.len() as f32 >= 0.8
}

/// Validate valley candidates with vertical consistency checks and build column regions.
///
/// When `center_assign` is true, items are assigned to columns based on their
/// center point rather than their right edge. This helps when justified text
/// items extend past the gutter.
#[allow(clippy::too_many_arguments)]
fn validate_and_build_columns(
    valleys: &[(usize, usize)],
    page_items: &[&TextItem],
    x_min: f32,
    bin_width: f32,
    x_max: f32,
    min_items: usize,
    min_vertical_span: f32,
    page: u32,
    center_assign: bool,
) -> Vec<ColumnRegion> {
    // Compute the Y range from column-eligible items only — the same items
    // the histogram counted. Spanning items (full-width captions, titles)
    // are excluded from the projection, so letting them stretch the page's
    // vertical extent here would sink the overlap ratio for column regions
    // that legitimately occupy only part of the page (e.g. two-column text
    // below a figure).
    let x_span = page_items
        .iter()
        .map(|i| i.x + effective_width(i))
        .fold(f32::NEG_INFINITY, f32::max)
        - page_items.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let narrow: Vec<&&TextItem> = page_items
        .iter()
        .filter(|i| effective_width(i) <= x_span * 0.6)
        .collect();
    let span_items: &[&&TextItem] = if narrow.is_empty() { &[] } else { &narrow };
    let y_min = span_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let y_max = span_items
        .iter()
        .map(|i| i.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_range = y_max - y_min;

    // Validate each valley with vertical consistency
    let mut valid_valleys: Vec<(usize, usize, usize, usize)> = Vec::new();
    for &(start, end) in valleys {
        let gutter_left = x_min + start as f32 * bin_width;
        let gutter_right = x_min + end as f32 * bin_width;
        let gutter_center = (gutter_left + gutter_right) / 2.0;

        // Collect items on each side of the gutter.
        // Center-based: use item midpoint (better for justified text).
        // Edge-based: use item right edge (original behavior).
        let left_items: Vec<&&TextItem> = page_items
            .iter()
            .filter(|i| {
                if center_assign {
                    i.x + effective_width(i) / 2.0 <= gutter_center
                } else {
                    i.x + effective_width(i) <= gutter_center
                }
            })
            .collect();
        let right_items: Vec<&&TextItem> = page_items
            .iter()
            .filter(|i| {
                if center_assign {
                    i.x + effective_width(i) / 2.0 > gutter_center
                } else {
                    i.x >= gutter_center
                }
            })
            .collect();

        // Require both sides to have items. Symmetric layout needs min_items
        // on each side. Asymmetric layouts (sidebars) are accepted when the
        // dominant side has ≥ min_items and the smaller side has ≥ 3 items.
        let (smaller, larger) = if left_items.len() <= right_items.len() {
            (left_items.len(), right_items.len())
        } else {
            (right_items.len(), left_items.len())
        };
        if larger < min_items || smaller < 3 {
            debug!(
                "  valley rejected: counts smaller={} larger={}",
                smaller, larger
            );
            continue;
        }

        // Reject valleys where the smaller side is just a column of list
        // markers (bullets aligned at the left margin). This is a common
        // pattern in PDFs where ● starts each list item: histogram detection
        // sees the gap between bullet and content as a gutter.
        let smaller_items: &[&&TextItem] = if left_items.len() <= right_items.len() {
            &left_items
        } else {
            &right_items
        };
        if is_list_marker_column(smaller_items) {
            debug!("  valley rejected: list-marker column");
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

            if overlap / y_range < min_vertical_span {
                debug!(
                    "  valley rejected: overlap {:.0}/{:.0} = {:.2} < {:.2}",
                    overlap,
                    y_range,
                    overlap / y_range,
                    min_vertical_span
                );
                continue;
            }
        }

        valid_valleys.push((start, end, left_items.len(), right_items.len()));
    }

    if valid_valleys.is_empty() {
        debug!(
            "page {}: {} valleys found but none passed validation",
            page,
            valleys.len()
        );
        return vec![ColumnRegion { x_min, x_max }];
    }

    debug!(
        "page {}: {} columns detected (boundaries: {:?})",
        page,
        valid_valleys.len() + 1,
        valid_valleys
            .iter()
            .map(|(s, e, _, _)| x_min + ((*s + *e) as f32 / 2.0) * bin_width)
            .collect::<Vec<_>>()
    );

    // Limit to at most 3 gutters (4 columns).
    // Score = width_in_bins * min(left_count, right_count)
    if valid_valleys.len() > 3 {
        valid_valleys.sort_by(|a, b| {
            let score_a = (a.1 - a.0) as f32 * (a.2.min(a.3) as f32);
            let score_b = (b.1 - b.0) as f32 * (b.2.min(b.3) as f32);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        valid_valleys.truncate(3);
        valid_valleys.sort_by_key(|v| v.0);
    }

    // Build column regions from gutter boundaries
    let mut columns = Vec::new();
    let mut col_start = x_min;
    for &(start, end, _, _) in &valid_valleys {
        let gutter_center = x_min + ((start + end) as f32 / 2.0) * bin_width;
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

/// Identify items that belong to lines spanning across detected columns.
///
/// Groups items into rough lines by Y-proximity and marks items whose line's
/// combined X-span exceeds 1.3× the widest column AND has no gap located at
/// a detected gutter boundary. Returns a boolean mask parallel to `items`.
fn identify_spanning_lines(items: &[TextItem], columns: &[ColumnRegion]) -> Vec<bool> {
    let n = items.len();
    let mut mask = vec![false; n];

    if n < 3 || columns.len() < 2 {
        return mask;
    }

    let max_col_width = columns
        .iter()
        .map(|c| c.x_max - c.x_min)
        .fold(0.0_f32, f32::max);
    let span_threshold = max_col_width * 1.3;

    // Gutter centers: boundaries between adjacent columns
    let gutters: Vec<f32> = columns.windows(2).map(|c| c[0].x_max).collect();
    let gutter_tol = 15.0;
    let y_tol = 5.0;

    // Build (original_index, y) pairs sorted by Y descending for grouping
    let mut indexed: Vec<(usize, f32)> =
        items.iter().enumerate().map(|(i, it)| (i, it.y)).collect();
    indexed.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Group by Y-proximity into rough lines (as index sets)
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();
    let mut current_y = f32::NAN;

    for (idx, y) in indexed {
        if current_group.is_empty() || (current_y - y).abs() < y_tol {
            if current_group.is_empty() {
                current_y = y;
            }
            current_group.push(idx);
        } else {
            groups.push(std::mem::take(&mut current_group));
            current_y = y;
            current_group.push(idx);
        }
    }
    if !current_group.is_empty() {
        groups.push(current_group);
    }

    for group in groups {
        if group.len() < 2 {
            continue;
        }

        // Sort group indices by X to compute span
        let mut sorted_by_x: Vec<usize> = group;
        sorted_by_x.sort_by(|&a, &b| {
            items[a]
                .x
                .partial_cmp(&items[b].x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let line_left = items[sorted_by_x[0]].x;
        let last = *sorted_by_x.last().unwrap();
        let line_right = items[last].x + effective_width(&items[last]);
        let span = line_right - line_left;

        if span <= span_threshold {
            continue;
        }

        // Check if any inter-item gap falls at a detected gutter boundary.
        // If so, this is items from different columns at the same Y, not a
        // true spanning line (like a title or section header).
        let has_gutter_gap = sorted_by_x.windows(2).any(|pair| {
            let left_end = items[pair[0]].x + effective_width(&items[pair[0]]);
            let right_start = items[pair[1]].x;
            let gap = right_start - left_end;
            if gap < 5.0 {
                return false;
            }
            // Check if any gutter falls within the gap interval (with tolerance)
            gutters
                .iter()
                .any(|&g| g > left_end - gutter_tol && g < right_start + gutter_tol)
        });

        if !has_gutter_gap {
            for &idx in &sorted_by_x {
                mask[idx] = true;
            }
        }
    }

    mask
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

const PAGE_NUMBER_Y_TOLERANCE: f32 = 3.0;
const PAGE_NUMBER_CONTEXT_GAP_EM: f32 = 1.5;
const PAGE_NUMBER_BOTTOM_Y: f32 = 100.0;
const PAGE_NUMBER_TOP_Y: f32 = 720.0;
const SPREAD_MIN_CONTENT_WIDTH_EM: f32 = 40.0;
const SPREAD_EDGE_FRACTION: f32 = 0.25;
const ADJACENT_PAGE_MIN_CONTENT_WIDTH_EM: f32 = 26.0;

type ContextualCandidateOccurrence = (u32, f32, Vec<(usize, u32)>);

fn page_number_value(item: &TextItem) -> Option<u32> {
    if !matches!(
        item.item_type,
        crate::types::ItemType::Text | crate::types::ItemType::FormField
    ) {
        return None;
    }

    let text = item.text.trim();

    if text.is_empty() || text.len() > 4 || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Must be at top or bottom of page.
    // US Letter = 792pt, A4 = 841pt. Page numbers are typically in the
    // top ~5% or bottom ~12% of the page.
    if item.y <= PAGE_NUMBER_TOP_Y && item.y >= PAGE_NUMBER_BOTTOM_Y {
        return None;
    }

    text.parse().ok()
}

/// Mark numeric slots that advance inside a repeated deep-margin line.
///
/// A folio can be emitted as part of a footer text run (for example,
/// `42 Company report`) and therefore look contextual on a single page. Across
/// the document, however, the surrounding text and Y position repeat while the
/// numeric slot advances. Require that full signal before treating the slot as
/// a folio so constant metadata and substantive rows near the page edge remain
/// untouched.
fn mark_repeated_folio_candidates(
    occurrences_by_signature: HashMap<String, Vec<ContextualCandidateOccurrence>>,
    document_page_count: usize,
    explicit_folio: &mut [bool],
) {
    // Both thresholds are evidence floors: short documents still need four
    // occurrences, while long documents also need meaningful coverage. Using
    // `min` here would make one occurrence sufficient in a one-page document.
    let min_pages = 4usize.max((document_page_count * 30).div_ceil(100));

    for occurrences in occurrences_by_signature.into_values() {
        if occurrences.len() < min_pages {
            continue;
        }

        let distinct_pages: HashSet<u32> = occurrences.iter().map(|(page, _, _)| *page).collect();
        // Repeated table rows or duplicated drawing labels can share a
        // signature multiple times on one page. They are not running folios.
        if distinct_pages.len() != occurrences.len() || distinct_pages.len() < min_pages {
            continue;
        }

        let min_y = occurrences
            .iter()
            .map(|(_, y, _)| *y)
            .fold(f32::INFINITY, f32::min);
        let max_y = occurrences
            .iter()
            .map(|(_, y, _)| *y)
            .fold(f32::NEG_INFINITY, f32::max);
        if max_y - min_y >= PAGE_NUMBER_Y_TOLERANCE {
            continue;
        }

        let slot_count = occurrences[0].2.len();
        if slot_count == 0
            || occurrences
                .iter()
                .any(|(_, _, candidates)| candidates.len() != slot_count)
        {
            continue;
        }

        for slot in 0..slot_count {
            let mut values: Vec<(u32, u32, usize)> = occurrences
                .iter()
                .map(|(page, _, candidates)| {
                    let (index, value) = candidates[slot];
                    (*page, value, index)
                })
                .collect();
            values.sort_by_key(|(page, _, _)| *page);

            let unique_values: HashSet<u32> = values.iter().map(|(_, value, _)| *value).collect();
            let mostly_unique = unique_values.len() * 5 >= values.len() * 4;
            let page_tracking_pairs = values
                .windows(2)
                .filter(|pair| {
                    let page_delta = pair[1].0 - pair[0].0;
                    let value_delta = pair[1].1.saturating_sub(pair[0].1);
                    value_delta == page_delta || value_delta == page_delta.saturating_mul(2)
                })
                .count();
            let mostly_tracks_page_order = page_tracking_pairs * 5 >= (values.len() - 1) * 4;
            // A running folio can be offset by front matter or advance twice per
            // PDF page in a two-page spread, but its magnitude should still be
            // plausible for the document. This keeps changing metadata such as
            // a sequence of years from becoming a deletion signal.
            let max_plausible_folio = (document_page_count as u32).saturating_mul(4).max(100);
            let plausible_magnitude = values
                .iter()
                .all(|(_, value, _)| *value <= max_plausible_folio);

            if mostly_unique && mostly_tracks_page_order && plausible_magnitude {
                for (_, _, index) in values {
                    explicit_folio[index] = true;
                }
            }
        }
    }
}

/// Mark the contextual half of a facing-page folio pair.
///
/// A landscape PDF can contain two printed pages per PDF page. One folio may be
/// isolated while the other touches footer text; they remain a pair because
/// they are consecutive, share a deep-margin baseline, and sit on opposite
/// sides of the spread. The isolated half is strong evidence that the touching
/// half is also a folio.
fn mark_spread_folio_pairs(
    items: &[TextItem],
    candidate_values: &[Option<u32>],
    contextual: &[bool],
    explicit_folio: &mut [bool],
) {
    let mut candidates_by_page: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut page_bounds: HashMap<u32, (f32, f32)> = HashMap::new();
    for (index, value) in candidate_values.iter().enumerate() {
        if value.is_some() {
            candidates_by_page
                .entry(items[index].page)
                .or_default()
                .push(index);
        }
    }
    for item in items.iter().filter(|item| !item.text.trim().is_empty()) {
        let bounds = page_bounds
            .entry(item.page)
            .or_insert((f32::INFINITY, f32::NEG_INFINITY));
        bounds.0 = bounds.0.min(item.x);
        bounds.1 = bounds.1.max(item.x + effective_width(item));
    }

    for (page, page_candidates) in candidates_by_page {
        let Some(&(page_left, page_right)) = page_bounds.get(&page) else {
            continue;
        };
        let page_width = page_right - page_left;
        if page_width <= 0.0 {
            continue;
        }
        let left_edge = page_left + page_width * SPREAD_EDGE_FRACTION;
        let right_edge = page_right - page_width * SPREAD_EDGE_FRACTION;
        let max_pair_font_size = page_width / SPREAD_MIN_CONTENT_WIDTH_EM;
        let edge_side = |index: usize| {
            let center = items[index].x + effective_width(&items[index]) / 2.0;
            if center <= left_edge {
                Some(false)
            } else if center >= right_edge {
                Some(true)
            } else {
                None
            }
        };

        // Index strong folio evidence by value and spread edge. Sorted
        // baselines let each contextual candidate query only the two adjacent
        // values on the opposite edge in O(log n), rather than comparing every
        // candidate pair on numeric-heavy pages.
        let mut known_baselines: HashMap<(u32, bool), Vec<f32>> = HashMap::new();
        for &index in &page_candidates {
            if (contextual[index] && !explicit_folio[index])
                || items[index].font_size >= max_pair_font_size
            {
                continue;
            }
            let Some(side) = edge_side(index) else {
                continue;
            };
            let value = candidate_values[index].unwrap();
            known_baselines
                .entry((value, side))
                .or_default()
                .push(items[index].y);
        }
        for baselines in known_baselines.values_mut() {
            baselines.sort_by(f32::total_cmp);
        }

        for index in page_candidates {
            if !contextual[index]
                || explicit_folio[index]
                || items[index].font_size >= max_pair_font_size
            {
                continue;
            }
            let Some(value) = candidate_values[index] else {
                continue;
            };
            let Some(side) = edge_side(index) else {
                continue;
            };
            let y = items[index].y;
            let paired = [value.checked_sub(1), value.checked_add(1)]
                .into_iter()
                .flatten()
                .filter_map(|other_value| known_baselines.get(&(other_value, !side)))
                .any(|baselines| {
                    let first = baselines
                        .partition_point(|baseline| *baseline <= y - PAGE_NUMBER_Y_TOLERANCE);
                    baselines
                        .get(first)
                        .is_some_and(|baseline| *baseline < y + PAGE_NUMBER_Y_TOLERANCE)
                });
            if paired {
                explicit_folio[index] = true;
            }
        }
    }
}

/// Mark a contextual folio that alternates with an isolated folio on the
/// neighboring PDF page.
///
/// Facing pages commonly put folios on opposite outer edges. A running header
/// can touch the right-hand folio while the next left-hand folio is isolated.
/// A single isolated candidate is not enough to remove nearby contextual text.
/// Require a second pre-existing anchor in the same advancing sequence, along
/// with a genuinely wide content span, stable baselines/font sizes, and
/// alternating outer edges.
fn mark_adjacent_page_folio_pairs(
    items: &[TextItem],
    candidate_values: &[Option<u32>],
    contextual: &[bool],
    document_page_count: usize,
    explicit_folio: &mut [bool],
) {
    let max_plausible_folio = (document_page_count as u32).saturating_mul(4).max(100);
    // Do not let newly inferred candidates recursively become evidence for
    // later candidates; every match must be anchored by evidence established
    // before this cross-page pass.
    let strong_folio_evidence = explicit_folio.to_vec();
    let mut page_bounds: HashMap<u32, (f32, f32)> = HashMap::new();
    for item in items.iter().filter(|item| !item.text.trim().is_empty()) {
        let bounds = page_bounds
            .entry(item.page)
            .or_insert((f32::INFINITY, f32::NEG_INFINITY));
        bounds.0 = bounds.0.min(item.x);
        bounds.1 = bounds.1.max(item.x + effective_width(item));
    }

    let edge_side = |index: usize| {
        let &(page_left, page_right) = page_bounds.get(&items[index].page)?;
        let page_width = page_right - page_left;
        if page_width < items[index].font_size * ADJACENT_PAGE_MIN_CONTENT_WIDTH_EM {
            return None;
        }
        let center = items[index].x + effective_width(&items[index]) / 2.0;
        let left_edge = page_left + page_width * SPREAD_EDGE_FRACTION;
        let right_edge = page_right - page_width * SPREAD_EDGE_FRACTION;
        if center <= left_edge {
            Some(false)
        } else if center >= right_edge {
            Some(true)
        } else {
            None
        }
    };

    // Anchor sequences by outer edge and the value/page offset. This enforces
    // forward page tracking and lets candidates query adjacent pages directly,
    // while a baseline-sorted index finds a second independent anchor without
    // a document-wide quadratic scan.
    let mut anchors_by_page: HashMap<(u32, bool, i64), Vec<usize>> = HashMap::new();
    let mut anchors_by_sequence: HashMap<(bool, i64), Vec<usize>> = HashMap::new();
    for (index, value) in candidate_values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        if *value > max_plausible_folio || (contextual[index] && !strong_folio_evidence[index]) {
            continue;
        }
        let Some(side) = edge_side(index) else {
            continue;
        };
        let page = items[index].page;
        let offset = i64::from(*value) - i64::from(page);
        anchors_by_page
            .entry((page, side, offset))
            .or_default()
            .push(index);
        anchors_by_sequence
            .entry((side, offset))
            .or_default()
            .push(index);
    }
    for anchors in anchors_by_sequence.values_mut() {
        anchors.sort_by(|&left, &right| items[left].y.total_cmp(&items[right].y));
    }

    for (index, value) in candidate_values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        if !contextual[index] || explicit_folio[index] || *value > max_plausible_folio {
            continue;
        }
        let Some(side) = edge_side(index) else {
            continue;
        };

        let page = items[index].page;
        let offset = i64::from(*value) - i64::from(page);
        let Some(sequence_anchors) = anchors_by_sequence.get(&(!side, offset)) else {
            continue;
        };
        let neighbor = [page.checked_sub(1), page.checked_add(1)]
            .into_iter()
            .flatten()
            .filter_map(|neighbor_page| anchors_by_page.get(&(neighbor_page, !side, offset)))
            .flatten()
            .copied()
            .find(|&neighbor_index| {
                (items[index].y - items[neighbor_index].y).abs() < PAGE_NUMBER_Y_TOLERANCE
                    && (items[index].font_size - items[neighbor_index].font_size).abs() < 1.0
            });
        let Some(neighbor_index) = neighbor else {
            continue;
        };

        let first = sequence_anchors.partition_point(|&anchor_index| {
            items[anchor_index].y <= items[index].y - PAGE_NUMBER_Y_TOLERANCE
        });
        let has_second_anchor = sequence_anchors[first..]
            .iter()
            .take_while(|&&anchor_index| {
                items[anchor_index].y < items[index].y + PAGE_NUMBER_Y_TOLERANCE
            })
            .any(|&anchor_index| {
                items[anchor_index].page != page
                    && items[anchor_index].page != items[neighbor_index].page
                    && (items[index].font_size - items[anchor_index].font_size).abs() < 1.0
            });
        if has_second_anchor {
            explicit_folio[index] = true;
        }
    }
}

/// Identify page-edge numeric items that belong to a nearby content run.
///
/// Numeric candidates on their own do not establish context for one another.
/// A connected same-baseline run is contextual only when it also contains a
/// non-candidate item, preserving lines such as `Chapter 1 2026` while still
/// removing isolated numeric footer clusters.
fn page_number_context_masks(
    items: &[TextItem],
    candidate_values: &[Option<u32>],
    document_page_count: usize,
) -> (Vec<bool>, Vec<bool>) {
    let mut contextual = vec![false; items.len()];
    let mut explicit_folio = vec![false; items.len()];
    let mut occurrences_by_signature: HashMap<String, Vec<ContextualCandidateOccurrence>> =
        HashMap::new();
    let mut indices_by_page: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if matches!(
            item.item_type,
            crate::types::ItemType::Text | crate::types::ItemType::FormField
        ) && !item.text.trim().is_empty()
        {
            indices_by_page.entry(item.page).or_default().push(index);
        }
    }
    for mut page_indices in indices_by_page.into_values() {
        page_indices.sort_by(|&left, &right| {
            items[right]
                .y
                .total_cmp(&items[left].y)
                .then(items[left].x.total_cmp(&items[right].x))
        });

        let mut rows: Vec<Vec<usize>> = Vec::new();
        for index in page_indices {
            if rows.last().is_some_and(|row| {
                (items[row[0]].y - items[index].y).abs() < PAGE_NUMBER_Y_TOLERANCE
            }) {
                rows.last_mut().unwrap().push(index);
            } else {
                rows.push(vec![index]);
            }
        }

        for mut row in rows {
            row.sort_by(|&left, &right| items[left].x.total_cmp(&items[right].x));
            let mut start = 0;
            while start < row.len() {
                let mut end = start + 1;
                let first = &items[row[start]];
                let mut group_right = first.x + effective_width(first);
                let mut group_font_size = first.font_size;

                while end < row.len() {
                    let item = &items[row[end]];
                    let gap = item.x - group_right;
                    if gap > group_font_size.max(item.font_size) * PAGE_NUMBER_CONTEXT_GAP_EM {
                        break;
                    }
                    group_right = group_right.max(item.x + effective_width(item));
                    group_font_size = group_font_size.max(item.font_size);
                    end += 1;
                }

                let group = &row[start..end];
                let has_lexical_context = group.iter().any(|&index| {
                    candidate_values[index].is_none()
                        && items[index]
                            .text
                            .chars()
                            .any(|character| character.is_alphabetic())
                });
                // Numeric data near a page edge also needs protection, but a
                // lone long integer beside a short candidate is not enough to
                // establish context. Preserve explicit numeric structures
                // (list markers, ranges, comma-formatted values, dotted index
                // entries) and dense runs with at least one long integer.
                let numeric_like = |text: &str| {
                    text.chars().any(|character| character.is_numeric())
                        && !text.chars().any(|character| character.is_alphabetic())
                };
                let is_structured_numeric_context = |index: usize| {
                    if candidate_values[index].is_some() {
                        return false;
                    }
                    let text = items[index].text.trim();
                    numeric_like(text)
                        && text
                            .chars()
                            .any(|character| !character.is_numeric() && !character.is_whitespace())
                };
                let has_structured_numeric_context = group
                    .iter()
                    .any(|&index| is_structured_numeric_context(index));
                let numeric_item_count = group
                    .iter()
                    .filter(|&&index| numeric_like(items[index].text.trim()))
                    .count();
                let has_long_integer = group.iter().any(|&index| {
                    candidate_values[index].is_none()
                        && items[index]
                            .text
                            .trim()
                            .chars()
                            .all(|character| character.is_numeric())
                });
                let has_dense_numeric_context = numeric_item_count >= 3 && has_long_integer;
                let has_context = has_lexical_context
                    || has_structured_numeric_context
                    || has_dense_numeric_context;
                let has_candidate = row[start..end]
                    .iter()
                    .any(|&index| candidate_values[index].is_some());
                // Decorative centered folios have no lexical context, so
                // recognize the complete delimiter-number-delimiter triplet
                // before the contextual-content gate. This prevents `- 42 -`
                // from leaving a malformed `- -` line.
                if group.len() == 3
                    && items[group[0]].text.trim() == "-"
                    && candidate_values[group[1]].is_some()
                    && items[group[2]].text.trim() == "-"
                {
                    for &index in group {
                        explicit_folio[index] = true;
                    }
                }
                if has_context && has_candidate {
                    let group = &row[start..end];
                    let group_text = group
                        .iter()
                        .map(|&index| items[index].text.trim())
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let group_is_folio =
                        crate::text_utils::is_explicit_page_number_expression(&group_text);
                    let context_text = group
                        .iter()
                        .filter(|&&index| candidate_values[index].is_none())
                        .map(|&index| items[index].text.trim())
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let candidates: Vec<(usize, u32)> = group
                        .iter()
                        .filter_map(|&index| candidate_values[index].map(|value| (index, value)))
                        .collect();
                    // Recurrence is only evidence for numeric slots at the
                    // outer boundary of a contextual run. An embedded number
                    // in repeated prose such as `Page 42 explains the result`
                    // is substantive content, not a running folio.
                    let recurrence_candidates: Vec<(usize, u32)> = candidates
                        .iter()
                        .copied()
                        .filter(|(index, _)| {
                            group.first() == Some(index) || group.last() == Some(index)
                        })
                        .collect();
                    let in_deep_margin = candidates.iter().all(|(index, _)| {
                        items[*index].y < PAGE_NUMBER_BOTTOM_Y
                            || items[*index].y > PAGE_NUMBER_TOP_Y
                    });
                    if in_deep_margin
                        && !recurrence_candidates.is_empty()
                        && context_text
                            .chars()
                            .filter(|character| character.is_alphanumeric())
                            .count()
                            >= 8
                    {
                        let signature = group
                            .iter()
                            .map(|&index| {
                                if candidate_values[index].is_some() {
                                    "{number}".to_string()
                                } else {
                                    items[index]
                                        .text
                                        .split_whitespace()
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                        .to_lowercase()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        occurrences_by_signature
                            .entry(signature)
                            .or_default()
                            .push((
                                items[group[0]].page,
                                items[group[0]].y,
                                recurrence_candidates,
                            ));
                    }
                    for (position, &index) in group.iter().enumerate() {
                        if let Some(value) = candidate_values[index] {
                            let adjacent_context = [position.checked_sub(1), Some(position + 1)]
                                .into_iter()
                                .flatten()
                                .filter_map(|position| group.get(position).copied())
                                .any(|adjacent| {
                                    candidate_values[adjacent].is_none()
                                        && items[adjacent].text.chars().any(|character| {
                                            !character.is_numeric() && !character.is_whitespace()
                                        })
                                });
                            let max_plausible_folio =
                                (document_page_count as u32).saturating_mul(4).max(100);
                            // Large year/identifier-like values stay attached
                            // to their lexical run even when a smaller numeric
                            // candidate sits between them and the text.
                            let implausible_folio_with_lexical_context =
                                value > max_plausible_folio && has_lexical_context;
                            contextual[index] = adjacent_context
                                || has_dense_numeric_context
                                || implausible_folio_with_lexical_context;
                            let previous = position
                                .checked_sub(1)
                                .map(|position| items[group[position]].text.trim());
                            let next = group
                                .get(position + 1)
                                .map(|&index| items[index].text.trim());
                            let follows_page_label =
                                previous.is_some_and(|text| text.eq_ignore_ascii_case("page"));
                            let starts_of_expression =
                                next.is_some_and(|text| text.eq_ignore_ascii_case("of"));
                            let is_centered_folio = previous == Some("-") && next == Some("-");
                            explicit_folio[index] |= group_is_folio
                                && (follows_page_label
                                    || starts_of_expression
                                    || is_centered_folio);
                        }
                    }
                    // Remove the complete labeled expression rather than
                    // leaving fragments such as `Page of 15`. A trailing
                    // running-header suffix remains untouched.
                    if group_is_folio
                        && group.len() >= 4
                        && items[group[0]].text.trim().eq_ignore_ascii_case("page")
                        && candidate_values[group[1]].is_some()
                        && items[group[2]].text.trim().eq_ignore_ascii_case("of")
                        && items[group[3]]
                            .text
                            .trim()
                            .chars()
                            .all(|character| character.is_ascii_digit())
                    {
                        for &index in &group[..4] {
                            explicit_folio[index] = true;
                        }
                    }
                }
                start = end;
            }
        }
    }

    mark_repeated_folio_candidates(
        occurrences_by_signature,
        document_page_count,
        &mut explicit_folio,
    );
    mark_spread_folio_pairs(items, candidate_values, &contextual, &mut explicit_folio);
    mark_adjacent_page_folio_pairs(
        items,
        candidate_values,
        &contextual,
        document_page_count,
        &mut explicit_folio,
    );

    (contextual, explicit_folio)
}

/// Decide which digit-only page-edge items can be removed before layout.
///
/// PDF producers commonly emit one text-showing operation per word. A numeric
/// item attached to neighboring content on the same baseline is therefore kept.
/// Complete page-number expressions such as `Page 42` remain removable even
/// though their numeric item has lexical context.
fn page_number_removal_mask(items: &[TextItem], document_page_count: usize) -> Vec<bool> {
    let candidate_values: Vec<Option<u32>> = items.iter().map(page_number_value).collect();
    let (contextual, explicit_folio) =
        page_number_context_masks(items, &candidate_values, document_page_count);

    candidate_values
        .iter()
        .enumerate()
        .map(|(index, value)| explicit_folio[index] || (value.is_some() && !contextual[index]))
        .collect()
}

/// Return whether selected-page extraction contains a page-edge number whose
/// folio status depends on evidence from other pages. Isolated and explicitly
/// labeled folios can be decided locally; only contextual candidates require
/// a document-wide extraction pass.
pub(super) fn needs_document_page_number_context(
    items: &[TextItem],
    document_page_count: usize,
) -> bool {
    let candidate_values: Vec<Option<u32>> = items.iter().map(page_number_value).collect();
    let (contextual, explicit_folio) =
        page_number_context_masks(items, &candidate_values, document_page_count);

    candidate_values
        .iter()
        .enumerate()
        .any(|(index, value)| value.is_some() && contextual[index] && !explicit_folio[index])
}

/// Remove numeric folios using complete document context before downstream
/// non-table layout partitions could separate the evidence needed to recognize
/// them.
#[cfg(test)]
pub(crate) fn filter_markdown_page_numbers(
    items: Vec<TextItem>,
    document_page_count: u32,
) -> Vec<TextItem> {
    filter_markdown_page_numbers_with_removed_pages(items, document_page_count).0
}

/// Filter Markdown folios while retaining the pages where items were removed.
///
/// The page set lets downstream table-continuation classification preserve its
/// pre-filter semantics even though structural layout consumes the cleaned
/// item collection.
pub(crate) fn filter_markdown_page_numbers_with_removed_pages(
    items: Vec<TextItem>,
    document_page_count: u32,
) -> (Vec<TextItem>, HashSet<u32>, Vec<bool>) {
    let remove = page_number_removal_mask(&items, document_page_count as usize);
    let mut removed_pages = HashSet::new();
    let items = items
        .into_iter()
        .zip(remove.iter().copied())
        .filter_map(|(item, remove)| {
            if remove {
                removed_pages.insert(item.page);
                None
            } else {
                Some(item)
            }
        })
        .collect();
    (items, removed_pages, remove)
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
                        ys.sort_by(|a, b| a.total_cmp(b));
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
    sorted_gaps.sort_by(|a, b| a.total_cmp(b));
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
    group_into_lines_with_thresholds(items, &HashMap::new(), &HashSet::new())
}

/// Group text items into lines without removing numeric page headers or footers.
///
/// Plain-text extraction uses this path because every extracted item is part of
/// the API result. Markdown conversion keeps using [`group_into_lines`], where
/// page-number suppression is an intentional presentation cleanup.
pub fn group_into_lines_preserving_all_text(items: Vec<TextItem>) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_regions_impl(
        items,
        &HashMap::new(),
        &HashSet::new(),
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
}

/// Group text items into lines, using pre-computed per-page adaptive thresholds
/// from Canva-style letter-spacing detection. Falls back to computing the
/// threshold from item gaps when no pre-computed value is available.
pub(crate) fn group_into_lines_with_thresholds(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_charts(
        items,
        page_thresholds,
        table_pages,
        &HashMap::new(),
    )
}

/// Like `group_into_lines_with_thresholds`, but items inside chart regions
/// are excluded from column detection: chart text scattered across the page
/// fills the gutter in the projection histogram, so two-column pages read as
/// one column and same-baseline items from both columns fuse into one line
/// (headings absorbed into the neighboring column's body text).
pub(crate) fn group_into_lines_with_thresholds_and_charts(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
    chart_regions: &HashMap<u32, Vec<(f32, f32, f32, f32)>>,
) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_regions(
        items,
        page_thresholds,
        table_pages,
        chart_regions,
        &HashMap::new(),
    )
}

/// Group items after document-level page-number filtering has already run.
///
/// Partitioned Markdown layout uses this path so a contextual candidate that
/// was preserved with its complete baseline context is not reconsidered after
/// its neighboring text lands in another band or chart/prose zone.
pub(crate) fn group_prefiltered_items_into_lines_with_thresholds_and_charts(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
    chart_regions: &HashMap<u32, Vec<(f32, f32, f32, f32)>>,
) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_regions_impl(
        items,
        page_thresholds,
        table_pages,
        chart_regions,
        &HashMap::new(),
        false,
    )
}

pub(crate) fn group_into_lines_with_thresholds_and_regions(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
    chart_regions: &HashMap<u32, Vec<(f32, f32, f32, f32)>>,
    image_regions: &HashMap<u32, Vec<super::reading_order::ImageRegion>>,
) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_regions_impl(
        items,
        page_thresholds,
        table_pages,
        chart_regions,
        image_regions,
        true,
    )
}

pub(crate) fn group_prefiltered_items_into_lines_with_thresholds_and_regions(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
    chart_regions: &HashMap<u32, Vec<(f32, f32, f32, f32)>>,
    image_regions: &HashMap<u32, Vec<super::reading_order::ImageRegion>>,
) -> Vec<TextLine> {
    group_into_lines_with_thresholds_and_regions_impl(
        items,
        page_thresholds,
        table_pages,
        chart_regions,
        image_regions,
        false,
    )
}

fn group_into_lines_with_thresholds_and_regions_impl(
    items: Vec<TextItem>,
    page_thresholds: &HashMap<u32, f32>,
    table_pages: &HashSet<u32>,
    chart_regions: &HashMap<u32, Vec<(f32, f32, f32, f32)>>,
    image_regions: &HashMap<u32, Vec<super::reading_order::ImageRegion>>,
    filter_page_numbers: bool,
) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Markdown output omits standalone numeric headers/footers. Determine
    // standalone status from rough baseline context before layout analysis so
    // removed page numbers cannot affect column detection. Plain-text callers
    // opt out because dropping extracted text violates that API.
    let items = if filter_page_numbers {
        // Item-only grouping has no document metadata, so use the highest
        // observed 1-based page as its best available coverage denominator.
        // The Markdown document path passes the authoritative PDF page count
        // through `filter_markdown_page_numbers` before reaching this helper.
        let observed_page_count = items
            .iter()
            .map(|item| item.page as usize)
            .max()
            .unwrap_or(0);
        let remove = page_number_removal_mask(&items, observed_page_count);
        items
            .into_iter()
            .zip(remove)
            .filter_map(|(item, remove)| (!remove).then_some(item))
            .collect()
    } else {
        items
    };

    // Get unique pages
    let mut pages: Vec<u32> = items.iter().map(|i| i.page).collect();
    pages.sort();
    pages.dedup();

    let mut all_lines = Vec::new();

    for page in pages {
        let page_items: Vec<TextItem> = items.iter().filter(|i| i.page == page).cloned().collect();
        // Page-edge numeric runs are weak evidence for column geometry. Keep
        // contextual values for line assembly, but prevent their preservation
        // from changing the page's inferred layout.
        let column_detection_items: Vec<TextItem> = page_items
            .iter()
            .filter(|item| page_number_value(item).is_none())
            .cloned()
            .collect();
        let column_detection_items = column_detection_items.as_slice();

        // Use pre-computed threshold from fix_letterspaced_items if available
        // (computed before embedded-space removal, with full signal).
        // Non-Canva pages use the default 0.10 threshold.
        let adaptive_threshold = page_thresholds.get(&page).copied().unwrap_or(0.10);

        // Image-backed region graphs recover local/asymmetric column flows
        // that a whole-page projection cannot represent. Charts already have
        // their own positioned-region ordering and therefore stay on that path.
        if !chart_regions.contains_key(&page) {
            let preliminary_columns =
                detect_columns(column_detection_items, page, table_pages.contains(&page));
            let detected_split =
                (preliminary_columns.len() == 2).then(|| preliminary_columns[0].x_max);
            if let Some(band) = image_regions.get(&page).and_then(|regions| {
                super::reading_order::infer_image_anchored_flow(
                    &page_items,
                    regions,
                    detected_split,
                )
            }) {
                debug!(
                    "page {}: image-anchored region graph split={:.1} y=[{:.1}..{:.1}]",
                    page, band.split_x, band.y_bottom, band.y_top
                );
                for node in super::reading_order::build_region_graph(page_items, band) {
                    debug!(
                        "page {}: region node {:?} items={}",
                        page,
                        node.kind,
                        node.items.len()
                    );
                    all_lines.extend(group_single_column(node.items, adaptive_threshold));
                }
                continue;
            }
        }

        // Detect columns for this page, blind to chart text.
        debug!(
            "page {}: grouping chart-aware={} regions={:?}",
            page,
            chart_regions.contains_key(&page),
            chart_regions.get(&page).map(|v| v
                .iter()
                .map(|&(a, b, c, d)| (a as i32, b as i32, c as i32, d as i32))
                .collect::<Vec<_>>())
        );
        let columns = match chart_regions.get(&page).filter(|r| !r.is_empty()) {
            Some(regions) => {
                let col_input: Vec<TextItem> = page_items
                    .iter()
                    .filter(|it| {
                        if page_number_value(it).is_some() {
                            return false;
                        }
                        let cx = it.x + it.width / 2.0;
                        // Tight bounds: this only blinds the histogram to
                        // chart-internal text; rows adjacent to the chart
                        // belong to the column layout.
                        !regions.iter().any(|&(x0, y0, x1, y1)| {
                            cx >= x0 - 2.0 && cx <= x1 + 2.0 && it.y >= y0 - 2.0 && it.y <= y1 + 2.0
                        })
                    })
                    .cloned()
                    .collect();
                detect_columns(&col_input, page, table_pages.contains(&page))
            }
            None => detect_columns(column_detection_items, page, table_pages.contains(&page)),
        };

        if columns.len() <= 1 {
            // Single column - use simple sorting
            let lines = group_single_column(page_items, adaptive_threshold);
            all_lines.extend(lines);
        } else {
            // Multi-column detected. Pre-mask lines that span the full page
            // width (titles, section headers, footers). These multi-item lines
            // would otherwise be split across column buckets, corrupting
            // newspaper detection and reading order.
            let spanning_mask = identify_spanning_lines(&page_items, &columns);
            let premasked_count = spanning_mask.iter().filter(|&&m| m).count();
            if premasked_count > 0 {
                debug!(
                    "page {}: pre-masked {} spanning-line items",
                    page, premasked_count
                );
            }

            // Partition items preserving original order
            let mut spanning_items: Vec<TextItem> = Vec::new();
            let mut column_items: Vec<TextItem> = Vec::new();

            for (i, item) in page_items.into_iter().enumerate() {
                if spanning_mask[i] || spans_multiple_columns(&item, &columns) {
                    spanning_items.push(item);
                } else {
                    column_items.push(item);
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
                            super::trace_text_preview(&item.text, 60)
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

                above.sort_by(|a, b| b.y.total_cmp(&a.y));
                below_spanning.sort_by(|a, b| b.y.total_cmp(&a.y));

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
                    b.y.total_cmp(&a.y).then(
                        a.items
                            .first()
                            .map(|i| i.x)
                            .unwrap_or(0.0)
                            .total_cmp(&b.items.first().map(|i| i.x).unwrap_or(0.0)),
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
        sorted.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
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
            // Same baseline, but separated by a wide void, with the incoming
            // run starting alphabetic: the neighboring column's body text
            // sharing a y with this line, in gutters too narrow for column
            // detection. Both sides must be multi-word prose — TOC page
            // numbers, dot leaders, and outline-numbered table cells (which
            // start with digits) stay joined.
            if let Some(last_item) = last_line.items.last() {
                let gap = item.x - (last_item.x + last_item.width);
                if gap > (item.font_size.max(last_item.font_size) * 3.0).max(30.0)
                    && item
                        .text
                        .trim()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic())
                {
                    // The incoming run must be substantial prose; the line
                    // side may be short (a wrapped heading's last words).
                    let incoming_wordy = {
                        let t = item.text.trim();
                        t.split_whitespace().count() >= 3
                            && t.chars().filter(|c| c.is_alphabetic()).count() >= 10
                    };
                    let line_text = last_line
                        .items
                        .iter()
                        .map(|i| i.text.trim())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let line_wordy = line_text.split_whitespace().count() >= 2
                        && line_text.chars().filter(|c| c.is_alphabetic()).count() >= 8;
                    // Lowercase starts are mid-sentence continuations and
                    // split on prose signals alone. Uppercase starts also
                    // need a bold-style mismatch between the runs — a bold
                    // heading beside regular body text — otherwise same-style
                    // label rows (feature tiles, legends) would shatter.
                    let starts_lower = item
                        .text
                        .trim()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_lowercase());
                    // The whole line must be bold (a heading), not merely
                    // its last run — mixed bold-label/value rows stay joined.
                    let style_mismatch = last_line.items.iter().all(|i| i.is_bold) && !item.is_bold;
                    if line_wordy && incoming_wordy && (starts_lower || style_mismatch) {
                        return false;
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
            is_underline: false,
            is_strikeout: false,
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
    fn same_baseline_wide_gap_lowercase_continuation_splits() {
        // Heading in the left column, mid-sentence body text from the right
        // column at the same y, separated by a wide void: two lines.
        let items = vec![
            make_item(1, 94.0, 242.0, "6.2. Expectations for Re-Hiring Staff"),
            make_item(1, 380.0, 242.0, "they had no plans to re-hire and more"),
        ];
        let lines = group_single_column(items, 0.10);
        assert_eq!(lines.len(), 2, "independent column runs must not fuse");
    }

    #[test]
    fn same_baseline_wide_gap_table_label_stays_joined() {
        // Outline-numbered cell content to the right: table-ish, keep joined.
        let items = vec![
            make_item(1, 94.0, 242.0, "2. Embracing complexity in"),
            make_item(1, 380.0, 242.0, "2.1 Systems thinking and practice"),
        ];
        let lines = group_single_column(items, 0.10);
        assert_eq!(lines.len(), 1, "numbered table cells stay on one line");
    }

    #[test]
    fn three_zone_layout_detected() {
        // Left months (x=15..330), right months (x=345..660), sidebar (x=675..800)
        // Each zone is >100pt wide so min_col_width won't reject any.
        let mut items = Vec::new();
        items.extend(fill_zone(1, 15.0, 330.0, 750.0, 50.0));
        items.extend(fill_zone(1, 345.0, 660.0, 750.0, 50.0));
        items.extend(fill_zone(1, 675.0, 800.0, 750.0, 50.0));

        let cols = detect_columns(&items, 1, false);
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

        let cols = detect_columns(&items, 1, false);
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

        let cols = detect_columns(&items, 1, false);
        // Should detect the gutters between the 3 dense zones, not the wide gap
        // before the sparse zone
        assert!(
            cols.len() >= 3,
            "Expected >=3 columns for dense zones, got {}",
            cols.len()
        );
    }

    /// Helper: create items that fill a zone but with widths that extend past
    /// the zone boundary (simulating justified text). Items start within the zone
    /// but their reported width extends `overshoot` points past the zone end.
    fn fill_zone_justified(
        page: u32,
        x_start: f32,
        x_end: f32,
        overshoot: f32,
        y_start: f32,
        y_end: f32,
    ) -> Vec<TextItem> {
        let mut items = Vec::new();
        let mut y = y_start;
        while y >= y_end {
            // Each line: 3-4 items that together span x_start to x_end+overshoot
            let item_width = (x_end - x_start + overshoot) / 3.0;
            for i in 0..3 {
                let x = x_start + i as f32 * (x_end - x_start) / 3.0;
                let text_len = (item_width / 6.0).ceil() as usize;
                let text: String = "W".repeat(text_len);
                items.push(TextItem {
                    text,
                    x,
                    y,
                    width: item_width,
                    height: 12.0,
                    font_size: 12.0,
                    font: String::new(),
                    page,
                    is_bold: false,
                    is_italic: false,
                    is_underline: false,
                    is_strikeout: false,
                    item_type: ItemType::Text,
                    mcid: None,
                });
            }
            y -= 14.0;
        }
        items
    }

    #[test]
    fn relative_valley_detects_justified_text_columns() {
        // Two columns of justified text where item widths overshoot the gutter
        // by a few points, preventing absolute valley detection from finding
        // an empty gutter.
        let mut items = Vec::new();
        // Left column: x=40..290, items extend to ~297 (7pt overshoot)
        items.extend(fill_zone_justified(1, 40.0, 290.0, 7.0, 750.0, 50.0));
        // Right column: x=300..550, items extend to ~557
        items.extend(fill_zone_justified(1, 300.0, 550.0, 7.0, 750.0, 50.0));

        let cols = detect_columns(&items, 1, false);
        assert_eq!(
            cols.len(),
            2,
            "Expected 2 columns for justified text, got {}",
            cols.len()
        );

        let gutter = cols[0].x_max;
        assert!(
            (280.0..=310.0).contains(&gutter),
            "Gutter at {gutter}, expected ~295"
        );
    }

    #[test]
    fn relative_valley_rejects_single_column_margin() {
        // Single column of text — the right margin drop-off should NOT be
        // detected as a column gutter.
        let items = fill_zone_justified(1, 40.0, 350.0, 0.0, 750.0, 50.0);

        let cols = detect_columns(&items, 1, false);
        assert_eq!(
            cols.len(),
            1,
            "Expected 1 column for single-column text, got {}",
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

    #[test]
    fn premask_spanning_title_removed_from_columns() {
        // Title spans x=30..550 as 5 adjacent items (no gap near gutter at x=300)
        // Two columns: left (x=0..300), right (x=300..600)
        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        let mut items = Vec::new();

        // Spanning title: 5 items at Y=750, each ~100pt wide, gaps ~4pt
        // No item gap falls near the gutter at x=300
        for i in 0..5 {
            items.push(make_item(
                1,
                30.0 + i as f32 * 104.0,
                750.0,
                "TitleWord_________",
            ));
        }

        // Left column body: 20 lines
        for i in 0..20 {
            items.push(make_item(1, 30.0, 700.0 - i as f32 * 14.0, "LeftText__"));
        }

        // Right column body: 20 lines
        for i in 0..20 {
            items.push(make_item(1, 320.0, 700.0 - i as f32 * 14.0, "RightText_"));
        }

        let mask = identify_spanning_lines(&items, &cols);
        let spanning_count = mask.iter().filter(|&&m| m).count();
        let non_spanning_count = mask.iter().filter(|&&m| !m).count();
        assert_eq!(spanning_count, 5, "Title items should be pre-masked");
        assert_eq!(non_spanning_count, 40, "Column items should remain");
    }

    #[test]
    fn premask_does_not_mask_column_items_at_same_y() {
        // Two items at same Y with gap at gutter → NOT masked
        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        let mut items = Vec::new();

        // Items in two columns at same Y — gap center ~305 is near gutter at 300
        for i in 0..15 {
            let y = 700.0 - i as f32 * 14.0;
            items.push(make_item(1, 30.0, y, "LeftText__"));
            items.push(make_item(1, 320.0, y, "RightText_"));
        }

        let mask = identify_spanning_lines(&items, &cols);
        let spanning_count = mask.iter().filter(|&&m| m).count();
        assert_eq!(
            spanning_count, 0,
            "Column items with gap at gutter should NOT be pre-masked"
        );
    }

    #[test]
    fn bullet_marker_column_not_detected_as_column() {
        // Pattern: every line is `● <content>`, with ● at x=90 and content
        // starting at x=104. Histogram detection sees a gutter between them
        // and would split the page into a "bullet column" and "content column",
        // scrambling every list item.
        let mut items = Vec::new();
        for i in 0..15 {
            let y = 750.0 - i as f32 * 30.0;
            items.push(make_item(1, 90.0, y, "●"));
            items.push(make_item(
                1,
                104.0,
                y,
                "FullContentLineTextHere________________",
            ));
        }
        // Pad with content to satisfy min item count for column detection.
        for i in 0..15 {
            let y = 300.0 - i as f32 * 14.0;
            items.push(make_item(1, 72.0, y, "FootnoteText_____________________"));
        }

        let cols = detect_columns(&items, 1, false);
        assert_eq!(
            cols.len(),
            1,
            "Bullet markers aligned at left margin should not be treated as their own column"
        );
    }

    #[test]
    fn is_list_marker_column_detects_bullets() {
        let items = vec![
            make_item(1, 90.0, 100.0, "●"),
            make_item(1, 90.0, 114.0, "●"),
            make_item(1, 90.0, 128.0, "●"),
            make_item(1, 90.0, 142.0, "●"),
        ];
        let refs: Vec<&TextItem> = items.iter().collect();
        let wrapped: Vec<&&TextItem> = refs.iter().collect();
        assert!(is_list_marker_column(&wrapped));
    }

    #[test]
    fn is_list_marker_column_rejects_prose() {
        let items = vec![
            make_item(1, 30.0, 100.0, "Regular prose line"),
            make_item(1, 30.0, 114.0, "Another sentence"),
            make_item(1, 30.0, 128.0, "Third line"),
            make_item(1, 30.0, 142.0, "Fourth line"),
        ];
        let refs: Vec<&TextItem> = items.iter().collect();
        let wrapped: Vec<&&TextItem> = refs.iter().collect();
        assert!(!is_list_marker_column(&wrapped));
    }

    #[test]
    fn premask_narrow_line_not_masked() {
        // Items that form a line spanning only ~40% of column width → not masked
        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        let mut items = Vec::new();

        // Narrow header at top (spans ~240pt, max col width = 300, threshold = 390)
        for i in 0..3 {
            items.push(make_item(
                1,
                180.0 + i as f32 * 84.0,
                750.0,
                "SmallHeader___",
            ));
        }

        // Two columns below
        for i in 0..15 {
            let y = 700.0 - i as f32 * 14.0;
            items.push(make_item(1, 30.0, y, "LeftText__"));
            items.push(make_item(1, 400.0, y, "RightText_"));
        }

        let mask = identify_spanning_lines(&items, &cols);
        let spanning_count = mask.iter().filter(|&&m| m).count();
        assert_eq!(spanning_count, 0, "Narrow header should NOT be pre-masked");
    }
}
