//! Structure-tree-based table detection.
//!
//! When a PDF has a well-formed structure tree with `/Table` > `/TR` > `/TD|TH`
//! elements linked to MCIDs, this module builds `Table` structs directly from
//! the semantic hierarchy — no geometry heuristics needed.

use std::collections::{HashMap, HashSet};

use log::debug;

use crate::structure_tree::{StructTable, StructTableRow};
use crate::types::TextItem;

use super::Table;

#[derive(Debug, Clone)]
struct MatchedCell {
    text: String,
    item_indices: Vec<usize>,
    x: Option<f32>,
    y: Option<f32>,
}

fn legacy_column_positions(
    page_rows: &[&StructTableRow],
    mcid_to_items: &HashMap<i64, Vec<usize>>,
    items: &[TextItem],
    page: u32,
    num_cols: usize,
) -> Vec<f32> {
    let mut col_positions: Vec<f32> = vec![0.0; num_cols];
    for (col, col_pos) in col_positions.iter_mut().enumerate() {
        for row in page_rows {
            if col < row.cells.len() {
                if let Some(x) = row.cells[col]
                    .mcids
                    .iter()
                    .filter(|(_, p)| *p == page)
                    .filter_map(|(mcid, _)| mcid_to_items.get(mcid))
                    .flatten()
                    .map(|&idx| items[idx].x)
                    .reduce(f32::min)
                {
                    *col_pos = x;
                    break;
                }
            }
        }
    }
    col_positions
}

fn infer_column_positions(
    raw_rows: &[Vec<MatchedCell>],
    fallback_positions: &[f32],
    num_cols: usize,
) -> Vec<f32> {
    const SAME_COLUMN_TOLERANCE: f32 = 18.0;

    let mut anchors = raw_rows
        .iter()
        .max_by_key(|row| row.iter().filter(|cell| cell.x.is_some()).count())
        .map(|row| row.iter().filter_map(|cell| cell.x).collect::<Vec<_>>())
        .unwrap_or_default();

    if anchors.len() > num_cols {
        anchors.truncate(num_cols);
    }

    let mut additional_positions: Vec<f32> = raw_rows
        .iter()
        .flat_map(|row| row.iter().filter_map(|cell| cell.x))
        .collect();
    additional_positions.sort_by(|a, b| a.total_cmp(b));

    for x in additional_positions {
        if anchors.len() >= num_cols {
            break;
        }
        if anchors
            .iter()
            .all(|existing| (x - *existing).abs() > SAME_COLUMN_TOLERANCE)
        {
            anchors.push(x);
            anchors.sort_by(|a, b| a.total_cmp(b));
        }
    }

    if anchors.len() < num_cols {
        for &x in fallback_positions {
            if anchors.len() >= num_cols {
                break;
            }
            if anchors
                .iter()
                .all(|existing| (x - *existing).abs() > SAME_COLUMN_TOLERANCE)
            {
                anchors.push(x);
                anchors.sort_by(|a, b| a.total_cmp(b));
            }
        }
    }

    if anchors.is_empty() {
        return fallback_positions.to_vec();
    }

    while anchors.len() < num_cols {
        anchors.push(*anchors.last().unwrap());
    }

    anchors
}

