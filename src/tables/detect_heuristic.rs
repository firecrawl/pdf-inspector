//! Heuristic table detection and validation.

use crate::text_utils::is_rtl_text;
use crate::types::TextItem;
use log::debug;

use super::financial::try_split_financial_item;
use super::grid::{
    find_column_boundaries, find_column_index, find_row_boundaries, find_row_index,
    join_cell_items, recover_header_row,
};
use super::{Table, TableDetectionMode};

/// PDF text is often emitted as one item per glyph. That produces
/// hundreds of single-char items that confuse column detection. This function
/// merges adjacent items within the same line (similar Y, close X, similar font
/// size) into multi-character items, similar to PyMuPDF's `merge_chars()`.
///
/// Returns `(merged_items, index_map)` where `index_map[merged_idx]` contains
/// the original item indices that were merged into that item.
pub(crate) fn merge_adjacent_items(items: &[TextItem]) -> (Vec<TextItem>, Vec<Vec<usize>>) {
    if items.is_empty() {
        return (vec![], vec![]);
    }

    // Group items by Y position (5pt tolerance for same line)
    let y_tolerance = 5.0;
    let mut line_groups: Vec<(f32, Vec<(usize, &TextItem)>)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let found = line_groups
            .iter_mut()
            .find(|(y, _)| (item.y - *y).abs() < y_tolerance);
        if let Some((_, group)) = found {
            group.push((idx, item));
        } else {
            line_groups.push((item.y, vec![(idx, item)]));
        }
    }

    // Sort each group by X position
    for (_, group) in &mut line_groups {
        group.sort_by(|a, b| {
            a.1.x
                .partial_cmp(&b.1.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Sort groups by Y descending (top of page first)
    line_groups.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut merged_items = Vec::new();
    let mut index_map: Vec<Vec<usize>> = Vec::new();

    for (_, group) in &line_groups {
        let mut i = 0;
        while i < group.len() {
            let (first_idx, first_item) = group[i];
            let mut text = first_item.text.clone();
            let mut end_x = first_item.x + first_item.width;
            let mut indices = vec![first_idx];
            let x_gap_max = first_item.font_size * 0.5;

            let mut j = i + 1;
            while j < group.len() {
                let (next_idx, next_item) = group[j];

                // Must be similar font size (within 20%)
                if (next_item.font_size - first_item.font_size).abs() > first_item.font_size * 0.20
                {
                    break;
                }

                let gap = next_item.x - end_x;
                // Stop if gap exceeds threshold (inter-column gap)
                if gap > x_gap_max {
                    break;
                }
                // Stop on large overlap (different column overlapping)
                if gap < -first_item.font_size * 0.5 {
                    break;
                }

                // Insert space at word boundaries: within a word characters
                // touch (gap ≈ 0), between words there's a visible gap.
                if gap > first_item.font_size * 0.08 {
                    text.push(' ');
                }
                text.push_str(&next_item.text);
                end_x = next_item.x + next_item.width;
                indices.push(next_idx);
                j += 1;
            }

            merged_items.push(TextItem {
                text,
                x: first_item.x,
                y: first_item.y,
                width: end_x - first_item.x,
                height: first_item.height,
                font: first_item.font.clone(),
                font_size: first_item.font_size,
                page: first_item.page,
                is_bold: first_item.is_bold,
                is_italic: first_item.is_italic,
                item_type: first_item.item_type.clone(),
                mcid: first_item.mcid,
            });
            index_map.push(indices);

            i = j;
        }
    }

    (merged_items, index_map)
}

/// Iterates all items, expanding qualifying consolidated financial items.
/// Returns `(expanded_items, index_map)` where `index_map[expanded_idx] = original_idx`.
fn expand_consolidated_items(items: &[TextItem]) -> (Vec<TextItem>, Vec<usize>) {
    let mut expanded = Vec::with_capacity(items.len());
    let mut index_map = Vec::with_capacity(items.len());
    for (orig_idx, item) in items.iter().enumerate() {
        if let Some(sub_items) = try_split_financial_item(item) {
            for sub in sub_items {
                expanded.push(sub);
                index_map.push(orig_idx);
            }
        } else {
            expanded.push(item.clone());
            index_map.push(orig_idx);
        }
    }
    (expanded, index_map)
}

/// Detect tables in a set of text items from a single page
pub fn detect_tables(items: &[TextItem], base_font_size: f32, skip_body_font: bool) -> Vec<Table> {
    if items.len() < 6 {
        return vec![];
    }

    // Step 1: Merge adjacent single-char items into words (handles per-character PDFs)
    let (merged_items, merge_map) = merge_adjacent_items(items);

    // Step 2: Expand consolidated financial items (e.g. "$ 1,234 $ 5,678" → sub-items)
    let (expanded_items, expand_map) = expand_consolidated_items(&merged_items);
    let items = &expanded_items[..]; // shadow parameter — all detection uses processed items

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

            if let Some(mut table) =
                detect_table_in_region(&region_items, TableDetectionMode::SmallFont)
            {
                // Try to recover body-font header row above the small-font table
                recover_header_row(&mut table, items, table_font_threshold);
                // Try to recover a label column from unclaimed items to the left
                try_add_label_column(
                    &mut table,
                    &table_candidates,
                    &claimed_indices,
                    y_min,
                    y_max,
                );
                for &idx in &table.item_indices {
                    claimed_indices.insert(idx);
                }
                tables.push(table);
            }
        }
    }

    // === Pass 2: Body-font tables (stricter criteria) ===
    // Skip on multi-column pages where body-font detection causes false positives
    if !skip_body_font {
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

        log::debug!(
            "body-font pass: {} candidates (base={:.1}, range={:.1}..{:.1})",
            body_candidates.len(),
            base_font_size,
            body_font_low,
            body_font_high,
        );
        if body_candidates.len() >= 6 {
            let regions = find_table_regions_strict(&body_candidates);
            log::debug!("body-font: {} strict regions found", regions.len());

            for (y_min, y_max, _x_min, _x_max) in &regions {
                // Use full X range for region items — the strict X bounds from
                // qualifying rows can exclude continuation lines in wrapped cells.
                // Y bounds from the region are sufficient to scope the table area.
                let region_items: Vec<(usize, &TextItem)> = body_candidates
                    .iter()
                    .filter(|(_, item)| item.y >= *y_min && item.y <= *y_max)
                    .cloned()
                    .collect();

                log::debug!(
                    "  region y={:.0}..{:.0}: {} items of {} candidates",
                    y_min,
                    y_max,
                    region_items.len(),
                    body_candidates.len()
                );

                if region_items.len() < 6 {
                    continue;
                }

                if let Some(table) =
                    detect_table_in_region(&region_items, TableDetectionMode::BodyFont)
                {
                    tables.push(table);
                }
            }
        }
    }

    // Map indices back: expanded → merged → original
    for table in &mut tables {
        let original_indices: std::collections::HashSet<usize> = table
            .item_indices
            .iter()
            .flat_map(|&exp_idx| {
                let merged_idx = expand_map[exp_idx];
                merge_map[merged_idx].iter().copied()
            })
            .collect();
        table.item_indices = original_indices.into_iter().collect();
        table.item_indices.sort_unstable();
        log::debug!(
            "  heuristic table: {}x{}, {} item indices",
            table.rows.len(),
            table.columns.len(),
            table.item_indices.len()
        );
    }

    tables
}

/// Find Y-regions that likely contain tables
fn find_table_regions(items: &[(usize, &TextItem)]) -> Vec<(f32, f32)> {
    if items.is_empty() {
        return vec![];
    }

    let mut y_positions: Vec<f32> = items.iter().map(|(_, i)| i.y).collect();
    y_positions.sort_by(|a, b| a.total_cmp(b));

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
fn find_table_regions_strict(items: &[(usize, &TextItem)]) -> Vec<(f32, f32, f32, f32)> {
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
        sorted_xs.sort_by(|a, b| a.total_cmp(b));

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

        if cluster_starts.len() >= 2 {
            qualifying_rows.push((*y, cluster_starts));
        }
    }

    log::debug!(
        "find_table_regions_strict: {} row groups, {} qualifying (2+ X-clusters)",
        row_groups.len(),
        qualifying_rows.len()
    );
    if qualifying_rows.len() < 3 {
        return vec![];
    }

    // Step 3: Find contiguous runs of qualifying rows.
    // Use adaptive gap: median spacing × 3 (handles wrapped cells where
    // qualifying rows are spaced further apart), with a floor of 25pt.
    qualifying_rows.sort_by(|a, b| a.0.total_cmp(&b.0));

    let max_gap = if qualifying_rows.len() >= 3 {
        let mut gaps: Vec<f32> = qualifying_rows
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).abs())
            .collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let median_gap = gaps[gaps.len() / 2];
        (median_gap * 3.0).max(25.0)
    } else {
        25.0
    };

    let mut candidate_regions: Vec<Vec<&(f32, Vec<f32>)>> = Vec::new();
    let mut current_region: Vec<&(f32, Vec<f32>)> = vec![&qualifying_rows[0]];

    for row in qualifying_rows.iter().skip(1) {
        let prev_y = current_region.last().unwrap().0;
        if row.0 - prev_y > max_gap {
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
        log::debug!(
            "  candidate region: {} rows, avg alignment score={:.2}",
            num_rows,
            avg_score
        );
        if avg_score >= 0.5 {
            let y_min = region_rows.first().unwrap().0;
            let y_max = region_rows.last().unwrap().0;
            // Compute X bounds from qualifying row cluster positions
            let x_min = region_rows
                .iter()
                .flat_map(|(_, clusters)| clusters.iter())
                .cloned()
                .fold(f32::INFINITY, f32::min);
            let x_max = region_rows
                .iter()
                .flat_map(|(_, clusters)| clusters.iter())
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
            regions.push((y_min - 5.0, y_max + 5.0, x_min - 15.0, x_max + 50.0));
        }
    }

    regions
}

