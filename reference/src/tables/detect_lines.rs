//! Line-based table detection.
//!
//! Detects tables from PDF path operators (`m`/`l`/`S`) that draw ruled
//! gridlines.  Many IRS forms and government PDFs use these instead of
//! `re` (rectangle) operators.

use std::collections::HashSet;

use crate::tables::Table;
use crate::types::{PdfLine, TextItem};

use super::detect_rects::{assign_items_to_grid, snap_edges};

const RULE_Y_TOLERANCE: f32 = 2.0;
const RULE_JOIN_GAP: f32 = 6.0;
const RULE_SPAN_TOLERANCE: f32 = 8.0;
const TEXT_ROW_TOLERANCE: f32 = 2.5;

type HorizontalRule = (f32, f32, f32); // (y, x_min, x_max)
type VerticalRule = (f32, f32, f32); // (x, y_min, y_max)
type AnchoredRow<'a> = (f32, Vec<(usize, &'a TextItem)>);

#[derive(Debug)]
struct TextAnchorTable {
    table: Table,
    x_left: f32,
    x_right: f32,
    y_bottom: f32,
    y_top: f32,
}

/// Merge touching path segments into logical horizontal rules.
///
/// Forms and segmented-cell tables often stroke one segment per cell at the
/// same y coordinate. Treating those as unrelated rules manufactures column
/// edges from path endpoints; joining them first exposes the actual table
/// band while text anchors recover the columns.
fn merge_horizontal_segments(horizontals: &[HorizontalRule]) -> Vec<HorizontalRule> {
    let mut sorted = horizontals.to_vec();
    sorted.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.total_cmp(&right.1))
    });

    let mut y_groups: Vec<Vec<HorizontalRule>> = Vec::new();
    for rule in sorted {
        if y_groups
            .last()
            .and_then(|group| group.first())
            .is_some_and(|first| (first.0 - rule.0).abs() <= RULE_Y_TOLERANCE)
        {
            y_groups.last_mut().expect("checked above").push(rule);
        } else {
            y_groups.push(vec![rule]);
        }
    }

    let mut merged = Vec::new();
    for mut group in y_groups {
        group.sort_by(|left, right| left.1.total_cmp(&right.1));
        let y = group.iter().map(|rule| rule.0).sum::<f32>() / group.len() as f32;
        let mut current = (y, group[0].1, group[0].2);
        for rule in group.into_iter().skip(1) {
            if rule.1 <= current.2 + RULE_JOIN_GAP {
                current.2 = current.2.max(rule.2);
            } else {
                merged.push(current);
                current = (y, rule.1, rule.2);
            }
        }
        merged.push(current);
    }
    merged.sort_by(|left, right| right.0.total_cmp(&left.0));
    merged
}

fn group_rules_by_span(rules: &[HorizontalRule]) -> Vec<Vec<HorizontalRule>> {
    let mut groups: Vec<Vec<HorizontalRule>> = Vec::new();
    for &rule in rules {
        let best_group = groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                let first = group[0];
                let endpoint_error = (first.1 - rule.1).abs() + (first.2 - rule.2).abs();
                ((first.1 - rule.1).abs() <= RULE_SPAN_TOLERANCE
                    && (first.2 - rule.2).abs() <= RULE_SPAN_TOLERANCE)
                    .then_some((index, endpoint_error))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index);
        if let Some(index) = best_group {
            groups[index].push(rule);
        } else {
            groups.push(vec![rule]);
        }
    }
    for group in &mut groups {
        group.sort_by(|left, right| right.0.total_cmp(&left.0));
    }
    groups
}

fn numbered_table_caption(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("table ") else {
        return false;
    };
    rest.split_whitespace().next().is_some_and(|token| {
        token
            .trim_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .is_ok()
    })
}

/// Split equal-width rules into independent vertical runs.
///
/// Consecutive booktabs tables often share identical x endpoints. A numbered
/// caption is an explicit separator; a large mostly-empty gap is the fallback
/// for captionless tables. Requiring at least two rules on both sides and a
/// proportionally large empty interval protects long tables whose top/middle/
/// bottom rules surround many regularly spaced text rows.
fn split_independent_rule_runs(
    rules: &[HorizontalRule],
    items: &[TextItem],
    page: u32,
) -> Vec<Vec<HorizontalRule>> {
    let Some(&first) = rules.first() else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    let mut current = vec![first];
    for (index, pair) in rules.windows(2).enumerate() {
        let y_min = pair[0].0.min(pair[1].0);
        let y_max = pair[0].0.max(pair[1].0);
        let has_caption = items.iter().any(|item| {
            item.page == page
                && item.y > y_min
                && item.y < y_max
                && numbered_table_caption(&item.text)
        });
        let can_form_two_runs = index + 1 >= 2 && rules.len() - (index + 1) >= 2;
        let rule_gap = y_max - y_min;
        let has_empty_separator = can_form_two_runs && rule_gap >= 36.0 && {
            let x_min = pair[0].1.min(pair[1].1) - RULE_JOIN_GAP;
            let x_max = pair[0].2.max(pair[1].2) + RULE_JOIN_GAP;
            let mut occupied_y: Vec<f32> = items
                .iter()
                .filter(|item| {
                    item.page == page
                        && crate::extractor::is_text_layout_item(item)
                        && !item.text.trim().is_empty()
                        && item.y > y_min
                        && item.y < y_max
                        && item.x + item.width.max(0.0) >= x_min
                        && item.x <= x_max
                })
                .map(|item| item.y)
                .collect();
            occupied_y.push(y_min);
            occupied_y.push(y_max);
            occupied_y.sort_by(|left, right| left.total_cmp(right));
            occupied_y.dedup_by(|left, right| (*left - *right).abs() <= TEXT_ROW_TOLERANCE);
            let largest_empty_gap = occupied_y
                .windows(2)
                .map(|window| window[1] - window[0])
                .fold(0.0_f32, f32::max);
            largest_empty_gap >= 36.0_f32.max(rule_gap * 0.45)
        };
        if has_caption || has_empty_separator {
            groups.push(current);
            current = vec![pair[1]];
        } else {
            current.push(pair[1]);
        }
    }
    groups.push(current);
    groups
}

fn collect_anchored_rows<'a>(
    items: &'a [TextItem],
    rules: &[HorizontalRule],
    page: u32,
) -> Vec<AnchoredRow<'a>> {
    let y_top = rules
        .iter()
        .map(|rule| rule.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_bottom = rules
        .iter()
        .map(|rule| rule.0)
        .fold(f32::INFINITY, f32::min);
    let x_min = rules
        .iter()
        .map(|rule| rule.1)
        .fold(f32::INFINITY, f32::min);
    let x_max = rules
        .iter()
        .map(|rule| rule.2)
        .fold(f32::NEG_INFINITY, f32::max);

    let mut selected: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.page == page
                && crate::extractor::is_text_layout_item(item)
                && !item.text.trim().is_empty()
                && item.y >= y_bottom - RULE_Y_TOLERANCE
                && item.y <= y_top + RULE_Y_TOLERANCE
                && item.x + item.width.max(0.0) >= x_min - RULE_JOIN_GAP
                && item.x <= x_max + RULE_JOIN_GAP
        })
        .collect();
    selected.sort_by(|left, right| {
        right
            .1
            .y
            .total_cmp(&left.1.y)
            .then_with(|| left.1.x.total_cmp(&right.1.x))
    });

    let mut rows: Vec<AnchoredRow<'a>> = Vec::new();
    for (index, item) in selected {
        if let Some((row_y, row_items)) = rows.last_mut() {
            if (*row_y - item.y).abs() <= TEXT_ROW_TOLERANCE {
                row_items.push((index, item));
                continue;
            }
        }
        rows.push((item.y, vec![(index, item)]));
    }
    for (_, row) in &mut rows {
        row.sort_by(|left, right| left.1.x.total_cmp(&right.1.x));
    }
    rows
}

fn rules_are_uniform_grid(rules: &[HorizontalRule]) -> bool {
    if rules.len() < 5 {
        return false;
    }
    let spacings: Vec<f32> = rules
        .windows(2)
        .map(|pair| (pair[0].0 - pair[1].0).abs())
        .collect();
    let mean = spacings.iter().sum::<f32>() / spacings.len() as f32;
    if mean <= 0.1 {
        return false;
    }
    let variance = spacings
        .iter()
        .map(|spacing| (spacing - mean).powi(2))
        .sum::<f32>()
        / spacings.len() as f32;
    variance.sqrt() / mean < 0.02
}