fn align_positions_to_columns(cell_xs: &[f32], columns: &[f32]) -> Vec<usize> {
    if cell_xs.is_empty() || columns.is_empty() {
        return Vec::new();
    }
    if cell_xs.len() >= columns.len() {
        return (0..cell_xs.len().min(columns.len())).collect();
    }

    let mut dp = vec![vec![f32::INFINITY; columns.len() + 1]; cell_xs.len() + 1];
    let mut take = vec![vec![false; columns.len() + 1]; cell_xs.len() + 1];

    for value in &mut dp[0] {
        *value = 0.0;
    }

    for i in 1..=cell_xs.len() {
        for j in 1..=columns.len() {
            let skip_cost = dp[i][j - 1];
            let take_cost = dp[i - 1][j - 1] + (cell_xs[i - 1] - columns[j - 1]).abs();
            if take_cost <= skip_cost {
                dp[i][j] = take_cost;
                take[i][j] = true;
            } else {
                dp[i][j] = skip_cost;
            }
        }
    }

    let mut assignments_rev = Vec::with_capacity(cell_xs.len());
    let mut i = cell_xs.len();
    let mut j = columns.len();
    while i > 0 && j > 0 {
        if take[i][j] {
            assignments_rev.push(j - 1);
            i -= 1;
            j -= 1;
        } else {
            j -= 1;
        }
    }

    assignments_rev.reverse();
    assignments_rev
}

fn align_struct_rows(
    raw_rows: &[Vec<MatchedCell>],
    col_positions: &[f32],
) -> (Vec<Vec<String>>, Vec<f32>, Vec<usize>) {
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(raw_rows.len());
    let mut row_positions: Vec<f32> = Vec::with_capacity(raw_rows.len());
    let mut all_item_indices: Vec<usize> = Vec::new();

    for row in raw_rows {
        let present_cells: Vec<&MatchedCell> = row
            .iter()
            .filter(|cell| {
                !cell.item_indices.is_empty() || !cell.text.is_empty() || cell.x.is_some()
            })
            .collect();
        let cell_xs: Vec<f32> = present_cells.iter().filter_map(|cell| cell.x).collect();
        let assignments = if cell_xs.len() == present_cells.len() {
            align_positions_to_columns(&cell_xs, col_positions)
        } else {
            (0..present_cells.len().min(col_positions.len())).collect()
        };

        let mut row_cells = vec![String::new(); col_positions.len()];
        for (cell, &col_idx) in present_cells.iter().zip(assignments.iter()) {
            if !cell.text.is_empty() {
                if !row_cells[col_idx].is_empty() {
                    row_cells[col_idx].push(' ');
                }
                row_cells[col_idx].push_str(&cell.text);
            }
            all_item_indices.extend(cell.item_indices.iter().copied());
        }

        let row_y = row
            .iter()
            .filter_map(|cell| cell.y)
            .reduce(f32::max)
            .unwrap_or(0.0);
        cells.push(row_cells);
        row_positions.push(row_y);
    }

    (cells, row_positions, all_item_indices)
}

fn left_align_struct_rows(
    raw_rows: &[Vec<MatchedCell>],
    num_cols: usize,
) -> (Vec<Vec<String>>, Vec<f32>, Vec<usize>) {
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(raw_rows.len());
    let mut row_positions: Vec<f32> = Vec::with_capacity(raw_rows.len());
    let mut all_item_indices: Vec<usize> = Vec::new();

    for row in raw_rows {
        let mut row_cells: Vec<String> = row.iter().map(|cell| cell.text.clone()).collect();
        row_cells.truncate(num_cols);
        while row_cells.len() < num_cols {
            row_cells.push(String::new());
        }
        cells.push(row_cells);

        all_item_indices.extend(
            row.iter()
                .flat_map(|cell| cell.item_indices.iter().copied()),
        );
        row_positions.push(
            row.iter()
                .filter_map(|cell| cell.y)
                .reduce(f32::max)
                .unwrap_or(0.0),
        );
    }

    (cells, row_positions, all_item_indices)
}

