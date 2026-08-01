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
mod heading;
mod postprocess;
mod preprocess;

pub use convert::to_markdown_from_lines;

use std::collections::{HashMap, HashSet};

use crate::types::{PdfLine, PdfRect, TextItem};

use analysis::calculate_font_stats_from_items;
use classify::{format_list_item, is_caption_line, is_code_like, is_list_item};
use convert::{
    merge_continuation_tables, to_markdown_from_lines_with_tables_and_images, ChartProseOrder,
    PositionedMarkdown,
};

const CHART_REGION_PAD: f32 = 20.0;
const CHART_SEPARATOR_PAD: f32 = 8.0;

fn is_chart_adjacent_label(item: &TextItem, region: (f32, f32, f32, f32)) -> bool {
    let text = item.text.trim();
    let is_bare_bullet = matches!(text, "•" | "●" | "○" | "◦" | "-" | "*");
    if text.is_empty() || is_list_item(text) || is_bare_bullet {
        return false;
    }

    let (x0, y0, x1, y1) = region;
    let (left, right) = (x0.min(x1), x0.max(x1));
    let (bottom, top) = (y0.min(y1), y0.max(y1));
    let item_left = item.x.min(item.x + item.width);
    let item_right = item.x.max(item.x + item.width);
    let item_width = (item_right - item_left).max(1.0);
    let chart_width = (right - left).max(1.0);
    let horizontal_overlap = (item_right.min(right) - item_left.max(left)).max(0.0);
    let mostly_inside_chart_width = horizontal_overlap >= item_width * 0.8;
    let vertical_gap = if item.y < bottom {
        bottom - item.y
    } else if item.y > top {
        item.y - top
    } else {
        0.0
    };
    let is_caption = is_caption_line(text);
    let em = item.height.max(item.font_size).max(1.0);
    let compact_label = item_width <= em * 18.5;
    let category_band = (em * 1.85).clamp(6.0, CHART_REGION_PAD);
    let close_to_chart_edge = if is_caption {
        vertical_gap <= CHART_REGION_PAD
    } else {
        vertical_gap <= category_band
    };
    let category_sized = item_width <= chart_width * 0.75;

    vertical_gap <= CHART_REGION_PAD
        && (compact_label
            || is_caption
            || (mostly_inside_chart_width && close_to_chart_edge && category_sized))
}

fn item_is_in_chart_region(item: &TextItem, regions: &[(f32, f32, f32, f32)]) -> bool {
    regions.iter().any(|&(x0, y0, x1, y1)| {
        let cx = item.x + item.width / 2.0;
        let within_padded_x = cx >= x0 - CHART_REGION_PAD && cx <= x1 + CHART_REGION_PAD;
        let within_core_y = item.y >= y0 && item.y <= y1;
        let within_padded_y = item.y >= y0 - CHART_REGION_PAD
            && item.y <= y1 + CHART_REGION_PAD
            && is_chart_adjacent_label(item, (x0, y0, x1, y1));
        within_padded_x && (within_core_y || within_padded_y)
    })
}

fn items_outside_chart_regions(
    items: &[TextItem],
    regions: &[(f32, f32, f32, f32)],
) -> Vec<TextItem> {
    items
        .iter()
        .filter(|item| !item_is_in_chart_region(item, regions))
        .cloned()
        .collect()
}

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
    xs.sort_by(|a, b| a.total_cmp(b));

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
    balanced_positions.sort_by(|a, b| a.total_cmp(b));
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

/// Detect two short prose columns on a chart page from repeated left anchors.
///
/// Chart masking can leave fewer than 20 lines per column, while justified text
/// can reduce the physical gutter below the projection detector's 8pt minimum.
/// Repeated prose left edges remain reliable in that case. The caller scopes
/// this signal to pages with a confirmed chart region.
fn chart_page_prose_column_split(items: &[TextItem]) -> Option<f32> {
    const X_TOLERANCE: f32 = 12.0;
    const MIN_LINES_PER_COLUMN: usize = 6;
    const MIN_ANCHOR_SEPARATION: f32 = 120.0;
    const MIN_VERTICAL_SPAN: f32 = 60.0;

    let mut prose: Vec<&TextItem> = items
        .iter()
        .filter(|item| {
            let words = item.text.split_whitespace().count();
            let chars = item.text.chars().count().max(1);
            let alphabetic = item.text.chars().filter(|c| c.is_alphabetic()).count();
            words >= 4 && item.width >= 80.0 && alphabetic * 2 >= chars
        })
        .collect();
    if prose.len() < MIN_LINES_PER_COLUMN * 2 {
        return None;
    }
    prose.sort_by(|a, b| a.x.total_cmp(&b.x));

    let mut clusters: Vec<(f32, Vec<&TextItem>)> = Vec::new();
    for item in prose {
        if let Some((anchor, members)) = clusters
            .iter_mut()
            .find(|(anchor, _)| (item.x - *anchor).abs() <= X_TOLERANCE)
        {
            members.push(item);
            *anchor = members.iter().map(|member| member.x).sum::<f32>() / members.len() as f32;
        } else {
            clusters.push((item.x, vec![item]));
        }
    }

    let mut dominant: Vec<(f32, Vec<&TextItem>)> = clusters
        .into_iter()
        .filter(|(_, members)| members.len() >= MIN_LINES_PER_COLUMN)
        .collect();
    if dominant.len() != 2 {
        return None;
    }
    dominant.sort_by(|a, b| a.0.total_cmp(&b.0));
    if dominant[1].0 - dominant[0].0 < MIN_ANCHOR_SEPARATION {
        return None;
    }

    let vertical_range = |members: &[&TextItem]| {
        let y_min = members
            .iter()
            .map(|item| item.y)
            .fold(f32::INFINITY, f32::min);
        let y_max = members
            .iter()
            .map(|item| item.y)
            .fold(f32::NEG_INFINITY, f32::max);
        (y_min, y_max)
    };
    let left_y = vertical_range(&dominant[0].1);
    let right_y = vertical_range(&dominant[1].1);
    if left_y.1 - left_y.0 < MIN_VERTICAL_SPAN || right_y.1 - right_y.0 < MIN_VERTICAL_SPAN {
        return None;
    }
    let overlap = (left_y.1.min(right_y.1) - left_y.0.max(right_y.0)).max(0.0);
    let shorter_span = (left_y.1 - left_y.0).min(right_y.1 - right_y.0);
    if overlap < shorter_span * 0.4 {
        return None;
    }

    Some((dominant[0].0 + dominant[1].0) / 2.0)
}

