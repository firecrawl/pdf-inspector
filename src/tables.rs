//! Table detection and formatting
//!
//! Detects tabular data in PDF text items and converts to markdown tables.

use crate::extractor::TextItem;

/// A detected table
#[derive(Debug, Clone)]
pub struct Table {
    /// Column boundaries (x positions)
    pub columns: Vec<f32>,
    /// Row boundaries (y positions, descending order)
    pub rows: Vec<f32>,
    /// Cell contents indexed by (row, col)
    pub cells: Vec<Vec<String>>,
    /// Items that belong to this table
    pub item_indices: Vec<usize>,
}

/// Detect tables in a set of text items from a single page
pub fn detect_tables(items: &[TextItem], base_font_size: f32) -> Vec<Table> {
    if items.len() < 6 {
        return vec![];
    }

    // Tables typically use smaller font than body text
    let table_font_threshold = base_font_size * 0.90;

    // Find items that might be table content (smaller font)
    let table_candidates: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.font_size <= table_font_threshold && item.font_size >= 6.0)
        .collect();

    if table_candidates.len() < 6 {
        return vec![];
    }

    // Find table regions - contiguous Y ranges with dense content
    let regions = find_table_regions(&table_candidates);

    let mut tables = Vec::new();
    for (y_min, y_max) in regions {
        // Get items in this region
        let region_items: Vec<(usize, &TextItem)> = table_candidates
            .iter()
            .filter(|(_, item)| item.y >= y_min && item.y <= y_max)
            .cloned()
            .collect();

        if region_items.len() < 6 {
            continue;
        }

        // Detect column structure for this region
        if let Some(table) = detect_table_in_region(&region_items) {
            tables.push(table);
        }
    }

    tables
}

/// Find Y-regions that likely contain tables
fn find_table_regions(items: &[(usize, &TextItem)]) -> Vec<(f32, f32)> {
    if items.is_empty() {
        return vec![];
    }

    let mut y_positions: Vec<f32> = items.iter().map(|(_, i)| i.y).collect();
    y_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Find clusters of Y positions (table regions)
    let mut regions = Vec::new();
    let gap_threshold = 50.0; // Large Y gap suggests separate regions

    let mut region_start = y_positions[0];
    let mut region_end = y_positions[0];
    let mut region_count = 1;

    for &y in &y_positions[1..] {
        if y - region_end > gap_threshold {
            // End current region if it has enough items
            if region_count >= 4 {
                regions.push((region_start - 5.0, region_end + 5.0));
            }
            region_start = y;
            region_end = y;
            region_count = 1;
        } else {
            region_end = y;
            region_count += 1;
        }
    }

    // Don't forget last region
    if region_count >= 4 {
        regions.push((region_start - 5.0, region_end + 5.0));
    }

    regions
}

