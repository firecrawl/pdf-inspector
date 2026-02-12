//! Table detection and formatting
//!
//! Detects tabular data in PDF text items and converts to markdown tables.

use crate::extractor::TextItem;

/// Detection mode controls thresholds for table validation
#[derive(Debug, Clone, Copy, PartialEq)]
enum TableDetectionMode {
    /// Existing behavior: items with font size smaller than body text
    SmallFont,
    /// New: body-font items with stricter structural criteria
    BodyFont,
}

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

    let mut tables = Vec::new();
    let mut claimed_indices = std::collections::HashSet::new();

    // === Pass 1: Small-font tables (existing behavior) ===
    let table_font_threshold = base_font_size * 0.90;

    let table_candidates: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.font_size <= table_font_threshold && item.font_size >= 6.0)
        .collect();

    if table_candidates.len() >= 6 {
        let regions = find_table_regions(&table_candidates);

        for (y_min, y_max) in regions {
            let region_items: Vec<(usize, &TextItem)> = table_candidates
                .iter()
                .filter(|(_, item)| item.y >= y_min && item.y <= y_max)
                .cloned()
                .collect();

            if region_items.len() < 6 {
                continue;
            }

            if let Some(table) =
                detect_table_in_region(&region_items, TableDetectionMode::SmallFont)
            {
                for &idx in &table.item_indices {
                    claimed_indices.insert(idx);
                }
                tables.push(table);
            }
        }
    }

    // === Pass 2: Body-font tables (stricter criteria) ===
    let body_font_low = base_font_size * 0.85;
    let body_font_high = base_font_size * 1.05;

    let body_candidates: Vec<(usize, &TextItem)> = items
        .iter()
        .enumerate()
        .filter(|(idx, item)| {
            !claimed_indices.contains(idx)
                && item.font_size >= body_font_low
                && item.font_size <= body_font_high
                && item.font_size >= 6.0
        })
        .collect();

    if body_candidates.len() >= 9 {
        let regions = find_table_regions_strict(&body_candidates);

        for (y_min, y_max) in regions {
            let region_items: Vec<(usize, &TextItem)> = body_candidates
                .iter()
                .filter(|(_, item)| item.y >= y_min && item.y <= y_max)
                .cloned()
                .collect();

            if region_items.len() < 9 {
                continue;
            }

            if let Some(table) = detect_table_in_region(&region_items, TableDetectionMode::BodyFont)
            {
                tables.push(table);
            }
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
    let gap_threshold = 30.0; // Smaller gap threshold to separate header from content

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

/// Find Y-regions for body-font table candidates using strict structural criteria.
/// Requires rows with 3+ distinct X-position clusters to qualify, and verifies
/// that column positions are consistent across rows (tables have fixed columns,
/// paragraph text has varying word positions).
fn find_table_regions_strict(items: &[(usize, &TextItem)]) -> Vec<(f32, f32)> {
    if items.is_empty() {
        return vec![];
    }

    // Step 1: Group items by Y position (8pt tolerance for same row)
    let mut row_groups: Vec<(f32, Vec<f32>)> = Vec::new();
    for (_, item) in items {
        let mut found = false;
        for (center, x_positions) in row_groups.iter_mut() {
            if (item.y - *center).abs() < 8.0 {
                x_positions.push(item.x);
                found = true;
                break;
            }
        }
        if !found {
            row_groups.push((item.y, vec![item.x]));
        }
    }

    // Step 2: Filter to rows with 3+ distinct X-position clusters (20pt tolerance)
    // Collect cluster start positions for cross-row alignment analysis
    let mut qualifying_rows: Vec<(f32, Vec<f32>)> = Vec::new(); // (y, cluster_starts)
    for (y, x_positions) in &row_groups {
        let mut sorted_xs = x_positions.clone();
        sorted_xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if sorted_xs.is_empty() {
            continue;
        }

        let mut cluster_starts: Vec<f32> = vec![sorted_xs[0]];
        let mut last_x = sorted_xs[0];
        for &x in &sorted_xs[1..] {
            if x - last_x > 20.0 {
                cluster_starts.push(x);
                last_x = x;
            }
        }

        if cluster_starts.len() >= 3 {
            qualifying_rows.push((*y, cluster_starts));
        }
    }

    if qualifying_rows.len() < 3 {
        return vec![];
    }

    // Step 3: Find contiguous runs of qualifying rows (25pt max Y-gap)
    qualifying_rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut candidate_regions: Vec<Vec<&(f32, Vec<f32>)>> = Vec::new();
    let mut current_region: Vec<&(f32, Vec<f32>)> = vec![&qualifying_rows[0]];

    for row in qualifying_rows.iter().skip(1) {
        let prev_y = current_region.last().unwrap().0;
        if row.0 - prev_y > 25.0 {
            if current_region.len() >= 3 {
                candidate_regions.push(current_region);
            }
            current_region = vec![row];
        } else {
            current_region.push(row);
        }
    }
    if current_region.len() >= 3 {
        candidate_regions.push(current_region);
    }

    // Step 4: Cross-row column alignment check per region
    // Real tables have consistent column X positions across rows (high pairwise score).
    // Paragraph text has varying word positions line-to-line (low pairwise score).
    let mut regions = Vec::new();
    for region_rows in &candidate_regions {
        let num_rows = region_rows.len();
        let mut total_score = 0.0f32;
        let mut pair_count = 0u32;
        let tolerance = 10.0f32;

        for i in 0..num_rows {
            for j in (i + 1)..num_rows {
                let centers_a = &region_rows[i].1;
                let centers_b = &region_rows[j].1;

                let matches_a = centers_a
                    .iter()
                    .filter(|&&a| centers_b.iter().any(|&b| (a - b).abs() < tolerance))
                    .count();
                let matches_b = centers_b
                    .iter()
                    .filter(|&&b| centers_a.iter().any(|&a| (a - b).abs() < tolerance))
                    .count();

                let max_len = centers_a.len().max(centers_b.len());
                if max_len > 0 {
                    total_score += (matches_a + matches_b) as f32 / (2 * max_len) as f32;
                    pair_count += 1;
                }
            }
        }

        let avg_score = if pair_count > 0 {
            total_score / pair_count as f32
        } else {
            0.0
        };
        if avg_score >= 0.5 {
            let y_min = region_rows.first().unwrap().0;
            let y_max = region_rows.last().unwrap().0;
            regions.push((y_min - 5.0, y_max + 5.0));
        }
    }

    regions
}

/// Detect a table within a specific region
fn detect_table_in_region(items: &[(usize, &TextItem)], mode: TableDetectionMode) -> Option<Table> {
    // Find column boundaries
    let columns = find_column_boundaries(items, mode);
    let min_cols = match mode {
        TableDetectionMode::SmallFont => 2,
        TableDetectionMode::BodyFont => 3,
    };
    if columns.len() < min_cols || columns.len() > 15 {
        return None;
    }

    // Find row boundaries
    let rows = find_row_boundaries(items);
    let min_rows = match mode {
        TableDetectionMode::SmallFont => 2,
        TableDetectionMode::BodyFont => 3,
    };
    if rows.len() < min_rows {
        return None;
    }

    // Verify this looks like a table: multiple items should align to columns
    let col_alignment = check_column_alignment(items, &columns, mode);
    let min_alignment = match mode {
        TableDetectionMode::SmallFont => 0.5,
        TableDetectionMode::BodyFont => 0.7,
    };
    if col_alignment < min_alignment {
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

    // Detect form header rows and exclude their items
    // We need to do this BEFORE finalizing item_indices
    let (first_table_row, excluded_items) = find_first_table_row(&cell_items, &rows, items);

    // Remove excluded items from item_indices
    let item_indices: Vec<usize> = item_indices
        .into_iter()
        .filter(|idx| !excluded_items.contains(idx))
        .collect();

    // If we excluded rows, adjust the cell_items and rows
    let (rows, mut cell_items) = if first_table_row > 0 {
        let new_rows = rows[first_table_row..].to_vec();
        let new_cell_items = cell_items[first_table_row..].to_vec();
        (new_rows, new_cell_items)
    } else {
        (rows, cell_items)
    };

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
    let rows_with_multi_cols = cells
        .iter()
        .filter(|row| row.iter().filter(|c| !c.is_empty()).count() >= 2)
        .count();
    let multi_col_threshold = match mode {
        TableDetectionMode::SmallFont => (rows.len() / 3).max(1), // 33%
        TableDetectionMode::BodyFont => (rows.len() / 2).max(1),  // 50%
    };
    if rows_with_multi_cols < multi_col_threshold {
        return None;
    }

    // Validation 3: tables shouldn't have too many rows (likely misdetected text)
    let max_rows = match mode {
        TableDetectionMode::SmallFont => 50,
        TableDetectionMode::BodyFont => 100,
    };
    if rows.len() > max_rows {
        return None;
    }

    // Validation 4: average cells per row should be reasonable
    let total_filled: usize = cells
        .iter()
        .map(|row| row.iter().filter(|c| !c.is_empty()).count())
        .sum();
    let avg_cells_per_row = total_filled as f32 / rows.len() as f32;
    let min_avg_cells = match mode {
        TableDetectionMode::SmallFont => 1.5,
        TableDetectionMode::BodyFont => 2.5,
    };
    if avg_cells_per_row < min_avg_cells {
        return None;
    }

    // Validation 5: Check for key-value pair layout (NOT a table)
    // Key-value layouts have: mostly 2 filled columns, first column is labels
    if is_key_value_layout(&cells) {
        return None;
    }

    // Validation 6: Check column count consistency
    // Real tables have similar column counts across rows
    if !has_consistent_columns(&cells) {
        return None;
    }

    // Validation 7: Tables should have some numeric/data content
    // (not just text labels)
    if !has_table_like_content(&cells, mode) {
        return None;
    }

    // Validation 8: Check for Table of Contents pattern
    // TOCs have dots (leader lines) and page numbers, not real table data
    if is_table_of_contents(&cells) {
        return None;
    }

    Some(Table {
        columns,
        rows,
        cells,
        item_indices,
    })
}

/// Check if this looks like a key-value pair layout rather than a table
fn is_key_value_layout(cells: &[Vec<String>]) -> bool {
    if cells.is_empty() {
        return false;
    }

    let num_cols = cells[0].len();

    // Key-value layouts typically have 2-3 effective columns
    // where the first column contains labels ending with ":"
    let mut label_like_first_col = 0;
    let mut rows_with_two_or_less = 0;

    for row in cells {
        let filled_count = row.iter().filter(|c| !c.is_empty()).count();
        if filled_count <= 2 {
            rows_with_two_or_less += 1;
        }

        // Check if first column looks like a label (ends with : or is all caps)
        let first = row.first().map(|s| s.trim()).unwrap_or("");
        if first.ends_with(':')
            || (first.len() > 3
                && first
                    .chars()
                    .all(|c| c.is_uppercase() || c.is_whitespace() || c == '(' || c == ')'))
        {
            label_like_first_col += 1;
        }
    }

    // If most rows have only 2 columns filled and first column is label-like
    let pct_two_or_less = rows_with_two_or_less as f32 / cells.len() as f32;
    let pct_label_like = label_like_first_col as f32 / cells.len() as f32;

    // This is likely a key-value layout if:
    // - Most rows have 2 or fewer filled columns
    // - First column often looks like labels
    // - Total columns detected is 6 or fewer (real tables often have more)
    pct_two_or_less > 0.7 && pct_label_like > 0.5 && num_cols <= 6
}

/// Check if columns are consistent across rows (real tables have this)
fn has_consistent_columns(cells: &[Vec<String>]) -> bool {
    if cells.len() < 3 {
        return true; // Not enough rows to judge
    }

    // Count filled columns per row
    let filled_counts: Vec<usize> = cells
        .iter()
        .map(|row| row.iter().filter(|c| !c.is_empty()).count())
        .collect();

    // Find the most common filled count
    let mut count_freq: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for &count in &filled_counts {
        *count_freq.entry(count).or_insert(0) += 1;
    }

    let most_common_count = count_freq
        .iter()
        .max_by_key(|(_, freq)| *freq)
        .map(|(count, _)| *count)
        .unwrap_or(0);

    // At least 40% of rows should have the most common column count (or close to it)
    let consistent_rows = filled_counts
        .iter()
        .filter(|&&c| c >= most_common_count.saturating_sub(2) && c <= most_common_count + 2)
        .count();

    consistent_rows as f32 / cells.len() as f32 > 0.4
}

/// Check if the content looks like table data (numbers, short values, specs)
fn has_table_like_content(cells: &[Vec<String>], mode: TableDetectionMode) -> bool {
    let mut data_like_cells = 0;
    let mut total_cells = 0;

    for row in cells.iter().skip(1) {
        // Skip header row
        for cell in row {
            let trimmed = cell.trim();
            if !trimmed.is_empty() {
                total_cells += 1;
                // Check if it looks like table data
                if looks_like_table_data(trimmed) {
                    data_like_cells += 1;
                }
            }
        }
    }

    if total_cells == 0 {
        return false;
    }

    // Data-like content threshold depends on detection mode
    let pct_data = data_like_cells as f32 / total_cells as f32;
    let num_cols = cells.first().map(|r| r.len()).unwrap_or(0);

    let min_pct = match mode {
        TableDetectionMode::SmallFont => 0.2,
        TableDetectionMode::BodyFont => 0.3,
    };

    // For SmallFont, bypass content check for wide tables (5+ columns may have text headers).
    // For BodyFont, always require data-like content to prevent paragraph false positives.
    pct_data > min_pct || (mode == TableDetectionMode::SmallFont && num_cols >= 5)
}

/// Check if a cell value looks like table data
/// Includes: numbers, part numbers, specifications with units, codes
fn looks_like_table_data(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }

    // Pure numbers
    if looks_like_number(s) {
        return true;
    }

    // Part numbers / model codes (alphanumeric, typically short)
    // e.g., "NA555", "NE555", "LM358"
    if s.len() <= 10
        && s.chars().all(|c| c.is_alphanumeric())
        && s.chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }

    // Specifications with units (contains numbers and unit symbols)
    // e.g., "–40°C to +105°C", "5V", "200mA", "8-pin"
    let has_number = s.chars().any(|c| c.is_ascii_digit());
    let has_unit = s.contains('°')
        || s.contains('V')
        || s.contains('A')
        || s.contains("Hz")
        || s.contains("mA")
        || s.contains("µ")
        || s.contains("pin")
        || s.contains("MHz")
        || s.contains("kHz");
    if has_number && has_unit {
        return true;
    }

    // Package designations with parentheses
    // e.g., "D (SOIC, 8)", "P (PDIP, 8)"
    if s.contains('(') && s.contains(')') && s.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }

    // Temperature ranges
    // e.g., "TA = –40°C to +105°C"
    if (s.contains("°C") || s.contains("°F")) && s.contains("to") {
        return true;
    }

    false
}

