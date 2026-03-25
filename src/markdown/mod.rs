//! Markdown conversion with structure detection.
//!
//! Converts extracted text to markdown, detecting:
//! - Headers (by font size)
//! - Lists (bullet points, numbered lists)
//! - Code blocks (monospace fonts, indentation)
//! - Paragraphs

pub(crate) mod analysis;
mod classify;
mod convert;
mod postprocess;
mod preprocess;

pub use convert::to_markdown_from_lines;

use std::collections::{HashMap, HashSet};

use crate::extractor::group_into_lines_with_thresholds;
use crate::types::{PdfLine, PdfRect, TextItem};

use analysis::calculate_font_stats_from_items;
use classify::{format_list_item, is_code_like, is_list_item};
use convert::{merge_continuation_tables, to_markdown_from_lines_with_tables_and_images};

/// Detect side-by-side table layout by finding a significant X-position gap.
///
/// Returns X-band boundaries `[(x_min, split_x), (split_x, x_max)]` when a
/// clear vertical gap separates two groups of items, or an empty vec if the
/// page has a single-region layout.
///
/// Candidate gaps must be ≥30pt and in the middle 60% of the page's X range.
/// Items are counted by center position for accurate balance (each side ≥20%).
/// The candidate with the fewest bounding-box crossings is chosen (must be
/// under 5% of total items). To reject single wide tables with multiple
/// column gaps, only pages with one balanced-candidate cluster (within 50pt)
/// are accepted.
pub(crate) fn split_side_by_side(items: &[TextItem]) -> Vec<(f32, f32)> {
    if items.len() < 40 {
        return vec![];
    }

    // Sort items by left edge
    let mut xs: Vec<f32> = items.iter().map(|i| i.x).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Find all candidate gaps: ≥30pt, in the middle 60% of the X range,
    // with ≥20 items on each side.
    let x_min = xs[0];
    let x_max = *xs.last().unwrap();
    let x_range = x_max - x_min;
    let center_lo = x_min + x_range * 0.2;
    let center_hi = x_min + x_range * 0.8;
    let mut candidates: Vec<f32> = Vec::new();
    for i in 1..xs.len() {
        let gap = xs[i] - xs[i - 1];
        let split_x = (xs[i - 1] + xs[i]) / 2.0;
        if gap >= 30.0
            && i >= 20
            && (xs.len() - i) >= 20
            && split_x >= center_lo
            && split_x <= center_hi
        {
            candidates.push(split_x);
        }
    }

    if candidates.is_empty() {
        return vec![];
    }

    // Pick the candidate with the fewest bounding-box crossings,
    // but only consider balanced splits (each side ≥ 20% of total items
    // by center position, which is more accurate than left-edge counting).
    let min_side = items.len() / 5;
    let mut best_split = 0.0f32;
    let mut best_crossing = usize::MAX;
    for &split_x in &candidates {
        // Count items by center position for accurate balance check
        let left_count = items
            .iter()
            .filter(|i| i.x + i.width / 2.0 < split_x)
            .count();
        let right_count = items.len() - left_count;
        if left_count.min(right_count) < min_side {
            continue;
        }
        let crossing = items
            .iter()
            .filter(|item| item.x < split_x && (item.x + item.width) > split_x)
            .count();
        if crossing < best_crossing {
            best_crossing = crossing;
            best_split = split_x;
        }
    }

    if best_crossing == usize::MAX {
        return vec![];
    }

    // Crossing items must be < 5% of total (allows spanning headers/labels)
    let max_crossing = (items.len() / 20).max(2);
    if best_crossing > max_crossing {
        return vec![];
    }

    // Multiple balanced split candidates that are far apart indicate a
    // multi-column single table. Adjacent candidates (within 20pt) are
    // treated as the same split point. Side-by-side tables have exactly
    // one cluster of candidates near the inter-table gap.
    let mut balanced_positions: Vec<f32> = candidates
        .iter()
        .filter(|&&sx| {
            let lc = items.iter().filter(|i| i.x + i.width / 2.0 < sx).count();
            let rc = items.len() - lc;
            lc.min(rc) >= min_side
        })
        .copied()
        .collect();
    balanced_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    balanced_positions.dedup_by(|a, b| (*a - *b).abs() < 50.0);
    if balanced_positions.len() > 1 {
        return vec![];
    }

    // Don't split when the left side is text labels and the right side is numeric
    // data at matching Y positions — this is a single table (labels + numbers),
    // not two independent side-by-side regions.
    // Requires ALL THREE: left side is mostly non-numeric, right side is mostly
    // numeric, AND high Y-correlation between the two sides.
    let is_numeric_item = |item: &&&TextItem| -> bool {
        let text = item.text.trim();
        if text.is_empty() {
            return false;
        }
        let data_chars = text
            .chars()
            .filter(|c| c.is_ascii_digit() || ",.-+%€$£¥()".contains(*c))
            .count();
        data_chars as f32 / text.chars().count() as f32 >= 0.6
    };

    let left_items: Vec<&TextItem> = items
        .iter()
        .filter(|i| i.x + i.width / 2.0 < best_split)
        .collect();
    let right_items: Vec<&TextItem> = items
        .iter()
        .filter(|i| i.x + i.width / 2.0 >= best_split)
        .collect();

    if !left_items.is_empty() && !right_items.is_empty() {
        let left_numeric_ratio =
            left_items.iter().filter(is_numeric_item).count() as f32 / left_items.len() as f32;
        let right_numeric_ratio =
            right_items.iter().filter(is_numeric_item).count() as f32 / right_items.len() as f32;

        // Left side is mostly text (< 30% numeric) AND right side is mostly numbers (≥ 70%)
        if left_numeric_ratio < 0.30 && right_numeric_ratio >= 0.70 {
            let y_tol = 5.0;
            let y_matches = right_items
                .iter()
                .filter(|ri| left_items.iter().any(|li| (li.y - ri.y).abs() < y_tol))
                .count();
            if y_matches as f32 / right_items.len() as f32 >= 0.5 {
                return vec![];
            }
        }
    }

    vec![(x_min, best_split), (best_split, x_max)]
}