fn recover_unclaimed_header_row(table: &mut Table, items: &[TextItem], has_ragged_rows: bool) {
    if !has_ragged_rows || table.rows.is_empty() || table.columns.len() < 3 {
        return;
    }

    const MAX_HEADER_DISTANCE: f32 = 90.0;
    const MAX_GAP_TO_TABLE: f32 = 35.0;
    const MAX_INTER_HEADER_GAP: f32 = 25.0;
    const MAX_HEADER_ROWS: usize = 3;
    const Y_TOLERANCE: f32 = 5.0;

    let top_row_y = table.rows[0];
    let x_min = table.columns.first().copied().unwrap_or(0.0) - 25.0;
    let x_max = table.columns.last().copied().unwrap_or(0.0) + 120.0;
    let claimed: HashSet<usize> = table.item_indices.iter().copied().collect();

    let mut candidate_rows: Vec<(f32, Vec<(usize, &TextItem)>)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if claimed.contains(&idx)
            || item.text.trim().is_empty()
            || item.y <= top_row_y
            || item.y - top_row_y > MAX_HEADER_DISTANCE
            || item.x < x_min
            || item.x > x_max
        {
            continue;
        }

        if let Some((_, row_items)) = candidate_rows
            .iter_mut()
            .find(|(row_y, _)| (item.y - *row_y).abs() < Y_TOLERANCE)
        {
            row_items.push((idx, item));
        } else {
            candidate_rows.push((item.y, vec![(idx, item)]));
        }
    }

    if candidate_rows.is_empty() {
        return;
    }

    for (_, row_items) in &mut candidate_rows {
        row_items.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
    }
    candidate_rows.sort_by(|a, b| a.0.total_cmp(&b.0));

    if candidate_rows[0].0 - top_row_y > MAX_GAP_TO_TABLE {
        return;
    }

    let mut candidate_iter = candidate_rows.into_iter();
    let Some(first_row) = candidate_iter.next() else {
        return;
    };
    let mut selected_rows: Vec<(f32, Vec<(usize, &TextItem)>)> = vec![first_row];
    let mut prev_y = selected_rows[0].0;
    for (row_y, row_items) in candidate_iter {
        if selected_rows.len() >= MAX_HEADER_ROWS {
            break;
        }
        if row_y - prev_y > MAX_INTER_HEADER_GAP {
            break;
        }
        prev_y = row_y;
        selected_rows.push((row_y, row_items));
    }

    if selected_rows.is_empty() {
        return;
    }

    let mut assigned_rows: Vec<(f32, Vec<String>, Vec<usize>)> = Vec::new();
    let mut closest_row_populated = 0usize;
    let mut combined_cols: HashSet<usize> = HashSet::new();

    for (row_idx, (row_y, row_items)) in selected_rows.iter().enumerate() {
        if row_items.len() > table.columns.len() {
            return;
        }

        let row_xs: Vec<f32> = row_items.iter().map(|(_, item)| item.x).collect();
        let assignments = align_positions_to_columns(&row_xs, &table.columns);
        if assignments.len() != row_items.len() {
            return;
        }

        let mut row_cells = vec![String::new(); table.columns.len()];
        let mut row_indices = Vec::with_capacity(row_items.len());
        let mut populated_cols: HashSet<usize> = HashSet::new();

        for ((idx, item), &col_idx) in row_items.iter().zip(assignments.iter()) {
            let text = item.text.trim();
            if text.is_empty() {
                continue;
            }
            if !row_cells[col_idx].is_empty() {
                row_cells[col_idx].push(' ');
            }
            row_cells[col_idx].push_str(text);
            row_indices.push(*idx);
            populated_cols.insert(col_idx);
        }

        if row_idx == 0 {
            closest_row_populated = populated_cols.len();
        }

        combined_cols.extend(populated_cols.iter().copied());
        assigned_rows.push((*row_y, row_cells, row_indices));
    }

    let required_cols = if table.columns.len() <= 4 {
        table.columns.len()
    } else {
        table.columns.len() - 1
    };
    if closest_row_populated < 2 || combined_cols.len() < required_cols {
        return;
    }

    let mut header_cells = vec![String::new(); table.columns.len()];
    let mut header_indices = Vec::new();
    for (_, row_cells, row_indices) in assigned_rows.iter().rev() {
        for (col_idx, cell_text) in row_cells.iter().enumerate() {
            if cell_text.is_empty() {
                continue;
            }
            if !header_cells[col_idx].is_empty() {
                header_cells[col_idx].push(' ');
            }
            header_cells[col_idx].push_str(cell_text);
        }
        header_indices.extend(row_indices.iter().copied());
    }

    table.rows.insert(
        0,
        assigned_rows
            .iter()
            .map(|(row_y, _, _)| *row_y)
            .reduce(f32::max)
            .unwrap_or(top_row_y),
    );
    table.cells.insert(0, header_cells);
    table.item_indices.extend(header_indices);
    table.item_indices.sort_unstable();
    table.item_indices.dedup();
}