/// Check if a string looks like a number
fn looks_like_number(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }

    // Handle common number formats: 9.0, 10, 8.6, etc.
    s.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || c == '-' || c == '+')
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Check if this looks like a Table of Contents
/// TOCs have characteristic patterns: leader dots, page numbers, section names
fn is_table_of_contents(cells: &[Vec<String>]) -> bool {
    if cells.is_empty() {
        return false;
    }

    let mut dot_cells = 0;
    let mut page_number_cells = 0;
    let mut total_cells = 0;

    for row in cells {
        for cell in row {
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            total_cells += 1;

            // Check for leader dots (sequences of periods)
            // TOCs often have "........" or ". . . ." patterns
            let dot_count = trimmed.chars().filter(|&c| c == '.').count();
            let is_mostly_dots = dot_count > trimmed.len() / 2 && dot_count >= 3;
            if is_mostly_dots {
                dot_cells += 1;
            }

            // Check for standalone page numbers (1-4 digits, possibly with spaces)
            let digits_only: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
            if digits_only.len() <= 4
                && !digits_only.is_empty()
                && digits_only.chars().all(|c| c.is_ascii_digit())
            {
                page_number_cells += 1;
            }
        }
    }

    if total_cells == 0 {
        return false;
    }

    // If a significant portion of cells are dots or page numbers, it's likely a TOC
    let dot_ratio = dot_cells as f32 / total_cells as f32;
    let page_num_ratio = page_number_cells as f32 / total_cells as f32;

    // TOC typically has >15% dot cells and >10% page number cells
    dot_ratio > 0.15 || (dot_ratio > 0.05 && page_num_ratio > 0.15)
}