/// Derive a side-by-side split from rect hint regions.
///
/// When `split_side_by_side` doesn't detect a gap (e.g. the text gap is too
/// small), hint regions from large rect clusters can still reveal a left/right
/// zone layout (calendar months, form sections).  This function checks if hint
/// regions pair up at the same Y bands and returns `[(x_min, split), (split,
/// x_max)]` if a consistent split exists.
fn split_from_hint_regions(items: &[TextItem], rects: &[PdfRect], page: u32) -> Vec<(f32, f32)> {
    use crate::tables::{cluster_rects, RectHintRegion};

    // Quick hint region computation (same logic as detect_tables_from_rects
    // but without table detection).
    let mut page_rects: Vec<(f32, f32, f32, f32)> = Vec::new();
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
        if w < 5.0 || h < 5.0 {
            continue;
        }
        page_rects.push((x, y, w, h));
    }
    if page_rects.len() < 60 {
        return vec![];
    }

    // Width outlier filter (same as detect_tables_from_rects)
    let mut widths: Vec<f32> = page_rects.iter().map(|&(_, _, w, _)| w).collect();
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_width = widths[widths.len() / 2];
    page_rects.retain(|&(_, _, w, _)| w <= median_width * 10.0);

    let clusters = cluster_rects(&page_rects, 3.0, 6);
    if clusters.len() < 4 {
        return vec![];
    }

    // Build hint regions from large clusters
    let mut hints: Vec<RectHintRegion> = Vec::new();
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
            hints.push(RectHintRegion {
                y_top,
                y_bottom,
                x_left,
                x_right,
                cluster_rects: Vec::new(),
            });
        }
    }
    if hints.len() < 4 {
        return vec![];
    }

    // Check for left/right pairing: hints at the same Y band should split
    // into distinct X groups.  Count pairs where two hints share a Y band
    // (>50% overlap) but occupy different X halves.
    let page_x_mid = {
        let x_min = items.iter().map(|i| i.x).reduce(f32::min).unwrap_or(0.0);
        let x_max = items
            .iter()
            .map(|i| i.x + i.width)
            .reduce(f32::max)
            .unwrap_or(800.0);
        (x_min + x_max) / 2.0
    };

    let mut pair_count = 0;
    for (i, a) in hints.iter().enumerate() {
        for b in hints.iter().skip(i + 1) {
            let y_overlap = a.y_top.min(b.y_top) - a.y_bottom.max(b.y_bottom);
            let y_min_span = (a.y_top - a.y_bottom).min(b.y_top - b.y_bottom);
            if y_overlap > y_min_span * 0.5 {
                let a_center = (a.x_left + a.x_right) / 2.0;
                let b_center = (b.x_left + b.x_right) / 2.0;
                if (a_center < page_x_mid) != (b_center < page_x_mid) {
                    pair_count += 1;
                }
            }
        }
    }

    // Require at least 3 left/right pairs to confirm the layout
    if pair_count < 3 {
        return vec![];
    }

    // Find the split X: midpoint between the rightmost left-zone hint
    // and the leftmost right-zone hint
    let max_left_x = hints
        .iter()
        .filter(|h| (h.x_left + h.x_right) / 2.0 < page_x_mid)
        .map(|h| h.x_right)
        .reduce(f32::max);
    let min_right_x = hints
        .iter()
        .filter(|h| (h.x_left + h.x_right) / 2.0 >= page_x_mid)
        .map(|h| h.x_left)
        .reduce(f32::min);

    if let (Some(left_edge), Some(right_edge)) = (max_left_x, min_right_x) {
        let split_x = (left_edge + right_edge) / 2.0;
        let x_min = items.iter().map(|i| i.x).reduce(f32::min).unwrap_or(0.0);
        let x_max = items
            .iter()
            .map(|i| i.x + i.width)
            .reduce(f32::max)
            .unwrap_or(800.0);
        log::debug!(
            "page {}: hint-derived side-by-side split at x={:.1}",
            page,
            split_x
        );
        vec![(x_min, split_x), (split_x, x_max)]
    } else {
        vec![]
    }
}