/// Build tables from structure-tree table descriptors by matching MCIDs to TextItems.
///
/// Returns tables for the given page.  Tables where fewer than 50% of cells
/// resolve to TextItems are rejected (stale or broken structure tree).
pub fn detect_tables_from_struct_tree(
    items: &[TextItem],
    struct_tables: &[StructTable],
    page: u32,
) -> Vec<Table> {
    if struct_tables.is_empty() {
        return Vec::new();
    }

    // Build MCID → item indices for this page
    let mut mcid_to_items: HashMap<i64, Vec<usize>> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if item.page == page {
            if let Some(mcid) = item.mcid {
                mcid_to_items.entry(mcid).or_default().push(idx);
            }
        }
    }

    let mut tables = Vec::new();

    for st in struct_tables {
        // Filter rows to this page
        let page_rows: Vec<_> = st
            .rows
            .iter()
            .filter(|row| {
                row.cells
                    .iter()
                    .any(|cell| cell.mcids.iter().any(|&(_, p)| p == page))
            })
            .collect();

        debug!(
            "page {}: struct table has {} rows on this page (from {} total)",
            page,
            page_rows.len(),
            st.rows.len()
        );

        if page_rows.len() < 2 {
            continue;
        }

        // Determine column count from max cells per row
        let num_cols = page_rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if num_cols < 2 {
            continue;
        }

        // Build cell text and geometry for alignment and header recovery.
        let mut raw_rows: Vec<Vec<MatchedCell>> = Vec::new();
        let mut total_cells = 0u32;
        let mut matched_cells = 0u32;

        for row in &page_rows {
            let mut row_cells = Vec::with_capacity(row.cells.len());
            for cell in &row.cells {
                total_cells += 1;

                // Collect all items for this cell's MCIDs
                let mut cell_items: Vec<(usize, &TextItem)> = Vec::new();
                for &(mcid, p) in &cell.mcids {
                    if p == page {
                        if let Some(indices) = mcid_to_items.get(&mcid) {
                            for &idx in indices {
                                cell_items.push((idx, &items[idx]));
                            }
                        }
                    }
                }

                if !cell_items.is_empty() {
                    matched_cells += 1;
                }

                // Sort by Y (descending = top-to-bottom) then X
                cell_items.sort_by(|a, b| {
                    b.1.y
                        .partial_cmp(&a.1.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(
                            a.1.x
                                .partial_cmp(&b.1.x)
                                .unwrap_or(std::cmp::Ordering::Equal),
                        )
                });

                let text: String = cell_items
                    .iter()
                    .map(|(_, item)| item.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");

                let item_indices = cell_items.iter().map(|(idx, _)| *idx).collect::<Vec<_>>();
                let x = cell_items.iter().map(|(_, item)| item.x).reduce(f32::min);
                let y = cell_items.iter().map(|(_, item)| item.y).reduce(f32::max);

                row_cells.push(MatchedCell {
                    text,
                    item_indices,
                    x,
                    y,
                });
            }
            raw_rows.push(row_cells);
        }

        // Reject if too few cells matched (stale structure tree)
        let coverage = if total_cells > 0 {
            matched_cells as f32 / total_cells as f32
        } else {
            0.0
        };
        debug!(
            "page {}: struct table {}x{}, {}/{} cells matched ({:.0}%)",
            page,
            page_rows.len(),
            num_cols,
            matched_cells,
            total_cells,
            coverage * 100.0
        );
        if total_cells == 0 || coverage < 0.3 {
            continue;
        }

        let has_ragged_rows = raw_rows
            .iter()
            .any(|row| row.iter().filter(|cell| cell.x.is_some()).count() < num_cols);
        let first_row_has_tagged_header = page_rows.first().is_some_and(|row| {
            let header_cells = row.cells.iter().filter(|cell| cell.is_header).count();
            header_cells * 2 >= row.cells.len()
        });
        let fallback_col_positions =
            legacy_column_positions(&page_rows, &mcid_to_items, items, page, num_cols);
        let (legacy_cells, legacy_row_positions, mut legacy_item_indices) =
            left_align_struct_rows(&raw_rows, num_cols);
        legacy_item_indices.sort_unstable();
        legacy_item_indices.dedup();
        let legacy_table = Table::new(
            fallback_col_positions.clone(),
            legacy_row_positions,
            legacy_cells,
            legacy_item_indices,
        );

        let col_positions = infer_column_positions(&raw_rows, &fallback_col_positions, num_cols);
        let (aligned_cells, aligned_row_positions, mut aligned_item_indices) =
            align_struct_rows(&raw_rows, &col_positions);
        aligned_item_indices.sort_unstable();
        aligned_item_indices.dedup();

        let mut aligned_table = Table::new(
            col_positions,
            aligned_row_positions,
            aligned_cells,
            aligned_item_indices,
        );
        let item_count_before_header = aligned_table.item_indices.len();
        let row_count_before_header = aligned_table.cells.len();
        recover_unclaimed_header_row(
            &mut aligned_table,
            items,
            has_ragged_rows && !first_row_has_tagged_header,
        );

        let recovered_header = aligned_table.item_indices.len() > item_count_before_header
            || aligned_table.cells.len() > row_count_before_header;
        let prefer_aligned = recovered_header;

        tables.push(if prefer_aligned {
            aligned_table
        } else {
            legacy_table
        });
    }

    tables
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure_tree::{StructTableCell, StructTableRow};
    use crate::types::ItemType;

    fn make_item(text: &str, x: f32, y: f32, page: u32, mcid: Option<i64>) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * 5.0,
            height: 10.0,
            font: "Test".to_string(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid,
        }
    }

    #[test]
    fn basic_struct_table() {
        let items = vec![
            make_item("Name", 50.0, 700.0, 1, Some(10)),
            make_item("Age", 200.0, 700.0, 1, Some(11)),
            make_item("Alice", 50.0, 680.0, 1, Some(20)),
            make_item("30", 200.0, 680.0, 1, Some(21)),
            make_item("Bob", 50.0, 660.0, 1, Some(30)),
            make_item("25", 200.0, 660.0, 1, Some(31)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(11, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(30, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(31, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells.len(), 3);
        assert_eq!(table.cells[0], vec!["Name", "Age"]);
        assert_eq!(table.cells[1], vec!["Alice", "30"]);
        assert_eq!(table.cells[2], vec!["Bob", "25"]);
        assert_eq!(table.item_indices.len(), 6);
    }

    #[test]
    fn rejects_low_mcid_coverage() {
        // Items have no MCIDs matching the struct table
        let items = vec![
            make_item("Orphan", 50.0, 700.0, 1, Some(999)),
            make_item("Text", 200.0, 700.0, 1, None),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(11, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert!(
            tables.is_empty(),
            "should reject table with no MCID matches"
        );
    }

    #[test]
    fn filters_by_page() {
        let items = vec![
            make_item("A", 50.0, 700.0, 2, Some(10)),
            make_item("B", 200.0, 700.0, 2, Some(11)),
            make_item("C", 50.0, 680.0, 2, Some(20)),
            make_item("D", 200.0, 680.0, 2, Some(21)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(10, 2)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(11, 2)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 2)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 2)],
                        },
                    ],
                },
            ],
        }];

        // Page 1 should find nothing
        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert!(tables.is_empty());

        // Page 2 should find the table
        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 2);
        assert_eq!(tables.len(), 1);
    }

    #[test]
    fn realigns_ragged_rows_and_recovers_untagged_header() {
        let items = vec![
            make_item("Category", 50.0, 120.0, 1, None),
            make_item("Potentially", 150.0, 120.0, 1, None),
            make_item("Summary", 250.0, 120.0, 1, None),
            make_item("Most commonly", 350.0, 120.0, 1, None),
            make_item("concerning aspect", 150.0, 110.0, 1, None),
            make_item("suggested", 350.0, 110.0, 1, None),
            make_item("of circumstances", 150.0, 100.0, 1, None),
            make_item("intervention", 350.0, 100.0, 1, None),
            make_item("Existence of red-teaming", 150.0, 80.0, 1, Some(10)),
            make_item("Important for safety", 250.0, 80.0, 1, Some(11)),
            make_item("Ensure welfare interviews", 350.0, 80.0, 1, Some(12)),
            make_item("Identity & self-knowledge", 50.0, 60.0, 1, Some(20)),
            make_item("Lack of knowledge", 150.0, 60.0, 1, Some(21)),
            make_item("Overall negative", 250.0, 60.0, 1, Some(22)),
            make_item("Describe training process", 350.0, 60.0, 1, Some(23)),
            make_item("Uncertainty around other copies", 150.0, 40.0, 1, Some(30)),
            make_item("High uncertainty", 250.0, 40.0, 1, Some(31)),
            make_item("No intervention suggested", 350.0, 40.0, 1, Some(32)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(11, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(12, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(22, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(23, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(30, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(31, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(32, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells.len(), 4);
        assert_eq!(
            table.cells[0],
            vec![
                "Category",
                "Potentially concerning aspect of circumstances",
                "Summary",
                "Most commonly suggested intervention",
            ]
        );
        assert_eq!(table.cells[1][0], "");
        assert_eq!(table.cells[1][1], "Existence of red-teaming");
        assert_eq!(table.cells[2][0], "Identity & self-knowledge");
        assert_eq!(table.cells[3][0], "");
        assert_eq!(table.columns.len(), 4);
        assert!(table.columns.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(table.item_indices.len(), items.len());
    }

    #[test]
    fn does_not_absorb_caption_above_ragged_struct_table() {
        let items = vec![
            make_item("Table 5-7: Summary of responses", 50.0, 120.0, 1, None),
            make_item("Aspect one", 150.0, 80.0, 1, Some(10)),
            make_item("Summary one", 250.0, 80.0, 1, Some(11)),
            make_item("Category", 50.0, 60.0, 1, Some(20)),
            make_item("Aspect two", 150.0, 60.0, 1, Some(21)),
            make_item("Summary two", 250.0, 60.0, 1, Some(22)),
            make_item("Aspect three", 150.0, 40.0, 1, Some(30)),
            make_item("Summary three", 250.0, 40.0, 1, Some(31)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(11, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(22, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(30, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(31, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells.len(), 3);
        assert!(
            table
                .cells
                .iter()
                .flatten()
                .all(|cell| !cell.contains("Table 5-7")),
            "caption must stay outside the table"
        );
        assert!(!table.item_indices.contains(&0));
    }

    #[test]
    fn keeps_existing_tagged_header_without_absorbing_intro_or_caption() {
        let items = vec![
            make_item(
                "Eighteen people left other comments regarding the Project.",
                50.0,
                130.0,
                1,
                None,
            ),
            make_item("Table 5-1:", 220.0, 130.0, 1, None),
            make_item("Other Comments", 350.0, 130.0, 1, None),
            make_item("Theme", 50.0, 110.0, 1, Some(10)),
            make_item("Specific Concern/Inquiry", 200.0, 110.0, 1, Some(11)),
            make_item("Response", 420.0, 110.0, 1, Some(12)),
            make_item("Traffic", 50.0, 90.0, 1, Some(20)),
            make_item("Road conditions", 200.0, 90.0, 1, Some(21)),
            make_item("Maintenance response", 420.0, 90.0, 1, Some(22)),
            make_item("Noise", 50.0, 70.0, 1, Some(30)),
            make_item("Dust concerns", 200.0, 70.0, 1, Some(31)),
            make_item("Mitigation response", 420.0, 70.0, 1, Some(32)),
            make_item("Resource Use", 50.0, 50.0, 1, Some(40)),
            make_item("Snowmobile trails", 200.0, 50.0, 1, Some(41)),
            make_item("Access response", 420.0, 50.0, 1, Some(42)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(11, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(12, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(22, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(30, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(31, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(32, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(40, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(41, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(42, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells.len(), 4);
        assert_eq!(
            table.cells[0],
            vec!["Theme", "Specific Concern/Inquiry", "Response"]
        );
        assert!(!table.item_indices.contains(&0));
        assert!(!table.item_indices.contains(&1));
        assert!(!table.item_indices.contains(&2));
    }

    #[test]
    fn does_not_recover_header_for_narrow_two_column_table() {
        let items = vec![
            make_item("Alpha", 50.0, 120.0, 1, None),
            make_item("Beta", 200.0, 120.0, 1, None),
            make_item("First value", 200.0, 80.0, 1, Some(10)),
            make_item("Only labeled row", 50.0, 60.0, 1, Some(20)),
            make_item("Second value", 200.0, 60.0, 1, Some(21)),
            make_item("Third value", 200.0, 40.0, 1, Some(30)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![StructTableCell {
                        is_header: false,
                        mcids: vec![(10, 1)],
                    }],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![StructTableCell {
                        is_header: false,
                        mcids: vec![(30, 1)],
                    }],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells.len(), 3);
        assert_eq!(table.cells[0], vec!["First value", ""]);
        assert_eq!(table.cells[1], vec!["Only labeled row", "Second value"]);
        assert_eq!(table.cells[2], vec!["Third value", ""]);
        assert!(!table.item_indices.contains(&0));
        assert!(!table.item_indices.contains(&1));
    }

    #[test]
    fn ragged_rows_without_recovered_header_keep_legacy_alignment() {
        let items = vec![
            make_item("Date", 150.0, 120.0, 1, Some(10)),
            make_item("Title", 250.0, 120.0, 1, Some(11)),
            make_item("PE", 350.0, 120.0, 1, Some(12)),
            make_item("Bidder", 450.0, 120.0, 1, Some(13)),
            make_item("Amount", 550.0, 120.0, 1, Some(14)),
            make_item("1", 50.0, 100.0, 1, Some(20)),
            make_item("8/1", 150.0, 100.0, 1, Some(21)),
            make_item("Procurement", 250.0, 100.0, 1, Some(22)),
            make_item("PUC", 350.0, 100.0, 1, Some(23)),
            make_item("Vendor", 450.0, 100.0, 1, Some(24)),
            make_item("SR1", 550.0, 100.0, 1, Some(25)),
        ];

        let struct_tables = vec![StructTable {
            rows: vec![
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(10, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(11, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(12, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(13, 1)],
                        },
                        StructTableCell {
                            is_header: true,
                            mcids: vec![(14, 1)],
                        },
                    ],
                },
                StructTableRow {
                    cells: vec![
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(20, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(21, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(22, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(23, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(24, 1)],
                        },
                        StructTableCell {
                            is_header: false,
                            mcids: vec![(25, 1)],
                        },
                    ],
                },
            ],
        }];

        let tables = detect_tables_from_struct_tree(&items, &struct_tables, 1);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.cells[0][0], "Date");
        assert_eq!(table.cells[0][4], "Amount");
        assert_eq!(table.cells[0][5], "");
    }
}