/// Check what fraction of items align to detected columns
fn check_column_alignment(
    items: &[(usize, &TextItem)],
    columns: &[f32],
    mode: TableDetectionMode,
) -> f32 {
    let tolerance = match mode {
        TableDetectionMode::SmallFont => 40.0,
        TableDetectionMode::BodyFont => 30.0,
    };
    let aligned = items
        .iter()
        .filter(|(_, item)| columns.iter().any(|&col| (item.x - col).abs() < tolerance))
        .count();

    aligned as f32 / items.len() as f32
}

/// Find column boundaries by clustering X positions
fn find_column_boundaries(items: &[(usize, &TextItem)], mode: TableDetectionMode) -> Vec<f32> {
    let mut x_positions: Vec<f32> = items.iter().map(|(_, i)| i.x).collect();
    x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    if x_positions.is_empty() {
        return vec![];
    }

    // Calculate adaptive threshold based on X-position density
    // For dense tables (like grade tables), use smaller threshold
    let x_range = x_positions.last().unwrap() - x_positions.first().unwrap();
    let avg_gap = if x_positions.len() > 1 {
        x_range / (x_positions.len() - 1) as f32
    } else {
        60.0
    };

    // Use smaller threshold for dense data, larger for sparse
    let cluster_threshold = avg_gap.clamp(25.0, 50.0);

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
    let min_items_per_col = (items.len() / columns.len().max(1) / 4).max(2);
    let columns: Vec<f32> = columns
        .into_iter()
        .filter(|&col_x| {
            items
                .iter()
                .filter(|(_, i)| (i.x - col_x).abs() < cluster_threshold)
                .count()
                >= min_items_per_col
        })
        .collect();

    // Anti-paragraph safeguard for BodyFont mode:
    // Paragraphs concentrate items at the left margin; tables distribute evenly.
    // Reject if any single column has >60% of all items.
    if mode == TableDetectionMode::BodyFont {
        let total_items = items.len();
        for &col_x in &columns {
            let count = items
                .iter()
                .filter(|(_, i)| (i.x - col_x).abs() < cluster_threshold)
                .count();
            if count as f32 / total_items as f32 > 0.60 {
                return vec![];
            }
        }
    }

    columns
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
    // Calculate adaptive threshold based on column spacing
    let threshold = if columns.len() >= 2 {
        let min_gap = columns
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(f32::INFINITY, f32::min);
        (min_gap / 2.0).clamp(25.0, 50.0)
    } else {
        50.0
    };

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

    // Clean up the table: merge continuation rows, extract footnotes, remove empty rows
    let (cleaned_cells, footnotes) = clean_table_cells(&table.cells);

    if cleaned_cells.is_empty() {
        return String::new();
    }

    let num_cols = cleaned_cells[0].len();
    let mut output = String::new();

    // Calculate column widths for alignment
    let col_widths: Vec<usize> = (0..num_cols)
        .map(|col| {
            cleaned_cells
                .iter()
                .map(|row| row.get(col).map(|c| c.len()).unwrap_or(0))
                .max()
                .unwrap_or(3)
                .max(3)
        })
        .collect();

    // Output each row
    for (row_idx, row) in cleaned_cells.iter().enumerate() {
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

    // Add footnotes below the table
    if !footnotes.is_empty() {
        output.push('\n');
        for footnote in footnotes {
            output.push_str(&footnote);
            output.push('\n');
        }
    }

    output
}

/// Clean up table cells: merge continuation rows, extract footnotes, remove empty rows
fn clean_table_cells(cells: &[Vec<String>]) -> (Vec<Vec<String>>, Vec<String>) {
    let mut cleaned: Vec<Vec<String>> = Vec::new();
    let mut footnotes: Vec<String> = Vec::new();

    for row in cells {
        // Check if this row is empty
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }

        // Check if this row is a footnote (starts with (1), (2), etc. or just a number reference)
        let first_cell = row.first().map(|s| s.trim()).unwrap_or("");
        if is_footnote_row(first_cell) {
            // Combine all cells into a single footnote line
            let footnote_text: String = row
                .iter()
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            footnotes.push(footnote_text);
            continue;
        }

        // Check if this is a continuation row (first column is empty but others have content)
        let is_continuation = first_cell.is_empty()
            && row.iter().skip(1).any(|c| !c.trim().is_empty())
            && !cleaned.is_empty();

        if is_continuation {
            // Merge with previous row
            if let Some(prev_row) = cleaned.last_mut() {
                for (col_idx, cell) in row.iter().enumerate() {
                    let cell_text = cell.trim();
                    if !cell_text.is_empty() && col_idx < prev_row.len() {
                        if !prev_row[col_idx].is_empty() {
                            prev_row[col_idx].push(' ');
                        }
                        prev_row[col_idx].push_str(cell_text);
                    }
                }
            }
        } else {
            // Regular row - add as new row
            cleaned.push(row.iter().map(|c| c.trim().to_string()).collect());
        }
    }

    (cleaned, footnotes)
}