/// True when a chart crosses the inferred prose gutter by enough to act as a
/// page-wide separator. A chart confined to one column must stay in that
/// column's local reading order instead of reordering the entire page.
fn chart_spans_prose_split(region: (f32, f32, f32, f32), split_x: f32) -> bool {
    const MIN_CHART_WIDTH_PER_SIDE: f32 = 40.0;

    let (x0, _, x1, _) = region;
    let left = x0.min(x1);
    let right = x0.max(x1);
    split_x - left >= MIN_CHART_WIDTH_PER_SIDE && right - split_x >= MIN_CHART_WIDTH_PER_SIDE
}

/// True when adjacent physical rows form an unterminated, lowercase prose
/// continuation in the same projected column.
fn is_cross_row_prose_continuation(previous: &str, current: &str) -> bool {
    let previous = previous.trim();
    let current = current.trim();
    if previous.is_empty() || current.is_empty() {
        return false;
    }

    let previous_without_closers = previous.trim_end_matches(['"', '\'', '”', ')', ']']);
    let previous_is_open = previous_without_closers
        .chars()
        .next_back()
        .is_some_and(|ch| !matches!(ch, '.' | '!' | '?' | ':' | ';'));
    let current_starts_as_continuation = current
        .chars()
        .find(|ch| ch.is_alphabetic())
        .is_some_and(|ch| ch.is_lowercase());

    previous_is_open && current_starts_as_continuation
}

/// Section-numbered headings embedded in a candidate are strong evidence that
/// a heuristic grid has captured page prose rather than a real table.
fn looks_like_numbered_section_heading(text: &str) -> bool {
    let Some((prefix, title)) = text.trim().split_once(char::is_whitespace) else {
        return false;
    };
    let prefix = prefix.trim_end_matches('.');
    let mut group_count = 0;
    for group in prefix.split('.') {
        if group.is_empty() || group.len() > 3 || !group.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        group_count += 1;
    }
    let title = title.trim();
    (1..=4).contains(&group_count)
        && title.split_whitespace().count() >= 3
        && title
            .chars()
            .find(|ch| ch.is_alphabetic())
            .is_some_and(|ch| ch.is_uppercase())
}

fn merged_retry_skips_body_font(detected_columns: bool, has_chart_regions: bool) -> bool {
    detected_columns && !has_chart_regions
}