/// Detect a table within a specific region
fn detect_table_in_region(items: &[(usize, &TextItem)], mode: TableDetectionMode) -> Option<Table> {
    // Find column boundaries
    let columns = find_column_boundaries(items, mode);
    let min_cols = 2;
    if columns.len() < min_cols || columns.len() > 25 {
        log::debug!(
            "  detect_table_in_region: rejected {} cols (need {}..25)",
            columns.len(),
            min_cols
        );
        return None;
    }

    // Find row boundaries
    let rows = find_row_boundaries(items);
    let min_rows = 2;
    if rows.len() < min_rows {
        log::debug!(
            "  detect_table_in_region: rejected {} rows (need {}+)",
            rows.len(),
            min_rows
        );
        return None;
    }

    log::debug!(
        "  detect_table_in_region: {} cols, {} rows, {} items",
        columns.len(),
        rows.len(),
        items.len()
    );

    // Verify this looks like a table: multiple items should align to columns
    let col_alignment = check_column_alignment(items, &columns, mode);
    let min_alignment = match mode {
        TableDetectionMode::SmallFont => 0.5,
        TableDetectionMode::BodyFont => 0.7,
    };
    if col_alignment < min_alignment {
        log::debug!(
            "  detect_table_in_region: rejected alignment {:.2} < {:.2} ({} cols, {} rows)",
            col_alignment,
            min_alignment,
            columns.len(),
            rows.len()
        );
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
            // Sort by X position (direction-aware)
            let rtl = is_rtl_text(col_items.iter().map(|i| &i.text));
            if rtl {
                col_items.sort_by(|a, b| b.x.total_cmp(&a.x));
            } else {
                col_items.sort_by(|a, b| a.x.total_cmp(&b.x));
            }

            // Join items with subscript-aware spacing
            let text = join_cell_items(col_items);
            row_cells.push(text);
        }
        cells.push(row_cells);
    }

    // Validation 1: some rows should have content in first column.
    // Use a lower threshold (25%) for tables with wrapped cells where
    // continuation lines leave the first column empty.
    // Skip when cells form a narrow TOC pattern: hierarchical entries indented
    // across multiple X levels leave the leftmost column sparse (only top-level
    // chapters land there) but the structure is still a valid TOC. Narrow only
    // (<=5 cols) — wide multi-column TOCs (e.g. 2-up indices) would render
    // poorly through format_toc_as_list, which assumes one entry per row.
    let rows_with_first_col = cells.iter().filter(|row| !row[0].is_empty()).count();
    let is_narrow_toc = columns.len() <= 5 && is_table_of_contents(&cells);
    if rows_with_first_col < rows.len() / 4 && !is_narrow_toc {
        log::debug!(
            "  validation 1 fail: {}/{} rows have first col",
            rows_with_first_col,
            rows.len()
        );
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
        log::debug!(
            "  validation 2 fail: {}/{} rows multi-col (need {})",
            rows_with_multi_cols,
            rows.len(),
            multi_col_threshold
        );
        return None;
    }

    // Validation 3: tables shouldn't have too many rows (likely misdetected text)
    let max_rows = match mode {
        TableDetectionMode::SmallFont => 200,
        TableDetectionMode::BodyFont => 200,
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
    let min_avg_cells = 1.5;
    if avg_cells_per_row < min_avg_cells {
        log::debug!(
            "  validation 4 fail: avg_cells={:.1} < {:.1}",
            avg_cells_per_row,
            min_avg_cells
        );
        return None;
    }

    // Validation 5: Check for key-value pair layout (NOT a table)
    if is_key_value_layout(&cells) {
        log::debug!("  validation 5 fail: key-value layout");
        return None;
    }

    // Validation 6: Check column count consistency
    if !has_consistent_columns(&cells) {
        log::debug!("  validation 6 fail: inconsistent columns");
        return None;
    }

    // Validation 7: Tables should have some numeric/data content
    if !has_table_like_content(&cells, mode) {
        log::debug!("  validation 7 fail: no table-like content");
        return None;
    }

    // Validation 8: Reject paragraph-like content falsely detected as tables.
    // TOC pages with deep indentation (top-level chapters in col 0, subsections
    // in cols 1-3, page numbers in last col) leave most cells empty and trip
    // the paragraph heuristic; TOC shape is a safer signal here. Narrow only
    // — see narrow-TOC rationale at validation 1.
    if is_paragraph_content(&cells) && !is_narrow_toc {
        log::debug!("  validation 9 fail: paragraph content");
        return None;
    }

    // Validation 9: Reject wide "index" layouts where every cell carries a
    // full "label ... page" fragment (back-of-book IRS-style indices).
    // These render poorly in any structured form; text flow is the best
    // fallback.  Narrow dot-leader TOCs (2-3 cols) are kept so format.rs
    // can emit them as a per-row flat list with titles tab-joined to page
    // numbers.
    if is_inline_leader_index(&cells) {
        log::debug!("  validation 9 fail: inline-leader index");
        return None;
    }

    debug!(
        "table detected: {} rows x {} cols, {} items",
        rows.len(),
        columns.len(),
        item_indices.len()
    );

    Some(Table::new(columns, rows, cells, item_indices))
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

    // Break ties by preferring higher column count for deterministic output
    let most_common_count = count_freq
        .iter()
        .max_by(|(count_a, freq_a), (count_b, freq_b)| {
            freq_a.cmp(freq_b).then_with(|| count_a.cmp(count_b))
        })
        .map(|(count, _)| *count)
        .unwrap_or(0);

    // At least 40% of rows should have the most common column count (or close to it).
    // Very wide tables (e.g. 24-column train schedules) have inherently variable fill,
    // so use wider tolerance and lower ratio.  Threshold at 15 to avoid false-positives
    // on moderately-wide tables where the strict check works well.
    let num_cols = cells[0].len();
    let tolerance = if num_cols > 15 { num_cols / 4 } else { 2 };
    let consistent_rows = filled_counts
        .iter()
        .filter(|&&c| {
            c >= most_common_count.saturating_sub(tolerance) && c <= most_common_count + tolerance
        })
        .count();

    let min_ratio = if num_cols > 15 { 0.25 } else { 0.40 };
    consistent_rows as f32 / cells.len() as f32 > min_ratio
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

    // Bypass content check for wide tables (3+ columns) — text-only tables
    // (category lists, program descriptions) are legitimate if they passed
    // all structural validations (alignment, consistency, not key-value).
    // Also bypass for 2-column body-font tables with short cells (avg ≤40 chars),
    // which are likely definition/category lists, not paragraph text.
    if pct_data > min_pct || num_cols >= 3 {
        return true;
    }
    if num_cols == 2 && matches!(mode, TableDetectionMode::BodyFont) {
        let non_empty: Vec<usize> = cells
            .iter()
            .skip(1)
            .flat_map(|row| row.iter())
            .filter(|c| !c.trim().is_empty())
            .map(|c| c.trim().len())
            .collect();
        if !non_empty.is_empty() {
            let avg_len = non_empty.iter().sum::<usize>() / non_empty.len();
            return avg_len <= 25;
        }
    }
    false
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

    // Dates: MM/DD/YYYY, DD/MM/YYYY, YYYY-MM-DD, etc.
    if s.len() <= 10
        && s.chars().filter(|c| c.is_ascii_digit()).count() >= 4
        && (s.contains('/') || s.contains('-'))
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == '-')
    {
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

/// Check if this looks like a Table of Contents (either style).
///
/// Used by format.rs to render TOCs as flat lists instead of markdown tables.
pub fn is_table_of_contents(cells: &[Vec<String>]) -> bool {
    is_dot_leader_toc(cells) || is_tabular_toc(cells)
}

/// Dot-leader TOC: any "Chapter 1 ........ 42" style with explicit leader
/// dots.  Covers both narrow 2-3 col TOCs (where the leader is a dedicated
/// cell) and wide indices (where each cell encodes a full "label ... page"
/// fragment).  Used by format.rs to render as a flat list.
pub(super) fn is_dot_leader_toc(cells: &[Vec<String>]) -> bool {
    has_structural_dot_leader(cells) || is_inline_leader_index(cells)
}

/// Rows with a dedicated dots-only cell flanked by label + number (2-3 col
/// TOC layout).  Format.rs handles these well via per-row flat-list
/// rendering; they should NOT be rejected at detect time.
fn has_structural_dot_leader(cells: &[Vec<String>]) -> bool {
    if cells.is_empty() {
        return false;
    }
    let structural_rows = cells.iter().filter(|row| row_has_dot_leader(row)).count();
    structural_rows as f32 / cells.len() as f32 >= 0.3
}

/// Wide index layout: each cell holds a full "label ... page" fragment
/// because the column detector kept multi-column indices as single cells.
/// These render poorly both as markdown tables (column boundaries are
/// arbitrary) and as flat lists (each row holds 3+ separate index
/// entries).  Reject these at detect time so they fall back to the page's
/// normal text flow.
pub(super) fn is_inline_leader_index(cells: &[Vec<String>]) -> bool {
    let mut inline_cells = 0;
    let mut total_nonempty = 0;
    for row in cells {
        for cell in row {
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            total_nonempty += 1;
            if cell_is_inline_leader(trimmed) {
                inline_cells += 1;
            }
        }
    }
    total_nonempty >= 4 && inline_cells as f32 / total_nonempty as f32 >= 0.25
}

/// A row with a dot-leader.  Accepts two layouts:
///   1. A dedicated dots-only cell ("....") with a text label somewhere
///      to its left and a page number somewhere to its right.
///   2. A "title ... " cell (trailing leader dots glued to the title)
///      with a page number elsewhere in the same row.
fn row_has_dot_leader(row: &[String]) -> bool {
    let has_page_number = row.iter().any(|c| row_cell_is_page_number(c));

    for (ci, cell) in row.iter().enumerate() {
        let trimmed = cell.trim();

        // Pattern 1: dedicated dots-only cell.
        let dot_count = trimmed.chars().filter(|&c| c == '.').count();
        let is_mostly_dots = dot_count >= 3
            && dot_count > trimmed.len() / 2
            && trimmed.chars().all(|c| c == '.' || c.is_whitespace());
        if is_mostly_dots {
            let has_label_left = row[..ci].iter().any(|c| {
                let t = c.trim();
                !t.is_empty() && t.chars().any(|ch| ch.is_alphabetic())
            });
            if has_label_left && has_page_number {
                return true;
            }
            continue;
        }

        // Pattern 2: cell ends with a trailing " ... " run after a label.
        if has_page_number && cell_has_trailing_leader(trimmed) {
            return true;
        }
    }
    false
}

/// Cell ends with a run of ≥3 dots preceded by alphabetic text and a
/// space — the "Title ... " layout where the leader is glued to the name.
/// Alphabetic (not alphanumeric) so that data-table row labels like
/// "1973 ... " do not register as titles.
fn cell_has_trailing_leader(cell: &str) -> bool {
    let trimmed = cell.trim_end();
    if !trimmed.ends_with('.') {
        return false;
    }
    let without_dots = trimmed.trim_end_matches('.');
    let dot_run = trimmed.len() - without_dots.len();
    if dot_run < 3 {
        return false;
    }
    // Require a space before the dot run (rules out "etc..." / "Mr...") and
    // at least one alphabetic char (rules out "1973 ... " data-row labels).
    without_dots.ends_with(' ') && without_dots.trim().chars().any(|c| c.is_alphabetic())
}

/// Page-number shape: single ≤4-digit integer, a ", "-separated list of
/// ≤4-digit integers ("18, 36, 107"), or a dashed section-page ID
/// ("A-1", "5-21").  Rejects decimal cells ("4. 0"), thousands-separated
/// values ("189,164"), and other long numeric data that appears in
/// statistical tables.
fn row_cell_is_page_number(cell: &str) -> bool {
    let t = cell.trim();
    if t.is_empty() {
        return false;
    }
    if looks_like_section_page_id(t) {
        return true;
    }
    // Page list: ", " separator (with space) distinguishes real page lists
    // from thousands-separated numbers like "189,164".
    let parts: Vec<&str> = t.split(", ").collect();
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit()))
}