/// Detect a table within a specific region
fn detect_table_in_region(items: &[(usize, &TextItem)]) -> Option<Table> {
    // Find column boundaries
    let columns = find_column_boundaries(items);
    if columns.len() < 2 || columns.len() > 8 {
        return None;
    }

    // Find row boundaries
    let rows = find_row_boundaries(items);
    if rows.len() < 2 {
        return None;
    }

    // Verify this looks like a table: multiple items should align to columns
    let col_alignment = check_column_alignment(items, &columns);
    if col_alignment < 0.5 {
        // Less than 50% of items align to detected columns
        return None;
    }

    // Build the table grid - first collect items per cell, then join properly
    let mut cell_items: Vec<Vec<Vec<&TextItem>>> =
        vec![vec![Vec::new(); columns.len()]; rows.len()];
    let mut item_indices = Vec::new();

    for (idx, item) in items {
        let col = find_column_index(&columns, item.x);
        let row = find_row_index(&rows, item.y);

        if let (Some(col), Some(row)) = (col, row) {
            cell_items[row][col].push(item);
            item_indices.push(*idx);
        }
    }

    // Sort items within each cell by X position and join with subscript-aware spacing
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row_items in &mut cell_items {
        let mut row_cells = Vec::with_capacity(columns.len());
        for col_items in row_items.iter_mut() {
            // Sort by X position
            col_items.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

            // Join items with subscript-aware spacing
            let text = join_cell_items(col_items);
            row_cells.push(text);
        }
        cells.push(row_cells);
    }

    // Validation 1: most rows should have content in first column
    let rows_with_first_col = cells.iter().filter(|row| !row[0].is_empty()).count();
    if rows_with_first_col < rows.len() / 2 {
        return None;
    }

    // Validation 2: real tables have content in MULTIPLE columns, not just first
    // At least 30% of rows should have content in 2+ columns
    let rows_with_multi_cols = cells
        .iter()
        .filter(|row| row.iter().filter(|c| !c.is_empty()).count() >= 2)
        .count();
    if rows_with_multi_cols < rows.len() / 3 {
        return None;
    }

    // Validation 3: tables shouldn't have too many rows (likely misdetected text)
    if rows.len() > 30 {
        return None;
    }

    // Validation 4: average cells per row should be reasonable
    let total_filled: usize = cells
        .iter()
        .map(|row| row.iter().filter(|c| !c.is_empty()).count())
        .sum();
    let avg_cells_per_row = total_filled as f32 / rows.len() as f32;
    if avg_cells_per_row < 1.5 {
        // Less than 1.5 cells per row on average - probably not a table
        return None;
    }

    Some(Table {
        columns,
        rows,
        cells,
        item_indices,
    })
}

/// Check what fraction of items align to detected columns
fn check_column_alignment(items: &[(usize, &TextItem)], columns: &[f32]) -> f32 {
    let tolerance = 40.0;
    let aligned = items
        .iter()
        .filter(|(_, item)| columns.iter().any(|&col| (item.x - col).abs() < tolerance))
        .count();

    aligned as f32 / items.len() as f32
}

/// Find column boundaries by clustering X positions
fn find_column_boundaries(items: &[(usize, &TextItem)]) -> Vec<f32> {
    let mut x_positions: Vec<f32> = items.iter().map(|(_, i)| i.x).collect();
    x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    if x_positions.is_empty() {
        return vec![];
    }

    // Use larger threshold for column detection
    let cluster_threshold = 60.0;
    let mut columns = Vec::new();
    let mut cluster_items: Vec<f32> = vec![x_positions[0]];

    for &x in &x_positions[1..] {
        let cluster_center = cluster_items.iter().sum::<f32>() / cluster_items.len() as f32;

        if x - cluster_center > cluster_threshold {
            // End current cluster
            columns.push(cluster_center);
            cluster_items = vec![x];
        } else {
            cluster_items.push(x);
        }
    }

    // Don't forget last cluster
    if !cluster_items.is_empty() {
        columns.push(cluster_items.iter().sum::<f32>() / cluster_items.len() as f32);
    }

    // Filter columns - each should have multiple items
    let min_items_per_col = (items.len() / columns.len().max(1) / 3).max(2);
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

/// Find row boundaries by clustering Y positions
fn find_row_boundaries(items: &[(usize, &TextItem)]) -> Vec<f32> {
    let mut y_positions: Vec<f32> = items.iter().map(|(_, i)| i.y).collect();
    y_positions.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)); // Descending

    if y_positions.is_empty() {
        return vec![];
    }

    // Cluster Y positions - items within 10px are same row
    let cluster_threshold = 10.0;
    let mut rows = Vec::new();
    let mut cluster_items: Vec<f32> = vec![y_positions[0]];

    for &y in &y_positions[1..] {
        let cluster_center = cluster_items.iter().sum::<f32>() / cluster_items.len() as f32;

        if cluster_center - y > cluster_threshold {
            // End current cluster (note: Y is descending)
            rows.push(cluster_center);
            cluster_items = vec![y];
        } else {
            cluster_items.push(y);
        }
    }

    if !cluster_items.is_empty() {
        rows.push(cluster_items.iter().sum::<f32>() / cluster_items.len() as f32);
    }

    rows
}