/// Reject a heuristic table only when its cells are overwhelmingly parallel
/// prose fragments. This is deliberately narrower than disabling body-font
/// detection for the whole page: numeric, compact, headed, and otherwise
/// table-shaped candidates remain eligible on chart pages.
fn is_parallel_prose_table(table: &crate::tables::Table) -> bool {
    if table.kind != crate::tables::TableKind::Data
        || !(2..=3).contains(&table.columns.len())
        || table.rows.len() < 3
    {
        return false;
    }

    let mut non_empty = 0;
    let mut long_prose = 0;
    let mut rows_with_parallel_prose = 0;
    let mut occupied_rows = 0;
    let has_numbered_section_heading = table
        .cells
        .iter()
        .flatten()
        .any(|cell| looks_like_numbered_section_heading(cell));
    let has_compact_header = table
        .cells
        .iter()
        .find(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .is_some_and(|row| {
            let filled: Vec<&String> = row.iter().filter(|cell| !cell.trim().is_empty()).collect();
            filled.len() >= 2
                && filled.iter().all(|cell| {
                    cell.split_whitespace().count() <= 4 && cell.trim().chars().count() <= 28
                })
        });

    for row in &table.cells {
        let mut row_long_prose = 0;
        let mut row_non_empty = 0;
        for cell in row {
            let text = cell.trim();
            if text.is_empty() {
                continue;
            }
            non_empty += 1;
            row_non_empty += 1;
            let chars = text.chars().filter(|ch| !ch.is_whitespace()).count();
            let alphabetic = text.chars().filter(|ch| ch.is_alphabetic()).count();
            let words = text.split_whitespace().count();
            if chars >= 28 && words >= 5 && alphabetic * 5 >= chars * 3 {
                long_prose += 1;
                row_long_prose += 1;
            }
        }
        if row_long_prose >= 2 {
            rows_with_parallel_prose += 1;
        }
        if row_non_empty > 0 {
            occupied_rows += 1;
        }
    }

    // A lowercase cell is not continuation evidence by itself: legitimate
    // headerless tables often use sentence fragments as row values. Require a
    // direct physical-row transition from an unterminated cell in the same
    // column. This is the shape produced when independent prose columns are
    // accidentally projected onto one table grid.
    let mut continuation_fragments = 0;
    let mut continuation_columns = vec![false; table.columns.len()];
    for rows in table.cells.windows(2) {
        for (column, has_continuation) in continuation_columns.iter_mut().enumerate() {
            let previous = rows[0].get(column).map(String::as_str).unwrap_or("");
            let current = rows[1].get(column).map(String::as_str).unwrap_or("");
            if is_cross_row_prose_continuation(previous, current) {
                continuation_fragments += 1;
                *has_continuation = true;
            }
        }
    }

    let is_parallel = !has_compact_header
        && non_empty >= 5
        // Independent prose columns have asynchronous line/paragraph breaks;
        // a fully populated grid is positive evidence for a real descriptive
        // table even when every value is a lowercase sentence fragment.
        && non_empty < table.cells.len() * table.columns.len()
        && long_prose >= 4
        && long_prose * 5 >= non_empty * 3
        // Row-spanning blanks are common in real headerless description
        // tables. Require long text in parallel on at least half of occupied
        // rows, unless a section heading was swallowed into the grid: that is
        // direct evidence that this candidate is page prose.
        && ((rows_with_parallel_prose >= 2
            && rows_with_parallel_prose * 2 >= occupied_rows)
            || (rows_with_parallel_prose >= 1 && has_numbered_section_heading))
        && continuation_fragments >= 3
        && continuation_columns.iter().filter(|&&value| value).count() >= 2;
    log::debug!(
        "chart table hypothesis: {}x{}, non_empty={}, long_prose={}, parallel_rows={}/{}, section_heading={}, continuation_fragments={}, continuation_columns={}, reject={}",
        table.rows.len(),
        table.columns.len(),
        non_empty,
        long_prose,
        rows_with_parallel_prose,
        occupied_rows,
        has_numbered_section_heading,
        continuation_fragments,
        continuation_columns.iter().filter(|&&value| value).count(),
        is_parallel
    );
    is_parallel
}

fn positioned_table(
    table: &crate::tables::Table,
    chart_order: Option<ChartProseOrder>,
) -> PositionedMarkdown {
    PositionedMarkdown::new(
        table.rows.first().copied().unwrap_or(0.0),
        table.columns.first().copied().unwrap_or(0.0),
        crate::tables::table_to_markdown(table),
        chart_order,
    )
}

/// Derive a side-by-side split from rect hint regions.
///
/// When `split_side_by_side` doesn't detect a gap (e.g. the text gap is too
/// small), hint regions from large rect clusters can still reveal a left/right
/// zone layout (calendar months, form sections).  This function checks if hint
/// regions pair up at the same Y bands and returns `[(x_min, split), (split,
/// x_max)]` if a consistent split exists.
/// True when a table-shaped rect cluster (≥6 rects) ends at an interior band
/// boundary and its rows visibly continue on the far side: cell-like text
/// across the boundary is y-aligned with most cluster rows, and nearly all
/// far-side text in the cluster's y-range participates in that alignment.
/// Tables often rule only their leading columns, so the text gap before the
/// borderless columns masquerades as a page-layout gutter — a real second
/// layout column would instead be dense prose that doesn't track table rows.
fn rect_cluster_spans_band_boundary(
    items: &[TextItem],
    rects: &[PdfRect],
    page: u32,
    bands: &[(f32, f32)],
) -> bool {
    if bands.len() < 2 {
        return false;
    }
    // Normalize: raw PDF rects can carry negative extents.
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
        .collect();
    if page_rects.len() < 6 {
        return false;
    }
    let clusters = crate::tables::detect_rects::cluster_rects(&page_rects, 3.0, 6);
    let boundaries: Vec<f32> = bands[..bands.len() - 1].iter().map(|&(_, hi)| hi).collect();

    boundaries.iter().any(|&b| {
        // Y-ranges of clusters that individually indicate the split cuts a
        // table: either ruled on both sides of the boundary, or ending at
        // the boundary with cell-like text row-aligned beyond it.
        let mut table_y_ranges: Vec<(f32, f32)> = Vec::new();
        for cluster in &clusters {
            let bbox = cluster.iter().fold(
                (
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ),
                |(x0, y0, x1, y1), &i| {
                    let (x, y, w, h) = page_rects[i];
                    (x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h))
                },
            );
            let spans = bbox.0 < b - 20.0 && bbox.2 > b + 20.0;
            let ends_at = bbox.2 >= b - 60.0 && bbox.2 <= b + 10.0 && bbox.0 <= b;
            if !spans && !ends_at {
                continue;
            }
            // Distinct row baselines of items inside the cluster bbox.
            let mut row_ys: Vec<f32> = Vec::new();
            for it in items {
                let cx = it.x + it.width / 2.0;
                if it.page == page
                    && cx > bbox.0
                    && cx < bbox.2
                    && it.y >= bbox.1 - 2.0
                    && it.y <= bbox.3 + 2.0
                    && !row_ys.iter().any(|&y| (y - it.y).abs() <= 2.0)
                {
                    row_ys.push(it.y);
                }
            }
            if row_ys.len() < 2 {
                continue;
            }
            // Cell-like far-side items row-aligned with the cluster.
            let cell_like = |it: &&TextItem| it.width <= 150.0;
            let far_aligned_rows = row_ys
                .iter()
                .filter(|&&y| {
                    items.iter().any(|it| {
                        it.page == page
                            && it.x + it.width / 2.0 > b
                            && cell_like(&it)
                            && (it.y - y).abs() <= 2.0
                    })
                })
                .count();
            if far_aligned_rows >= 2 && far_aligned_rows * 2 >= row_ys.len() {
                table_y_ranges.push((bbox.1, bbox.3));
            }
        }
        if table_y_ranges.is_empty() {
            return false;
        }
        // The split is only wrong if the table rows account for most of the
        // far side. A figure legitimately spanning two text columns leaves
        // the majority of far-side text (column prose) outside its y-range.
        let far: Vec<&TextItem> = items
            .iter()
            .filter(|it| it.page == page && it.x + it.width / 2.0 > b)
            .collect();
        if far.is_empty() {
            return false;
        }
        let inside = far
            .iter()
            .filter(|it| {
                table_y_ranges
                    .iter()
                    .any(|&(lo, hi)| it.y >= lo - 2.0 && it.y <= hi + 2.0)
            })
            .count();
        inside * 10 >= far.len() * 6
    })
}

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
    widths.sort_by(|a, b| a.total_cmp(b));
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

/// Output policy for Markdown post-processing.
///
/// [`MarkdownProfile::Fidelity`] preserves source characters wherever possible.
/// [`MarkdownProfile::Compact`] enables optional token-saving rewrites that may
/// be useful for agent context windows but are not byte-faithful to the PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkdownProfile {
    /// Preserve source text fidelity. This is the default.
    #[default]
    Fidelity,
    /// Prefer token-efficient output, including collapsing long dot leaders.
    Compact,
}