/// Find the first row that looks like actual table data (not form header)
/// Returns (first_table_row_index, set of item indices to exclude)
fn find_first_table_row(
    cell_items: &[Vec<Vec<&TextItem>>],
    rows: &[f32],
    original_items: &[(usize, &TextItem)],
) -> (usize, std::collections::HashSet<usize>) {
    let mut excluded_items = std::collections::HashSet::new();

    // Build string cells for analysis
    let cells: Vec<Vec<String>> = cell_items
        .iter()
        .map(|row| row.iter().map(|col| join_cell_items(col)).collect())
        .collect();

    if cells.is_empty() {
        return (0, excluded_items);
    }

    // Strategy: Skip leading rows that look like form metadata
    //
    // Form/metadata rows have:
    // 1. Cells ending with ":" (form labels)
    // 2. Very sparse fill with document metadata (grade level, year, etc.)
    //
    // Table rows have:
    // 1. Dense fill (headers spanning columns)
    // 2. Numeric content (data rows)
    // 3. No form label patterns

    let total_cols = cells[0].len();
    let mut first_table_row = 0;

    for (row_idx, row) in cells.iter().enumerate() {
        let filled_cells: Vec<&String> = row.iter().filter(|c| !c.trim().is_empty()).collect();
        let filled_count = filled_cells.len();
        let fill_ratio = filled_count as f32 / total_cols as f32;

        // Check for form-like patterns (cells with colons)
        let has_form_patterns = filled_cells.iter().any(|c| {
            let text = c.trim();
            (text.ends_with(':') && text.len() > 1)
                || (text.contains(": ") && !looks_like_number(text))
        });

        // Check for numeric content
        let numeric_count = filled_cells
            .iter()
            .filter(|c| looks_like_number(c.trim()))
            .count();
        let has_data = numeric_count >= 2;

        // Skip rows with form patterns (regardless of density)
        if has_form_patterns {
            continue;
        }

        // Data rows are definitely table content
        if has_data {
            first_table_row = row_idx;
            break;
        }

        // Dense rows without form patterns are likely table headers
        if fill_ratio >= 0.4 {
            first_table_row = row_idx;
            break;
        }

        // Very sparse rows at the start are likely metadata - skip them
        if fill_ratio < 0.3 {
            continue;
        }

        // Moderately sparse row without form patterns - could be multi-line header
        // Look ahead to decide
        if row_idx + 1 < cells.len() {
            let next_row = &cells[row_idx + 1];
            let next_filled = next_row.iter().filter(|c| !c.trim().is_empty()).count();
            let next_fill_ratio = next_filled as f32 / total_cols as f32;
            let next_has_form = next_row.iter().any(|c| {
                let text = c.trim();
                (text.ends_with(':') && text.len() > 1)
                    || (text.contains(": ") && !looks_like_number(text))
            });

            // If next row is dense or has data (and no form patterns), this row starts the table
            if (next_fill_ratio >= 0.4
                || next_row
                    .iter()
                    .filter(|c| looks_like_number(c.trim()))
                    .count()
                    >= 2)
                && !next_has_form
            {
                first_table_row = row_idx;
                break;
            }
        }

        // Otherwise skip this sparse row
    }

    // Collect item indices from excluded rows
    if first_table_row > 0 {
        let y_tolerance = 15.0;
        for (idx, item) in original_items {
            // Check if this item is in one of the excluded rows
            for row_y in rows.iter().take(first_table_row) {
                if (item.y - *row_y).abs() < y_tolerance {
                    excluded_items.insert(*idx);
                    break;
                }
            }
        }
    }

    (first_table_row, excluded_items)
}