/// Filter rects to those mostly contained within an X band.
///
/// Excludes rects that extend significantly beyond the band (e.g. page-wide
/// background stripes spanning both side-by-side tables). A rect must have
/// at least 70% of its width inside the band to be included.
pub(crate) fn filter_rects_to_band(
    rects: &[PdfRect],
    page: u32,
    x_lo: f32,
    x_hi: f32,
) -> Vec<PdfRect> {
    let band_width = x_hi - x_lo;
    rects
        .iter()
        .filter(|r| {
            r.page == page && {
                let rx_min = if r.width >= 0.0 { r.x } else { r.x + r.width };
                let rx_max = if r.width >= 0.0 { r.x + r.width } else { r.x };
                let rw = rx_max - rx_min;
                // Overlap region
                let overlap = rx_max.min(x_hi) - rx_min.max(x_lo);
                if overlap <= 0.0 {
                    return false;
                }
                // Small rects (< 70% of band): require any overlap (cell borders, etc.)
                // Large rects (≥ 70% of band): require ≥70% of rect inside band
                if rw < band_width * 0.7 {
                    true
                } else {
                    overlap >= rw * 0.7
                }
            }
        })
        .cloned()
        .collect()
}

/// A band of items/indices/rects/lines for side-by-side table detection.
type BandSpec = (Vec<TextItem>, Vec<usize>, Vec<PdfRect>, Vec<PdfLine>);

/// Filter PDF lines to those overlapping an X band.
pub(crate) fn filter_lines_to_band(
    lines: &[PdfLine],
    page: u32,
    x_lo: f32,
    x_hi: f32,
) -> Vec<PdfLine> {
    lines
        .iter()
        .filter(|l| {
            l.page == page && {
                let lx_min = l.x1.min(l.x2);
                let lx_max = l.x1.max(l.x2);
                lx_max > x_lo && lx_min < x_hi
            }
        })
        .cloned()
        .collect()
}

/// Options for markdown conversion
#[derive(Debug, Clone)]
pub struct MarkdownOptions {
    /// Detect headers by font size
    pub detect_headers: bool,
    /// Detect list items
    pub detect_lists: bool,
    /// Detect code blocks
    pub detect_code: bool,
    /// Base font size for comparison
    pub base_font_size: Option<f32>,
    /// Remove standalone page numbers
    pub remove_page_numbers: bool,
    /// Convert URLs to markdown links
    pub format_urls: bool,
    /// Fix hyphenation (broken words across lines)
    pub fix_hyphenation: bool,
    /// Detect and format bold text from font names
    pub detect_bold: bool,
    /// Detect and format italic text from font names
    pub detect_italic: bool,
    /// Include image placeholders in output
    pub include_images: bool,
    /// Include extracted hyperlinks
    pub include_links: bool,
    /// Insert page break markers (<!-- Page N -->) between pages
    pub include_page_numbers: bool,
    /// Strip repeated headers/footers that appear on many pages
    pub strip_headers_footers: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            detect_headers: true,
            detect_lists: true,
            detect_code: true,
            base_font_size: None,
            remove_page_numbers: true,
            format_urls: true,
            fix_hyphenation: true,
            detect_bold: true,
            detect_italic: true,
            include_images: true,
            include_links: true,
            include_page_numbers: false,
            strip_headers_footers: true,
        }
    }
}

/// Convert plain text to markdown (basic conversion)
pub fn to_markdown(text: &str, options: MarkdownOptions) -> String {
    let mut output = String::new();
    let mut in_list = false;
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if in_list {
                in_list = false;
            }
            if in_code_block {
                output.push_str("```\n");
                in_code_block = false;
            }
            output.push('\n');
            continue;
        }

        // Detect list items
        if options.detect_lists && is_list_item(trimmed) {
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            continue;
        }

        // Detect code blocks (indented lines)
        if options.detect_code && is_code_like(trimmed) {
            if !in_code_block {
                output.push_str("```\n");
                in_code_block = true;
            }
            output.push_str(trimmed);
            output.push('\n');
            continue;
        } else if in_code_block {
            output.push_str("```\n");
            in_code_block = false;
        }

        // Regular paragraph text
        output.push_str(trimmed);
        output.push('\n');
    }

    if in_code_block {
        output.push_str("```\n");
    }

    output
}

/// Convert positioned text items to markdown with structure detection
pub fn to_markdown_from_items(items: Vec<TextItem>, options: MarkdownOptions) -> String {
    to_markdown_from_items_with_rects(items, options, &[])
}