/// Find which column index an X position belongs to
fn find_column_index(columns: &[f32], x: f32) -> Option<usize> {
    let threshold = 60.0;
    columns
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (x - *a)
                .abs()
                .partial_cmp(&(x - *b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|(_, col_x)| (x - *col_x).abs() < threshold)
        .map(|(idx, _)| idx)
}

/// Find which row index a Y position belongs to
fn find_row_index(rows: &[f32], y: f32) -> Option<usize> {
    let threshold = 15.0;
    rows.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (y - *a)
                .abs()
                .partial_cmp(&(y - *b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|(_, row_y)| (y - *row_y).abs() < threshold)
        .map(|(idx, _)| idx)
}

/// Join cell items with subscript/superscript-aware spacing
/// Same logic as TextLine::text() but for table cells
fn join_cell_items(items: &[&TextItem]) -> String {
    let mut result = String::new();

    for (i, item) in items.iter().enumerate() {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }

        if result.is_empty() {
            result.push_str(text);
        } else {
            let prev_item = items[i - 1];

            // Don't add space before/after hyphens
            let prev_ends_with_hyphen = result.ends_with('-');
            let curr_is_hyphen = text == "-";
            let curr_starts_with_hyphen = text.starts_with('-');

            // Detect subscript/superscript: smaller font size and/or Y offset
            let font_ratio = item.font_size / prev_item.font_size;
            let reverse_font_ratio = prev_item.font_size / item.font_size;
            let y_diff = (item.y - prev_item.y).abs();

            // Current item is subscript/superscript (smaller than previous)
            let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
            // Previous item was subscript/superscript (returning to normal size)
            let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

            if prev_ends_with_hyphen
                || curr_is_hyphen
                || curr_starts_with_hyphen
                || is_sub_super
                || was_sub_super
            {
                result.push_str(text);
            } else {
                result.push(' ');
                result.push_str(text);
            }
        }
    }

    result
}

/// Format a table as markdown
pub fn table_to_markdown(table: &Table) -> String {
    if table.cells.is_empty() || table.cells[0].is_empty() {
        return String::new();
    }

    let num_cols = table.cells[0].len();
    let mut output = String::new();

    // Calculate column widths for alignment
    let col_widths: Vec<usize> = (0..num_cols)
        .map(|col| {
            table
                .cells
                .iter()
                .map(|row| row.get(col).map(|c| c.len()).unwrap_or(0))
                .max()
                .unwrap_or(3)
                .max(3)
        })
        .collect();

    // Output each row
    for (row_idx, row) in table.cells.iter().enumerate() {
        output.push('|');
        for (col_idx, cell) in row.iter().enumerate() {
            let width = col_widths[col_idx];
            output.push_str(&format!(" {:width$} |", cell, width = width));
        }
        output.push('\n');

        // Add separator after header row
        if row_idx == 0 {
            output.push('|');
            for width in &col_widths {
                output.push_str(&format!(" {} |", "-".repeat(*width)));
            }
            output.push('\n');
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(text: &str, x: f32, y: f32, font_size: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width: 10.0,
            height: font_size,
            font: "F1".into(),
            font_size,
            page: 1,
        }
    }

    #[test]
    fn test_table_detection() {
        let items = vec![
            make_item("Header 1", 100.0, 500.0, 8.0),
            make_item("Header 2", 200.0, 500.0, 8.0),
            make_item("Cell 1", 100.0, 480.0, 8.0),
            make_item("Cell 2", 200.0, 480.0, 8.0),
            make_item("Cell 3", 100.0, 460.0, 8.0),
            make_item("Cell 4", 200.0, 460.0, 8.0),
        ];

        let tables = detect_tables(&items, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns.len(), 2);
        assert_eq!(tables[0].rows.len(), 3);
    }

    #[test]
    fn test_table_to_markdown() {
        let table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0],
            cells: vec![
                vec!["Header 1".into(), "Header 2".into()],
                vec!["Cell 1".into(), "Cell 2".into()],
            ],
            item_indices: vec![],
        };

        let md = table_to_markdown(&table);
        assert!(md.contains("| Header 1"));
        assert!(md.contains("| ---"));
        assert!(md.contains("| Cell 1"));
    }
}