/// Check if a cell value indicates a footnote row
fn is_footnote_row(text: &str) -> bool {
    let trimmed = text.trim();

    // Check for common footnote patterns
    // (1), (2), etc.
    if trimmed.starts_with('(') && trimmed.len() >= 2 {
        let inside = &trimmed[1..];
        if let Some(close_idx) = inside.find(')') {
            let num_part = &inside[..close_idx];
            if num_part.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // 1), 2), etc.
    if trimmed.len() >= 2 {
        if let Some(paren_idx) = trimmed.find(')') {
            let num_part = &trimmed[..paren_idx];
            if !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // Check for "Note:" or "Notes:" at the start
    let lower = trimmed.to_lowercase();
    if lower.starts_with("note:") || lower.starts_with("notes:") {
        return true;
    }

    false
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
            is_bold: false,
            is_italic: false,
            item_type: crate::extractor::ItemType::Text,
        }
    }

    #[test]
    fn test_table_detection() {
        // Create a more realistic table with numeric data (like grades)
        let items = vec![
            // Header row
            make_item("Subject", 100.0, 500.0, 8.0),
            make_item("Q1", 200.0, 500.0, 8.0),
            make_item("Q2", 280.0, 500.0, 8.0),
            make_item("Q3", 360.0, 500.0, 8.0),
            // Data row 1
            make_item("Math", 100.0, 480.0, 8.0),
            make_item("9.0", 200.0, 480.0, 8.0),
            make_item("8.5", 280.0, 480.0, 8.0),
            make_item("9.5", 360.0, 480.0, 8.0),
            // Data row 2
            make_item("Science", 100.0, 460.0, 8.0),
            make_item("8.0", 200.0, 460.0, 8.0),
            make_item("9.0", 280.0, 460.0, 8.0),
            make_item("8.5", 360.0, 460.0, 8.0),
            // Data row 3
            make_item("English", 100.0, 440.0, 8.0),
            make_item("9.5", 200.0, 440.0, 8.0),
            make_item("9.0", 280.0, 440.0, 8.0),
            make_item("9.5", 360.0, 440.0, 8.0),
        ];

        let tables = detect_tables(&items, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns.len(), 4);
        assert_eq!(tables[0].rows.len(), 4);
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

    #[test]
    fn test_body_font_table_detected() {
        // 4-column, 4-row table at font_size == base_font_size
        // Pass 1 rejects (not small font), Pass 2 should detect
        let items = vec![
            // Header row
            make_item("Name", 100.0, 500.0, 10.0),
            make_item("Price", 200.0, 500.0, 10.0),
            make_item("Qty", 300.0, 500.0, 10.0),
            make_item("Total", 400.0, 500.0, 10.0),
            // Data row 1
            make_item("Widget", 100.0, 480.0, 10.0),
            make_item("5.00", 200.0, 480.0, 10.0),
            make_item("10", 300.0, 480.0, 10.0),
            make_item("50.00", 400.0, 480.0, 10.0),
            // Data row 2
            make_item("Gadget", 100.0, 460.0, 10.0),
            make_item("12.50", 200.0, 460.0, 10.0),
            make_item("4", 300.0, 460.0, 10.0),
            make_item("50.00", 400.0, 460.0, 10.0),
            // Data row 3
            make_item("Gizmo", 100.0, 440.0, 10.0),
            make_item("3.25", 200.0, 440.0, 10.0),
            make_item("20", 300.0, 440.0, 10.0),
            make_item("65.00", 400.0, 440.0, 10.0),
        ];

        let tables = detect_tables(&items, 10.0);
        assert_eq!(
            tables.len(),
            1,
            "Body-font table should be detected by Pass 2"
        );
        assert_eq!(tables[0].columns.len(), 4);
        assert!(tables[0].rows.len() >= 3);
    }

    #[test]
    fn test_paragraph_not_falsely_detected() {
        // Body-font single-column paragraph text — must return 0 tables
        let items = vec![
            make_item(
                "This is a paragraph of text that spans the full width",
                72.0,
                500.0,
                10.0,
            ),
            make_item(
                "of the page and should not be detected as a table.",
                72.0,
                485.0,
                10.0,
            ),
            make_item(
                "It continues for several lines with normal body text",
                72.0,
                470.0,
                10.0,
            ),
            make_item(
                "that is left-aligned and has no columnar structure.",
                72.0,
                455.0,
                10.0,
            ),
            make_item(
                "The paragraph keeps going with more content here.",
                72.0,
                440.0,
                10.0,
            ),
            make_item(
                "And it has even more text on this line as well.",
                72.0,
                425.0,
                10.0,
            ),
            make_item(
                "Finally the paragraph concludes with this last line.",
                72.0,
                410.0,
                10.0,
            ),
            make_item(
                "One more line to have enough items for detection.",
                72.0,
                395.0,
                10.0,
            ),
            make_item(
                "And another line of plain paragraph text content.",
                72.0,
                380.0,
                10.0,
            ),
            make_item(
                "Last line of the paragraph ends here for the test.",
                72.0,
                365.0,
                10.0,
            ),
        ];

        let tables = detect_tables(&items, 10.0);
        assert_eq!(
            tables.len(),
            0,
            "Single-column paragraph must not be detected as table"
        );
    }

    #[test]
    fn test_word_level_paragraph_not_detected_as_table() {
        // Paragraph text with per-word TextItems (as produced by some PDFs).
        // Word X positions vary from line to line — NOT a table.
        let items = vec![
            // Line 1
            make_item("We", 72.0, 500.0, 10.0),
            make_item("would", 95.0, 500.0, 10.0),
            make_item("like", 145.0, 500.0, 10.0),
            make_item("to", 180.0, 500.0, 10.0),
            make_item("thank", 200.0, 500.0, 10.0),
            make_item("all", 250.0, 500.0, 10.0),
            make_item("the", 278.0, 500.0, 10.0),
            make_item("practitioners", 305.0, 500.0, 10.0),
            // Line 2
            make_item("and", 72.0, 485.0, 10.0),
            make_item("researchers", 105.0, 485.0, 10.0),
            make_item("across", 185.0, 485.0, 10.0),
            make_item("the", 232.0, 485.0, 10.0),
            make_item("University", 260.0, 485.0, 10.0),
            make_item("of", 335.0, 485.0, 10.0),
            make_item("Leeds", 355.0, 485.0, 10.0),
            // Line 3
            make_item("Libraries", 72.0, 470.0, 10.0),
            make_item("whose", 142.0, 470.0, 10.0),
            make_item("contributions", 190.0, 470.0, 10.0),
            make_item("made", 290.0, 470.0, 10.0),
            make_item("this", 328.0, 470.0, 10.0),
            make_item("report", 360.0, 470.0, 10.0),
            // Line 4
            make_item("possible", 72.0, 455.0, 10.0),
            make_item("Both", 140.0, 455.0, 10.0),
            make_item("constituent", 178.0, 455.0, 10.0),
            make_item("studies", 262.0, 455.0, 10.0),
            make_item("were", 315.0, 455.0, 10.0),
            make_item("approved", 350.0, 455.0, 10.0),
        ];

        let tables = detect_tables(&items, 10.0);
        assert_eq!(
            tables.len(),
            0,
            "Word-level paragraph text must not be detected as table"
        );
    }

    #[test]
    fn test_large_data_table_not_rejected() {
        // 50-row table at small font — must not be rejected by row limit
        let mut items = Vec::new();
        // Header row
        items.push(make_item("Temp", 100.0, 800.0, 8.0));
        items.push(make_item("Pressure", 200.0, 800.0, 8.0));
        items.push(make_item("Volume", 300.0, 800.0, 8.0));
        items.push(make_item("Enthalpy", 400.0, 800.0, 8.0));

        // 49 data rows
        for i in 1..50 {
            let y = 800.0 - (i as f32 * 12.0);
            items.push(make_item(&format!("{}", -40 + i * 2), 100.0, y, 8.0));
            items.push(make_item(
                &format!("{:.1}", 100.0 + i as f32 * 5.0),
                200.0,
                y,
                8.0,
            ));
            items.push(make_item(
                &format!("{:.3}", 0.05 + i as f32 * 0.01),
                300.0,
                y,
                8.0,
            ));
            items.push(make_item(
                &format!("{:.1}", 150.0 + i as f32 * 2.5),
                400.0,
                y,
                8.0,
            ));
        }

        let tables = detect_tables(&items, 10.0);
        assert_eq!(tables.len(), 1, "Large data table should not be rejected");
        assert!(
            tables[0].rows.len() >= 40,
            "Large table should preserve most rows, got {}",
            tables[0].rows.len()
        );
    }
}