/// Convert positioned text items to markdown, using rectangle data for table detection
pub fn to_markdown_from_items_with_rects(
    items: Vec<TextItem>,
    options: MarkdownOptions,
    rects: &[crate::types::PdfRect],
) -> String {
    to_markdown_from_items_with_rects_and_lines(
        items,
        options,
        rects,
        &[],
        &HashMap::new(),
        None,
        &[],
    )
}

/// Convert positioned text items to markdown, using rectangles and line segments for table detection.
///
/// Line-based detection runs first (strongest structural evidence), then rect-based,
/// then heuristic fallback on unclaimed items.
pub(crate) fn to_markdown_from_items_with_rects_and_lines(
    items: Vec<TextItem>,
    options: MarkdownOptions,
    rects: &[crate::types::PdfRect],
    pdf_lines: &[crate::types::PdfLine],
    page_thresholds: &HashMap<u32, f32>,
    struct_roles: Option<&HashMap<u32, HashMap<i64, crate::structure_tree::StructRole>>>,
    struct_tables: &[crate::structure_tree::StructTable],
) -> String {
    use crate::tables::{
        detect_tables, detect_tables_from_lines, detect_tables_from_rects,
        detect_tables_from_struct_tree, table_to_markdown, try_build_rect_guided_table,
    };
    use crate::types::ItemType;

    if items.is_empty() {
        return String::new();
    }

    // Separate images and links from text items
    let mut images: Vec<TextItem> = Vec::new();
    let mut links: Vec<TextItem> = Vec::new();
    let mut text_items: Vec<TextItem> = Vec::new();

    for item in items {
        match &item.item_type {
            ItemType::Image => {
                if options.include_images {
                    images.push(item);
                }
            }
            ItemType::Link(_) => {
                if options.include_links {
                    links.push(item);
                }
            }
            ItemType::Text | ItemType::FormField => {
                text_items.push(item);
            }
        }
    }

    // Calculate base font size for table detection
    let font_stats = calculate_font_stats_from_items(&text_items);
    let base_size = options
        .base_font_size
        .unwrap_or(font_stats.most_common_size);

    // Detect tables on each page
    let mut table_items: HashSet<usize> = HashSet::new();
    let mut page_tables: HashMap<u32, Vec<(f32, String)>> = HashMap::new();

    // Store images by page and Y position for insertion
    let mut page_images: HashMap<u32, Vec<(f32, String)>> = HashMap::new();

    for img in &images {
        // Extract image name from "[Image: Im0]" format
        let img_name = img
            .text
            .strip_prefix("[Image: ")
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(&img.text);
        let img_md = format!("![Image: {}](image)\n", img_name);
        page_images
            .entry(img.page)
            .or_default()
            .push((img.y, img_md));
    }

    // Pre-group items by page with their global indices (O(n) instead of O(pages*n))
    let mut page_groups: HashMap<u32, Vec<(usize, &TextItem)>> = HashMap::new();
    for (global_idx, item) in text_items.iter().enumerate() {
        page_groups
            .entry(item.page)
            .or_default()
            .push((global_idx, item));
    }

    let mut pages: Vec<u32> = page_groups.keys().copied().collect();
    pages.sort();
    let page_count = pages.last().copied().unwrap_or(0) + 1;

    // Track band splits per page so we can split non-table items later
    let mut page_band_splits: HashMap<u32, Vec<(f32, f32)>> = HashMap::new();

    for page in pages {
        let group = page_groups.get(&page).unwrap();
        let page_items: Vec<TextItem> = group.iter().map(|(_, item)| (*item).clone()).collect();

        // Check for side-by-side layout (e.g. two tables placed left and right)
        let mut bands = split_side_by_side(&page_items);
        // Fallback: use rect hint regions to detect side-by-side layout
        // when the text gap is too narrow for split_side_by_side to detect
        // (e.g. calendars with left/right month columns ~10pt apart).
        if bands.is_empty() {
            bands = split_from_hint_regions(&page_items, rects, page);
            // Only track hint-derived splits for non-table line grouping.
            // split_side_by_side splits already scope table detection and
            // their non-table items should flow through normal line grouping.
            if !bands.is_empty() {
                page_band_splits.insert(page, bands.clone());
            }
        }

        // Build list of (band_items, band_index_map, band_rects, band_lines).
        // band_index_map[local_band_idx] → page_items index.
        let band_specs: Vec<BandSpec> = if bands.is_empty() {
            // Single-region page — use all items/rects/lines as-is
            let identity: Vec<usize> = (0..page_items.len()).collect();
            vec![(
                page_items.clone(),
                identity,
                rects.iter().filter(|r| r.page == page).cloned().collect(),
                pdf_lines
                    .iter()
                    .filter(|l| l.page == page)
                    .cloned()
                    .collect(),
            )]
        } else {
            bands
                .iter()
                .map(|&(x_lo, x_hi)| {
                    let margin = 2.0; // small margin to avoid clipping edge items
                    let (items_in_band, idx_map): (Vec<TextItem>, Vec<usize>) = page_items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| item.x >= x_lo - margin && item.x < x_hi + margin)
                        .map(|(idx, item)| (item.clone(), idx))
                        .unzip();
                    let band_rects = filter_rects_to_band(rects, page, x_lo, x_hi);
                    let band_lines = filter_lines_to_band(pdf_lines, page, x_lo, x_hi);
                    (items_in_band, idx_map, band_rects, band_lines)
                })
                .collect()
        };

        // When the page is split into bands but no band produces a table,
        // retry with all items merged as a single band.  This handles
        // borderless tables whose column alignment is misclassified as
        // page-layout columns by split_side_by_side.
        let was_split = band_specs.len() > 1;
        log::debug!(
            "page {}: {} bands (was_split={})",
            page,
            band_specs.len(),
            was_split
        );
        let merged_band: BandSpec = if was_split {
            let identity: Vec<usize> = (0..page_items.len()).collect();
            (
                page_items.clone(),
                identity,
                rects.iter().filter(|r| r.page == page).cloned().collect(),
                pdf_lines
                    .iter()
                    .filter(|l| l.page == page)
                    .cloned()
                    .collect(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };

        for (band_items, band_index_map, band_rects, band_lines) in &band_specs {
            if band_items.is_empty() {
                continue;
            }

            // Track which band-local indices are claimed by structural detection
            let mut rect_claimed: HashSet<usize> = HashSet::new();

            // 0. Structure-tree detection (highest priority — semantic PDF tagging)
            //    Only use struct-tree tables when they capture a majority (≥50%) of
            //    band items.  Incomplete struct trees (partial tagging) should fall
            //    through to geometry detection which sees all items.
            if !struct_tables.is_empty() {
                let st_tables = detect_tables_from_struct_tree(band_items, struct_tables, page);
                for table in &st_tables {
                    let coverage = table.item_indices.len() as f32 / band_items.len().max(1) as f32;
                    if coverage < 0.5 {
                        continue;
                    }
                    for &idx in &table.item_indices {
                        rect_claimed.insert(idx);
                        if let Some(&page_idx) = band_index_map.get(idx) {
                            if let Some(&(global_idx, _)) = group.get(page_idx) {
                                table_items.insert(global_idx);
                            }
                        }
                    }
                    let table_y = table.rows.first().copied().unwrap_or(0.0);
                    let table_md = table_to_markdown(table);
                    page_tables
                        .entry(page)
                        .or_default()
                        .push((table_y, table_md));
                }
            }

            // 1. Rect-based detection (skips tables overlapping struct-tree claims)
            let (rect_tables, hint_regions) =
                detect_tables_from_rects(band_items, band_rects, page);
            for table in &rect_tables {
                if !rect_claimed.is_empty()
                    && table
                        .item_indices
                        .iter()
                        .any(|idx| rect_claimed.contains(idx))
                {
                    continue;
                }
                for &idx in &table.item_indices {
                    rect_claimed.insert(idx);
                    if let Some(&page_idx) = band_index_map.get(idx) {
                        if let Some(&(global_idx, _)) = group.get(page_idx) {
                            table_items.insert(global_idx);
                        }
                    }
                }
                let table_y = table.rows.first().copied().unwrap_or(0.0);
                let table_md = table_to_markdown(table);
                page_tables
                    .entry(page)
                    .or_default()
                    .push((table_y, table_md));
            }

            // 2. Line-based detection on unclaimed items (when rects didn't find tables)
            if rect_claimed.is_empty() {
                let line_tables = detect_tables_from_lines(band_items, band_lines, page);
                for table in &line_tables {
                    for &idx in &table.item_indices {
                        rect_claimed.insert(idx);
                        if let Some(&page_idx) = band_index_map.get(idx) {
                            if let Some(&(global_idx, _)) = group.get(page_idx) {
                                table_items.insert(global_idx);
                            }
                        }
                    }
                    let table_y = table.rows.first().copied().unwrap_or(0.0);
                    let table_md = table_to_markdown(table);
                    page_tables
                        .entry(page)
                        .or_default()
                        .push((table_y, table_md));
                }
            }

            // 3a. Try rect-guided table construction on hint regions before
            //     creating the heuristic closure (avoids borrow conflicts).
            if rect_claimed.is_empty() && !hint_regions.is_empty() {
                let padding = 15.0;
                for hint in &hint_regions {
                    if hint.cluster_rects.is_empty() {
                        continue;
                    }
                    let (inside_items, inside_map): (Vec<TextItem>, Vec<usize>) = band_items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            item.y >= hint.y_bottom - padding
                                && item.y <= hint.y_top + padding
                                && item.x >= hint.x_left - padding
                                && item.x <= hint.x_right + padding
                        })
                        .map(|(idx, item)| (item.clone(), idx))
                        .unzip();

                    if let Some(table) =
                        try_build_rect_guided_table(&inside_items, &hint.cluster_rects)
                    {
                        for &idx in &table.item_indices {
                            if let Some(&band_idx) = inside_map.get(idx) {
                                if let Some(&page_idx) = band_index_map.get(band_idx) {
                                    if let Some(&(global_idx, _)) = group.get(page_idx) {
                                        table_items.insert(global_idx);
                                    }
                                }
                            }
                        }
                        let table_y = table.rows.first().copied().unwrap_or(0.0);
                        let table_md = table_to_markdown(&table);
                        page_tables
                            .entry(page)
                            .or_default()
                            .push((table_y, table_md));
                        for &band_idx in &inside_map {
                            rect_claimed.insert(band_idx);
                        }
                    }
                }
            }

            // 3b. Heuristic fallback on unclaimed items
            let mut run_heuristic =
                |subset_items: &[TextItem], index_map: &[usize], min_items: usize| {
                    if subset_items.len() < min_items {
                        return;
                    }
                    let tables = detect_tables(subset_items, base_size, false);
                    for table in tables {
                        for &idx in &table.item_indices {
                            if let Some(&band_idx) = index_map.get(idx) {
                                if let Some(&page_idx) = band_index_map.get(band_idx) {
                                    if let Some(&(global_idx, _)) = group.get(page_idx) {
                                        table_items.insert(global_idx);
                                    }
                                }
                            }
                        }
                        let table_y = table.rows.first().copied().unwrap_or(0.0);
                        let table_md = table_to_markdown(&table);
                        page_tables
                            .entry(page)
                            .or_default()
                            .push((table_y, table_md));
                    }
                };

            // Run heuristic detection on unclaimed items
            if rect_claimed.is_empty() && hint_regions.is_empty() {
                // No rect tables or hints — run heuristic on all band items
                let identity_map: Vec<usize> = (0..band_items.len()).collect();
                run_heuristic(band_items, &identity_map, 6);
            } else if rect_claimed.is_empty() && !hint_regions.is_empty() {
                // No rect tables but hint regions exist — run heuristic separately
                // on items inside each hint region and on items outside all hints.
                let padding = 15.0;
                for hint in &hint_regions {
                    let (inside_items, inside_map): (Vec<TextItem>, Vec<usize>) = band_items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            item.y >= hint.y_bottom - padding && item.y <= hint.y_top + padding
                        })
                        .map(|(idx, item)| (item.clone(), idx))
                        .unzip();
                    run_heuristic(&inside_items, &inside_map, 6);
                    for &band_idx in &inside_map {
                        rect_claimed.insert(band_idx);
                    }
                }
                let (outside_items, outside_map): (Vec<TextItem>, Vec<usize>) = band_items
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| !rect_claimed.contains(idx))
                    .map(|(idx, item)| (item.clone(), idx))
                    .unzip();
                run_heuristic(&outside_items, &outside_map, 6);
            } else {
                // Rect tables found — run heuristic on unclaimed items
                let (unclaimed_items, unclaimed_map): (Vec<TextItem>, Vec<usize>) = band_items
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| !rect_claimed.contains(idx))
                    .map(|(idx, item)| (item.clone(), idx))
                    .unzip();
                run_heuristic(&unclaimed_items, &unclaimed_map, 6);
            }

            // 4. Column-based table detection: last resort for borderless tabular
            //    layouts (e.g. exam/reference grids) when ALL structural methods
            //    found nothing. Only runs when no rects/lines exist (truly borderless)
            //    and no other detection method found tables in this band.
            let band_has_tables = band_items.iter().enumerate().any(|(idx, _)| {
                band_index_map
                    .get(idx)
                    .and_then(|&page_idx| group.get(page_idx))
                    .is_some_and(|&(global_idx, _)| table_items.contains(&global_idx))
            });
            let has_structural_elements = band_rects.len() >= 6 || band_lines.len() >= 4;
            if !band_has_tables && !has_structural_elements {
                if let Some(table) = crate::tables::try_build_table_from_columns(band_items, page) {
                    for &idx in &table.item_indices {
                        if let Some(&page_idx) = band_index_map.get(idx) {
                            if let Some(&(global_idx, _)) = group.get(page_idx) {
                                table_items.insert(global_idx);
                            }
                        }
                    }
                    let table_y = table.rows.first().copied().unwrap_or(0.0);
                    let table_md = table_to_markdown(&table);
                    page_tables
                        .entry(page)
                        .or_default()
                        .push((table_y, table_md));
                }
            }
        }

        // Merged-band retry: if we split into bands but found no tables in
        // any band, retry heuristic detection with all items as a single band.
        // This catches borderless tables whose text-column alignment was
        // misclassified as page-layout columns.
        if was_split && !page_tables.contains_key(&page) && !merged_band.0.is_empty() {
            let (ref band_items, ref band_index_map, _, _) = merged_band;
            log::debug!(
                "page {}: merged-band retry ({} items, was_split={})",
                page,
                band_items.len(),
                was_split
            );
            let heuristic_tables = detect_tables(band_items, base_size, false);
            for table in &heuristic_tables {
                for &idx in &table.item_indices {
                    if let Some(&page_idx) = band_index_map.get(idx) {
                        if let Some(&(global_idx, _)) = group.get(page_idx) {
                            table_items.insert(global_idx);
                        }
                    }
                }
                let table_y = table.rows.first().copied().unwrap_or(0.0);
                let table_md = table_to_markdown(table);
                page_tables
                    .entry(page)
                    .or_default()
                    .push((table_y, table_md));
            }
        }
    }

    // Check structure tree coverage on ALL text items (before table filtering)
    // to decide whether to use structure-aware markdown generation.
    let struct_roles_coverage_ok = struct_roles.is_some_and(|roles| {
        let total = text_items.len();
        if total == 0 {
            return false;
        }
        let tagged = text_items
            .iter()
            .filter(|item| {
                item.mcid
                    .and_then(|mcid| {
                        roles
                            .get(&item.page)
                            .and_then(|page_roles| page_roles.get(&mcid))
                    })
                    .is_some()
            })
            .count();
        let coverage = tagged as f32 / total as f32;
        log::debug!(
            "structure tree coverage: {}/{} items ({:.0}%)",
            tagged,
            total,
            coverage * 100.0
        );
        coverage >= 0.5
    });
    let effective_struct_roles = if struct_roles_coverage_ok {
        struct_roles
    } else {
        None
    };

    // Filter out table items and process the rest
    let non_table_items: Vec<TextItem> = text_items
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| !table_items.contains(idx))
        .map(|(_, item)| item)
        .collect();

    // Find pages that are table-only (no remaining non-table text)
    let table_only_pages: HashSet<u32> = {
        let pages_with_text: HashSet<u32> = non_table_items.iter().map(|i| i.page).collect();
        page_tables
            .keys()
            .filter(|p| !pages_with_text.contains(p))
            .copied()
            .collect()
    };

    // Merge continuation tables across page breaks, but only for table-only pages
    merge_continuation_tables(&mut page_tables, &table_only_pages);

    // Collect pages that have detected tables — used to suppress relative valley
    // column detection on pages where table column gaps would be misidentified.
    let table_page_set: HashSet<u32> = page_tables.keys().copied().collect();

    // Split non-table items by band boundaries before line grouping so that
    // items from different side-by-side zones (e.g. left/right month columns
    // in a calendar) don't merge into the same line.
    let lines = if page_band_splits.is_empty() {
        group_into_lines_with_thresholds(non_table_items, page_thresholds, &table_page_set)
    } else {
        // Separate items into band-split pages and non-split pages
        let mut split_page_items: HashMap<u32, Vec<TextItem>> = HashMap::new();
        let mut unsplit_items: Vec<TextItem> = Vec::new();
        for item in non_table_items {
            if page_band_splits.contains_key(&item.page) {
                split_page_items.entry(item.page).or_default().push(item);
            } else {
                unsplit_items.push(item);
            }
        }
        // Process unsplit pages normally
        let mut all_lines =
            group_into_lines_with_thresholds(unsplit_items, page_thresholds, &table_page_set);
        // Process each split page's bands independently, then interleave
        // by Y position so paired zones (e.g. left/right months) appear together.
        let mut split_pages: Vec<u32> = split_page_items.keys().copied().collect();
        split_pages.sort();
        for page in split_pages {
            let items = split_page_items.remove(&page).unwrap();
            let bands = &page_band_splits[&page];
            let mut page_lines: Vec<crate::types::TextLine> = Vec::new();
            for &(x_lo, x_hi) in bands {
                let margin = 2.0;
                let band_items: Vec<TextItem> = items
                    .iter()
                    .filter(|i| i.x >= x_lo - margin && i.x < x_hi + margin)
                    .cloned()
                    .collect();
                if !band_items.is_empty() {
                    page_lines.extend(group_into_lines_with_thresholds(
                        band_items,
                        page_thresholds,
                        &table_page_set,
                    ));
                }
            }
            // Sort by Y descending (top to bottom) so left and right
            // band lines interleave in visual reading order.
            page_lines.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));
            all_lines.extend(page_lines);
        }
        all_lines
    };

    // Strip repeated headers/footers before conversion
    let lines = if options.strip_headers_footers {
        preprocess::strip_repeated_lines(lines, page_count)
    } else {
        lines
    };

    // Convert to markdown, inserting tables and images at appropriate positions
    let band_split_page_set: HashSet<u32> = page_band_splits.keys().copied().collect();
    to_markdown_from_lines_with_tables_and_images(
        lines,
        options,
        page_tables,
        page_images,
        &band_split_page_set,
        effective_struct_roles,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use analysis::detect_header_level;
    use classify::{is_code_like, is_list_item};

    #[test]
    fn test_is_list_item() {
        assert!(is_list_item("• Item one"));
        assert!(is_list_item("- Item two"));
        assert!(is_list_item("* Item three"));
        assert!(is_list_item("1. First"));
        assert!(is_list_item("2) Second"));
        assert!(is_list_item("a. Letter item"));
        assert!(!is_list_item("Regular text"));
    }

    #[test]
    fn test_format_list_item() {
        assert_eq!(format_list_item("• Item"), "- Item");
        assert_eq!(format_list_item("- Item"), "- Item");
        assert_eq!(format_list_item("1. First"), "1. First");
    }

    #[test]
    fn test_is_code_like() {
        assert!(is_code_like("const x = 5;"));
        assert!(is_code_like("function foo() {"));
        assert!(is_code_like("import React from 'react'"));
        assert!(!is_code_like("This is regular text."));
    }

    #[test]
    fn test_detect_header_level() {
        // With three tiers: 24→H1, 18→H2, 15→H3, 12→None
        let tiers = vec![24.0, 18.0, 15.0];
        assert_eq!(detect_header_level(24.0, 12.0, &tiers), Some(1));
        assert_eq!(detect_header_level(18.0, 12.0, &tiers), Some(2));
        assert_eq!(detect_header_level(15.0, 12.0, &tiers), Some(3));
        assert_eq!(detect_header_level(12.0, 12.0, &tiers), None);

        // Single tier: 15→H1 (ratio 1.25 ≥ 1.2), 14→None (ratio 1.17 < 1.2)
        let tiers = vec![15.0];
        assert_eq!(detect_header_level(15.0, 12.0, &tiers), Some(1));
        assert_eq!(detect_header_level(14.0, 12.0, &tiers), None);
        assert_eq!(detect_header_level(12.0, 12.0, &tiers), None);

        // No tiers (empty): falls back to ratio thresholds
        let tiers: Vec<f32> = vec![];
        assert_eq!(detect_header_level(24.0, 12.0, &tiers), Some(1));
        assert_eq!(detect_header_level(18.0, 12.0, &tiers), Some(2));
        assert_eq!(detect_header_level(15.0, 12.0, &tiers), Some(3));
        assert_eq!(detect_header_level(14.5, 12.0, &tiers), Some(4));
        assert_eq!(detect_header_level(14.0, 12.0, &tiers), None);
        assert_eq!(detect_header_level(12.0, 12.0, &tiers), None);

        // Body text excluded when tiers exist: 13pt (ratio 1.08) → None
        let tiers = vec![20.0];
        assert_eq!(detect_header_level(13.0, 12.0, &tiers), None);
    }

    #[test]
    fn test_to_markdown() {
        let text = "• First item\n• Second item\n\nRegular paragraph.";
        let md = to_markdown(text, MarkdownOptions::default());
        assert!(md.contains("- First item"));
        assert!(md.contains("- Second item"));
    }

    fn make_item(x: f32, y: f32, page: u32) -> TextItem {
        TextItem {
            text: "A".into(),
            x,
            y,
            width: 5.0,
            height: 10.0,
            font: String::new(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn split_from_hint_regions_too_few_rects() {
        // Fewer than 60 rects → no split
        let items = vec![make_item(10.0, 100.0, 1)];
        let rects: Vec<PdfRect> = (0..30)
            .map(|i| PdfRect {
                x: 10.0 + (i % 7) as f32 * 15.0,
                y: 100.0 + (i / 7) as f32 * 15.0,
                width: 10.0,
                height: 10.0,
                page: 1,
            })
            .collect();
        assert!(split_from_hint_regions(&items, &rects, 1).is_empty());
    }

    #[test]
    fn split_from_hint_regions_no_pairs() {
        // Enough rects but all in one X zone → no left/right pairs → no split
        let items = vec![make_item(10.0, 100.0, 1)];
        // 80 rects all in left half
        let rects: Vec<PdfRect> = (0..80)
            .map(|i| PdfRect {
                x: 10.0 + (i % 10) as f32 * 15.0,
                y: 100.0 + (i / 10) as f32 * 15.0,
                width: 10.0,
                height: 10.0,
                page: 1,
            })
            .collect();
        assert!(split_from_hint_regions(&items, &rects, 1).is_empty());
    }

    #[test]
    fn no_split_label_plus_number_table() {
        // Balance sheet layout: text labels on left, numbers on right.
        // Should NOT split because it's one table, not side-by-side regions.
        let mut items = Vec::new();
        for row in 0..30 {
            // Label at x=50
            let mut label = make_item(50.0, 700.0 - row as f32 * 15.0, 1);
            label.text = format!("Row label {}", row);
            label.width = 100.0;
            items.push(label);
            // Number at x=400
            let mut num1 = make_item(400.0, 700.0 - row as f32 * 15.0, 1);
            num1.text = format!("{},000.0", 100 + row);
            num1.width = 50.0;
            items.push(num1);
            // Number at x=470
            let mut num2 = make_item(470.0, 700.0 - row as f32 * 15.0, 1);
            num2.text = format!("{},500.0", 200 + row);
            num2.width = 50.0;
            items.push(num2);
        }
        let split = split_side_by_side(&items);
        assert!(
            split.is_empty(),
            "label+number table should not be split side-by-side"
        );
    }
}