fn build_stacked_token_table(rows: &[AnchoredRow<'_>], rules: &[HorizontalRule]) -> Option<Table> {
    if rules.len() != 3 || rows.len() < 5 || rows.iter().any(|(_, row)| row.len() != 1) {
        return None;
    }
    let anchor_x = rows[0].1[0].1.x;
    if rows
        .iter()
        .any(|(_, row)| (row[0].1.x - anchor_x).abs() > RULE_JOIN_GAP)
    {
        return None;
    }
    let body = &rows[1..];
    let token_rows = body
        .iter()
        .filter(|(_, row)| {
            let text = row[0].1.text.trim();
            text.split_whitespace().count() == 1 && text.chars().any(|c| c == '_' || c == ':')
        })
        .count();
    if token_rows * 4 < body.len() * 3 {
        return None;
    }

    let header = rows[0].1[0].1.text.trim().to_string();
    let value = body
        .iter()
        .map(|(_, row)| row[0].1.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let mut item_indices: Vec<usize> = rows
        .iter()
        .flat_map(|(_, row)| row.iter().map(|(index, _)| *index))
        .collect();
    item_indices.sort_unstable();
    item_indices.dedup();

    let x_min = rules
        .iter()
        .map(|rule| rule.1)
        .fold(f32::INFINITY, f32::min);
    let x_max = rules
        .iter()
        .map(|rule| rule.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let split = x_min + (x_max - x_min) * 0.35;
    Some(Table::new(
        vec![x_min, split, x_max],
        vec![rows[0].0],
        vec![vec![header, value]],
        item_indices,
    ))
}

fn build_text_anchor_table(
    items: &[TextItem],
    rules: &[HorizontalRule],
    page: u32,
) -> Option<Table> {
    if rules.len() < 2 || rules_are_uniform_grid(rules) {
        return None;
    }
    let rows = collect_anchored_rows(items, rules, page);
    if rows.len() < 2 {
        return None;
    }

    let mut anchors: Vec<f32> = Vec::new();
    for (_, item) in &rows[0].1 {
        if anchors
            .last()
            .is_none_or(|last| (item.x - *last).abs() > RULE_JOIN_GAP)
        {
            anchors.push(item.x);
        }
    }
    if anchors.len() == 1 {
        return build_stacked_token_table(&rows, rules);
    }
    if !(2..=25).contains(&anchors.len()) || anchors.last()? - anchors[0] < 30.0 {
        return None;
    }
    let numeric_header_cells = rows[0]
        .1
        .iter()
        .filter(|(_, item)| {
            let text = item.text.trim();
            text.chars().any(|c| c.is_ascii_digit()) && !text.chars().any(char::is_alphabetic)
        })
        .count();
    if rows[0]
        .1
        .iter()
        .all(|(_, item)| !item.text.chars().any(char::is_alphabetic))
        || numeric_header_cells * 2 > rows[0].1.len()
        || rows[1..]
            .iter()
            .flat_map(|(_, row)| row)
            .any(|(_, item)| item.x < anchors[0] - RULE_JOIN_GAP)
    {
        // Header anchors must describe every column. A numeric data row is
        // weak evidence for a header, and a body stub to the left of the
        // first header anchor proves that the inferred grid omitted a column.
        // Let the legacy line/segment detector handle these partial views.
        return None;
    }
    if rules.len() == 2 {
        // A bounded response form can have only top/bottom rules: the header
        // names both columns, while each prompt row fills the leading column
        // and deliberately leaves the response column blank.
        let response_form = rows.len() >= 5
            && anchors.len() <= 4
            && rows[1..].iter().all(|(_, row)| {
                !row.is_empty()
                    && row.len() < anchors.len()
                    && row.iter().all(|(_, item)| {
                        item.text.split_whitespace().count() <= 4
                            && (item.x - anchors[0]).abs() <= RULE_JOIN_GAP
                    })
            });
        if !response_form {
            return None;
        }
    } else if anchors.len() == 2 && (rules.len() < 5 || rows.len() > rules.len() + 2) {
        // Two text columns bracketed by a few decorative rules are
        // indistinguishable from a two-column prose layout using geometry
        // alone. Keep the high-confidence cases: response forms (above), the
        // stacked-token special case, and densely ruled forms where rule and
        // row counts corroborate one another. Wider booktabs tables have much
        // stronger anchor evidence and do not need this restriction.
        return None;
    }
    if anchors.len() > 2 && rules.len() > 3 {
        // Four or more full-width rules describe row structure, not a sparse
        // booktabs band. Other table detectors can combine that evidence with
        // rectangles, segments, or whitespace; first-row anchors alone may
        // start below a real header and preempt a better hypothesis.
        return None;
    }

    let x_min = rules
        .iter()
        .map(|rule| rule.1)
        .fold(f32::INFINITY, f32::min)
        .min(anchors[0]);
    let x_max = rules
        .iter()
        .map(|rule| rule.2)
        .fold(f32::NEG_INFINITY, f32::max)
        .max(*anchors.last()?);
    if x_max - x_min < 50.0 {
        return None;
    }
    let mut columns = vec![x_min];
    columns.extend(anchors.windows(2).map(|pair| (pair[0] + pair[1]) / 2.0));
    columns.push(x_max);

    let mut cells = vec![vec![String::new(); anchors.len()]; rows.len()];
    let mut item_indices = Vec::new();
    let mut wide_items = 0usize;
    let mut measured_items = 0usize;
    for (row_index, (_, row)) in rows.iter().enumerate() {
        for (item_index, item) in row {
            let column = anchors
                .iter()
                .enumerate()
                .min_by(|left, right| (left.1 - item.x).abs().total_cmp(&(right.1 - item.x).abs()))
                .map(|(index, _)| index)?;
            let column_width = columns[column + 1] - columns[column];
            if column_width > 0.0 {
                measured_items += 1;
                if item.width.max(0.0) > column_width * 0.72 {
                    wide_items += 1;
                }
            }
            if !cells[row_index][column].is_empty() {
                cells[row_index][column].push(' ');
            }
            cells[row_index][column].push_str(item.text.trim());
            item_indices.push(*item_index);
        }
    }
    item_indices.sort_unstable();
    item_indices.dedup();

    let occupied_rows = cells
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .count();
    let occupied_columns = (0..anchors.len())
        .filter(|&column| cells.iter().any(|row| !row[column].is_empty()))
        .count();
    if occupied_rows < 2 || occupied_columns < 2 {
        return None;
    }

    // Sparse rules around a full multi-column text region can expose dozens
    // of paragraph baselines whose starts repeat at the column margins. Reject
    // sustained prose, not height alone: long tables made of short labels and
    // values remain valid regardless of their row count.
    let body_cells: Vec<&str> = cells
        .iter()
        .skip(1)
        .flatten()
        .map(String::as_str)
        .filter(|cell| !cell.is_empty())
        .collect();
    let prose_like_body_cells = body_cells
        .iter()
        .filter(|cell| {
            let alpha_words = cell
                .split_whitespace()
                .filter(|word| word.chars().any(char::is_alphabetic))
                .count();
            alpha_words >= 3 && cell.chars().count() >= 12
        })
        .count();
    let sustained_sparse_prose = rules.len() <= 4
        && rows.len() > rules.len() * 2 + 2
        && !body_cells.is_empty()
        && prose_like_body_cells * 2 >= body_cells.len();
    if sustained_sparse_prose
        || (anchors.len() >= 3
            && rows.len() >= 4
            && measured_items > 0
            && wide_items * 3 >= measured_items)
    {
        log::trace!(
            "detect_lines p{}: rejected unbounded text-anchor candidate ({} rows, {} wide of {} items)",
            page,
            rows.len(),
            wide_items,
            measured_items
        );
        return None;
    }

    // A handful of long decorative rules can bracket an entire multi-column
    // prose region. Its first baseline then looks like a header and the text
    // anchors look like columns, but assigning the intervening paragraphs to
    // those anchors produces very large "cells". Real ruled tables can wrap
    // labels, so use a deliberately loose guard: reject only an extreme cell,
    // or a sustained concentration of paragraph-sized cells.
    let nonempty_cells: Vec<&str> = cells
        .iter()
        .flatten()
        .map(String::as_str)
        .filter(|cell| !cell.is_empty())
        .collect();
    let long_cells = nonempty_cells
        .iter()
        .filter(|cell| cell.chars().count() > 100)
        .count();
    if nonempty_cells.iter().any(|cell| cell.chars().count() > 240)
        || (long_cells >= 2 && long_cells * 5 >= nonempty_cells.len())
    {
        log::trace!(
            "detect_lines p{}: rejected prose-like text-anchor candidate ({} long of {} cells)",
            page,
            long_cells,
            nonempty_cells.len()
        );
        return None;
    }

    Some(Table::new(
        columns,
        rows.iter().map(|(y, _)| *y).collect(),
        cells,
        item_indices,
    ))
}

fn detect_text_anchor_rule_tables(
    items: &[TextItem],
    horizontals: &[HorizontalRule],
    verticals: &[VerticalRule],
    path_lines: &[PdfLine],
    page: u32,
) -> Vec<TextAnchorTable> {
    let logical_rules = merge_horizontal_segments(horizontals);
    log::trace!(
        "detect_lines p{}: logical horizontal rules: {:?}",
        page,
        logical_rules
    );
    let mut tables = Vec::new();
    for span_group in group_rules_by_span(&logical_rules) {
        for rules in split_independent_rule_runs(&span_group, items, page) {
            let y_top = rules
                .iter()
                .map(|rule| rule.0)
                .fold(f32::NEG_INFINITY, f32::max);
            let y_bottom = rules
                .iter()
                .map(|rule| rule.0)
                .fold(f32::INFINITY, f32::min);
            let x_left = rules
                .iter()
                .map(|rule| rule.1)
                .fold(f32::INFINITY, f32::min);
            let x_right = rules
                .iter()
                .map(|rule| rule.2)
                .fold(f32::NEG_INFINITY, f32::max);
            let dense_path_region = path_lines
                .iter()
                .filter(|line| {
                    line.page == page
                        && line.x1.max(line.x2) >= x_left - RULE_JOIN_GAP
                        && line.x1.min(line.x2) <= x_right + RULE_JOIN_GAP
                        && line.y1.max(line.y2) >= y_bottom - RULE_Y_TOLERANCE
                        && line.y1.min(line.y2) <= y_top + RULE_Y_TOLERANCE
                })
                .take(200)
                .count()
                >= 200;
            if dense_path_region {
                // Text-anchor inference is a sparse-geometry fallback. Dense
                // line art inside the same band is a chart/schematic signal;
                // physical grid and chart detectors should own that region.
                continue;
            }
            let band_verticals: Vec<VerticalRule> = verticals
                .iter()
                .filter(|&&(x, y_min, y_max)| {
                    x >= x_left - RULE_JOIN_GAP
                        && x <= x_right + RULE_JOIN_GAP
                        && y_max >= y_bottom - RULE_Y_TOLERANCE
                        && y_min <= y_top + RULE_Y_TOLERANCE
                })
                .copied()
                .collect();
            let spanning_xs: Vec<f32> = band_verticals
                .iter()
                .filter(|&&(_, y_min, y_max)| {
                    y_min <= y_bottom + RULE_Y_TOLERANCE && y_max >= y_top - RULE_Y_TOLERANCE
                })
                .map(|rule| rule.0)
                .collect();
            let band_xs: Vec<f32> = band_verticals.iter().map(|rule| rule.0).collect();
            // Two coordinates can be the outer borders of an otherwise
            // borderless table. A physical multi-column grid needs at least
            // one interior divider spanning the candidate as well. Conversely,
            // many short coordinates are strong diagram/chart evidence even
            // though no single mark proves a cell grid.
            if snap_edges(&spanning_xs, 3.0).len() >= 3 || snap_edges(&band_xs, 3.0).len() >= 6 {
                continue;
            }
            if let Some(table) = build_text_anchor_table(items, &rules, page) {
                log::debug!(
                    "detect_lines p{}: accepted text-anchor rule table {}x{} from {} rules",
                    page,
                    table.cells.len(),
                    table.cells.first().map_or(0, Vec::len),
                    rules.len()
                );
                tables.push(TextAnchorTable {
                    table,
                    x_left,
                    x_right,
                    y_bottom,
                    y_top,
                });
            }
        }
    }
    tables.sort_by(|left, right| {
        right
            .table
            .rows
            .first()
            .copied()
            .unwrap_or_default()
            .total_cmp(&left.table.rows.first().copied().unwrap_or_default())
    });
    tables
}

fn line_overlaps_text_anchor_band(line: &PdfLine, table: &TextAnchorTable) -> bool {
    let line_x_min = line.x1.min(line.x2);
    let line_x_max = line.x1.max(line.x2);
    let line_y_min = line.y1.min(line.y2);
    let line_y_max = line.y1.max(line.y2);
    line_x_max >= table.x_left - RULE_JOIN_GAP
        && line_x_min <= table.x_right + RULE_JOIN_GAP
        && line_y_max >= table.y_bottom - RULE_Y_TOLERANCE
        && line_y_min <= table.y_top + RULE_Y_TOLERANCE
}

fn combine_non_overlapping_tables(mut primary: Vec<Table>, secondary: Vec<Table>) -> Vec<Table> {
    let claimed_items: HashSet<usize> = primary
        .iter()
        .flat_map(|table| table.item_indices.iter().copied())
        .collect();
    primary.extend(secondary.into_iter().filter(|table| {
        table
            .item_indices
            .iter()
            .all(|index| !claimed_items.contains(index))
    }));
    primary.sort_by(|left, right| {
        right
            .rows
            .first()
            .copied()
            .unwrap_or_default()
            .total_cmp(&left.rows.first().copied().unwrap_or_default())
    });
    primary
}

fn logical_row_anchors(row: &[(usize, &TextItem)]) -> Vec<f32> {
    let mut spans: Vec<(f32, f32)> = row
        .iter()
        .map(|(_, item)| (item.x, item.x + item.width.max(0.0)))
        .collect();
    spans.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut anchors = Vec::new();
    let mut current_right = f32::NEG_INFINITY;
    for (x, right) in spans {
        if anchors.is_empty() || x > current_right + RULE_JOIN_GAP {
            anchors.push(x);
            current_right = right;
        } else {
            current_right = current_right.max(right);
        }
    }
    anchors
}

fn nearest_anchor_column(item: &TextItem, anchors: &[f32]) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .min_by(|left, right| (left.1 - item.x).abs().total_cmp(&(right.1 - item.x).abs()))
        .map(|(index, _)| index)
}

fn matched_anchor_column_count(row: &[(usize, &TextItem)], anchors: &[f32]) -> usize {
    row.iter()
        .filter_map(|(_, item)| nearest_anchor_column(item, anchors))
        .collect::<HashSet<_>>()
        .len()
}

/// Build a table hypothesis from the densest text row inside a ruled band.
///
/// Multi-level booktabs headers often put one or two spanning labels on the
/// first baselines, while a later data row exposes every real column. Using
/// only the first row therefore collapses the table before the legacy segment
/// detector gets a chance to compare structures. This deliberately strict
/// alternative requires repeated dense rows and numeric body evidence.
fn build_dense_row_anchor_table(
    items: &[TextItem],
    horizontals: &[HorizontalRule],
    verticals: &[VerticalRule],
    page: u32,
) -> Option<Table> {
    let rules = merge_horizontal_segments(horizontals);
    if rules.len() < 4 || rules_are_uniform_grid(&rules) {
        return None;
    }

    // A page with several stacked charts can contribute one dense numeric
    // row per chart. Do not combine separated rule bands into a synthetic
    // page-wide table; a valid competing hypothesis must be one contiguous
    // ruled region.
    let mut distinct_rule_ys: Vec<f32> = rules.iter().map(|rule| rule.0).collect();
    distinct_rule_ys.sort_by(|left, right| right.total_cmp(left));
    distinct_rule_ys.dedup_by(|left, right| (*left - *right).abs() <= RULE_Y_TOLERANCE);
    let mut rule_gaps: Vec<f32> = distinct_rule_ys
        .windows(2)
        .map(|pair| pair[0] - pair[1])
        .collect();
    rule_gaps.sort_by(f32::total_cmp);
    if let Some(&median_gap) = rule_gaps.get(rule_gaps.len() / 2) {
        if median_gap > 0.0
            && rule_gaps
                .last()
                .is_some_and(|largest_gap| *largest_gap > median_gap * 2.5)
        {
            return None;
        }
    }
    let has_uniform_run = distinct_rule_ys.windows(4).any(|window| {
        let spacings = [
            window[0] - window[1],
            window[1] - window[2],
            window[2] - window[3],
        ];
        let mean = spacings.iter().sum::<f32>() / spacings.len() as f32;
        if mean <= 0.1 {
            return false;
        }
        let variance = spacings
            .iter()
            .map(|spacing| (spacing - mean).powi(2))
            .sum::<f32>()
            / spacings.len() as f32;
        variance.sqrt() / mean < 0.02
    });
    if has_uniform_run {
        return None;
    }

    let x_min = rules
        .iter()
        .map(|rule| rule.1)
        .fold(f32::INFINITY, f32::min);
    let x_max = rules
        .iter()
        .map(|rule| rule.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let table_width = x_max - x_min;
    if table_width < 100.0 {
        return None;
    }
    let y_top = *distinct_rule_ys.first()?;
    let y_bottom = *distinct_rule_ys.last()?;
    let band_vertical_xs: Vec<f32> = verticals
        .iter()
        .filter(|&&(x, y_min, y_max)| {
            x >= x_min - RULE_JOIN_GAP
                && x <= x_max + RULE_JOIN_GAP
                && y_max >= y_bottom - RULE_Y_TOLERANCE
                && y_min <= y_top + RULE_Y_TOLERANCE
        })
        .map(|rule| rule.0)
        .collect();
    if snap_edges(&band_vertical_xs, 3.0).len() >= 2 {
        // Dense-row anchors are a horizontal-rule fallback. Vertical strokes
        // elsewhere on the page are irrelevant, but a pair inside this ruled
        // band means the physical-grid hypotheses should own the region.
        return None;
    }
    let spanning_rules = rules
        .iter()
        .filter(|rule| rule.2 - rule.1 >= table_width * 0.8)
        .count();
    if spanning_rules < 2 {
        return None;
    }

    let rows = collect_anchored_rows(items, &rules, page);
    if !(3..=30).contains(&rows.len()) {
        return None;
    }
    if rows.len() > distinct_rule_ys.len() * 2 + 2 {
        // Sparse page decorations around prose can expose many aligned text
        // starts, but their rule levels do not corroborate the row schema.
        return None;
    }
    let anchors = rows
        .iter()
        .map(|(_, row)| logical_row_anchors(row))
        .max_by_key(Vec::len)?;
    if !(4..=25).contains(&anchors.len()) || anchors.last()? - anchors[0] < table_width * 0.6 {
        return None;
    }

    // At least two rows must independently expose nearly the whole schema.
    // A single busy line inside a chart or form is not enough evidence.
    let dense_threshold = (anchors.len() * 3).div_ceil(4);
    let dense_rows = rows
        .iter()
        .filter(|(_, row)| matched_anchor_column_count(row, &anchors) >= dense_threshold)
        .count();
    if dense_rows < 2 {
        return None;
    }

    let mut columns = vec![x_min.min(anchors[0])];
    columns.extend(anchors.windows(2).map(|pair| (pair[0] + pair[1]) / 2.0));
    columns.push(x_max.max(*anchors.last()?));

    let mut cells = vec![vec![String::new(); anchors.len()]; rows.len()];
    let mut item_indices = Vec::new();
    for (row_index, (_, row)) in rows.iter().enumerate() {
        for (item_index, item) in row {
            let column = nearest_anchor_column(item, &anchors)?;
            if !cells[row_index][column].is_empty() {
                cells[row_index][column].push(' ');
            }
            cells[row_index][column].push_str(item.text.trim());
            item_indices.push(*item_index);
        }
    }
    item_indices.sort_unstable();
    item_indices.dedup();

    let body_rows = &cells[1..];
    let numeric_cells = body_rows
        .iter()
        .flatten()
        .filter(|cell| cell.chars().any(|character| character.is_ascii_digit()))
        .count();
    let nonempty_body_cells = body_rows
        .iter()
        .flatten()
        .filter(|cell| !cell.is_empty())
        .count();
    if numeric_cells < anchors.len().min(3) || numeric_cells * 4 < nonempty_body_cells.max(1) {
        return None;
    }

    Some(Table::new(
        columns,
        rows.iter().map(|(y, _)| *y).collect(),
        cells,
        item_indices,
    ))
}

/// Recover grids whose horizontal rules provide the outer x bounds while
/// vertical strokes draw only the internal column dividers. A header may sit
/// immediately above the top rule, as is common in presentation tables.
fn build_open_edge_grid_table_for_rules(
    items: &[TextItem],
    logical_rules: &[HorizontalRule],
    rules: &[HorizontalRule],
    verticals: &[(f32, f32, f32)],
    page: u32,
) -> Option<Table> {
    let x_min = rules
        .iter()
        .map(|rule| rule.1)
        .fold(f32::INFINITY, f32::min);
    let x_max = rules
        .iter()
        .map(|rule| rule.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_top = rules
        .iter()
        .map(|rule| rule.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_bottom = rules
        .iter()
        .map(|rule| rule.0)
        .fold(f32::INFINITY, f32::min);
    let width = x_max - x_min;
    let height = y_top - y_bottom;
    if width < 100.0 || height < 20.0 {
        return None;
    }

    let scoped_vertical_xs: Vec<f32> = verticals
        .iter()
        .filter(|(x, y_min, y_max)| {
            *x > x_min + RULE_JOIN_GAP
                && *x < x_max - RULE_JOIN_GAP
                && *y_min <= y_bottom + RULE_Y_TOLERANCE
                && *y_max >= y_top - RULE_Y_TOLERANCE
                && *y_max - *y_min >= height * 0.8
        })
        .map(|(x, _, _)| *x)
        .collect();
    let interior_edges = snap_edges(&scoped_vertical_xs, 3.0);
    if !(1..=24).contains(&interior_edges.len()) {
        return None;
    }

    let mut col_edges = Vec::with_capacity(interior_edges.len() + 2);
    col_edges.push(x_min);
    col_edges.extend(interior_edges);
    col_edges.push(x_max);

    let row_y_values: Vec<f32> = rules.iter().map(|rule| rule.0).collect();
    let mut row_edges = snap_edges(&row_y_values, 3.0);
    row_edges.sort_by(|left, right| right.total_cmp(left));
    if row_edges.len() < 3 {
        return None;
    }

    let (body_cells, mut item_indices) = assign_items_to_grid(items, &col_edges, &row_edges, page);
    let column_count = col_edges.len() - 1;
    let occupied_body_rows = body_cells
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .count();
    let occupied_body_columns = (0..column_count)
        .filter(|&column| body_cells.iter().any(|row| !row[column].is_empty()))
        .count();
    if occupied_body_rows < 2 || occupied_body_columns != column_count {
        return None;
    }

    let header_band = [
        (y_top + 30.0, x_min, x_max),
        (y_top + RULE_Y_TOLERANCE, x_min, x_max),
    ];
    let header_rows = collect_anchored_rows(items, &header_band, page);
    let header_y = header_rows.first()?.0;
    let mut header_cells = vec![String::new(); column_count];
    let mut header_indices = Vec::new();
    for (_, header_items) in &header_rows {
        for (item_index, item) in header_items {
            let center_x = item.x + item.width / 2.0;
            let column = (0..column_count)
                .find(|&index| center_x >= col_edges[index] && center_x <= col_edges[index + 1])?;
            if !header_cells[column].is_empty() {
                header_cells[column].push(' ');
            }
            header_cells[column].push_str(item.text.trim());
            header_indices.push(*item_index);
        }
    }
    let mixed_rule_span_in_band = logical_rules.iter().any(|rule| {
        rule.0 >= y_bottom - RULE_Y_TOLERANCE
            && rule.0 <= y_top + RULE_Y_TOLERANCE
            && rule.2 >= x_min - RULE_JOIN_GAP
            && rule.1 <= x_max + RULE_JOIN_GAP
            && !rules.contains(rule)
    });
    if header_cells[1..].iter().any(String::is_empty)
        || (!header_cells[0].is_empty() && mixed_rule_span_in_band)
    {
        // The first column may be either an unlabeled row-header stub or a
        // normal labeled column. A fully populated header is less distinctive,
        // so only accept it when every logical horizontal rule corroborates
        // the same open-edge band; mixed rule spans are better left to the
        // physical-grid detector.
        return None;
    }

    let mut cells = Vec::with_capacity(body_cells.len() + 1);
    cells.push(header_cells);
    cells.extend(body_cells);
    item_indices.extend(header_indices);
    item_indices.sort_unstable();
    item_indices.dedup();

    let mut rows = Vec::with_capacity(row_edges.len());
    rows.push(header_y);
    rows.extend_from_slice(&row_edges[..row_edges.len() - 1]);
    Some(Table::new(col_edges, rows, cells, item_indices))
}

fn build_open_edge_grid_tables(
    items: &[TextItem],
    horizontals: &[HorizontalRule],
    verticals: &[(f32, f32, f32)],
    page: u32,
) -> Vec<Table> {
    let logical_rules = merge_horizontal_segments(horizontals);
    group_rules_by_span(&logical_rules)
        .into_iter()
        .flat_map(|span_group| split_independent_rule_runs(&span_group, items, page))
        .filter(|rules| rules.len() >= 3)
        .filter_map(|rules| {
            build_open_edge_grid_table_for_rules(items, &logical_rules, &rules, verticals, page)
        })
        .collect()
}

fn table_evidence_score(table: &Table) -> usize {
    let filled_cells = table
        .cells
        .iter()
        .flatten()
        .filter(|cell| !cell.is_empty())
        .count();
    let occupied_rows = table
        .cells
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .count();
    let column_count = table.cells.first().map_or(0, Vec::len);
    let occupied_columns = (0..column_count)
        .filter(|&column| {
            table
                .cells
                .iter()
                .any(|row| row.get(column).is_some_and(|cell| !cell.is_empty()))
        })
        .count();
    let empty_cells = table.cells.len() * column_count - filled_cells;

    let positive_evidence = table.item_indices.len() * 100
        + filled_cells * 25
        + occupied_columns * 60
        + occupied_rows * 20;
    positive_evidence.saturating_sub(empty_cells.saturating_mul(4))
}

fn select_non_overlapping_hypotheses(mut candidates: Vec<Table>) -> Vec<Table> {
    candidates.sort_by(|left, right| {
        table_evidence_score(right)
            .cmp(&table_evidence_score(left))
            .then_with(|| right.item_indices.len().cmp(&left.item_indices.len()))
    });

    let mut selected = Vec::new();
    let mut claimed_items = HashSet::new();
    for table in candidates {
        if table
            .item_indices
            .iter()
            .any(|index| claimed_items.contains(index))
        {
            continue;
        }
        claimed_items.extend(table.item_indices.iter().copied());
        selected.push(table);
    }
    selected.sort_by(|left, right| {
        right
            .rows
            .first()
            .copied()
            .unwrap_or_default()
            .total_cmp(&left.rows.first().copied().unwrap_or_default())
    });
    selected
}

fn tables_share_items(left: &Table, right: &Table) -> bool {
    left.item_indices
        .iter()
        .any(|index| right.item_indices.contains(index))
}

fn overlaps_multiple_tables(candidate: &Table, tables: &[Table]) -> bool {
    tables
        .iter()
        .filter(|table| tables_share_items(candidate, table))
        .take(2)
        .count()
        > 1
}

fn select_table_hypothesis(legacy: Vec<Table>, alternatives: Vec<Table>, page: u32) -> Vec<Table> {
    if alternatives.is_empty() {
        return legacy;
    }
    if legacy.is_empty() {
        let selected = select_non_overlapping_hypotheses(alternatives);
        log::debug!(
            "detect_lines p{}: accepted {} alternative table(s) (no legacy candidate)",
            page,
            selected.len()
        );
        return selected;
    }

    let legacy_count = legacy.len();
    let mut candidates = legacy;
    candidates.extend(alternatives);
    let selected = select_non_overlapping_hypotheses(candidates);
    log::debug!(
        "detect_lines p{}: selected {} table(s) from {} legacy and alternative hypotheses",
        page,
        selected.len(),
        legacy_count
    );
    selected
}

/// Derive column edges from the x-endpoints of horizontal-rule
/// segments when no vertical lines were drawn.
///
/// Catalog and archival-finding-aid tables are commonly drawn with
/// per-row horizontal rules broken into N segments (one segment per
/// cell), with no vertical dividers at all. The segment break points
/// (e.g. `[50, 127], [127, 485], [485, 562]` per row) implicitly
/// encode the column boundaries.
///
/// Returns column edges if ≥3 distinct x-positions each show up as a
/// segment endpoint on ≥50% of the unique horizontal-line rows.
/// Returns `None` otherwise — decorative rules with varying widths
/// shouldn't be mistaken for a table.
fn derive_columns_from_horizontal_segments(horizontals: &[(f32, f32, f32)]) -> Option<Vec<f32>> {
    if horizontals.len() < 3 {
        return None;
    }

    let mut endpoints: Vec<f32> = Vec::with_capacity(horizontals.len() * 2);
    for &(_, x_min, x_max) in horizontals {
        endpoints.push(x_min);
        endpoints.push(x_max);
    }
    let clusters = snap_edges(&endpoints, 5.0);
    if clusters.len() < 3 {
        return None;
    }

    // Bucket y-values to count unique rows. Tolerance ~0.1pt (×10
    // rounding) tolerates the snap_edges 3pt clustering used later
    // for row edges.
    let unique_rows: HashSet<i32> = horizontals
        .iter()
        .map(|&(y, _, _)| (y * 10.0).round() as i32)
        .collect();
    if unique_rows.len() < 2 {
        return None;
    }
    let min_rows = (unique_rows.len() as f32 * 0.5).ceil() as usize;

    let qualifying: Vec<f32> = clusters
        .iter()
        .copied()
        .filter(|&cluster_x| {
            let rows_touched: HashSet<i32> = horizontals
                .iter()
                .filter(|&&(_, x_min, x_max)| {
                    (x_min - cluster_x).abs() < 5.0 || (x_max - cluster_x).abs() < 5.0
                })
                .map(|&(y, _, _)| (y * 10.0).round() as i32)
                .collect();
            rows_touched.len() >= min_rows
        })
        .collect();

    if qualifying.len() < 3 {
        return None;
    }
    Some(qualifying)
}

/// Detect tables from line segments on a given page.
///
/// Lines are classified as horizontal or vertical, snapped into grid edges,
/// and validated before assigning text items to the resulting grid.
pub fn detect_tables_from_lines(items: &[TextItem], lines: &[PdfLine], page: u32) -> Vec<Table> {
    detect_tables_from_lines_inner(items, lines, page, true, true)
}

/// Detect only tables whose cell grid is backed by explicit vector geometry.
///
/// Region-level TSR callers need physical cell boundaries for crop bboxes, so
/// text-anchor columns inferred from sparse rules are deliberately excluded.
pub(crate) fn detect_vector_grid_tables_from_lines(
    items: &[TextItem],
    lines: &[PdfLine],
    page: u32,
) -> Vec<Table> {
    detect_tables_from_lines_inner(items, lines, page, false, false)
}

fn detect_tables_from_lines_inner(
    items: &[TextItem],
    lines: &[PdfLine],
    page: u32,
    allow_text_anchors: bool,
    allow_alternatives: bool,
) -> Vec<Table> {
    // Filter lines for this page
    let page_lines: Vec<&PdfLine> = lines.iter().filter(|l| l.page == page).collect();
    if page_lines.is_empty() {
        return Vec::new();
    }

    // Classify lines as horizontal or vertical (within 2° of axis)
    let mut horizontals: Vec<(f32, f32, f32)> = Vec::new(); // (y, x_min, x_max)
    let mut verticals: Vec<(f32, f32, f32)> = Vec::new(); // (x, y_min, y_max)

    let angle_tolerance = 2.0_f32.to_radians().tan(); // ~0.035

    for line in &page_lines {
        let dx = (line.x2 - line.x1).abs();
        let dy = (line.y2 - line.y1).abs();
        let length = (dx * dx + dy * dy).sqrt();

        // Skip very short lines (decorations, tick marks)
        if length < 20.0 {
            continue;
        }

        if dx > 0.01 && dy / dx <= angle_tolerance {
            // Horizontal line
            let y = (line.y1 + line.y2) / 2.0;
            let x_min = line.x1.min(line.x2);
            let x_max = line.x1.max(line.x2);
            horizontals.push((y, x_min, x_max));
        } else if dy > 0.01 && dx / dy <= angle_tolerance {
            // Vertical line
            let x = (line.x1 + line.x2) / 2.0;
            let y_min = line.y1.min(line.y2);
            let y_max = line.y1.max(line.y2);
            verticals.push((x, y_min, y_max));
        }
        // Diagonal lines are ignored
    }

    if horizontals.len() < 2 {
        return Vec::new();
    }
    let mut alternatives = Vec::new();
    if allow_alternatives {
        if let Some(table) = build_dense_row_anchor_table(items, &horizontals, &verticals, page) {
            alternatives.push(table);
        }
        alternatives.extend(build_open_edge_grid_tables(
            items,
            &horizontals,
            &verticals,
            page,
        ));
    }
    // Booktabs and response-form tables commonly draw horizontal rules only.
    // Their rules describe table bands, not row/cell boundaries, so infer
    // columns from the first text row and rows from text baselines before the
    // legacy endpoint-grid path has a chance to collapse adjacent tables.
    if allow_text_anchors {
        let text_anchor_tables =
            detect_text_anchor_rule_tables(items, &horizontals, &verticals, lines, page);
        if !text_anchor_tables.is_empty() {
            // Sparse-rule and physical-grid tables can coexist on one page.
            // Remove only the graphics belonging to the accepted sparse
            // regions, then let the geometry-only path recover independent
            // tables without allowing an overlapping lower-quality grid to
            // replace the anchor-derived result.
            let remaining_lines: Vec<PdfLine> = lines
                .iter()
                .filter(|line| {
                    !text_anchor_tables
                        .iter()
                        .any(|table| line_overlaps_text_anchor_band(line, table))
                })
                .cloned()
                .collect();
            let anchor_tables: Vec<Table> = text_anchor_tables
                .into_iter()
                .map(|candidate| candidate.table)
                .collect();
            // Re-evaluate geometry and dense/open-edge alternatives on the
            // remaining page, but do not recurse into sparse text anchors.
            // Keep a geometry-only result as a fallback: an inferred header
            // can otherwise reach into an adjacent accepted anchor band and
            // replace a valid physical grid with an overlapping hypothesis.
            let vector_tables =
                detect_tables_from_lines_inner(items, &remaining_lines, page, false, false);
            let mut inferred_tables =
                detect_tables_from_lines_inner(items, &remaining_lines, page, false, true);
            inferred_tables.retain(|table| {
                !anchor_tables
                    .iter()
                    .any(|anchor| tables_share_items(table, anchor))
            });
            let remaining_tables = combine_non_overlapping_tables(inferred_tables, vector_tables);
            alternatives
                .retain(|alternative| !overlaps_multiple_tables(alternative, &anchor_tables));
            // A page-wide alternative that consumes two already-independent
            // sparse tables is a synthetic merge, not a stronger hypothesis.
            // Alternatives may still replace one overlapping anchor or add an
            // independent table elsewhere on the page.
            let mut competing_tables = anchor_tables;
            competing_tables.extend(alternatives);
            let selected_tables = select_non_overlapping_hypotheses(competing_tables);
            return combine_non_overlapping_tables(selected_tables, remaining_tables);
        }
    }
    if horizontals.len() < 3 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // If no/very-few vertical lines are drawn, try to derive column edges
    // from the x-endpoints of the horizontal-rule segments. Catalog and
    // archival-finding-aid layouts commonly draw each row's horizontal
    // rule as N segments (one per cell), with no vertical dividers at
    // all — the segment break points encode the column boundaries.
    let implicit_col_edges: Option<Vec<f32>> = if verticals.len() < 2 {
        derive_columns_from_horizontal_segments(&horizontals)
    } else {
        None
    };
    if verticals.len() < 2 && implicit_col_edges.is_none() {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }
    let cols_from_segments = implicit_col_edges.is_some();

    log::debug!(
        "detect_lines p{}: {} horiz, {} vert lines (of {} total on page){}",
        page,
        horizontals.len(),
        verticals.len(),
        page_lines.len(),
        if cols_from_segments {
            " — columns from horizontal segments"
        } else {
            ""
        }
    );

    // Snap Y-values of horizontal lines → row edges
    let h_ys: Vec<f32> = horizontals.iter().map(|(y, _, _)| *y).collect();
    let row_edges = snap_edges(&h_ys, 3.0);

    // Column edges from drawn verticals when present, else from the
    // horizontal-segment endpoints derived above.
    let col_edges = if let Some(c) = implicit_col_edges {
        c
    } else {
        let v_xs: Vec<f32> = verticals.iter().map(|(x, _, _)| *x).collect();
        snap_edges(&v_xs, 3.0)
    };

    log::debug!(
        "detect_lines p{}: {} row edges, {} col edges after snap",
        page,
        row_edges.len(),
        col_edges.len()
    );

    // Require at least 2 columns (3 col edges) and 2 rows (3 row edges).
    // A single column of horizontal lines is just separator rules, not a table.
    if row_edges.len() < 3 || col_edges.len() < 3 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // Cap grid size: >20 columns is almost certainly a diagram, not a table
    if col_edges.len() > 21 || row_edges.len() > 80 {
        log::debug!(
            "detect_lines p{}: rejected — too many edges ({}x{})",
            page,
            row_edges.len(),
            col_edges.len()
        );
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    let table_x_min = col_edges.first().copied().unwrap_or(0.0);
    let table_x_max = col_edges.last().copied().unwrap_or(0.0);
    let table_width = table_x_max - table_x_min;
    if table_width < 50.0 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    let table_y_min = row_edges.first().copied().unwrap_or(0.0);
    let table_y_max = row_edges.last().copied().unwrap_or(0.0);
    let table_height = (table_y_max - table_y_min).abs();
    if table_height < 20.0 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // Reject page-spanning frames: a decorative outer border has just 4
    // edges (top/bottom/left/right). Real full-page tables — common in
    // governmental ledgers, financial reports, etc. — span the same A4 /
    // Letter dimensions but have many internal row/column rules. Only
    // reject when the line set looks like a bare frame, not a grid.
    // Standard pages are ~595×842 (A4) or ~612×792 (Letter).
    if table_width > 500.0 && table_height > 700.0 && horizontals.len() <= 4 && verticals.len() <= 4
    {
        log::debug!(
            "detect_lines p{}: rejected — page-spanning frame ({:.0}×{:.0}, {} h + {} v)",
            page,
            table_width,
            table_height,
            horizontals.len(),
            verticals.len()
        );
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // Validate horizontal lines: at least 3 should span a meaningful width.
    // Full-width spanning (>50%) is ideal, but tables with partial horizontal
    // rules (column-level separators) are also valid if there are enough.
    let spanning_h = horizontals
        .iter()
        .filter(|(_, x_min, x_max)| (x_max - x_min) > table_width * 0.5)
        .count();
    let partial_h = horizontals
        .iter()
        .filter(|(_, x_min, x_max)| (x_max - x_min) > table_width * 0.15)
        .count();
    if spanning_h < 3 && partial_h < 6 {
        log::debug!(
            "detect_lines p{}: rejected — {} spanning + {} partial H lines",
            page,
            spanning_h,
            partial_h
        );
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // Validate vertical lines: at least 2 should span a meaningful height.
    // Full spanning (>30%) is ideal, but accept many shorter lines (>10%)
    // for tables with partial column separators. Skipped entirely when
    // columns came from horizontal-segment endpoints — there are no
    // vertical lines to validate against, and the segment-endpoint
    // consistency check in `derive_columns_from_horizontal_segments`
    // is the equivalent guard.
    let spanning_v = if cols_from_segments {
        0
    } else {
        let s = verticals
            .iter()
            .filter(|(_, y_min, y_max)| (y_max - y_min) > table_height * 0.3)
            .count();
        let p = verticals
            .iter()
            .filter(|(_, y_min, y_max)| (y_max - y_min) > table_height * 0.10)
            .count();
        if s < 2 && p < 4 {
            log::debug!(
                "detect_lines p{}: rejected — {} spanning + {} partial V lines",
                page,
                s,
                p
            );
            return select_table_hypothesis(Vec::new(), alternatives, page);
        }
        s
    };

    // Row edges need to be in descending order (top of page = higher Y first)
    let mut row_edges_desc = row_edges;
    row_edges_desc.sort_by(|a, b| b.total_cmp(a));

    log::debug!(
        "detect_lines p{}: {} row_edges, {} col_edges, table=({:.0},{:.0})-({:.0},{:.0}), spanning_h={}, spanning_v={}",
        page, row_edges_desc.len(), col_edges.len(),
        table_x_min, table_y_min, table_x_max, table_y_max,
        spanning_h, spanning_v
    );

    // Assign items to grid
    let (cells, item_indices) = assign_items_to_grid(items, &col_edges, &row_edges_desc, page);

    // Require at least 2 non-empty rows
    let non_empty_rows = cells
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .count();
    if non_empty_rows < 2 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // Content density: at least 15% of cells should have content
    let num_cols_grid = cells.first().map_or(0, |r| r.len());
    let total_cells = cells.len() * num_cols_grid;
    if total_cells > 0 {
        let filled_cells = cells
            .iter()
            .flat_map(|row| row.iter())
            .filter(|cell| !cell.is_empty())
            .count();
        let density = filled_cells as f32 / total_cells as f32;
        if density < 0.15 {
            return select_table_hypothesis(Vec::new(), alternatives, page);
        }
    }

    // Require that at least 2 distinct columns have content.
    // Charts/diagrams have text concentrated on axes (1 column);
    // real tables spread data across multiple columns.
    let cols_with_content = (0..num_cols_grid)
        .filter(|&c| {
            cells
                .iter()
                .any(|row| row.get(c).is_some_and(|cell| !cell.is_empty()))
        })
        .count();
    if cols_with_content < 2 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    // The grid must capture a meaningful portion of the page's text items.
    // Chart/graph grids on textbook pages capture scattered labels but miss
    // the bulk of the page content (explanatory text, problem statements).
    let page_item_count = items.iter().filter(|i| i.page == page).count();
    if page_item_count > 0 {
        let capture_ratio = item_indices.len() as f32 / page_item_count as f32;
        // If the grid captures less than 20% of items, it's not a real table
        if capture_ratio < 0.20 {
            return select_table_hypothesis(Vec::new(), alternatives, page);
        }
    }

    // Reject grids with very uniform row spacing — likely chart gridlines.
    // Real tables have variable row heights; chart Y-axes have equal spacing.
    if row_edges_desc.len() >= 5 {
        let spacings: Vec<f32> = row_edges_desc
            .windows(2)
            .map(|w| (w[0] - w[1]).abs())
            .collect();
        let mean_spacing = spacings.iter().sum::<f32>() / spacings.len() as f32;
        if mean_spacing > 0.1 {
            let variance = spacings
                .iter()
                .map(|s| (s - mean_spacing).powi(2))
                .sum::<f32>()
                / spacings.len() as f32;
            let cv = variance.sqrt() / mean_spacing;
            // CV < 0.02 means nearly identical spacing — likely chart grid.
            // Spreadsheet-exported tables often have uniform rows (CV 0.03-0.05),
            // so we use a tighter threshold to avoid false negatives.
            if cv < 0.02 {
                return select_table_hypothesis(Vec::new(), alternatives, page);
            }
        }
    }

    let num_cols = col_edges.len() - 1;
    let num_rows = row_edges_desc.len() - 1;

    if num_rows < 2 || num_cols < 2 {
        return select_table_hypothesis(Vec::new(), alternatives, page);
    }

    log::debug!(
        "detect_lines p{}: ACCEPTED {}x{} grid, {} items captured of {} on page, non_empty_rows={}, cols_with_content={}",
        page, num_rows, num_cols, item_indices.len(), page_item_count, non_empty_rows, cols_with_content
    );

    select_table_hypothesis(
        vec![Table::new(
            col_edges,
            row_edges_desc[..num_rows].to_vec(),
            cells,
            item_indices,
        )],
        alternatives,
        page,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn make_item(text: &str, x: f32, y: f32, page: u32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width: 30.0,
            height: 10.0,
            font: "F1".into(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn make_hline(y: f32, x1: f32, x2: f32, page: u32) -> PdfLine {
        PdfLine {
            x1,
            y1: y,
            x2,
            y2: y,
            page,
        }
    }

    fn make_vline(x: f32, y1: f32, y2: f32, page: u32) -> PdfLine {
        PdfLine {
            x1: x,
            y1,
            x2: x,
            y2,
            page,
        }
    }

    #[test]
    fn test_basic_grid_detection() {
        // 3x2 grid with horizontal lines at y=500, 480, 460 and vertical at x=100, 200, 300
        let lines = vec![
            make_hline(500.0, 100.0, 300.0, 1),
            make_hline(480.0, 100.0, 300.0, 1),
            make_hline(460.0, 100.0, 300.0, 1),
            make_vline(100.0, 460.0, 500.0, 1),
            make_vline(200.0, 460.0, 500.0, 1),
            make_vline(300.0, 460.0, 500.0, 1),
        ];

        let items = vec![
            make_item("Col A", 110.0, 490.0, 1),
            make_item("Col B", 210.0, 490.0, 1),
            make_item("val 1", 110.0, 470.0, 1),
            make_item("val 2", 210.0, 470.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 2); // 2 data rows
        assert_eq!(tables[0].cells[0].len(), 2); // 2 columns
    }

    #[test]
    fn test_short_lines_ignored() {
        // Lines shorter than 20pt should be ignored
        let lines = vec![
            make_hline(500.0, 100.0, 110.0, 1), // 10pt - too short
            make_hline(480.0, 100.0, 115.0, 1), // 15pt - too short
            make_hline(460.0, 100.0, 112.0, 1), // 12pt - too short
        ];

        let items = vec![make_item("text", 105.0, 490.0, 1)];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_wrong_page_ignored() {
        let lines = vec![
            make_hline(500.0, 100.0, 300.0, 2),
            make_hline(480.0, 100.0, 300.0, 2),
            make_hline(460.0, 100.0, 300.0, 2),
            make_vline(100.0, 460.0, 500.0, 2),
            make_vline(200.0, 460.0, 500.0, 2),
            make_vline(300.0, 460.0, 500.0, 2),
        ];

        let items = vec![make_item("text", 110.0, 490.0, 1)];

        // Request page 1, but lines are on page 2
        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_empty_grid_rejected() {
        // Grid with no text items inside
        let lines = vec![
            make_hline(500.0, 100.0, 300.0, 1),
            make_hline(480.0, 100.0, 300.0, 1),
            make_hline(460.0, 100.0, 300.0, 1),
            make_vline(100.0, 460.0, 500.0, 1),
            make_vline(200.0, 460.0, 500.0, 1),
            make_vline(300.0, 460.0, 500.0, 1),
        ];

        let items: Vec<TextItem> = Vec::new();

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_horizontal_rules_not_table() {
        // Only horizontal lines with no verticals — separator rules, not a table
        let lines = vec![
            make_hline(500.0, 100.0, 500.0, 1),
            make_hline(480.0, 100.0, 500.0, 1),
            make_hline(460.0, 100.0, 500.0, 1),
            make_hline(440.0, 100.0, 500.0, 1),
        ];

        let items = vec![
            make_item("text1", 110.0, 490.0, 1),
            make_item("text2", 110.0, 470.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_two_rule_response_form_uses_header_anchors() {
        let lines = vec![
            make_hline(500.0, 80.0, 300.0, 1),
            make_hline(400.0, 80.0, 300.0, 1),
        ];
        let mut items = vec![
            make_item("Prompt", 100.0, 485.0, 1),
            make_item("Response", 200.0, 485.0, 1),
        ];
        for (index, y) in [470.0, 455.0, 440.0, 425.0, 410.0].into_iter().enumerate() {
            items.push(make_item(&format!("item {index}"), 100.0, y, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 6);
        assert_eq!(tables[0].cells[0], vec!["Prompt", "Response"]);
        assert!(tables[0].cells[1..].iter().all(|row| row[1].is_empty()));
    }

    #[test]
    fn test_booktabs_table_uses_text_anchors_for_columns() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(440.0, 80.0, 420.0, 1),
        ];
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 3);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
        assert_eq!(tables[0].cells[2], vec!["Beta", "89.7", "9"]);
    }

    #[test]
    fn test_long_booktabs_table_uses_short_cell_evidence() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(280.0, 80.0, 420.0, 1),
        ];
        let mut items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
        ];
        for index in 0..10 {
            let y = 465.0 - index as f32 * 16.0;
            items.push(make_item(&format!("M{index}"), 100.0, y, 1));
            items.push(make_item(&format!("{}%", 90 + index), 220.0, y, 1));
            items.push(make_item(&format!("{}ms", 5 + index), 340.0, y, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 11);
        assert_eq!(tables[0].cells[10], vec!["M9", "99%", "14ms"]);
    }

    #[test]
    fn test_booktabs_table_with_outer_borders_uses_text_anchors() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(440.0, 80.0, 420.0, 1),
            make_vline(80.0, 440.0, 500.0, 1),
            make_vline(420.0, 440.0, 500.0, 1),
        ];
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
    }

    #[test]
    fn test_booktabs_table_ignores_unrelated_vertical_strokes() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(440.0, 80.0, 420.0, 1),
            make_vline(30.0, 80.0, 140.0, 1),
            make_vline(550.0, 700.0, 780.0, 1),
        ];
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
    }

    #[test]
    fn test_booktabs_table_ignores_short_interior_vertical_mark() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(440.0, 80.0, 420.0, 1),
            make_vline(80.0, 440.0, 500.0, 1),
            make_vline(420.0, 440.0, 500.0, 1),
            make_vline(220.0, 455.0, 480.0, 1),
        ];
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
    }

    #[test]
    fn test_many_short_vertical_marks_reject_text_anchor_candidate() {
        let horizontals = vec![
            (500.0, 80.0, 420.0),
            (480.0, 80.0, 420.0),
            (440.0, 80.0, 420.0),
        ];
        let verticals: Vec<VerticalRule> = [100.0, 150.0, 200.0, 250.0, 300.0, 350.0]
            .into_iter()
            .map(|x| (x, 455.0, 480.0))
            .collect();
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        assert!(
            detect_text_anchor_rule_tables(&items, &horizontals, &verticals, &[], 1).is_empty()
        );
    }

    #[test]
    fn test_dense_line_art_rejects_text_anchor_candidate() {
        let horizontals = vec![
            (500.0, 80.0, 420.0),
            (480.0, 80.0, 420.0),
            (440.0, 80.0, 420.0),
        ];
        let path_lines: Vec<PdfLine> = (0..200)
            .map(|index| PdfLine {
                x1: 90.0 + index as f32,
                y1: 445.0,
                x2: 100.0 + index as f32,
                y2: 495.0,
                page: 1,
            })
            .collect();
        let items = vec![
            make_item("Model", 100.0, 490.0, 1),
            make_item("Accuracy", 220.0, 490.0, 1),
            make_item("Latency", 340.0, 490.0, 1),
            make_item("Alpha", 100.0, 465.0, 1),
            make_item("91.2", 220.0, 465.0, 1),
            make_item("12", 340.0, 465.0, 1),
            make_item("Beta", 100.0, 450.0, 1),
            make_item("89.7", 220.0, 450.0, 1),
            make_item("9", 340.0, 450.0, 1),
        ];

        assert!(
            detect_text_anchor_rule_tables(&items, &horizontals, &[], &path_lines, 1).is_empty()
        );
    }

    #[test]
    fn test_sparse_rule_and_vector_grid_tables_coexist() {
        let lines = vec![
            make_hline(700.0, 60.0, 360.0, 1),
            make_hline(680.0, 60.0, 360.0, 1),
            make_hline(640.0, 60.0, 360.0, 1),
            make_hline(500.0, 400.0, 600.0, 1),
            make_hline(480.0, 400.0, 600.0, 1),
            make_hline(460.0, 400.0, 600.0, 1),
            make_vline(400.0, 460.0, 500.0, 1),
            make_vline(500.0, 460.0, 500.0, 1),
            make_vline(600.0, 460.0, 500.0, 1),
        ];
        let items = vec![
            make_item("Model", 80.0, 690.0, 1),
            make_item("Accuracy", 180.0, 690.0, 1),
            make_item("Latency", 280.0, 690.0, 1),
            make_item("Alpha", 80.0, 665.0, 1),
            make_item("91.2", 180.0, 665.0, 1),
            make_item("12", 280.0, 665.0, 1),
            make_item("Beta", 80.0, 650.0, 1),
            make_item("89.7", 180.0, 650.0, 1),
            make_item("9", 280.0, 650.0, 1),
            make_item("Grid A", 410.0, 490.0, 1),
            make_item("Grid B", 510.0, 490.0, 1),
            make_item("one", 410.0, 470.0, 1),
            make_item("two", 510.0, 470.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
        assert_eq!(tables[1].cells[0], vec!["Grid A", "Grid B"]);
        assert_eq!(tables[1].cells[1], vec!["one", "two"]);
    }

    #[test]
    fn test_captionless_stacked_booktabs_tables_split_at_empty_rule_gap() {
        let lines = vec![
            make_hline(700.0, 60.0, 360.0, 1),
            make_hline(680.0, 60.0, 360.0, 1),
            make_hline(640.0, 60.0, 360.0, 1),
            make_hline(500.0, 60.0, 360.0, 1),
            make_hline(480.0, 60.0, 360.0, 1),
            make_hline(440.0, 60.0, 360.0, 1),
        ];
        let items = vec![
            make_item("Model", 80.0, 690.0, 1),
            make_item("Accuracy", 180.0, 690.0, 1),
            make_item("Latency", 280.0, 690.0, 1),
            make_item("Alpha", 80.0, 665.0, 1),
            make_item("91.2", 180.0, 665.0, 1),
            make_item("12", 280.0, 665.0, 1),
            make_item("Beta", 80.0, 650.0, 1),
            make_item("89.7", 180.0, 650.0, 1),
            make_item("9", 280.0, 650.0, 1),
            make_item("Region", 80.0, 490.0, 1),
            make_item("Revenue", 180.0, 490.0, 1),
            make_item("Growth", 280.0, 490.0, 1),
            make_item("North", 80.0, 465.0, 1),
            make_item("120", 180.0, 465.0, 1),
            make_item("8%", 280.0, 465.0, 1),
            make_item("South", 80.0, 450.0, 1),
            make_item("105", 180.0, 450.0, 1),
            make_item("6%", 280.0, 450.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
        assert_eq!(tables[1].cells[0], vec!["Region", "Revenue", "Growth"]);
    }

    #[test]
    fn test_sparse_rule_mask_stops_at_rule_band_before_vector_table() {
        let lines = vec![
            make_hline(700.0, 60.0, 360.0, 1),
            make_hline(680.0, 60.0, 360.0, 1),
            make_hline(640.0, 60.0, 360.0, 1),
            // The vector table starts only 10pt below the sparse table's
            // bottom rule. Baseline-derived 20pt padding used to consume its
            // top edge and verticals.
            make_hline(630.0, 60.0, 260.0, 1),
            make_hline(610.0, 60.0, 260.0, 1),
            make_hline(590.0, 60.0, 260.0, 1),
            make_vline(60.0, 590.0, 630.0, 1),
            make_vline(160.0, 590.0, 630.0, 1),
            make_vline(260.0, 590.0, 630.0, 1),
        ];
        let items = vec![
            make_item("Model", 80.0, 690.0, 1),
            make_item("Accuracy", 180.0, 690.0, 1),
            make_item("Latency", 280.0, 690.0, 1),
            make_item("Alpha", 80.0, 665.0, 1),
            make_item("91.2", 180.0, 665.0, 1),
            make_item("12", 280.0, 665.0, 1),
            make_item("Beta", 80.0, 650.0, 1),
            make_item("89.7", 180.0, 650.0, 1),
            make_item("9", 280.0, 650.0, 1),
            make_item("Grid A", 70.0, 620.0, 1),
            make_item("Grid B", 170.0, 620.0, 1),
            make_item("one", 70.0, 600.0, 1),
            make_item("two", 170.0, 600.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
        assert_eq!(tables[1].cells[0], vec!["Grid A", "Grid B"]);
    }

    #[test]
    fn test_numeric_data_row_is_not_used_as_booktabs_header() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(440.0, 80.0, 420.0, 1),
        ];
        let items = vec![
            make_item("Multifamily", 100.0, 490.0, 1),
            make_item("0.187", 200.0, 490.0, 1),
            make_item("0.771", 280.0, 490.0, 1),
            make_item("0.068", 360.0, 490.0, 1),
            make_item("Industrial", 100.0, 460.0, 1),
            make_item("-0.221", 200.0, 460.0, 1),
            make_item("0.748", 280.0, 460.0, 1),
            make_item("-0.307", 360.0, 460.0, 1),
        ];

        assert!(detect_tables_from_lines(&items, &lines, 1).is_empty());
    }

    #[test]
    fn test_header_missing_stub_column_defers_to_legacy_detector() {
        let lines = vec![
            make_hline(500.0, 60.0, 500.0, 1),
            make_hline(480.0, 60.0, 500.0, 1),
            make_hline(440.0, 60.0, 500.0, 1),
        ];
        let items = vec![
            make_item("Year 0", 140.0, 490.0, 1),
            make_item("Year 1", 220.0, 490.0, 1),
            make_item("Year 2", 300.0, 490.0, 1),
            make_item("Year 3", 380.0, 490.0, 1),
            make_item("NOI", 80.0, 460.0, 1),
            make_item("100", 140.0, 460.0, 1),
            make_item("103", 220.0, 460.0, 1),
            make_item("106", 300.0, 460.0, 1),
            make_item("109", 380.0, 460.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_dense_row_rules_defer_to_other_table_detectors() {
        let lines = [500.0, 480.0, 455.0, 435.0, 410.0]
            .into_iter()
            .map(|y| make_hline(y, 60.0, 500.0, 1))
            .collect::<Vec<_>>();
        let mut items = Vec::new();
        for (row, y) in [490.0, 470.0, 445.0, 422.0].into_iter().enumerate() {
            items.push(make_item(&format!("label {row}"), 80.0, y, 1));
            items.push(make_item(&format!("value {row}"), 240.0, y, 1));
            items.push(make_item(&format!("note {row}"), 400.0, y, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_sparse_rules_do_not_turn_two_column_prose_into_table() {
        let lines = vec![
            make_hline(500.0, 60.0, 540.0, 1),
            make_hline(350.0, 60.0, 540.0, 1),
            make_hline(200.0, 60.0, 540.0, 1),
        ];
        let mut items = Vec::new();
        for (index, y) in (0..12).map(|index| (index, 485.0 - index as f32 * 20.0)) {
            items.push(make_item(
                &format!("left paragraph line {index}"),
                80.0,
                y,
                1,
            ));
            items.push(make_item(
                &format!("right paragraph line {index}"),
                310.0,
                y,
                1,
            ));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_sparse_rules_do_not_wrap_narrow_text_region() {
        let lines = vec![
            make_hline(500.0, 60.0, 540.0, 1),
            make_hline(350.0, 60.0, 540.0, 1),
            make_hline(200.0, 60.0, 540.0, 1),
        ];
        let mut items = Vec::new();
        for (index, y) in (0..12).map(|index| (index, 485.0 - index as f32 * 20.0)) {
            items.push(make_item(
                &format!("left narrative line {index}"),
                80.0,
                y,
                1,
            ));
            items.push(make_item(
                &format!("middle narrative line {index}"),
                240.0,
                y,
                1,
            ));
            items.push(make_item(
                &format!("right narrative line {index}"),
                400.0,
                y,
                1,
            ));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(tables.is_empty());
    }

    #[test]
    fn test_stacked_machine_tokens_form_single_value_cell() {
        let lines = vec![
            make_hline(500.0, 80.0, 420.0, 1),
            make_hline(480.0, 80.0, 420.0, 1),
            make_hline(400.0, 80.0, 420.0, 1),
        ];
        let items = vec![
            make_item("Filtered Task Name", 100.0, 490.0, 1),
            make_item("task_228", 100.0, 470.0, 1),
            make_item("arc:1.0.0", 100.0, 455.0, 1),
            make_item("task_229", 100.0, 440.0, 1),
            make_item("gsm8k:1.0.0", 100.0, 425.0, 1),
            make_item("drop:2.0.0", 100.0, 410.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 1);
        assert_eq!(tables[0].cells[0][0], "Filtered Task Name");
        assert_eq!(
            tables[0].cells[0][1],
            "task_228 arc:1.0.0 task_229 gsm8k:1.0.0 drop:2.0.0"
        );
    }

    #[test]
    fn test_horizontal_segments_only_implicit_columns_accepted() {
        // Catalog/finding-aid pattern: each row's horizontal rule is
        // drawn as 3 segments at consistent x-endpoints (50, 127, 485,
        // 562), with no vertical lines anywhere. The segment break
        // points must be inferred as column edges.
        let mut lines = Vec::new();
        // Slightly uneven row spacing so the chart-gridline rejector
        // (CV < 0.02) doesn't fire.
        let row_ys = [80.0_f32, 145.0, 215.0, 280.0, 350.0, 415.0, 485.0];
        for &y in &row_ys {
            lines.push(make_hline(y, 50.0, 127.0, 1));
            lines.push(make_hline(y, 127.0, 485.0, 1));
            lines.push(make_hline(y, 485.0, 562.0, 1));
        }
        // Populate every cell so capture / density checks pass.
        let mut items = Vec::new();
        for w in row_ys.windows(2) {
            let row_y = (w[0] + w[1]) / 2.0;
            items.push(make_item("id", 80.0, row_y, 1));
            items.push(make_item("description here", 200.0, row_y, 1));
            items.push(make_item("date", 510.0, row_y, 1));
        }
        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(
            tables.len(),
            1,
            "horizontal-segment-only grid should be accepted"
        );
        let t = &tables[0];
        assert!(
            t.cells.len() >= 4,
            "expected ≥4 rows, got {}",
            t.cells.len()
        );
        assert_eq!(t.cells[0].len(), 3, "expected 3 columns");
    }

    #[test]
    fn test_horizontal_segments_with_inconsistent_endpoints_rejected() {
        // Decorative rules of varying widths shouldn't be detected as a
        // table — each line has its own x-endpoints, no consistent
        // column boundary survives the 50%-of-rows threshold.
        let lines = vec![
            make_hline(100.0, 50.0, 150.0, 1),
            make_hline(200.0, 50.0, 220.0, 1),
            make_hline(300.0, 50.0, 310.0, 1),
            make_hline(400.0, 50.0, 470.0, 1),
        ];
        let items = vec![
            make_item("decorative", 100.0, 150.0, 1),
            make_item("text", 100.0, 250.0, 1),
        ];
        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(
            tables.is_empty(),
            "varying-width decorative rules should not be detected"
        );
    }

    #[test]
    fn test_page_spanning_bare_frame_rejected() {
        // Just an outer A4-sized rectangle: 2 horizontals + 2 verticals.
        // No internal structure → decorative border, not a table.
        let lines = vec![
            make_hline(20.0, 20.0, 575.0, 1),  // top
            make_hline(820.0, 20.0, 575.0, 1), // bottom
            make_vline(20.0, 20.0, 820.0, 1),  // left
            make_vline(575.0, 20.0, 820.0, 1), // right
        ];
        let items = vec![
            make_item("title", 100.0, 100.0, 1),
            make_item("body", 100.0, 200.0, 1),
        ];
        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(
            tables.is_empty(),
            "Page-sized 4-edge frame should be rejected as decoration"
        );
    }

    #[test]
    fn test_page_spanning_grid_with_internal_lines_accepted() {
        // Full-page table (governmental-ledger pattern): A4-sized grid
        // that previously hit the "page-spanning frame" early reject
        // before downstream validation could even look at it.
        // Verticals span the full table height so we isolate the
        // frame-vs-grid decision under test.
        let mut lines = Vec::new();
        // 13 horizontal rules: header + 12 row separators
        let h_ys = [
            22.5, 37.9, 95.5, 144.5, 184.9, 233.9, 291.7, 340.7, 415.8, 499.6, 574.7, 623.7, 698.8,
        ];
        for &y in &h_ys {
            lines.push(make_hline(y, 22.6, 566.6, 1));
        }
        // 7 column dividers spanning full table height.
        let v_xs = [22.6, 66.3, 116.3, 186.6, 263.1, 493.5, 566.5];
        for &x in &v_xs {
            lines.push(make_vline(x, 22.5, 698.8, 1));
        }
        // Populate every cell so the capture-ratio + density checks pass.
        let mut items = Vec::new();
        for r in 0..(h_ys.len() - 1) {
            let row_y = (h_ys[r] + h_ys[r + 1]) / 2.0;
            for c in 0..(v_xs.len() - 1) {
                let col_x = (v_xs[c] + v_xs[c + 1]) / 2.0;
                items.push(make_item("x", col_x, row_y, 1));
            }
        }
        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(
            tables.len(),
            1,
            "Full-page table with internal grid should be accepted"
        );
        let t = &tables[0];
        assert!(
            t.cells.len() >= 6,
            "expected ≥6 rows, got {}",
            t.cells.len()
        );
        assert!(
            t.cells[0].len() >= 3,
            "expected ≥3 columns, got {}",
            t.cells[0].len()
        );
    }

    #[test]
    fn test_dense_row_hypothesis_beats_partial_segment_grid() {
        let lines = vec![
            make_hline(520.0, 80.0, 520.0, 1),
            make_hline(490.0, 160.0, 310.0, 1),
            make_hline(490.0, 320.0, 520.0, 1),
            make_hline(475.0, 80.0, 150.0, 1),
            make_hline(475.0, 160.0, 310.0, 1),
            make_hline(475.0, 320.0, 520.0, 1),
            make_hline(420.0, 80.0, 520.0, 1),
        ];
        let xs = [90.0, 160.0, 225.0, 290.0, 355.0, 420.0, 485.0];
        let mut items = vec![
            make_item("Training Datasets", 290.0, 510.0, 1),
            make_item("Properties", xs[0], 500.0, 1),
            make_item("Instruction", xs[2], 500.0, 1),
            make_item("Alignment", xs[5], 500.0, 1),
        ];
        for (column, x) in xs.into_iter().enumerate() {
            items.push(make_item(&format!("dataset {column}"), x, 485.0, 1));
            items.push(make_item(&format!("{}K", column + 1), x, 460.0, 1));
            items.push(make_item(&format!("{}00", column + 1), x, 440.0, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 5);
        assert_eq!(tables[0].cells[0].len(), 7);
        assert_eq!(tables[0].cells[3][0], "1K");
        assert_eq!(tables[0].cells[3][6], "7K");
    }

    #[test]
    fn test_dense_row_hypothesis_requires_numeric_body_evidence() {
        let lines = vec![
            make_hline(520.0, 80.0, 520.0, 1),
            make_hline(490.0, 80.0, 520.0, 1),
            make_hline(460.0, 80.0, 520.0, 1),
            make_hline(420.0, 80.0, 520.0, 1),
        ];
        let mut items = Vec::new();
        for (row, y) in [505.0, 475.0, 440.0].into_iter().enumerate() {
            for (column, x) in [90.0, 200.0, 310.0, 420.0].into_iter().enumerate() {
                let label = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"][row + column];
                items.push(make_item(label, x, y, 1));
            }
        }

        assert!(build_dense_row_anchor_table(
            &items,
            &lines
                .iter()
                .map(|line| (line.y1, line.x1, line.x2))
                .collect::<Vec<_>>(),
            &[],
            1
        )
        .is_none());
    }

    #[test]
    fn test_dense_row_hypothesis_does_not_join_stacked_charts() {
        let mut lines = Vec::new();
        for y in [700.0, 680.0, 660.0, 640.0, 500.0, 480.0, 460.0, 440.0] {
            lines.push(make_hline(y, 80.0, 520.0, 1));
        }
        let mut items = Vec::new();
        for (row, y) in [690.0, 670.0, 650.0, 490.0, 470.0, 450.0]
            .into_iter()
            .enumerate()
        {
            for (column, x) in [90.0, 200.0, 310.0, 420.0].into_iter().enumerate() {
                items.push(make_item(&format!("{}", row * 10 + column), x, y, 1));
            }
        }

        assert!(build_dense_row_anchor_table(
            &items,
            &lines
                .iter()
                .map(|line| (line.y1, line.x1, line.x2))
                .collect::<Vec<_>>(),
            &[],
            1
        )
        .is_none());
    }

    #[test]
    fn test_dense_row_hypothesis_rejects_uniform_chart_run() {
        let lines: Vec<PdfLine> = [520.0, 500.0, 480.0, 460.0, 440.0]
            .into_iter()
            .map(|y| make_hline(y, 80.0, 520.0, 1))
            .collect();
        let mut items = Vec::new();
        for (row, y) in [510.0, 490.0, 470.0, 450.0].into_iter().enumerate() {
            for (column, x) in [90.0, 200.0, 310.0, 420.0].into_iter().enumerate() {
                items.push(make_item(&format!("{}", row * 10 + column), x, y, 1));
            }
        }

        assert!(build_dense_row_anchor_table(
            &items,
            &lines
                .iter()
                .map(|line| (line.y1, line.x1, line.x2))
                .collect::<Vec<_>>(),
            &[],
            1
        )
        .is_none());
    }

    #[test]
    fn test_fragmented_row_counts_distinct_anchor_columns() {
        let items = vec![
            make_item("left", 100.0, 500.0, 1),
            make_item("fragment", 133.0, 500.0, 1),
            make_item("right", 300.0, 500.0, 1),
            make_item("tail", 333.0, 500.0, 1),
        ];
        let row: Vec<(usize, &TextItem)> = items.iter().enumerate().collect();

        assert_eq!(logical_row_anchors(&row), vec![100.0, 300.0]);
        assert_eq!(
            matched_anchor_column_count(&row, &[100.0, 200.0, 300.0, 400.0]),
            2
        );
    }

    #[test]
    fn test_dense_row_hypothesis_ignores_verticals_outside_its_band() {
        let lines = vec![
            make_hline(520.0, 80.0, 520.0, 1),
            make_hline(490.0, 160.0, 310.0, 1),
            make_hline(490.0, 320.0, 520.0, 1),
            make_hline(475.0, 80.0, 150.0, 1),
            make_hline(475.0, 160.0, 310.0, 1),
            make_hline(475.0, 320.0, 520.0, 1),
            make_hline(420.0, 80.0, 520.0, 1),
            make_vline(30.0, 600.0, 680.0, 1),
            make_vline(580.0, 250.0, 330.0, 1),
        ];
        let xs = [90.0, 160.0, 225.0, 290.0, 355.0, 420.0, 485.0];
        let mut items = vec![
            make_item("Training Datasets", 290.0, 510.0, 1),
            make_item("Properties", xs[0], 500.0, 1),
            make_item("Instruction", xs[2], 500.0, 1),
            make_item("Alignment", xs[5], 500.0, 1),
        ];
        for (column, x) in xs.into_iter().enumerate() {
            items.push(make_item(&format!("dataset {column}"), x, 485.0, 1));
            items.push(make_item(&format!("{}K", column + 1), x, 460.0, 1));
            items.push(make_item(&format!("{}00", column + 1), x, 440.0, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0].len(), 7);
    }

    #[test]
    fn test_sparse_table_does_not_hide_remaining_dense_hypothesis() {
        let lines = vec![
            make_hline(700.0, 60.0, 360.0, 1),
            make_hline(680.0, 60.0, 360.0, 1),
            make_hline(640.0, 60.0, 360.0, 1),
            make_hline(520.0, 80.0, 520.0, 1),
            make_hline(490.0, 160.0, 310.0, 1),
            make_hline(490.0, 320.0, 520.0, 1),
            make_hline(475.0, 80.0, 150.0, 1),
            make_hline(475.0, 160.0, 310.0, 1),
            make_hline(475.0, 320.0, 520.0, 1),
            make_hline(420.0, 80.0, 520.0, 1),
        ];
        let mut items = vec![
            make_item("Model", 80.0, 690.0, 1),
            make_item("Accuracy", 180.0, 690.0, 1),
            make_item("Latency", 280.0, 690.0, 1),
            make_item("Alpha", 80.0, 665.0, 1),
            make_item("91.2", 180.0, 665.0, 1),
            make_item("12", 280.0, 665.0, 1),
            make_item("Beta", 80.0, 650.0, 1),
            make_item("89.7", 180.0, 650.0, 1),
            make_item("9", 280.0, 650.0, 1),
            make_item("Training Datasets", 290.0, 510.0, 1),
            make_item("Properties", 90.0, 500.0, 1),
            make_item("Instruction", 225.0, 500.0, 1),
            make_item("Alignment", 420.0, 500.0, 1),
        ];
        let xs = [90.0, 160.0, 225.0, 290.0, 355.0, 420.0, 485.0];
        for (column, x) in xs.into_iter().enumerate() {
            items.push(make_item(&format!("dataset {column}"), x, 485.0, 1));
            items.push(make_item(&format!("{}K", column + 1), x, 460.0, 1));
            items.push(make_item(&format!("{}00", column + 1), x, 440.0, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].cells[0], vec!["Model", "Accuracy", "Latency"]);
        assert_eq!(tables[1].cells[0].len(), 7);
    }

    #[test]
    fn test_three_column_open_edge_preempts_text_anchor_and_keeps_first_header() {
        let lines = vec![
            make_hline(360.0, 30.0, 930.0, 1),
            make_hline(275.0, 30.0, 930.0, 1),
            make_hline(170.0, 30.0, 930.0, 1),
            make_vline(330.0, 168.0, 362.0, 1),
            make_vline(630.0, 168.0, 362.0, 1),
        ];
        let items = vec![
            make_item("Category", 60.0, 375.0, 1),
            make_item("Metric", 360.0, 375.0, 1),
            make_item("Value", 660.0, 375.0, 1),
            make_item("Pack", 60.0, 320.0, 1),
            make_item("OCR body", 360.0, 320.0, 1),
            make_item("42", 660.0, 320.0, 1),
            make_item("Application", 60.0, 220.0, 1),
            make_item("Search", 360.0, 220.0, 1),
            make_item("84", 660.0, 220.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 3);
        assert_eq!(tables[0].cells[0], vec!["Category", "Metric", "Value"]);
        assert_eq!(tables[0].cells[1], vec!["Pack", "OCR body", "42"]);
    }

    #[test]
    fn test_two_column_open_edge_uses_single_interior_divider() {
        let lines = vec![
            make_hline(360.0, 30.0, 630.0, 1),
            make_hline(275.0, 30.0, 630.0, 1),
            make_hline(170.0, 30.0, 630.0, 1),
            make_vline(330.0, 168.0, 362.0, 1),
        ];
        let items = vec![
            make_item("Category", 60.0, 375.0, 1),
            make_item("Value", 360.0, 375.0, 1),
            make_item("Pack", 60.0, 320.0, 1),
            make_item("42", 360.0, 320.0, 1),
            make_item("Application", 60.0, 220.0, 1),
            make_item("84", 360.0, 220.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0], vec!["Category", "Value"]);
        assert_eq!(tables[0].cells[1], vec!["Pack", "42"]);
    }

    #[test]
    fn test_multiple_open_edge_bands_on_one_page_are_preserved() {
        let lines = vec![
            make_hline(700.0, 30.0, 630.0, 1),
            make_hline(615.0, 30.0, 630.0, 1),
            make_hline(510.0, 30.0, 630.0, 1),
            make_vline(330.0, 508.0, 702.0, 1),
            make_hline(360.0, 80.0, 780.0, 1),
            make_hline(275.0, 80.0, 780.0, 1),
            make_hline(170.0, 80.0, 780.0, 1),
            make_vline(430.0, 168.0, 362.0, 1),
        ];
        let items = vec![
            make_item("Category", 60.0, 715.0, 1),
            make_item("Value", 360.0, 715.0, 1),
            make_item("Pack", 60.0, 660.0, 1),
            make_item("42", 360.0, 660.0, 1),
            make_item("Application", 60.0, 560.0, 1),
            make_item("84", 360.0, 560.0, 1),
            make_item("Region", 110.0, 375.0, 1),
            make_item("Count", 460.0, 375.0, 1),
            make_item("North", 110.0, 320.0, 1),
            make_item("12", 460.0, 320.0, 1),
            make_item("South", 110.0, 220.0, 1),
            make_item("18", 460.0, 220.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].cells[0], vec!["Category", "Value"]);
        assert_eq!(tables[1].cells[0], vec!["Region", "Count"]);
    }

    #[test]
    fn test_open_edge_grid_merges_multi_line_headers_by_column() {
        let lines = vec![
            make_hline(360.0, 30.0, 930.0, 1),
            make_hline(275.0, 30.0, 930.0, 1),
            make_hline(170.0, 30.0, 930.0, 1),
            make_vline(330.0, 168.0, 362.0, 1),
            make_vline(630.0, 168.0, 362.0, 1),
        ];
        let items = vec![
            make_item("Content", 60.0, 390.0, 1),
            make_item("Quality", 360.0, 390.0, 1),
            make_item("Result", 660.0, 390.0, 1),
            make_item("Category", 60.0, 375.0, 1),
            make_item("Metric", 360.0, 375.0, 1),
            make_item("Value", 660.0, 375.0, 1),
            make_item("Pack", 60.0, 320.0, 1),
            make_item("OCR body", 360.0, 320.0, 1),
            make_item("42", 660.0, 320.0, 1),
            make_item("Application", 60.0, 220.0, 1),
            make_item("Search", 360.0, 220.0, 1),
            make_item("84", 660.0, 220.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].cells[0],
            vec!["Content Category", "Quality Metric", "Result Value"]
        );
        assert_eq!(tables[0].item_indices.len(), items.len());
    }

    #[test]
    fn test_populated_open_edge_header_requires_unambiguous_rule_band() {
        let horizontals = vec![
            (360.0, 30.0, 930.0),
            (275.0, 30.0, 930.0),
            (170.0, 30.0, 930.0),
            (250.0, 200.0, 700.0),
        ];
        let verticals = vec![(330.0, 168.0, 362.0), (630.0, 168.0, 362.0)];
        let items = vec![
            make_item("Category", 60.0, 375.0, 1),
            make_item("Metric", 360.0, 375.0, 1),
            make_item("Value", 660.0, 375.0, 1),
            make_item("Pack", 60.0, 320.0, 1),
            make_item("OCR body", 360.0, 320.0, 1),
            make_item("42", 660.0, 320.0, 1),
            make_item("Application", 60.0, 220.0, 1),
            make_item("Search", 360.0, 220.0, 1),
            make_item("84", 660.0, 220.0, 1),
        ];

        assert!(build_open_edge_grid_tables(&items, &horizontals, &verticals, 1).is_empty());
    }

    #[test]
    fn test_table_evidence_score_saturates_for_sparse_candidates() {
        let table = Table::new(
            vec![0.0, 1.0, 2.0, 3.0, 4.0],
            vec![4.0, 3.0, 2.0, 1.0],
            vec![vec![String::new(); 4]; 4],
            Vec::new(),
        );

        assert_eq!(table_evidence_score(&table), 0);
    }

    #[test]
    fn test_page_wide_alternative_cannot_merge_independent_anchor_tables() {
        let make_table = |indices: Vec<usize>| {
            Table::new(
                vec![0.0, 100.0, 200.0],
                vec![200.0, 100.0],
                vec![vec!["a".into(), "b".into()]],
                indices,
            )
        };
        let anchors = vec![make_table(vec![0, 1]), make_table(vec![2, 3])];

        assert!(overlaps_multiple_tables(
            &make_table(vec![0, 1, 2, 3]),
            &anchors
        ));
        assert!(!overlaps_multiple_tables(&make_table(vec![0, 1]), &anchors));
    }

    #[test]
    fn test_open_edge_grid_adds_outer_bounds_and_header() {
        let lines = vec![
            make_hline(360.0, 30.0, 930.0, 1),
            make_hline(275.0, 30.0, 930.0, 1),
            make_hline(170.0, 30.0, 930.0, 1),
            make_vline(125.0, 168.0, 362.0, 1),
            make_vline(385.0, 168.0, 362.0, 1),
            make_vline(655.0, 168.0, 362.0, 1),
            make_vline(45.0, 425.0, 500.0, 1),
        ];
        let mut items = vec![
            make_item("OCR", 240.0, 375.0, 1),
            make_item("Recommendation", 465.0, 375.0, 1),
            make_item("Semantic search", 735.0, 375.0, 1),
        ];
        for (text, x) in [
            ("Pack", 60.0),
            ("OCR body", 145.0),
            ("Rec body", 405.0),
            ("Search body", 675.0),
        ] {
            items.push(make_item(text, x, 320.0, 1));
        }
        for (text, x) in [
            ("Application", 50.0),
            ("OCR use", 145.0),
            ("Rec use", 405.0),
            ("Search use", 675.0),
        ] {
            items.push(make_item(text, x, 220.0, 1));
        }

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells.len(), 3);
        assert_eq!(
            tables[0].cells[0],
            vec!["", "OCR", "Recommendation", "Semantic search"]
        );
        assert_eq!(tables[0].cells[1][0], "Pack");
        assert_eq!(tables[0].cells[2][3], "Search use");
    }

    #[test]
    fn test_single_column_rejected() {
        // Only 2 col edges (1 column) — not a table even with verticals
        let lines = vec![
            make_hline(500.0, 100.0, 200.0, 1),
            make_hline(480.0, 100.0, 200.0, 1),
            make_hline(460.0, 100.0, 200.0, 1),
            make_vline(100.0, 460.0, 500.0, 1),
            make_vline(200.0, 460.0, 500.0, 1),
        ];

        let items = vec![
            make_item("a", 110.0, 490.0, 1),
            make_item("b", 110.0, 470.0, 1),
        ];

        let tables = detect_tables_from_lines(&items, &lines, 1);
        assert!(
            tables.is_empty(),
            "Single-column grid should not be a table"
        );
    }
}