/// A cell shaped like an index leader fragment.  Accepts two forms:
///   - "text ... number" — label + dots + page number in one cell
///   - "... number"      — bare leader + number (row where the label
///     landed in a separate column)
///
/// Both only count if followed by pure numeric content (optionally
/// comma-separated page lists like "127, 213").
fn cell_is_inline_leader(cell: &str) -> bool {
    let cell = cell.trim();

    // Find the first "..." run.  Surrounding-whitespace checks below
    // reject intra-word ellipses ("etc...").
    let idx = match cell.match_indices("...").next() {
        Some((i, _)) => i,
        None => return false,
    };

    let before = &cell[..idx];
    let after_dots = &cell[idx + 3..];
    // Allow extra dots (e.g. "....") by skipping any additional '.'
    let after = after_dots.trim_start_matches('.');

    // Require space (or start-of-cell) before the dots and space/digit
    // after — blocks intra-word ellipses.
    let before_ok = before.is_empty() || before.ends_with(' ');
    let after_ok = after.starts_with(' ') || after.is_empty();
    if !before_ok || !after_ok {
        return false;
    }

    let after_trim = after.trim();
    if after_trim.is_empty() {
        return false;
    }
    // Tail must be purely numeric/page-list content.
    let tail_numeric = after_trim
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ',' | ' ' | '.' | '-' | '$'))
        && after_trim.chars().any(|c| c.is_ascii_digit());
    if !tail_numeric {
        return false;
    }

    // Either we have a label before, or the leader is bare (starts the cell)
    // — both are legitimate index fragments.
    before.chars().any(|c| c.is_alphabetic()) || before.trim().is_empty()
}