/// Options for markdown conversion
#[derive(Debug, Clone)]
pub struct MarkdownOptions {
    /// Source-fidelity versus token-efficient post-processing policy.
    pub profile: MarkdownProfile,
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
    /// Emit `<u>` runs for text with a geometrically-detected underline
    pub detect_underline: bool,
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
            profile: MarkdownProfile::default(),
            detect_headers: true,
            detect_lists: true,
            detect_code: true,
            base_font_size: None,
            remove_page_numbers: true,
            format_urls: true,
            fix_hyphenation: true,
            detect_bold: true,
            detect_italic: true,
            detect_underline: true,
            // `include_images: false` is intentional. The content-stream walker
            // now emits `ItemType::Image` `TextItem`s for every Image XObject
            // it encounters (see `extractor/content_stream.rs`). If we rendered
            // those into markdown by default, every existing caller would
            // suddenly see `![Image: Im0](image)` placeholders inserted
            // throughout their output — a silent regression for anyone who
            // upgrades. Image bboxes are still available via
            // `extract_text_with_positions` for callers (e.g. layout-aware
            // pipelines) that want to crop + caption figures themselves.
            include_images: false,
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
        detect_tables_from_struct_tree, try_build_rect_guided_table,
    };
    use crate::types::ItemType;

    if items.is_empty() {
        return String::new();
    }

    // Separate images and links from text items
    let mut images: Vec<TextItem> = Vec::new();
    let mut page_image_regions: HashMap<u32, Vec<(f32, f32, f32, f32)>> = HashMap::new();
    let mut links: Vec<TextItem> = Vec::new();
    let mut text_items: Vec<TextItem> = Vec::new();

    for item in items {
        match &item.item_type {
            ItemType::Image => {
                page_image_regions.entry(item.page).or_default().push((
                    item.x,
                    item.y,
                    item.x + item.width,
                    item.y + item.height,
                ));
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
    let mut page_tables: HashMap<u32, Vec<PositionedMarkdown>> = HashMap::new();

    // Pre-group items by page with their global indices (O(n) instead of O(pages*n))
    let mut page_groups: HashMap<u32, Vec<(usize, &TextItem)>> = HashMap::new();
    for (global_idx, item) in text_items.iter().enumerate() {
        page_groups
            .entry(item.page)
            .or_default()
            .push((global_idx, item));
    }

    // Chart regions per page: their text must not steer column detection
    // during line grouping (it fills the gutter and fuses two-column lines).
    let mut page_chart_map: HashMap<u32, Vec<(f32, f32, f32, f32)>> = HashMap::new();
    for &page in page_groups.keys() {
        let page_items_ref: Vec<TextItem> = page_groups[&page]
            .iter()
            .map(|(_, item)| (*item).clone())
            .collect();
        let regions = crate::tables::detect_chart_regions(&page_items_ref, rects, page);
        if !regions.is_empty() {
            page_chart_map.insert(page, regions);
        }
    }

    let mut pages: Vec<u32> = page_groups.keys().copied().collect();
    pages.sort();
    let page_count = pages.last().copied().unwrap_or(0) + 1;

    // Track band splits per page so we can split non-table items later
    let mut page_band_splits: HashMap<u32, Vec<(f32, f32)>> = HashMap::new();
    // Chart pages with two prose columns use the chart's vertical span as a
    // full-width separator and read each surrounding prose zone by column.
    let mut page_chart_prose_splits: HashMap<u32, f32> = HashMap::new();
    let mut page_chart_prose_orders: HashMap<u32, ChartProseOrder> = HashMap::new();

    for page in pages {
        let group = page_groups.get(&page).unwrap();
        let page_items: Vec<TextItem> = group.iter().map(|(_, item)| (*item).clone()).collect();

        // Chart-bar regions: bar charts drawn as filled rects read as cell
        // rects or aligned text and get gridded into phantom tables. Their
        // items are excluded from every table detector below and flow through
        // as plain text instead.
        let chart_regions: Vec<(f32, f32, f32, f32)> =
            page_chart_map.get(&page).cloned().unwrap_or_default();
        let in_chart = |item: &TextItem| item_is_in_chart_region(item, &chart_regions);
        let page_layout_items = items_outside_chart_regions(&page_items, &chart_regions);

        // Detect columns on chart-free text. Chart labels and values often fill
        // the prose gutter, hiding real columns and allowing body text to reach
        // heuristic table detection as one page-wide region.
        let detected_columns = {
            let cols = crate::extractor::detect_columns(&page_layout_items, page, false);
            cols.len() >= 2
        };
        if !chart_regions.is_empty() {
            log::debug!(
                "page {}: {} chart region(s) masked from table detection",
                page,
                chart_regions.len()
            );
        }

        // Repeated prose anchors provide a second, chart-scoped column signal.
        // It does not partition table detection: a narrow or partly spanning
        // gutter is too ambiguous for that. It can reject a body-font table
        // hypothesis and later order prose within chart-separated zones.
        // Multiple charts create several narrow vertical zones whose local
        // column structure needs stronger region-graph reasoning. Keep those
        // pages on the conservative full-page grouping path for now.
        let chart_prose_split = chart_regions.first().and_then(|&region| {
            if chart_regions.len() != 1 {
                return None;
            }
            chart_page_prose_column_split(&page_layout_items)
                .filter(|&split_x| chart_spans_prose_split(region, split_x))
        });
        let chart_prose_columns = chart_prose_split.is_some();

        // Check for side-by-side layout (e.g. two tables placed left and right)
        let mut bands = split_side_by_side(&page_items);
        // A rect table crossing a proposed split boundary means the "gutter"
        // is really the gap between ruled and borderless table columns —
        // splitting there cleaves the table in half. Veto the split.
        if !bands.is_empty() && rect_cluster_spans_band_boundary(&page_items, rects, page, &bands) {
            log::debug!(
                "page {}: side-by-side split vetoed by spanning rect cluster",
                page
            );
            bands.clear();
        }
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
        // Anchor-derived prose splits are lower-confidence than physical
        // gutters, so they do not partition table detection. They are applied
        // later only to vertical zones outside the chart. Physical bands can
        // still scope table detection without disabling this page-level prose
        // and positioned-block order.
        let chart_prose_order = chart_prose_split.and_then(|split_x| {
            chart_regions.first().copied().map(|region| {
                page_chart_prose_splits.insert(page, split_x);
                let order = ChartProseOrder::new(split_x, region);
                page_chart_prose_orders.insert(page, order);
                order
            })
        });

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

            // Pre-claim chart items: every detector below skips claimed
            // indices, and unclaimed-by-tables text flows out as plain lines.
            if !chart_regions.is_empty() {
                for (idx, item) in band_items.iter().enumerate() {
                    if in_chart(item) {
                        rect_claimed.insert(idx);
                    }
                }
            }

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
                    page_tables
                        .entry(page)
                        .or_default()
                        .push(positioned_table(table, chart_prose_order));
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
                page_tables
                    .entry(page)
                    .or_default()
                    .push(positioned_table(table, chart_prose_order));
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
                    page_tables
                        .entry(page)
                        .or_default()
                        .push(positioned_table(table, chart_prose_order));
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
                        page_tables
                            .entry(page)
                            .or_default()
                            .push(positioned_table(&table, chart_prose_order));
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
                    // Keep body-font detection available on chart pages: a real
                    // table can share the prose anchors. Reject only candidates
                    // whose cells prove they are parallel prose fragments.
                    let reject_parallel_prose = chart_prose_columns && !was_split;
                    let tables = detect_tables(subset_items, base_size, false);
                    for table in tables {
                        if reject_parallel_prose && is_parallel_prose_table(&table) {
                            log::debug!(
                                "page {}: rejected {}x{} parallel-prose table hypothesis",
                                page,
                                table.rows.len(),
                                table.columns.len()
                            );
                            continue;
                        }
                        for &idx in &table.item_indices {
                            if let Some(&band_idx) = index_map.get(idx) {
                                if let Some(&page_idx) = band_index_map.get(band_idx) {
                                    if let Some(&(global_idx, _)) = group.get(page_idx) {
                                        table_items.insert(global_idx);
                                    }
                                }
                            }
                        }
                        page_tables
                            .entry(page)
                            .or_default()
                            .push(positioned_table(&table, chart_prose_order));
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

            // 4. Column-based table detection for borderless tabular layouts.
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
                    page_tables
                        .entry(page)
                        .or_default()
                        .push(positioned_table(&table, chart_prose_order));
                }
            }
        }

        // 5. Thin-rect border synthesis: last resort for PDFs that draw table
        //    borders as thin filled rectangles (common in spreadsheet exports).
        //    Only runs when ALL other methods found nothing on this page.
        if !page_tables.contains_key(&page) {
            let page_rects: Vec<&crate::types::PdfRect> =
                rects.iter().filter(|r| r.page == page).collect();
            let mut synth_lines: Vec<crate::types::PdfLine> = Vec::new();
            for r in &page_rects {
                let (mut w, mut h) = (r.width, r.height);
                let (mut x, mut y) = (r.x, r.y);
                if w < 0.0 {
                    x += w;
                    w = -w;
                }
                if h < 0.0 {
                    y += h;
                    h = -h;
                }
                if h < 2.0 && w >= 10.0 {
                    let mid_y = y + h / 2.0;
                    synth_lines.push(crate::types::PdfLine {
                        x1: x,
                        y1: mid_y,
                        x2: x + w,
                        y2: mid_y,
                        page,
                    });
                } else if w < 2.0 && h >= 10.0 {
                    let mid_x = x + w / 2.0;
                    synth_lines.push(crate::types::PdfLine {
                        x1: mid_x,
                        y1: y,
                        x2: mid_x,
                        y2: y + h,
                        page,
                    });
                }
            }
            if synth_lines.len() >= 10 {
                // Chart text stays out of the thin-rect fallback too — a
                // chart's thin grid rules would otherwise re-grid it.
                let (page_text, page_text_map): (Vec<TextItem>, Vec<usize>) = text_items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.page == page && !in_chart(i))
                    .map(|(idx, i)| (i.clone(), idx))
                    .unzip();
                let line_tables = detect_tables_from_lines(&page_text, &synth_lines, page);
                for table in &line_tables {
                    for &idx in &table.item_indices {
                        if let Some(&global_idx) = page_text_map.get(idx) {
                            table_items.insert(global_idx);
                        }
                    }
                    page_tables
                        .entry(page)
                        .or_default()
                        .push(positioned_table(table, chart_prose_order));
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
            // Chart text stays out of the retry as well.
            let (chart_free, chart_free_map): (Vec<TextItem>, Vec<usize>) = band_items
                .iter()
                .enumerate()
                .filter(|(_, it)| !in_chart(it))
                .map(|(i, it)| (it.clone(), i))
                .unzip();
            // A chart-derived column signal must not disable body-font table
            // detection during the merged-band retry: this retry exists for
            // tables that only become visible after recombining false layout
            // bands. Keep the legacy skip on ordinary detected-column pages,
            // and reject chart-page prose candidates individually below.
            let skip_body_font =
                merged_retry_skips_body_font(detected_columns, !chart_regions.is_empty());
            let heuristic_tables = detect_tables(&chart_free, base_size, skip_body_font);
            for table in &heuristic_tables {
                if !chart_regions.is_empty() && is_parallel_prose_table(table) {
                    log::debug!(
                        "page {}: rejected {}x{} merged-band parallel-prose table hypothesis",
                        page,
                        table.rows.len(),
                        table.columns.len()
                    );
                    continue;
                }
                for &idx in &table.item_indices {
                    if let Some(&page_idx) = chart_free_map
                        .get(idx)
                        .and_then(|&band_idx| band_index_map.get(band_idx))
                    {
                        if let Some(&(global_idx, _)) = group.get(page_idx) {
                            table_items.insert(global_idx);
                        }
                    }
                }
                page_tables
                    .entry(page)
                    .or_default()
                    .push(positioned_table(table, chart_prose_order));
            }
        }
    }

    // Images are also removed before line grouping, so give them the same
    // logical chart-page position as tables before reinsertion.
    let mut page_images: HashMap<u32, Vec<PositionedMarkdown>> = HashMap::new();
    for img in &images {
        let img_name = img
            .text
            .strip_prefix("[Image: ")
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(&img.text);
        let img_md = format!("![Image: {}](image)\n", img_name);
        page_images
            .entry(img.page)
            .or_default()
            .push(PositionedMarkdown::new(
                img.y,
                img.x,
                img_md,
                page_chart_prose_orders.get(&img.page).copied(),
            ));
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
    let lines = if page_band_splits.is_empty() && page_chart_prose_splits.is_empty() {
        crate::extractor::group_into_lines_with_thresholds_and_regions(
            non_table_items,
            page_thresholds,
            &table_page_set,
            &page_chart_map,
            &page_image_regions,
        )
    } else {
        // Separate items into physical-band pages, chart/prose pages, and
        // ordinary pages. Chart/prose pages need a different reading order:
        // each chart is a full-width separator, while prose above and below
        // it reads down the left column and then down the right column.
        let mut split_page_items: HashMap<u32, Vec<TextItem>> = HashMap::new();
        let mut chart_prose_page_items: HashMap<u32, Vec<TextItem>> = HashMap::new();
        let mut unsplit_items: Vec<TextItem> = Vec::new();
        for item in non_table_items {
            if page_chart_prose_splits.contains_key(&item.page) {
                chart_prose_page_items
                    .entry(item.page)
                    .or_default()
                    .push(item);
            } else if page_band_splits.contains_key(&item.page) {
                split_page_items.entry(item.page).or_default().push(item);
            } else {
                unsplit_items.push(item);
            }
        }
        // Process unsplit pages normally
        let mut all_lines = crate::extractor::group_into_lines_with_thresholds_and_regions(
            unsplit_items,
            page_thresholds,
            &table_page_set,
            &page_chart_map,
            &page_image_regions,
        );
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
                    page_lines.extend(
                        crate::extractor::group_into_lines_with_thresholds_and_charts(
                            band_items,
                            page_thresholds,
                            &table_page_set,
                            &page_chart_map,
                        ),
                    );
                }
            }
            // Sort by Y descending (top to bottom) so left and right
            // band lines interleave in visual reading order.
            page_lines.sort_by(|a, b| b.y.total_cmp(&a.y));
            all_lines.extend(page_lines);
        }

        // Process chart/prose pages as alternating vertical zones. Within a
        // prose zone, group each column independently and append columns in
        // newspaper order. Within a chart zone, group the full width normally.
        let mut chart_prose_pages: Vec<u32> = chart_prose_page_items.keys().copied().collect();
        chart_prose_pages.sort();
        for page in chart_prose_pages {
            let mut remaining = chart_prose_page_items.remove(&page).unwrap();
            let split_x = page_chart_prose_splits[&page];
            let chart_regions = &page_chart_map[&page];
            let group_prose_zone = |zone_items: Vec<TextItem>| {
                let mut zone_lines = Vec::new();
                for right_column in [false, true] {
                    let column_items: Vec<TextItem> = zone_items
                        .iter()
                        .filter(|item| (item.x >= split_x) == right_column)
                        .cloned()
                        .collect();
                    if !column_items.is_empty() {
                        zone_lines.extend(
                            crate::extractor::group_into_lines_with_thresholds_and_charts(
                                column_items,
                                page_thresholds,
                                &table_page_set,
                                &page_chart_map,
                            ),
                        );
                    }
                }
                zone_lines
            };

            let mut chart_y_bands: Vec<(f32, f32)> = page_chart_map[&page]
                .iter()
                .map(|&(_, y0, _, y1)| (y0 - CHART_SEPARATOR_PAD, y1 + CHART_SEPARATOR_PAD))
                .collect();
            chart_y_bands.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut merged_chart_y_bands: Vec<(f32, f32)> = Vec::new();
            for (low, high) in chart_y_bands {
                if let Some(last) = merged_chart_y_bands.last_mut() {
                    if high >= last.0 {
                        last.0 = last.0.min(low);
                        last.1 = last.1.max(high);
                        continue;
                    }
                }
                merged_chart_y_bands.push((low, high));
            }

            for (low, high) in merged_chart_y_bands {
                let (above, at_or_below): (Vec<TextItem>, Vec<TextItem>) =
                    remaining.into_iter().partition(|item| {
                        item.y > high && !item_is_in_chart_region(item, chart_regions)
                    });
                all_lines.extend(group_prose_zone(above));

                let (chart_zone, below): (Vec<TextItem>, Vec<TextItem>) =
                    at_or_below.into_iter().partition(|item| {
                        item.y >= low || item_is_in_chart_region(item, chart_regions)
                    });
                all_lines.extend(
                    crate::extractor::group_into_lines_with_thresholds_and_charts(
                        chart_zone,
                        page_thresholds,
                        &table_page_set,
                        &page_chart_map,
                    ),
                );
                remaining = below;
            }
            all_lines.extend(group_prose_zone(remaining));
        }
        // The three processing paths above are accumulated separately. Restore
        // document page order while preserving each page's chosen line order.
        all_lines.sort_by_key(|line| line.page);
        all_lines
    };

    // Strip repeated headers/footers before conversion
    let lines = if options.strip_headers_footers {
        preprocess::strip_repeated_lines(lines, page_count)
    } else {
        lines
    };

    // Convert to markdown, inserting tables and images at appropriate positions
    let mut band_split_page_set: HashSet<u32> = page_band_splits.keys().copied().collect();
    band_split_page_set.extend(page_chart_prose_splits.keys().copied());
    to_markdown_from_lines_with_tables_and_images(
        lines,
        options,
        page_tables,
        page_images,
        &page_chart_map,
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
        assert_eq!(detect_header_level(24.0, 12.0, &tiers, false), Some(1));
        assert_eq!(detect_header_level(18.0, 12.0, &tiers, false), Some(2));
        assert_eq!(detect_header_level(15.0, 12.0, &tiers, false), Some(3));
        assert_eq!(detect_header_level(12.0, 12.0, &tiers, false), None);

        // Single tier: 15→H1 (ratio 1.25 ≥ 1.2), 14→None (ratio 1.17 < 1.2)
        let tiers = vec![15.0];
        assert_eq!(detect_header_level(15.0, 12.0, &tiers, false), Some(1));
        assert_eq!(detect_header_level(14.0, 12.0, &tiers, false), None);
        assert_eq!(detect_header_level(12.0, 12.0, &tiers, false), None);

        // No tiers (empty): falls back to ratio thresholds
        let tiers: Vec<f32> = vec![];
        assert_eq!(detect_header_level(24.0, 12.0, &tiers, false), Some(1));
        assert_eq!(detect_header_level(18.0, 12.0, &tiers, false), Some(2));
        assert_eq!(detect_header_level(15.0, 12.0, &tiers, false), Some(3));
        assert_eq!(detect_header_level(14.5, 12.0, &tiers, false), Some(4));
        assert_eq!(detect_header_level(14.0, 12.0, &tiers, false), None);
        assert_eq!(detect_header_level(12.0, 12.0, &tiers, false), None);

        // Body text excluded when tiers exist: 13pt (ratio 1.08) → None
        let tiers = vec![20.0];
        assert_eq!(detect_header_level(13.0, 12.0, &tiers, false), None);
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
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        }
    }

    fn make_item_w(x: f32, y: f32, width: f32, page: u32) -> TextItem {
        let mut it = make_item(x, y, page);
        it.width = width;
        it
    }

    #[test]
    fn early_layout_excludes_chart_items_before_column_detection() {
        let mut items = Vec::new();
        for row in 0..20 {
            let y = 100.0 + row as f32 * 14.0;
            items.push(make_item_w(90.0, y, 180.0, 1));
            items.push(make_item_w(340.0, y, 180.0, 1));
        }
        for col in 0..12 {
            items.push(make_item_w(
                100.0 + col as f32 * 35.0,
                620.0 - col as f32 * 4.0,
                25.0,
                1,
            ));
        }

        let chart_regions = vec![(90.0, 540.0, 530.0, 700.0)];
        let layout_items = items_outside_chart_regions(&items, &chart_regions);

        assert_eq!(layout_items.len(), 40);
        assert_eq!(
            crate::extractor::detect_columns(&layout_items, 1, false).len(),
            2
        );
    }

    #[test]
    fn chart_padding_claims_labels_but_not_adjacent_prose() {
        let regions = vec![(100.0, 100.0, 500.0, 300.0)];

        let mut label = make_item_w(220.0, 90.0, 60.0, 1);
        label.text = "January 2021".into();
        assert!(item_is_in_chart_region(&label, &regions));

        let mut long_label = make_item_w(210.0, 90.0, 180.0, 1);
        long_label.text = "Share of respondents by employment sector".into();
        assert!(item_is_in_chart_region(&long_label, &regions));

        let mut caption = make_item_w(120.0, 310.0, 350.0, 1);
        caption.text = "Figure 3. Results across every survey phase and sector".into();
        assert!(item_is_in_chart_region(&caption, &regions));

        let mut wide_short_prose = make_item_w(120.0, 90.0, 350.0, 1);
        wide_short_prose.text = "Results improved across all sectors".into();
        assert!(!item_is_in_chart_region(&wide_short_prose, &regions));

        let mut prose = make_item_w(120.0, 90.0, 350.0, 1);
        prose.text = "This paragraph continues below the chart into the next prose column".into();
        assert!(!item_is_in_chart_region(&prose, &regions));

        let mut bullet = make_item_w(340.0, 90.0, 5.0, 1);
        bullet.text = "•".into();
        assert!(!item_is_in_chart_region(&bullet, &regions));
    }

    #[test]
    fn chart_page_detects_two_short_prose_columns() {
        let mut items = Vec::new();
        for row in 0..12 {
            let y = 100.0 + row as f32 * 14.0;
            let mut left = make_item_w(90.0, y, 180.0, 1);
            left.text = "A complete prose line in the left column".into();
            items.push(left);
            let mut right = make_item_w(340.0, y, 180.0, 1);
            right.text = "A complete prose line in the right column".into();
            items.push(right);
        }

        let split = chart_page_prose_column_split(&items).unwrap();
        assert!(split > 200.0 && split < 300.0);
    }

    #[test]
    fn chart_page_rejects_non_overlapping_prose_anchors() {
        let mut items = Vec::new();
        for row in 0..8 {
            let mut left = make_item_w(90.0, 100.0 + row as f32 * 14.0, 180.0, 1);
            left.text = "A complete prose line in the upper column".into();
            items.push(left);
            let mut right = make_item_w(340.0, 300.0 + row as f32 * 14.0, 180.0, 1);
            right.text = "A complete prose line in the lower column".into();
            items.push(right);
        }

        assert_eq!(chart_page_prose_column_split(&items), None);
    }

    #[test]
    fn chart_separator_must_span_both_prose_columns() {
        let split_x = 280.0;

        assert!(chart_spans_prose_split(
            (100.0, 300.0, 500.0, 500.0),
            split_x
        ));
        assert!(!chart_spans_prose_split(
            (90.0, 300.0, 260.0, 500.0),
            split_x
        ));
        assert!(!chart_spans_prose_split(
            (300.0, 300.0, 520.0, 500.0),
            split_x
        ));
    }

    #[test]
    fn chart_prose_order_survives_physical_gutter_detection() {
        let mut items = Vec::new();
        for row in 0..20 {
            let y = 760.0 - row as f32 * 13.0;
            let mut left = make_item_w(90.0, y, 100.0, 1);
            left.text = format!("Left prose line {row} has several words");
            items.push(left);
            let mut right = make_item_w(340.0, y, 100.0, 1);
            right.text = format!("Right prose line {row} has several words");
            items.push(right);
        }
        assert!(
            !split_side_by_side(&items).is_empty(),
            "fixture must exercise the physical-gutter path"
        );

        // Connected, variably sized bars form a full-width chart separator.
        let mut rects = vec![PdfRect {
            x: 80.0,
            y: 280.0,
            width: 440.0,
            height: 150.0,
            page: 1,
        }];
        for (x, height) in [(120.0, 55.0), (210.0, 90.0), (300.0, 70.0), (390.0, 115.0)] {
            rects.push(PdfRect {
                x,
                y: 280.0,
                width: 45.0,
                height,
                page: 1,
            });
        }
        // A second connected family keeps the bar cluster above the detector's
        // minimum while retaining data-driven height variation.
        for (x, y, height) in [
            (120.0, 332.0, 45.0),
            (210.0, 367.0, 55.0),
            (300.0, 347.0, 35.0),
            (390.0, 392.0, 30.0),
        ] {
            rects.push(PdfRect {
                x,
                y,
                width: 45.0,
                height,
                page: 1,
            });
        }

        // Detection/input order is deliberately right before left. Only the
        // chart-scoped logical order can put these blocks into prose-column
        // order after the physical bands have already scoped table detection.
        let mut right_image = make_item(340.0, 650.0, 1);
        right_image.text = "[Image: RightFigure]".into();
        right_image.item_type = crate::types::ItemType::Image;
        items.push(right_image);
        let mut left_image = make_item(90.0, 600.0, 1);
        left_image.text = "[Image: LeftFigure]".into();
        left_image.item_type = crate::types::ItemType::Image;
        items.push(left_image);

        let options = MarkdownOptions {
            include_images: true,
            ..MarkdownOptions::default()
        };
        let markdown = to_markdown_from_items_with_rects(items, options, &rects);
        let left_last = markdown.find("Left prose line 19").unwrap();
        let right_first = markdown.find("Right prose line 0").unwrap();
        assert!(
            left_last < right_first,
            "chart prose must read down the left column before the right even when table detection found physical bands:\n{markdown}"
        );
        let left_image = markdown.find("LeftFigure").unwrap();
        let right_image = markdown.find("RightFigure").unwrap();
        assert!(
            left_image < right_image,
            "chart blocks must retain column order when physical bands are present:\n{markdown}"
        );
    }

    #[test]
    fn parallel_prose_table_is_rejected_but_real_table_is_preserved() {
        assert!(merged_retry_skips_body_font(true, false));
        assert!(!merged_retry_skips_body_font(true, true));
        assert!(!merged_retry_skips_body_font(false, true));

        assert!(looks_like_numbered_section_heading(
            "9.5. Adapting to the New Normal: Changing Business Models"
        ));
        assert!(!looks_like_numbered_section_heading(
            "2024 revenue by business segment"
        ));

        let prose = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![320.0, 300.0, 280.0, 260.0],
            vec![
                vec![
                    "This section investigates the impact of public health measures".into(),
                    "course of the research period and the impacts continued".into(),
                ],
                vec![
                    "measures on business operations during the national lockdown".into(),
                    "".into(),
                ],
                vec![
                    "asked about their expectations for business recovery".into(),
                    "felt by firms working under reduced operating conditions".into(),
                ],
                vec![
                    "respondents described their expectations for business recovery".into(),
                    "while many other businesses remained temporarily closed".into(),
                ],
            ],
            (0..7).collect(),
        );
        assert!(is_parallel_prose_table(&prose));

        let data = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![300.0, 280.0, 260.0],
            vec![
                vec!["Sector".into(), "Revenue".into()],
                vec!["Tourism".into(), "$1,240".into()],
                vec!["Agriculture".into(), "$980".into()],
            ],
            (0..6).collect(),
        );
        assert!(!is_parallel_prose_table(&data));

        let headed_text_table = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![320.0, 300.0, 280.0],
            vec![
                vec!["Program".into(), "Description".into()],
                vec![
                    "Business recovery and continuity planning support".into(),
                    "Provides tailored guidance to firms affected by disruptions".into(),
                ],
                vec![
                    "Regional market access and supplier development".into(),
                    "Connects eligible producers with new distribution partners".into(),
                ],
            ],
            (0..6).collect(),
        );
        assert!(!is_parallel_prose_table(&headed_text_table));

        let sparse_first_row_then_compact_body = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![340.0, 320.0, 300.0, 280.0],
            vec![
                vec![
                    "This introductory prose fragment occupies only the left column".into(),
                    "".into(),
                ],
                vec!["short".into(), "row".into()],
                vec![
                    "continuation text remains aligned with the left prose anchor".into(),
                    "parallel text continues down the right prose column".into(),
                ],
                vec![
                    "another wrapped fragment follows in the left column".into(),
                    "while its neighboring prose fragment continues on the right".into(),
                ],
            ],
            (0..8).collect(),
        );
        assert!(is_parallel_prose_table(&sparse_first_row_then_compact_body));

        let headerless_description_table = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![320.0, 300.0, 280.0],
            vec![
                vec![
                    "community preparedness and emergency response planning".into(),
                    "provides detailed support for local continuity programs".into(),
                ],
                vec![
                    "regional supplier and market development assistance".into(),
                    "connects eligible producers with new distribution partners".into(),
                ],
                vec![
                    "financial continuity and business recovery program".into(),
                    "offers tailored guidance to firms affected by disruptions".into(),
                ],
            ],
            (0..6).collect(),
        );
        assert!(!is_parallel_prose_table(&headerless_description_table));

        let sparse_rowspanning_description_table = crate::tables::Table::new(
            vec![90.0, 340.0],
            vec![360.0, 340.0, 320.0, 300.0, 280.0, 260.0],
            vec![
                vec![
                    "community preparedness and emergency response planning support".into(),
                    "provides detailed support for local continuity program delivery".into(),
                ],
                vec![
                    "through coordinated training and regional response exercises".into(),
                    "".into(),
                ],
                vec![
                    "".into(),
                    "with technical assistance for participating local organizations".into(),
                ],
                vec![
                    "regional supplier and market development assistance program".into(),
                    "connects eligible producers with new distribution opportunities".into(),
                ],
                vec![
                    "through procurement guidance and tailored readiness workshops".into(),
                    "".into(),
                ],
                vec![
                    "".into(),
                    "while expanding access to qualified commercial partners".into(),
                ],
            ],
            (0..8).collect(),
        );
        assert!(!is_parallel_prose_table(
            &sparse_rowspanning_description_table
        ));
    }

    /// 4-row × 2-col ruled grid from x=100..300 (rows every 20pt from y=600).
    fn ruled_cluster_rects() -> Vec<PdfRect> {
        let mut rects = Vec::new();
        for row in 0..4 {
            for col in 0..2 {
                rects.push(PdfRect {
                    x: 100.0 + col as f32 * 100.0,
                    y: 600.0 + row as f32 * 20.0,
                    width: 100.0,
                    height: 20.0,
                    page: 1,
                });
            }
        }
        rects
    }

    #[test]
    fn band_veto_cluster_ruled_across_boundary() {
        // Rects on both sides of the boundary and cell text on both sides,
        // row-aligned → the split cuts straight through a drawn table.
        let mut rects = ruled_cluster_rects();
        for r in &mut rects {
            r.width = 150.0; // right column now spans 250..400, past b=320
        }
        let mut items = Vec::new();
        for row in 0..4 {
            let y = 610.0 + row as f32 * 20.0;
            items.push(make_item_w(110.0, y, 80.0, 1)); // left cells
            items.push(make_item_w(330.0, y, 30.0, 1)); // right cells past b
        }
        assert!(rect_cluster_spans_band_boundary(
            &items,
            &rects,
            1,
            &[(90.0, 320.0), (320.0, 500.0)]
        ));
    }

    #[test]
    fn band_veto_ignores_spanning_figure() {
        // A figure's rects span the boundary at the top of the page, but the
        // far side is dominated by column prose below it → keep the split.
        let mut rects = ruled_cluster_rects(); // y 600..680
        for r in &mut rects {
            r.width = 150.0; // spans past b=320
        }
        let mut items = Vec::new();
        // A few figure labels inside the cluster, aligned rows.
        for row in 0..4 {
            let y = 610.0 + row as f32 * 20.0;
            items.push(make_item_w(110.0, y, 40.0, 1));
            items.push(make_item_w(330.0, y, 20.0, 1));
        }
        // Dense prose column far below the figure (outside cluster y-range).
        let mut y = 100.0;
        while y < 560.0 {
            items.push(make_item_w(330.0, y, 140.0, 1));
            y += 12.0;
        }
        assert!(!rect_cluster_spans_band_boundary(
            &items,
            &rects,
            1,
            &[(90.0, 320.0), (320.0, 500.0)]
        ));
    }

    #[test]
    fn band_veto_borderless_columns_continue_rows() {
        // Rects end at x=300 (just short of b=320); cell-like text at x=340
        // aligns with every grid row → the "gutter" is inside the table.
        let rects = ruled_cluster_rects();
        let mut items = Vec::new();
        for row in 0..4 {
            let y = 610.0 + row as f32 * 20.0;
            items.push(make_item_w(110.0, y, 80.0, 1)); // label cells
            items.push(make_item_w(340.0, y, 30.0, 1)); // borderless column
        }
        assert!(rect_cluster_spans_band_boundary(
            &items,
            &rects,
            1,
            &[(90.0, 320.0), (320.0, 500.0)]
        ));
    }

    #[test]
    fn band_veto_ignores_prose_column() {
        // Dense prose right of the boundary: wide lines, three per grid row,
        // mostly not row-aligned → keep the side-by-side split.
        let rects = ruled_cluster_rects();
        let mut items = Vec::new();
        for row in 0..4 {
            items.push(make_item_w(110.0, 610.0 + row as f32 * 20.0, 80.0, 1));
        }
        let mut y = 602.0;
        while y < 680.0 {
            items.push(make_item_w(340.0, y, 200.0, 1)); // full-width prose lines
            y += 7.0;
        }
        assert!(!rect_cluster_spans_band_boundary(
            &items,
            &rects,
            1,
            &[(90.0, 320.0), (320.0, 500.0)]
        ));
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