/// Dot-less tabular TOC: tagged PDFs emit entries as rows where the first
/// column starts with a dotted section number (e.g. "4.3.1 Something") and
/// the last column is one or more page numbers.  These have no leader dots
/// and benefit from flat-list formatting (page numbers aligned to titles).
pub(super) fn is_tabular_toc(cells: &[Vec<String>]) -> bool {
    if cells.is_empty() {
        return false;
    }
    let num_cols = cells[0].len();
    if num_cols < 2 || cells.len() < 4 {
        return false;
    }

    let section_rows = cells
        .iter()
        .filter(|row| {
            row.iter()
                .find(|c| !c.trim().is_empty())
                .is_some_and(|c| starts_with_section_number(c.trim()))
        })
        .count();

    let last_col = num_cols - 1;
    let (last_filled, last_page_num) = cells.iter().fold((0u32, 0u32), |(f, n), row| {
        let cell = row.get(last_col).map(|s| s.trim()).unwrap_or("");
        if cell.is_empty() {
            return (f, n);
        }
        let is_page_nums = cell
            .split_whitespace()
            .all(|tok| !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit()));
        (f + 1, n + if is_page_nums { 1 } else { 0 })
    });

    let section_ratio = section_rows as f32 / cells.len() as f32;
    let page_num_last_ratio = if last_filled > 0 {
        last_page_num as f32 / last_filled as f32
    } else {
        0.0
    };

    section_ratio >= 0.6 && last_filled >= 3 && page_num_last_ratio >= 0.7
}

/// Matches dashed section-page identifiers used in technical manuals:
/// "5-21", "A-1", "B--3", "TC-2".  At least one ASCII digit is required.
fn looks_like_section_page_id(s: &str) -> bool {
    let ok = s
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || c == '-');
    ok && s.chars().any(|c| c.is_ascii_digit())
}

/// Returns true when the leading token looks like a dotted section number:
/// "1", "1.2", "1.2.3", "4.3.1.2" — integer components joined by dots,
/// with at least one dot (single-number prefixes are too ambiguous).
fn starts_with_section_number(s: &str) -> bool {
    let Some(first) = s.split_whitespace().next() else {
        return false;
    };
    let first = first.trim_end_matches('.');
    let parts: Vec<&str> = first.split('.').collect();
    if parts.len() < 2 || parts.len() > 6 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

/// Check if detected "table" cells are actually paragraph text fragments.
///
/// Multi-column paragraph text falsely detected as tables produces:
/// - Many empty cells (text doesn't span all columns)
/// - Cells ending with hyphens (word breaks across "columns")
/// - Long sentence fragments or single-word fragments
fn is_paragraph_content(cells: &[Vec<String>]) -> bool {
    if cells.is_empty() {
        return false;
    }

    let num_cols = cells[0].len();
    let total_cells = cells.len() * num_cols;
    if total_cells == 0 {
        return false;
    }

    let filled: Vec<&str> = cells
        .iter()
        .flat_map(|r| r.iter())
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();

    let total_filled = filled.len();
    if total_filled < 4 {
        return false;
    }

    let empty_ratio = 1.0 - (total_filled as f32 / total_cells as f32);

    // Cells ending with a hyphen suggest word breaks across columns.
    // Real table cells almost never end with hyphens (except range indicators).
    let hyphen_breaks = filled
        .iter()
        .filter(|c| {
            c.ends_with('-') && c.len() > 1 && {
                let mut chars = c.chars().rev();
                chars.next(); // skip the '-'
                chars.next().is_some_and(|ch| ch.is_alphabetic())
            }
        })
        .count();
    let hyphen_ratio = hyphen_breaks as f32 / total_filled as f32;

    // Word-break hyphens are a strong paragraph signal
    if hyphen_ratio > 0.03 {
        return true;
    }

    // High empty ratio with many rows suggests paragraph text spread across a grid
    if empty_ratio > 0.55 && cells.len() > 10 {
        return true;
    }

    // Letter-spaced text (spaces between every character) is never real table data.
    // This happens when PDF uses wide character spacing for emphasis/formatting.
    // Require at least 9 chars (e.g., "a b c d e") to avoid matching short codes.
    let letter_spaced = filled
        .iter()
        .filter(|c| {
            let chars: Vec<char> = c.chars().collect();
            chars.len() >= 9
                && chars.windows(4).all(|w| {
                    (w[0].is_alphabetic() && w[1] == ' ' && w[2].is_alphabetic() && w[3] == ' ')
                        || (w[0] == ' '
                            && w[1].is_alphabetic()
                            && w[2] == ' '
                            && w[3].is_alphabetic())
                })
        })
        .count();
    if letter_spaced > 0 && letter_spaced as f32 / total_filled as f32 > 0.08 {
        return true;
    }

    // Long sentence fragments
    let long_cells = filled.iter().filter(|c| c.len() > 60).count();
    let long_ratio = long_cells as f32 / total_filled as f32;
    let avg_len = filled.iter().map(|c| c.len()).sum::<usize>() as f32 / total_filled as f32;

    if avg_len > 40.0 && long_ratio > 0.2 {
        return true;
    }
    if long_ratio > 0.3 {
        return true;
    }

    false
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

/// Find the first row that looks like actual table data (not form header).
/// Returns (first_table_row_index, set of item indices to exclude).
pub(crate) fn find_first_table_row(
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
        // Only treat as form row if most filled cells look form-like,
        // or the row is very sparse with any form pattern.
        let form_cell_count = filled_cells
            .iter()
            .filter(|c| {
                let text = c.trim();
                (text.ends_with(':') && text.len() > 1)
                    || (text.contains(": ") && !looks_like_number(text))
            })
            .count();
        let has_form_patterns =
            form_cell_count > 0 && (form_cell_count * 2 >= filled_count || fill_ratio < 0.3);

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

        // Skip rows that have duplicate non-empty cells. These are spanning
        // super-headers (e.g., "First Degree | First Degree | Higher Degree")
        // that sit above the real column header row. Using them as the markdown
        // header produces duplicate column names that downstream validation
        // rejects. Only skip if a subsequent row looks like a better header
        // (denser fill or has data).
        if filled_count >= 2 && !has_data {
            let mut text_counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for cell in &filled_cells {
                *text_counts.entry(cell.trim()).or_insert(0) += 1;
            }
            let has_duplicates = text_counts.values().any(|&count| count >= 2);
            if has_duplicates {
                // Check if a later row is a better header candidate
                let has_better_below = cells.iter().skip(row_idx + 1).take(3).any(|r| {
                    let next_filled = r.iter().filter(|c| !c.trim().is_empty()).count();
                    let next_fill = next_filled as f32 / total_cols as f32;
                    let next_numeric = r.iter().filter(|c| looks_like_number(c.trim())).count();
                    next_fill >= 0.4 || next_numeric >= 2
                });
                if has_better_below {
                    continue;
                }
            }
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

/// Try to recover a label column for numeric-only tables.
///
/// Financial balance sheets often have text labels (row descriptions) to the
/// left of numeric columns. The label X-positions vary due to indentation,
/// so they don't form a consistent column cluster and are excluded from the
/// initial table detection. This function finds unclaimed items at matching
/// Y-positions to the left of the table and prepends them as column 0.
fn try_add_label_column(
    table: &mut Table,
    all_candidates: &[(usize, &TextItem)],
    claimed_indices: &std::collections::HashSet<usize>,
    y_min: f32,
    y_max: f32,
) {
    // Only apply to tables with 2-3 numeric columns and ≥5 rows
    if table.columns.len() < 2 || table.columns.len() > 3 || table.rows.len() < 5 {
        return;
    }

    // Check if the table is predominantly numeric (no text labels in any column)
    let numeric_cells = table
        .cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|cell| {
            let text = cell.trim();
            if text.is_empty() {
                return false;
            }
            let data_chars = text
                .chars()
                .filter(|c| c.is_ascii_digit() || ",.-+%€$£¥()".contains(*c))
                .count();
            let total_chars = text.chars().count();
            total_chars > 0 && data_chars as f32 / total_chars as f32 >= 0.6
        })
        .count();
    let total_non_empty = table
        .cells
        .iter()
        .flat_map(|row| row.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    if total_non_empty == 0 || (numeric_cells as f32 / total_non_empty as f32) < 0.7 {
        return;
    }

    let table_x_min = table.columns.first().copied().unwrap_or(f32::MAX);
    let y_tol = 5.0;

    // For each table row, find unclaimed items to the left at the same Y
    let mut label_items_per_row: Vec<Vec<(usize, &TextItem)>> = Vec::new();
    let mut found_count = 0;
    for &row_y in &table.rows {
        let mut row_labels: Vec<(usize, &TextItem)> = all_candidates
            .iter()
            .filter(|(idx, item)| {
                !claimed_indices.contains(idx)
                    && !table.item_indices.contains(idx)
                    && (item.y - row_y).abs() < y_tol
                    && item.x < table_x_min - 10.0
                    && item.y >= y_min
                    && item.y <= y_max
            })
            .map(|(idx, item)| (*idx, *item))
            .collect();
        row_labels.sort_by(|a, b| {
            a.1.x
                .partial_cmp(&b.1.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !row_labels.is_empty() {
            found_count += 1;
        }
        label_items_per_row.push(row_labels);
    }

    // Require labels for at least 40% of rows
    if found_count < table.rows.len() * 2 / 5 {
        return;
    }

    debug!(
        "recovering label column: {}/{} rows have labels to the left",
        found_count,
        table.rows.len()
    );

    // Prepend label column
    let label_col_x = label_items_per_row
        .iter()
        .flat_map(|items| items.iter().map(|(_, i)| i.x))
        .fold(f32::INFINITY, f32::min);

    table.columns.insert(0, label_col_x);
    for (row_idx, row_labels) in label_items_per_row.iter().enumerate() {
        let label_text = row_labels
            .iter()
            .map(|(_, item)| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        table.cells[row_idx].insert(0, label_text);
        for (idx, _) in row_labels {
            table.item_indices.push(*idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_table_of_contents_rejects_toc() {
        // TOC with separate dot-leader cells and page number cells
        let cells = vec![
            vec![
                "Chapter 1".to_string(),
                "....................".to_string(),
                "1".to_string(),
            ],
            vec![
                "Chapter 2".to_string(),
                "....................".to_string(),
                "15".to_string(),
            ],
            vec![
                "Chapter 3".to_string(),
                "....................".to_string(),
                "42".to_string(),
            ],
            vec![
                "Appendix".to_string(),
                "....................".to_string(),
                "100".to_string(),
            ],
        ];
        assert!(is_table_of_contents(&cells));
    }

    #[test]
    fn is_table_of_contents_allows_data_table_with_dot_leaders() {
        // Simulates ERP appendix tables where the first column has year + dots
        // (e.g. "1973..........") and other columns have numeric data.
        let cells = vec![
            vec![
                "1973..........".to_string(),
                "0.80".to_string(),
                "1.08".to_string(),
                "1.05".to_string(),
                "0.02".to_string(),
                "-0.28".to_string(),
                "-0.33".to_string(),
                "5.16".to_string(),
            ],
            vec![
                "1974..........".to_string(),
                "73".to_string(),
                "56".to_string(),
                "49".to_string(),
                "08".to_string(),
                "17".to_string(),
                "17".to_string(),
                "-.28".to_string(),
            ],
            vec![
                "1975..........".to_string(),
                "86".to_string(),
                "-.05".to_string(),
                "-.14".to_string(),
                "09".to_string(),
                "91".to_string(),
                "85".to_string(),
                "1.03".to_string(),
            ],
            vec![
                "1976..........".to_string(),
                "-1.05".to_string(),
                "36".to_string(),
                "34".to_string(),
                "02".to_string(),
                "-1.41".to_string(),
                "-1.31".to_string(),
                "4.01".to_string(),
            ],
        ];
        assert!(
            !is_table_of_contents(&cells),
            "data table with dot-leader labels should not be rejected as TOC"
        );
    }

    #[test]
    fn is_table_of_contents_accepts_hierarchical_indented_toc() {
        // Mythos system card pages 4-5: top-level chapters indent at col 0,
        // subsections at cols 1-2, leaving col 0 mostly empty (only ~10% of
        // rows). Validation 1 was rejecting these even though the structure
        // is unambiguously a TOC.
        let cells = vec![
            vec!["Abstract".to_string(), String::new(), "3".to_string()],
            vec![
                "1 Introduction".to_string(),
                String::new(),
                "10".to_string(),
            ],
            vec![
                String::new(),
                "1.1 Model training".to_string(),
                "11".to_string(),
            ],
            vec![
                String::new(),
                "1.1.1 Training data".to_string(),
                "11".to_string(),
            ],
            vec![
                String::new(),
                "1.1.2 Crowd workers".to_string(),
                "12".to_string(),
            ],
            vec![
                String::new(),
                "1.2 Release decision".to_string(),
                "13".to_string(),
            ],
            vec![
                "2 RSP evaluations".to_string(),
                String::new(),
                "16".to_string(),
            ],
            vec![
                String::new(),
                "2.1 RSP risk assessment".to_string(),
                "16".to_string(),
            ],
            vec![String::new(), "2.1.1 Context".to_string(), "16".to_string()],
            vec![
                String::new(),
                "2.2 CB evaluations".to_string(),
                "20".to_string(),
            ],
        ];
        assert!(
            is_table_of_contents(&cells),
            "hierarchical TOC with sparse col 0 should still be detected"
        );
    }

    #[test]
    fn is_table_of_contents_rejects_dotless_toc() {
        // Tabular TOC without leader dots: first column starts with dotted
        // section numbers, last column is page numbers.  Pattern from
        // Mythos system card pages 6-8.
        let cells = vec![
            vec![
                "4.3 Case studies and targeted evaluations".to_string(),
                String::new(),
                "86".to_string(),
            ],
            vec![
                "4.3.1 Destructive or reckless actions".to_string(),
                "4.3.1.1 Synthetic-backend evaluation".to_string(),
                "86 86".to_string(),
            ],
            vec![
                "4.3.2 Adherence to constitution".to_string(),
                "4.3.2.1 Overview".to_string(),
                "89 89".to_string(),
            ],
            vec![
                "4.3.3 Honesty and hallucinations".to_string(),
                "4.3.3.1 Factual hallucinations".to_string(),
                "93 94".to_string(),
            ],
            vec![
                "4.4 Capability evaluations".to_string(),
                String::new(),
                "101".to_string(),
            ],
        ];
        assert!(
            is_table_of_contents(&cells),
            "dot-less TOC with section numbers + page numbers should be rejected"
        );
    }

    #[test]
    fn dot_leader_toc_accepts_short_inline_leaders() {
        // Index-style cells where the full "label ... number" pattern is
        // preserved in a single cell (IRS Publication 17 back-of-book index).
        let cells = vec![
            vec!["Child tax credit ... 235".to_string(), String::new()],
            vec!["Church employee ... 252".to_string(), String::new()],
            vec!["Citizens outside the U.S ... 6".to_string(), String::new()],
            vec![
                "Claim for refund ... 18, 36, 107".to_string(),
                String::new(),
            ],
            vec!["Clergy ... 7, 52".to_string(), String::new()],
        ];
        assert!(is_dot_leader_toc(&cells));
    }

    #[test]
    fn dot_leader_toc_allows_ellipsis_data_table() {
        // Data tables using "..." as a row-omission marker must not be
        // mistaken for dot-leader TOCs.  Based on MCF5235RM QSPI RAM layout.
        let cells = vec![
            vec![
                "0x00".to_string(),
                "QTR0".to_string(),
                "Transmit RAM".to_string(),
            ],
            vec!["0x01".to_string(), "QTR1".to_string(), String::new()],
            vec![
                "...".to_string(),
                "...".to_string(),
                "16 bits wide".to_string(),
            ],
            vec!["0x0F".to_string(), "QTR15".to_string(), String::new()],
            vec![
                "0x10".to_string(),
                "QRR0".to_string(),
                "Receive RAM".to_string(),
            ],
            vec!["0x11".to_string(), "QRR1".to_string(), String::new()],
            vec![
                "...".to_string(),
                "...".to_string(),
                "16 bits wide".to_string(),
            ],
            vec!["0x1F".to_string(), "QRR15".to_string(), String::new()],
        ];
        assert!(
            !is_dot_leader_toc(&cells),
            "ellipsis markers in a data table should not match TOC detection"
        );
    }

    #[test]
    fn dot_leader_toc_rejects_year_row_data_table() {
        // ERP-2025 economic data tables: year labels with trailing " ... ",
        // a final " ... " column, and decimal-looking numeric cells.  The
        // detection previously classified these as dot-leader TOCs and
        // routed them through flat-list formatting, destroying the grid.
        let cells = vec![
            vec![
                "1973 ... ".to_string(),
                "4. 0".to_string(),
                "1. 8".to_string(),
                "0. 4".to_string(),
                "3. 2".to_string(),
                " ... ".to_string(),
            ],
            vec![
                "1974 ... ".to_string(),
                "–1. 9".to_string(),
                "–1. 6".to_string(),
                "–5. 6".to_string(),
                "2. 4".to_string(),
                " ... ".to_string(),
            ],
            vec![
                "1975 ... ".to_string(),
                "2. 6".to_string(),
                "5. 1".to_string(),
                "6. 1".to_string(),
                "4. 1".to_string(),
                " ... ".to_string(),
            ],
            vec![
                "1976 ... ".to_string(),
                "4. 3".to_string(),
                "5. 4".to_string(),
                "6. 4".to_string(),
                "4. 5".to_string(),
                " ... ".to_string(),
            ],
        ];
        assert!(
            !is_dot_leader_toc(&cells),
            "year-indexed data tables with decimal cells must not match TOC detection"
        );
    }

    #[test]
    fn dot_leader_toc_rejects_monthly_data_table() {
        // ERP-2025 Table B-22: monthly labor-force rows with "Jan ... ",
        // "Feb ... " labels and thousands-separated cells ("189,164").
        // Previously matched TOC detection because "Jan ..." has alphabetic
        // text and "189,164" passed the page-number shape check.
        let cells = vec![
            vec![
                "2023: Jan ... ".to_string(),
                "265,962".to_string(),
                "165,871".to_string(),
                "160,152".to_string(),
                "62. 4".to_string(),
            ],
            vec![
                "Feb ... ".to_string(),
                "266,112".to_string(),
                "166,263".to_string(),
                "160,301".to_string(),
                "62. 5".to_string(),
            ],
            vec![
                "Mar ... ".to_string(),
                "266,272".to_string(),
                "166,690".to_string(),
                "160,824".to_string(),
                "62. 6".to_string(),
            ],
            vec![
                "Apr ... ".to_string(),
                "266,443".to_string(),
                "166,678".to_string(),
                "160,962".to_string(),
                "62. 6".to_string(),
            ],
        ];
        assert!(
            !is_dot_leader_toc(&cells),
            "monthly labor-force rows with thousands-separated data must not match TOC detection"
        );
    }

    #[test]
    fn tabular_toc_requires_section_numbers_and_pages() {
        // Dot-less tabular TOC matches is_tabular_toc but not dot-leader.
        let cells = vec![
            vec![
                "4.3 Case studies".to_string(),
                String::new(),
                "86".to_string(),
            ],
            vec![
                "4.3.1 Destructive actions".to_string(),
                String::new(),
                "86".to_string(),
            ],
            vec![
                "4.3.2 Adherence".to_string(),
                String::new(),
                "89".to_string(),
            ],
            vec!["4.3.3 Honesty".to_string(), String::new(), "93".to_string()],
        ];
        assert!(is_tabular_toc(&cells));
        assert!(!is_dot_leader_toc(&cells));
    }

    #[test]
    fn starts_with_section_number_matches_dotted() {
        assert!(starts_with_section_number("1.2"));
        assert!(starts_with_section_number("4.3.1"));
        assert!(starts_with_section_number("4.3.1.2"));
        assert!(starts_with_section_number("4.3 Case studies"));
        assert!(starts_with_section_number("2.2.5.1 Expert red teaming"));
    }

    #[test]
    fn starts_with_section_number_rejects_non_sections() {
        assert!(!starts_with_section_number("Chapter 1"));
        assert!(!starts_with_section_number("1973"));
        assert!(!starts_with_section_number("1.5M"));
        assert!(!starts_with_section_number("10.0%"));
        assert!(!starts_with_section_number(""));
        assert!(!starts_with_section_number("Hello world"));
    }
}
