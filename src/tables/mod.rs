//! Table detection and formatting.
//!
//! Detects tabular data in PDF text items and converts to markdown tables.

mod detect_heuristic;
mod detect_lines;
pub(crate) mod detect_rects;
mod detect_struct;
mod financial;
mod format;
mod grid;
pub mod structured;

pub use detect_heuristic::detect_tables;
pub(crate) use detect_heuristic::{
    content_width, detect_tables_with_page_width, is_table_of_contents,
};
pub use detect_lines::detect_tables_from_lines;
pub(crate) use detect_lines::{
    detect_dense_line_chart_regions, detect_vector_grid_tables_from_lines,
};
pub(crate) use detect_rects::cluster_rects;
pub use detect_rects::{detect_chart_regions, detect_tables_from_rects, RectHintRegion};
pub use detect_struct::detect_tables_from_struct_tree;
pub use format::table_to_markdown;
pub use structured::{cells_to_markdown, StructuredCell};

use crate::types::TextItem;

/// Try to build a table from items + cluster rects (calendar-style layouts).
///
/// Uses rect X positions as column boundaries to directly construct a `Table`,
/// bypassing heuristic detection. Splits merged multi-number items first.
pub(crate) fn try_build_rect_guided_table(
    items: &[TextItem],
    cluster_rects: &[(f32, f32, f32, f32)],
) -> Option<Table> {
    if items.is_empty() || cluster_rects.is_empty() {
        return None;
    }

    // 1. Derive column boundaries from rect X positions (snapped to 2pt tolerance)
    let mut x_lefts: Vec<f32> = cluster_rects.iter().map(|&(x, _, _, _)| x).collect();
    x_lefts.sort_by(|a, b| a.total_cmp(b));
    // Snap: deduplicate within 2pt tolerance
    let mut col_boundaries: Vec<f32> = Vec::new();
    for x in &x_lefts {
        if col_boundaries
            .last()
            .is_none_or(|last| (*x - *last).abs() > 2.0)
        {
            col_boundaries.push(*x);
        }
    }

    if col_boundaries.len() < 5 {
        return None;
    }

    // 1b. Interpolate missing boundaries: holidays/non-work days may not have
    // rects, creating gaps. Fill gaps > 1.5× median spacing with evenly spaced
    // boundaries so every day gets a column.
    if col_boundaries.len() >= 2 {
        let mut spacings: Vec<f32> = col_boundaries.windows(2).map(|w| w[1] - w[0]).collect();
        spacings.sort_by(|a, b| a.total_cmp(b));
        let median_spacing = spacings[spacings.len() / 2];
        let threshold = median_spacing * 1.5;

        let mut filled: Vec<f32> = vec![col_boundaries[0]];
        for i in 1..col_boundaries.len() {
            let gap = col_boundaries[i] - col_boundaries[i - 1];
            if gap > threshold {
                // Insert interpolated boundaries
                let n = (gap / median_spacing).round() as usize;
                if n >= 2 {
                    let step = gap / n as f32;
                    for j in 1..n {
                        filled.push(col_boundaries[i - 1] + j as f32 * step);
                    }
                }
            }
            filled.push(col_boundaries[i]);
        }
        col_boundaries = filled;
    }

    // 2. Split merged multi-number items
    let mut expanded_items: Vec<(TextItem, usize)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let splits = split_merged_numbers(item, &col_boundaries);
        for split_item in splits {
            expanded_items.push((split_item, idx));
        }
    }

    // 3. Derive row boundaries from item Y positions (5pt tolerance)
    let mut y_values: Vec<f32> = expanded_items.iter().map(|(item, _)| item.y).collect();
    y_values.sort_by(|a, b| b.total_cmp(a)); // descending
    let mut row_boundaries: Vec<f32> = Vec::new();
    for y in &y_values {
        if row_boundaries
            .last()
            .is_none_or(|last| (*last - *y).abs() > 5.0)
        {
            row_boundaries.push(*y);
        }
    }

    if row_boundaries.is_empty() {
        return None;
    }

    // 4. Assign items to cells
    let n_rows = row_boundaries.len();
    let n_cols = col_boundaries.len();
    let mut cells: Vec<Vec<String>> = vec![vec![String::new(); n_cols]; n_rows];
    let mut used_indices: Vec<usize> = Vec::new();

    // Compute max X to exclude legend text beyond the table area
    let col_spacing = if col_boundaries.len() >= 2 {
        (col_boundaries.last().unwrap() - col_boundaries.first().unwrap())
            / (col_boundaries.len() - 1) as f32
    } else {
        20.0
    };
    let max_x = col_boundaries.last().unwrap() + col_spacing * 1.5;

    for (item, orig_idx) in &expanded_items {
        // Skip items beyond the table's rightmost column (legend text)
        if item.x > max_x {
            continue;
        }
        // Find row (nearest Y within tolerance)
        let row = row_boundaries
            .iter()
            .position(|&ry| (ry - item.y).abs() <= 5.0);
        // Find column: rightmost boundary ≤ item.x + tolerance.
        // 4pt tolerance catches annotation items (e.g. "Memorial Day") that sit
        // slightly before the next column boundary.
        let col = col_boundaries.iter().rposition(|&cx| item.x >= cx - 4.0);

        if let (Some(r), Some(c)) = (row, col) {
            let cell = &mut cells[r][c];
            if !cell.is_empty() {
                cell.push(' ');
            }
            cell.push_str(item.text.trim());
            used_indices.push(*orig_idx);
        }
    }

    // 5. Clean up: strip tilde-leader noise from cells (legend text bleeding
    //    into the last column from the right side of the page)
    for row in &mut cells {
        for cell in row.iter_mut() {
            if let Some(pos) = cell.find("~~~") {
                cell.truncate(pos);
                *cell = cell.trim_end().to_string();
            }
        }
    }

    // 6. Validate: at least one row should have ≥ 5 non-empty cells
    let best_row_fill = cells
        .iter()
        .map(|row| row.iter().filter(|c| !c.is_empty()).count())
        .max()
        .unwrap_or(0);
    if best_row_fill < 5 {
        return None;
    }

    // Deduplicate used indices
    used_indices.sort_unstable();
    used_indices.dedup();

    Some(Table::new(
        col_boundaries,
        row_boundaries,
        cells,
        used_indices,
    ))
}

/// Canonical lowercase roman numeral for `n` (the i/v/x/l/c range).
pub(super) fn to_roman_lower(mut n: u32) -> String {
    const TABLE: [(u32, &str); 9] = [
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (val, sym) in TABLE {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    out
}

/// Parse a *canonical* roman numeral (i/v/x/l/c range, ≤8 chars) to its value.
/// Returns `None` for non-canonical strings, so ordinary words made of those
/// letters — "civil", "mix", "ill" — are not mistaken for numbers. Shared by
/// the TOC detector and the TOC formatter so the two stay in sync.
pub(super) fn canonical_roman_value(token: &str) -> Option<u32> {
    let lower = token.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.len() > 8 || !lower.chars().all(|c| "ivxlc".contains(c)) {
        return None;
    }
    let mut total = 0i32;
    let mut prev = 0i32;
    for c in lower.chars().rev() {
        let v = match c {
            'i' => 1,
            'v' => 5,
            'x' => 10,
            'l' => 50,
            'c' => 100,
            _ => return None,
        };
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    let value = u32::try_from(total).ok().filter(|&n| n > 0)?;
    (to_roman_lower(value) == lower).then_some(value)
}

/// Split a TextItem whose text contains multiple whitespace-separated tokens
/// (like "10 11 12 ... 31") into individual TextItems, each assigned to the
/// nearest column boundary.
fn split_merged_numbers(item: &TextItem, col_boundaries: &[f32]) -> Vec<TextItem> {
    let tokens: Vec<&str> = item.text.split_whitespace().collect();
    if tokens.len() <= 1 {
        return vec![item.clone()];
    }

    // Count consecutive leading numeric tokens (day numbers like "10 11 12")
    let leading_numeric = tokens
        .iter()
        .take_while(|t| t.chars().all(|c| c.is_ascii_digit()))
        .count();

    // Need at least one leading number to split
    if leading_numeric == 0 {
        return vec![item.clone()];
    }

    let token_width = item.width / tokens.len() as f32;
    let mut result = Vec::with_capacity(leading_numeric + 1);

    // Find the enclosing column boundary (rightmost boundary ≤ item.x + 2pt),
    // then advance through successive boundaries for each leading number.
    // Using rposition avoids overshooting when item.x sits between boundaries.
    let start_col = col_boundaries
        .iter()
        .rposition(|&cx| cx <= item.x + 2.0)
        .unwrap_or(0);

    // Split each leading numeric token into its own item at successive columns
    for (i, token) in tokens.iter().enumerate().take(leading_numeric) {
        let col_idx = start_col + i;
        let snapped_x = if col_idx < col_boundaries.len() {
            col_boundaries[col_idx]
        } else {
            // Fallback: distribute evenly if we run out of boundaries
            let raw_x = item.x + i as f32 * token_width + token_width / 2.0;
            col_boundaries
                .iter()
                .rev()
                .find(|&&cx| cx <= raw_x + 2.0)
                .copied()
                .unwrap_or(raw_x)
        };

        result.push(TextItem {
            text: token.to_string(),
            x: snapped_x,
            width: token_width,
            y: item.y,
            height: item.height,
            font: item.font.clone(),
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            is_underline: item.is_underline,
            is_strikeout: item.is_strikeout,
            item_type: item.item_type.clone(),
            mcid: item.mcid,
        });
    }

    // Trailing non-numeric tokens become annotation placed at last numeric column
    if leading_numeric < tokens.len() {
        let annotation = tokens[leading_numeric..].join(" ");
        let last_x = result.last().map(|i| i.x).unwrap_or(item.x);
        result.push(TextItem {
            text: annotation,
            x: last_x,
            width: token_width,
            y: item.y,
            height: item.height,
            font: item.font.clone(),
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            is_underline: item.is_underline,
            is_strikeout: item.is_strikeout,
            item_type: item.item_type.clone(),
            mcid: item.mcid,
        });
    }

    result
}

/// Detection mode controls thresholds for table validation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TableDetectionMode {
    /// Existing behavior: items with font size smaller than body text
    SmallFont,
    /// New: body-font items with stricter structural criteria
    BodyFont,
}

/// Build a table from layout-detected column boundaries.
///
/// When the layout engine detects multiple tabular columns (not newspaper),
/// this function uses those boundaries to construct a Table directly. This
/// handles borderless tables (no rects/lines) where columns are defined
/// purely by text alignment — common in exam/reference tables.
///
/// Requires ≥3 columns, ≥3 rows, and ≥40% cell fill rate.
pub(crate) fn try_build_table_from_columns(items: &[TextItem], page: u32) -> Option<Table> {
    use crate::extractor::{
        detect_columns, group_into_lines_with_thresholds, is_newspaper_layout, ColumnRegion,
    };
    use std::collections::HashMap;

    let mut columns = detect_columns(items, page, false);
    if columns.len() < 4 {
        return None;
    }

    // Refine columns: look for header-like rows where multiple items share
    // the same Y and are evenly spaced. If a wide column contains two header
    // items, split it at the gap between them.
    let page_items: Vec<&TextItem> = items.iter().filter(|i| i.page == page).collect();
    let y_tol = 3.0;

    // Find the top-most row with items in multiple columns (likely the header)
    let mut ys: Vec<f32> = page_items.iter().map(|i| i.y).collect();
    ys.sort_by(|a, b| b.total_cmp(a));
    ys.dedup_by(|a, b| (*a - *b).abs() < y_tol);

    for &header_y in ys.iter().take(5) {
        let row_items: Vec<&&TextItem> = page_items
            .iter()
            .filter(|i| (i.y - header_y).abs() < y_tol)
            .collect();
        if row_items.len() < columns.len() {
            continue;
        }
        // Check if any column contains 2+ items at this Y — needs splitting
        let mut new_columns = Vec::new();
        let mut did_split = false;
        for col in &columns {
            let col_items: Vec<&&&TextItem> = row_items
                .iter()
                .filter(|i| i.x >= col.x_min && i.x < col.x_max)
                .collect();
            if col_items.len() >= 2 {
                // Sort by X and find the split point
                let mut sorted: Vec<f32> = col_items.iter().map(|i| i.x).collect();
                sorted.sort_by(|a, b| a.total_cmp(b));
                // Split at the midpoint between the two items
                let split_x = (sorted[0]
                    + col_items.iter().find(|i| i.x == sorted[0]).unwrap().width
                    + sorted[1])
                    / 2.0;
                new_columns.push(ColumnRegion {
                    x_min: col.x_min,
                    x_max: split_x,
                });
                new_columns.push(ColumnRegion {
                    x_min: split_x,
                    x_max: col.x_max,
                });
                did_split = true;
            } else {
                new_columns.push(col.clone());
            }
        }
        if did_split {
            log::debug!(
                "column refinement: {} -> {} columns from header row at y={:.1}",
                columns.len(),
                new_columns.len(),
                header_y
            );
            columns = new_columns;
            break;
        }
    }

    // Group items into per-column lines to check newspaper vs tabular
    let mut col_buckets: Vec<Vec<TextItem>> = vec![Vec::new(); columns.len()];
    let mut spanning_items: Vec<TextItem> = Vec::new();
    for item in items {
        if item.page != page {
            continue;
        }
        // Check if item spans multiple columns
        let item_left = item.x;
        let item_right = item.x + item.width;
        let mut spans = 0;
        for col in &columns {
            let overlap = (item_right.min(col.x_max) - item_left.max(col.x_min)).max(0.0);
            if overlap > 0.0 {
                spans += 1;
            }
        }
        if spans > 1 {
            spanning_items.push(item.clone());
            continue;
        }
        // Assign to best-overlap column
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

    let thresholds = HashMap::new();
    let per_column_lines: Vec<Vec<crate::types::TextLine>> = col_buckets
        .iter()
        .map(|bucket| {
            group_into_lines_with_thresholds(
                bucket.clone(),
                &thresholds,
                &std::collections::HashSet::new(),
            )
        })
        .collect();

    // Must be tabular (not newspaper) layout
    if is_newspaper_layout(&per_column_lines, &columns) {
        return None;
    }

    // Collect all unique Y positions across all columns (row boundaries)
    let y_tol = 5.0;
    let mut row_ys: Vec<f32> = Vec::new();
    for col_lines in &per_column_lines {
        for line in col_lines {
            let y = line.y;
            if !row_ys.iter().any(|&ry| (ry - y).abs() < y_tol) {
                row_ys.push(y);
            }
        }
    }
    row_ys.sort_by(|a, b| b.total_cmp(a));

    if row_ys.len() < 3 || row_ys.len() > 40 {
        return None;
    }

    // Build cell grid
    let col_xs: Vec<f32> = columns.iter().map(|c| c.x_min).collect();
    let mut cells: Vec<Vec<String>> = vec![vec![String::new(); columns.len()]; row_ys.len()];
    let mut item_indices: Vec<usize> = Vec::new();

    for (item_idx, item) in items.iter().enumerate() {
        if item.page != page {
            continue;
        }
        // Find column
        let item_left = item.x;
        let item_right = item.x + item.width;
        let mut best_col = None;
        let mut best_overlap = 0.0f32;
        let mut span_count = 0;
        for (ci, col) in columns.iter().enumerate() {
            let overlap = (item_right.min(col.x_max) - item_left.max(col.x_min)).max(0.0);
            if overlap > 0.0 {
                span_count += 1;
            }
            if overlap > best_overlap {
                best_overlap = overlap;
                best_col = Some(ci);
            }
        }
        if span_count > 1 || best_col.is_none() {
            continue; // spanning item, skip
        }
        let col = best_col.unwrap();

        // Find row
        let row = row_ys.iter().position(|&ry| (ry - item.y).abs() < y_tol);
        if let Some(row) = row {
            if !cells[row][col].is_empty() {
                cells[row][col].push(' ');
            }
            cells[row][col].push_str(&item.text);
            item_indices.push(item_idx);
        }
    }
    merge_superscript_marker_rows(&mut row_ys, &mut cells);

    // Validate: need reasonable fill rate
    let total_cells = row_ys.len() * columns.len();
    let filled_cells = cells
        .iter()
        .flat_map(|r| r.iter())
        .filter(|c| !c.trim().is_empty())
        .count();
    let fill_rate = filled_cells as f32 / total_cells as f32;

    if fill_rate < 0.15 {
        return None;
    }

    // Need at least 40% of rows to have content in 2+ columns
    let multi_col_rows = cells
        .iter()
        .filter(|row| row.iter().filter(|c| !c.trim().is_empty()).count() >= 2)
        .count();
    // Need majority (>50%) of rows with content in 2+ columns
    if multi_col_rows * 2 < row_ys.len() {
        return None;
    }

    // Reject prose-like content: if cells are too long on average, this is
    // a multi-column text layout, not a data table. Real table cells are
    // typically short (≤ 40 chars). Prose paragraphs are much longer.
    let cell_lengths: Vec<usize> = cells
        .iter()
        .flat_map(|r| r.iter())
        .filter(|c| !c.trim().is_empty())
        .map(|c| c.trim().len())
        .collect();
    if !cell_lengths.is_empty() {
        let avg_cell_len = cell_lengths.iter().sum::<usize>() as f32 / cell_lengths.len() as f32;
        if avg_cell_len > 40.0 {
            return None;
        }
        // Reject if any significant number of cells are long prose (> 80 chars)
        let long_cells = cell_lengths.iter().filter(|&&len| len > 80).count();
        if long_cells as f32 / cell_lengths.len() as f32 > 0.10 {
            return None;
        }
    }

    // Reject when cells look like prose sentences: if too many cells contain
    // sentence-ending punctuation (.!?:) it's prose text, not table data.
    let prose_cells = cells
        .iter()
        .flat_map(|r| r.iter())
        .filter(|c| {
            let t = c.trim();
            t.len() > 20
                && (t.ends_with('.') || t.ends_with('!') || t.ends_with('?') || t.ends_with(':'))
        })
        .count();
    if filled_cells > 0 && prose_cells as f32 / filled_cells as f32 > 0.15 {
        return None;
    }

    // Reject when most content is in one column (newspaper-like asymmetry).
    // Count items per column; if any column has >60% of items, it's likely
    // a body text column with side annotations, not a data table.
    let mut items_per_col: Vec<usize> = vec![0; columns.len()];
    for row in &cells {
        for (ci, cell) in row.iter().enumerate() {
            if !cell.trim().is_empty() {
                items_per_col[ci] += 1;
            }
        }
    }
    let max_col_items = *items_per_col.iter().max().unwrap_or(&0);
    if filled_cells > 0 && max_col_items as f32 / filled_cells as f32 > 0.60 {
        return None;
    }

    log::debug!(
        "column-based table: {} cols x {} rows, fill={:.0}%, multi_col_rows={}",
        columns.len(),
        row_ys.len(),
        fill_rate * 100.0,
        multi_col_rows
    );

    Some(Table::new(col_xs, row_ys, cells, item_indices))
}

/// Build a region-scoped two-column key/value table from text baselines.
///
/// This intentionally lives outside the full-page heuristic detector. Layout
/// callers already supplied a table-shaped bbox, and some real table regions
/// are plain product/spec forms with only two visual columns. The main column
/// fallback starts at four columns to avoid newspaper/prose false positives;
/// this path keeps tighter key/value-specific guards instead.
pub(crate) fn try_build_key_value_table_from_rows(items: &[TextItem], page: u32) -> Option<Table> {
    let page_items: Vec<RowItem> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.page == page && !item.text.trim().is_empty())
        .map(|(idx, item)| RowItem {
            index: idx,
            item: item.clone(),
        })
        .collect();

    if page_items.len() < 2 {
        return None;
    }

    let median_font_size = median_f32(page_items.iter().map(|ri| ri.item.font_size).collect())
        .unwrap_or(10.0)
        .max(1.0);
    let y_tol = (median_font_size * 0.75).clamp(4.0, 9.0);
    let rows = group_key_value_visual_rows(page_items, y_tol);
    if rows.is_empty() || rows.len() > 80 {
        return None;
    }

    let split_x = infer_key_value_split_x(&rows, median_font_size)?;
    let mut kv_rows: Vec<KeyValueRow> = Vec::new();
    let mut left_starts = Vec::new();
    let mut right_starts = Vec::new();

    for row in &rows {
        let mut left_items = Vec::new();
        let mut right_items = Vec::new();
        for item in &row.items {
            if item.item.x < split_x {
                left_items.push(item);
            } else {
                right_items.push(item);
            }
        }

        let left = join_row_item_text(&left_items);
        let right = join_row_item_text(&right_items);
        if left.is_empty() && right.is_empty() {
            continue;
        }

        let mut item_indices: Vec<usize> = row.items.iter().map(|ri| ri.index).collect();
        item_indices.sort_unstable();
        item_indices.dedup();

        if !left.is_empty() && !right.is_empty() {
            if let Some(x) = left_items.first().map(|ri| ri.item.x) {
                left_starts.push(x);
            }
            if let Some(x) = right_items.first().map(|ri| ri.item.x) {
                right_starts.push(x);
            }
        }

        kv_rows.push(KeyValueRow {
            y: row.y,
            left,
            right,
            item_indices,
        });
    }

    if kv_rows.is_empty() {
        return None;
    }

    let raw_left_only_rows = kv_rows
        .iter()
        .filter(|row| !row.left.is_empty() && row.right.is_empty())
        .count();
    let raw_right_only_rows = kv_rows
        .iter()
        .filter(|row| row.left.is_empty() && !row.right.is_empty())
        .count();
    let edgar_tag_rows = key_value_rows_look_like_edgar_tags(&kv_rows);
    if edgar_tag_rows {
        kv_rows.retain(|row| !row.right.is_empty() || !is_edgar_table_boundary_cell(&row.left));
    }
    let header_inferred = !edgar_tag_rows && key_value_first_pair_is_header(&kv_rows);
    kv_rows = normalize_key_value_rows(kv_rows, header_inferred);

    let paired_rows = kv_rows
        .iter()
        .filter(|row| !row.left.is_empty() && !row.right.is_empty())
        .count();
    let section_rows = kv_rows
        .iter()
        .filter(|row| !row.left.is_empty() && row.right.is_empty())
        .count();
    let dangling_right_rows = kv_rows
        .iter()
        .filter(|row| row.left.is_empty() && !row.right.is_empty())
        .count();
    let left_label_like = kv_rows
        .iter()
        .filter(|row| !row.left.is_empty() && !row.right.is_empty())
        .filter(|row| looks_like_key_value_label(&row.left))
        .count();
    if paired_rows < 1 {
        return None;
    }
    if dangling_right_rows > 0 {
        return None;
    }

    let left_x = median_f32(left_starts).unwrap_or_else(|| {
        rows.iter()
            .flat_map(|row| row.items.iter().map(|ri| ri.item.x))
            .fold(f32::INFINITY, f32::min)
    });
    let right_x = median_f32(right_starts).unwrap_or(split_x);
    if !left_x.is_finite() || !right_x.is_finite() || right_x - left_x < 40.0 {
        return None;
    }

    let single_pair_allowed = key_value_single_pair_allowed(
        KeyValueSinglePairStats {
            paired_rows,
            section_rows,
            raw_left_only_rows,
            raw_right_only_rows,
        },
        &kv_rows,
        header_inferred,
        left_x,
        right_x,
    );
    if (kv_rows.len() < 2 || paired_rows < 2) && !single_pair_allowed {
        return None;
    }

    let data_pairs = if header_inferred {
        paired_rows.saturating_sub(1)
    } else {
        paired_rows
    };
    if data_pairs < 1 {
        return None;
    }

    if section_rows > paired_rows * 2 + 2 && !single_pair_allowed {
        return None;
    }

    let label_rows_for_score = if header_inferred {
        paired_rows.saturating_sub(1)
    } else {
        paired_rows
    };
    let label_like_for_score = if header_inferred && !kv_rows.is_empty() {
        left_label_like.saturating_sub(1)
    } else {
        left_label_like
    };
    if !header_inferred
        && !edgar_tag_rows
        && label_rows_for_score >= 2
        && label_like_for_score * 2 < label_rows_for_score
    {
        return None;
    }

    let right_cluster_count = significant_side_x_clusters(&rows, split_x, false);
    let marker_rows = marker_matrix_value_rows(&kv_rows);
    if !single_pair_allowed
        && !edgar_tag_rows
        && ((right_cluster_count >= 5 && paired_rows >= 3)
            || (right_cluster_count >= 3 && marker_rows >= 3 && marker_rows * 2 >= paired_rows))
    {
        return None;
    }

    if !edgar_tag_rows && key_value_rows_look_like_prose(&kv_rows, header_inferred) {
        return None;
    }

    let mut table_rows = Vec::new();
    let mut cells = Vec::new();
    let mut item_indices = Vec::new();

    let mut start_idx = 0usize;
    if header_inferred {
        let header = &kv_rows[0];
        table_rows.push(header.y);
        cells.push(vec![header.left.clone(), header.right.clone()]);
        item_indices.extend(header.item_indices.iter().copied());
        start_idx = 1;
    } else {
        table_rows.push(kv_rows.first().map(|row| row.y + y_tol).unwrap_or(0.0));
        cells.push(vec!["Field".to_string(), "Value".to_string()]);
    }

    for row in kv_rows.iter().skip(start_idx) {
        if !row.left.is_empty() && !row.right.is_empty() {
            table_rows.push(row.y);
            cells.push(vec![row.left.clone(), row.right.clone()]);
            item_indices.extend(row.item_indices.iter().copied());
        } else if !row.left.is_empty() {
            table_rows.push(row.y);
            cells.push(vec!["Section".to_string(), row.left.clone()]);
            item_indices.extend(row.item_indices.iter().copied());
        } else if !row.right.is_empty() {
            if let Some(last) = cells.last_mut() {
                if let Some(value) = last.get_mut(1) {
                    if !value.trim().is_empty() {
                        value.push(' ');
                    }
                    value.push_str(&row.right);
                    item_indices.extend(row.item_indices.iter().copied());
                }
            }
        }
    }

    if cells.len() < 2 {
        return None;
    }

    item_indices.sort_unstable();
    item_indices.dedup();

    log::debug!(
        "key-value table: {} rows, pairs={}, sections={}, split_x={:.1}",
        cells.len(),
        paired_rows,
        section_rows,
        split_x
    );

    Some(Table::new(
        vec![left_x, right_x],
        table_rows,
        cells,
        item_indices,
    ))
}

#[derive(Debug, Clone)]
struct RowItem {
    index: usize,
    item: TextItem,
}

#[derive(Debug, Clone)]
struct VisualRow {
    y: f32,
    items: Vec<RowItem>,
}

#[derive(Debug, Clone)]
struct KeyValueRow {
    y: f32,
    left: String,
    right: String,
    item_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
struct KeyValueSinglePairStats {
    paired_rows: usize,
    section_rows: usize,
    raw_left_only_rows: usize,
    raw_right_only_rows: usize,
}

fn normalize_key_value_rows(rows: Vec<KeyValueRow>, header_inferred: bool) -> Vec<KeyValueRow> {
    let mut normalized: Vec<KeyValueRow> = Vec::with_capacity(rows.len());

    for row in rows {
        if row.left.is_empty() && row.right.is_empty() {
            continue;
        }

        if row.left.is_empty() && !row.right.is_empty() {
            if let Some(last) = normalized.last_mut() {
                if !last.right.is_empty() {
                    append_key_value_text(&mut last.right, &row.right);
                    last.item_indices.extend(row.item_indices);
                    continue;
                }
            }
            normalized.push(row);
            continue;
        }

        if !row.left.is_empty() && row.right.is_empty() {
            let normalized_len = normalized.len();
            if let Some(last) = normalized.last_mut() {
                let last_is_header = header_inferred && normalized_len == 1;
                if !last_is_header
                    && !last.left.is_empty()
                    && !last.right.is_empty()
                    && key_value_left_continuation_allowed(&last.left, &row.left)
                {
                    append_key_value_text(&mut last.left, &row.left);
                    last.item_indices.extend(row.item_indices);
                    continue;
                }
            }
        }

        normalized.push(row);
    }

    normalized
}

fn append_key_value_text(target: &mut String, addition: &str) {
    let addition = addition.trim();
    if addition.is_empty() {
        return;
    }
    if !target.trim().is_empty() {
        target.push(' ');
    }
    target.push_str(addition);
}

fn key_value_left_continuation_allowed(previous_left: &str, continuation: &str) -> bool {
    let trimmed = continuation.trim();
    if trimmed.is_empty() || looks_like_key_value_section_label(trimmed) {
        return false;
    }

    let previous = previous_left.trim_end();
    let continuation_chars = trimmed.chars().count();
    let continuation_words = word_count_simple(trimmed);
    previous.ends_with(['-', '/', ',', ';', ':'])
        || first_alpha_is_lowercase(trimmed)
        || continuation_chars > 28
        || continuation_words > 4
}

fn group_key_value_visual_rows(mut items: Vec<RowItem>, y_tol: f32) -> Vec<VisualRow> {
    items.sort_by(|a, b| {
        b.item
            .y
            .total_cmp(&a.item.y)
            .then_with(|| a.item.x.total_cmp(&b.item.x))
    });

    let mut rows: Vec<VisualRow> = Vec::new();
    for row_item in items {
        if let Some(row) = rows
            .iter_mut()
            .find(|row| (row.y - row_item.item.y).abs() <= y_tol)
        {
            let len = row.items.len() as f32;
            row.y = (row.y * len + row_item.item.y) / (len + 1.0);
            row.items.push(row_item);
            continue;
        }

        rows.push(VisualRow {
            y: row_item.item.y,
            items: vec![row_item],
        });
    }

    for row in &mut rows {
        row.items.sort_by(|a, b| a.item.x.total_cmp(&b.item.x));
    }
    rows.sort_by(|a, b| b.y.total_cmp(&a.y));
    rows
}

fn infer_key_value_split_x(rows: &[VisualRow], median_font_size: f32) -> Option<f32> {
    let min_gap = (median_font_size * 2.0).max(24.0);
    let mut splits = Vec::new();

    for row in rows {
        if row.items.len() < 2 {
            continue;
        }

        let mut best_gap = 0.0f32;
        let mut best_split = None;
        for pair in row.items.windows(2) {
            let left = &pair[0].item;
            let right = &pair[1].item;
            let left_right = left.x + left.width.max(0.0);
            let gap = right.x - left_right;
            if gap > best_gap {
                best_gap = gap;
                best_split = Some(left_right + gap / 2.0);
            }
        }

        if best_gap >= min_gap {
            if let Some(split) = best_split {
                splits.push(split);
            }
        }
    }

    if splits.len() < 2 {
        let paired_visual_rows = rows.iter().filter(|row| row.items.len() >= 2).count();
        if splits.len() == 1
            && paired_visual_rows == 1
            && (rows.len() == 1 || rows.iter().all(|row| row.items.len() <= 2))
        {
            return splits.into_iter().next();
        }
        return None;
    }

    median_f32(splits)
}

fn join_row_item_text(items: &[&RowItem]) -> String {
    let mut parts = Vec::new();
    for item in items {
        let trimmed = item.item.text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed);
        }
    }
    normalize_cell_text(&parts.join(" "))
}

fn normalize_cell_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn key_value_first_pair_is_header(rows: &[KeyValueRow]) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    if first.left.is_empty() || first.right.is_empty() {
        return false;
    }
    if !looks_like_key_value_header_cell(&first.left)
        || !looks_like_key_value_header_cell(&first.right)
    {
        return false;
    }
    rows.iter()
        .skip(1)
        .any(|row| !row.left.is_empty() && !row.right.is_empty())
}

fn looks_like_key_value_header_cell(cell: &str) -> bool {
    let trimmed = cell.trim();
    if trimmed.len() < 2 || trimmed.len() > 40 {
        return false;
    }
    let words = word_count_simple(trimmed);
    if !(1..=4).contains(&words) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "yes" | "no" | "true" | "false" | "none" | "n/a" | "na"
    ) {
        return false;
    }
    trimmed.chars().any(|c| c.is_alphabetic())
        && !trimmed.chars().any(|c| c.is_ascii_digit())
        && !trimmed.ends_with(['.', ',', ';', ':'])
}

fn looks_like_key_value_label(cell: &str) -> bool {
    let trimmed = cell.trim();
    if trimmed.len() < 2 || trimmed.len() > 90 {
        return false;
    }
    let words = word_count_simple(trimmed);
    if words == 0 || words > 10 {
        return false;
    }
    if trimmed.ends_with(['.', ',', ';']) {
        return false;
    }
    trimmed.chars().any(|c| c.is_alphabetic())
}

fn key_value_rows_look_like_edgar_tags(rows: &[KeyValueRow]) -> bool {
    let paired_rows = rows
        .iter()
        .filter(|row| !row.left.is_empty() && !row.right.is_empty())
        .count();
    if paired_rows < 2 {
        return false;
    }

    let tag_pairs = rows
        .iter()
        .filter(|row| !row.left.is_empty() && !row.right.is_empty())
        .filter(|row| is_edgar_tag_cell(&row.left))
        .count();
    let first_marker = rows.first().is_some_and(|row| {
        row.left.eq_ignore_ascii_case("<S>") && row.right.eq_ignore_ascii_case("<C>")
    });

    tag_pairs >= 3 || (first_marker && tag_pairs >= 2)
}

fn is_edgar_tag_cell(cell: &str) -> bool {
    let trimmed = cell.trim();
    let Some(inner) = trimmed.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    !inner.is_empty()
        && inner.len() <= 48
        && inner
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn is_edgar_table_boundary_cell(cell: &str) -> bool {
    let trimmed = cell.trim();
    trimmed.eq_ignore_ascii_case("<TABLE>") || trimmed.eq_ignore_ascii_case("</TABLE>")
}

fn key_value_single_pair_allowed(
    stats: KeyValueSinglePairStats,
    rows: &[KeyValueRow],
    header_inferred: bool,
    left_x: f32,
    right_x: f32,
) -> bool {
    if header_inferred || stats.paired_rows != 1 || stats.section_rows != 0 || rows.len() != 1 {
        return false;
    }
    if right_x - left_x < 60.0 {
        return false;
    }

    let Some(row) = rows
        .iter()
        .find(|row| !row.left.is_empty() && !row.right.is_empty())
    else {
        return false;
    };
    let left_chars = row.left.chars().count();
    let right_chars = row.right.chars().count();
    if !(2..=120).contains(&left_chars) || right_chars == 0 {
        return false;
    }
    if key_value_cell_looks_like_sentence(&row.left) {
        return false;
    }
    if stats.raw_left_only_rows == 0
        && stats.raw_right_only_rows >= 2
        && left_chars <= 70
        && right_chars <= 1_500
        && looks_like_key_value_label(&row.left)
    {
        return true;
    }
    if right_chars > 80 {
        return false;
    }
    if key_value_cell_looks_like_sentence(&row.right) && !compact_key_value_scalar(&row.right) {
        return false;
    }

    (looks_like_key_value_label(&row.left) || left_chars <= 90)
        && compact_key_value_scalar(&row.right)
}

fn compact_key_value_scalar(cell: &str) -> bool {
    let trimmed = cell.trim();
    let chars = trimmed.chars().count();
    let words = word_count_simple(trimmed);
    if trimmed.is_empty() || chars > 60 || words > 6 || trimmed.ends_with(['.', '!', '?']) {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();
    trimmed.chars().any(|ch| ch.is_ascii_digit())
        || matches!(
            lower.as_str(),
            "yes" | "no" | "true" | "false" | "none" | "n/a" | "na"
        )
        || words <= 4
}

fn looks_like_key_value_section_label(cell: &str) -> bool {
    let trimmed = cell.trim();
    let chars = trimmed.chars().count();
    let words = word_count_simple(trimmed);
    if !(1..=5).contains(&words) || !(2..=48).contains(&chars) {
        return false;
    }
    if trimmed.ends_with(['.', ',', ';', ':']) || first_alpha_is_lowercase(trimmed) {
        return false;
    }
    if trimmed
        .chars()
        .any(|ch| matches!(ch, '.' | ',' | ';' | '(' | ')' | '[' | ']'))
    {
        return false;
    }

    trimmed.chars().any(|ch| ch.is_alphabetic())
}

fn first_alpha_is_lowercase(cell: &str) -> bool {
    cell.chars()
        .find(|ch| ch.is_alphabetic())
        .is_some_and(|ch| ch.is_lowercase())
}

fn key_value_cell_looks_like_sentence(cell: &str) -> bool {
    let trimmed = cell.trim();
    let chars = trimmed.chars().count();
    chars > 90
        || word_count_simple(trimmed) > 12
        || (chars > 42 && trimmed.ends_with(['.', '!', '?']))
}

fn key_value_rows_look_like_prose(rows: &[KeyValueRow], header_inferred: bool) -> bool {
    let mut left_cells = 0usize;
    let mut left_prose_cells = 0usize;
    let mut left_label_like = 0usize;
    let mut total_left_chars = 0usize;
    let mut paired_rows = 0usize;
    let mut paired_sentence_rows = 0usize;
    let mut solo_prose_rows = 0usize;

    for row in rows.iter().skip(usize::from(header_inferred)) {
        if !row.left.is_empty() && !row.right.is_empty() {
            paired_rows += 1;
            let left = row.left.trim();
            let right = row.right.trim();
            let left_prose = key_value_cell_looks_like_sentence(left);
            let right_prose = key_value_cell_looks_like_sentence(right);
            left_cells += 1;
            total_left_chars += left.chars().count();
            if looks_like_key_value_label(left) {
                left_label_like += 1;
            }
            if left_prose {
                left_prose_cells += 1;
            }
            if left_prose && right_prose {
                paired_sentence_rows += 1;
            }
        } else {
            let solo = if row.left.is_empty() {
                row.right.trim()
            } else {
                row.left.trim()
            };
            if solo.chars().count() > 70
                || word_count_simple(solo) > 9
                || (solo.chars().count() > 35 && solo.ends_with(['.', '!', '?']))
            {
                solo_prose_rows += 1;
            }
        }
    }

    if paired_rows < 1 || left_cells == 0 {
        return true;
    }
    if solo_prose_rows >= 3 {
        return true;
    }
    if paired_rows >= 2 && paired_sentence_rows * 2 >= paired_rows {
        return true;
    }
    if !header_inferred && left_prose_cells * 2 >= left_cells {
        return true;
    }

    let avg_left_chars = total_left_chars as f32 / left_cells as f32;
    !header_inferred && avg_left_chars > 70.0 && left_label_like * 2 < left_cells
}

fn marker_matrix_value_rows(rows: &[KeyValueRow]) -> usize {
    rows.iter()
        .filter(|row| !row.left.is_empty() && compact_marker_value(&row.right))
        .count()
}

fn compact_marker_value(cell: &str) -> bool {
    let trimmed = cell.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 {
        return false;
    }
    if trimmed.chars().any(|ch| ch.is_alphabetic()) {
        return false;
    }
    trimmed
        .chars()
        .any(|ch| ch.is_ascii_digit() || matches!(ch, '•' | '●' | '·'))
}

fn significant_side_x_clusters(rows: &[VisualRow], split_x: f32, left_side: bool) -> usize {
    let mut xs = Vec::new();
    for row in rows {
        for item in &row.items {
            let is_left = item.item.x < split_x;
            if is_left == left_side {
                xs.push(item.item.x);
            }
        }
    }
    xs.sort_by(|a, b| a.total_cmp(b));

    let mut counts = Vec::new();
    let mut center = None::<f32>;
    let mut count = 0usize;
    for x in xs {
        match center {
            Some(current) if (x - current).abs() <= 8.0 => {
                center = Some((current * count as f32 + x) / (count as f32 + 1.0));
                count += 1;
            }
            Some(_) => {
                counts.push(count);
                center = Some(x);
                count = 1;
            }
            None => {
                center = Some(x);
                count = 1;
            }
        }
    }
    if count > 0 {
        counts.push(count);
    }

    counts.into_iter().filter(|&count| count >= 2).count()
}

fn word_count_simple(cell: &str) -> usize {
    cell.split_whitespace()
        .filter(|word| word.chars().any(|c| c.is_alphanumeric()))
        .count()
}

fn median_f32(mut values: Vec<f32>) -> Option<f32> {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    Some(values[values.len() / 2])
}

fn merge_superscript_marker_rows(row_ys: &mut Vec<f32>, cells: &mut Vec<Vec<String>>) {
    let mut row_idx = 0;
    while row_idx < cells.len() {
        let non_empty: Vec<(usize, String)> = cells[row_idx]
            .iter()
            .enumerate()
            .filter_map(|(col_idx, cell)| {
                let trimmed = cell.trim();
                (!trimmed.is_empty()).then_some((col_idx, trimmed.to_string()))
            })
            .collect();

        if non_empty.len() != 1 || !is_superscript_marker_cell(&non_empty[0].1) {
            row_idx += 1;
            continue;
        }

        let (marker_col, marker) = &non_empty[0];
        let prev =
            (row_idx > 0).then(|| (row_idx - 1, (row_ys[row_idx - 1] - row_ys[row_idx]).abs()));
        let next = (row_idx + 1 < cells.len())
            .then(|| (row_idx + 1, (row_ys[row_idx] - row_ys[row_idx + 1]).abs()));
        let target = [prev, next]
            .into_iter()
            .flatten()
            .filter(|(_, gap)| *gap <= 10.0)
            .min_by(|(_, gap_a), (_, gap_b)| gap_a.total_cmp(gap_b))
            .map(|(idx, _)| idx);

        let Some(target_idx) = target else {
            row_idx += 1;
            continue;
        };

        let target_cell = &mut cells[target_idx][*marker_col];
        if target_cell.trim().is_empty() {
            *target_cell = marker.to_string();
        } else {
            target_cell.push_str(marker);
        }
        cells.remove(row_idx);
        row_ys.remove(row_idx);
    }
}

fn is_superscript_marker_cell(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 2
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '*' | '#' | 'o' | 'O' | '°' | 'º' | '†' | '‡'))
}

/// What kind of structure a detected `Table` represents. Classification is
/// computed once at construction so consumers don't have to re-analyze the
/// cells (and stay consistent across detection backends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableKind {
    /// A real data table — renders as markdown table syntax.
    #[default]
    Data,
    /// A table of contents — renders as a flat list with tab-aligned page
    /// numbers via `format_toc_as_list`. Detected through the table pipeline
    /// because TOCs share row/column structure with tables, but they are not
    /// data tables and shouldn't appear in `pages_with_tables` etc.
    Toc,
}

/// A detected table.
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
    /// Data table vs TOC. Set by `Table::new` from `cells`.
    pub kind: TableKind,
}

impl Table {
    /// Build a table and classify it (data vs TOC) from its cells.
    pub fn new(
        columns: Vec<f32>,
        rows: Vec<f32>,
        cells: Vec<Vec<String>>,
        item_indices: Vec<usize>,
    ) -> Self {
        let kind = if is_table_of_contents(&cells) {
            TableKind::Toc
        } else {
            TableKind::Data
        };
        Self {
            columns,
            rows,
            cells,
            item_indices,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemType, TextItem};

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
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn make_char(text: &str, x: f32, y: f32, font_size: f32, width: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width,
            height: font_size,
            font: "F1".into(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn test_table_detection() {
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

        let tables = detect_tables(&items, 10.0, false);
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
            kind: TableKind::Data,
        };

        let md = table_to_markdown(&table);
        assert!(md.contains("|Header 1|"));
        assert!(md.contains("|---|"));
        assert!(md.contains("|Cell 1|"));
    }

    #[test]
    fn test_merge_superscript_marker_rows() {
        let mut rows = vec![506.0, 500.0, 480.0];
        let mut cells = vec![
            vec!["".into(), "".into(), "*".into()],
            vec!["Name".into(), "Method".into(), "Typical values".into()],
            vec!["Flow".into(), "ASTM D1238".into(), "3.0".into()],
        ];

        merge_superscript_marker_rows(&mut rows, &mut cells);

        assert_eq!(rows, vec![500.0, 480.0]);
        assert_eq!(cells[0][2], "Typical values*");
    }

    #[test]
    fn test_column_builder_handles_borderless_specs_table() {
        let items = vec![
            make_char("*", 458.1, 544.2, 8.0, 4.4),
            make_char("Properties", 36.0, 538.6, 12.0, 53.1),
            make_char("Conditions", 195.8, 538.6, 12.0, 55.0),
            make_char("Method", 297.2, 538.6, 12.0, 39.4),
            make_char("Typical values", 384.1, 538.6, 12.0, 74.0),
            make_char("Units", 510.6, 538.6, 8.0, 17.9),
            make_char("Rheology", 36.0, 508.3, 10.0, 40.6),
            make_char("o", 209.8, 492.5, 6.5, 3.5),
            make_char("Melt Flow Rate", 36.0, 488.0, 10.0, 65.2),
            make_char("230 ", 190.4, 488.0, 10.0, 19.4),
            make_char("C/2.16 kg", 213.3, 488.0, 10.0, 42.8),
            make_char("ASTM D1238", 288.4, 488.0, 10.0, 56.8),
            make_char("3.0 ", 416.4, 488.0, 10.0, 16.9),
            make_char("g/10 min", 504.1, 488.0, 10.0, 39.5),
            make_char("Mechanical", 36.0, 451.5, 10.0, 48.3),
            make_char("Tensile Stress at Yield", 36.0, 431.3, 10.0, 96.7),
            make_char("50 mm/min", 197.9, 431.3, 10.0, 50.8),
            make_char("ASTM D638", 291.2, 431.3, 10.0, 51.3),
            make_char("31 ", 417.9, 431.3, 10.0, 13.9),
            make_char("MPa", 514.7, 431.3, 10.0, 18.4),
            make_char("Elongation at Yield", 36.0, 403.0, 10.0, 82.2),
            make_char("50 mm/min", 197.9, 403.0, 10.0, 50.8),
            make_char("ASTM D638", 291.2, 403.0, 10.0, 51.3),
            make_char("8 ", 420.6, 403.0, 10.0, 8.5),
            make_char("%", 519.1, 403.0, 10.0, 9.7),
            make_char("Flexural Modulus", 36.0, 374.6, 10.0, 74.0),
            make_char("ASTM D790", 291.2, 374.6, 10.0, 51.3),
            make_char("1400", 412.4, 374.6, 10.0, 21.8),
            make_char("MPa", 514.7, 374.6, 10.0, 18.4),
        ];

        let table = try_build_table_from_columns(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(
            md.contains("|Properties|Conditions|Method|Typical values*|Units|"),
            "{md}"
        );
        assert!(md.contains("|Mechanical|||||"), "{md}");
        assert!(
            md.contains("|Flexural Modulus||ASTM D790|1400|MPa|"),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_recovers_sectioned_specs_table() {
        let items = vec![
            make_char("Ordering Information", 69.0, 700.0, 9.0, 96.0),
            make_char("Package Contents", 69.0, 680.0, 9.0, 82.0),
            make_char(
                "CCH Adapter Panel with 3 m pigtail; installation guide",
                200.0,
                680.0,
                9.0,
                245.0,
            ),
            make_char("Units per Delivery", 69.0, 660.0, 9.0, 78.0),
            make_char("1/1", 200.0, 660.0, 9.0, 18.0),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.contains("|Field|Value|"), "{md}");
        assert!(md.contains("|Section|Ordering Information|"), "{md}");
        assert!(
            md.contains(
                "|Package Contents|CCH Adapter Panel with 3 m pigtail; installation guide|"
            ),
            "{md}"
        );
        assert!(md.contains("|Units per Delivery|1/1|"), "{md}");
    }

    #[test]
    fn test_key_value_builder_preserves_two_column_header() {
        let items = vec![
            make_char("Media", 86.0, 700.0, 10.0, 36.0),
            make_char("Options", 311.0, 700.0, 10.0, 44.0),
            make_char("BACnet/IP (Annex J)", 86.0, 680.0, 10.0, 115.0),
            make_char("Register as Foreign Device", 311.0, 680.0, 10.0, 138.0),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.starts_with("|Media|Options|"), "{md}");
        assert!(
            md.contains("|BACnet/IP (Annex J)|Register as Foreign Device|"),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_keeps_repeated_spec_sections() {
        let items = vec![
            make_char("1.33 DUAL VVT-i", 90.0, 700.0, 9.0, 82.0),
            make_char("Engine Code", 90.0, 682.0, 9.0, 62.0),
            make_char("1NR-FE", 406.0, 682.0, 9.0, 42.0),
            make_char("Type", 90.0, 664.0, 9.0, 24.0),
            make_char("Four cylinders in-line", 376.0, 664.0, 9.0, 104.0),
            make_char("1.6 VALVEMATIC", 90.0, 636.0, 9.0, 78.0),
            make_char("Engine Code", 90.0, 618.0, 9.0, 62.0),
            make_char("1ZR-FAE", 404.0, 618.0, 9.0, 44.0),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.contains("|Section|1.33 DUAL VVT-i|"), "{md}");
        assert!(md.contains("|Engine Code|1NR-FE|"), "{md}");
        assert!(md.contains("|Section|1.6 VALVEMATIC|"), "{md}");
        assert!(md.contains("|Engine Code|1ZR-FAE|"), "{md}");
    }

    #[test]
    fn test_key_value_builder_merges_wrapped_value_continuations() {
        let items = vec![
            make_char("Storage", 80.0, 700.0, 9.0, 42.0),
            make_char(
                "Store under normal conditions in dry rooms.",
                250.0,
                700.0,
                9.0,
                210.0,
            ),
            make_char(
                "Protect from heat and humidity in the original packaging material.",
                250.0,
                686.0,
                9.0,
                315.0,
            ),
            make_char("Shelf Life", 80.0, 668.0, 9.0, 48.0),
            make_char(
                "To obtain best performance use within 24 months.",
                250.0,
                668.0,
                9.0,
                255.0,
            ),
            make_char("Technical Information", 80.0, 650.0, 9.0, 104.0),
            make_char(
                "The product is designed for repeated industrial use and long service life.",
                250.0,
                650.0,
                9.0,
                340.0,
            ),
            make_char(
                "Additional details are provided for compatibility and installation planning.",
                250.0,
                636.0,
                9.0,
                350.0,
            ),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.contains("|Field|Value|"), "{md}");
        assert!(
            md.contains(
                "|Storage|Store under normal conditions in dry rooms. Protect from heat and humidity in the original packaging material.|"
            ),
            "{md}"
        );
        assert!(
            md.contains(
                "|Technical Information|The product is designed for repeated industrial use and long service life. Additional details are provided for compatibility and installation planning.|"
            ),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_merges_wrapped_left_labels() {
        let items = vec![
            make_char("Title/Description", 76.0, 700.0, 9.0, 86.0),
            make_char("Instances", 350.0, 700.0, 9.0, 48.0),
            make_char("RE: Homes Gerald Ford lived in.", 76.0, 682.0, 9.0, 150.0),
            make_char("Box 7", 350.0, 682.0, 9.0, 28.0),
            make_char(
                "Grand Rapids Remembers Gerald R. Ford issue. Grand",
                76.0,
                664.0,
                9.0,
                245.0,
            ),
            make_char("Box 7", 350.0, 664.0, 9.0, 28.0),
            make_char(
                "Rapids Magazine, September 1987, p. 65.",
                76.0,
                650.0,
                9.0,
                196.0,
            ),
            make_char(
                "A Workhorse not a show horse: Gerald Ford remembered as humble.",
                76.0,
                632.0,
                9.0,
                290.0,
            ),
            make_char("Box 7", 350.0, 632.0, 9.0, 28.0),
            make_char(
                "not flashy during his public life.",
                76.0,
                618.0,
                9.0,
                150.0,
            ),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.starts_with("|Title/Description|Instances|"), "{md}");
        assert!(
            md.contains(
                "|Grand Rapids Remembers Gerald R. Ford issue. Grand Rapids Magazine, September 1987, p. 65.|Box 7|"
            ),
            "{md}"
        );
        assert!(
            md.contains(
                "|A Workhorse not a show horse: Gerald Ford remembered as humble. not flashy during his public life.|Box 7|"
            ),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_allows_tiny_two_cell_region() {
        let items = vec![
            make_char(
                "3M E-A-R Classic Small Earplug Uncorded",
                80.0,
                700.0,
                9.0,
                210.0,
            ),
            make_char("02/05/24", 360.0, 700.0, 9.0, 42.0),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.contains("|Field|Value|"), "{md}");
        assert!(
            md.contains("|3M E-A-R Classic Small Earplug Uncorded|02/05/24|"),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_allows_single_wrapped_value_region() {
        let items = vec![
            make_char("Intrinsic Safety", 42.0, 174.0, 9.0, 60.0),
            make_char(
                "The powered air purifying respirator has been tested and classified",
                311.0,
                174.0,
                9.0,
                260.0,
            ),
            make_char(
                "for intrinsic safety in hazardous locations by Underwriters Laboratory",
                311.0,
                160.0,
                9.0,
                270.0,
            ),
            make_char(
                "for the following classes, divisions, groups, and temperature ratings.",
                311.0,
                146.0,
                9.0,
                275.0,
            ),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.contains("|Field|Value|"), "{md}");
        assert!(
            md.contains(
                "|Intrinsic Safety|The powered air purifying respirator has been tested and classified for intrinsic safety in hazardous locations by Underwriters Laboratory for the following classes, divisions, groups, and temperature ratings.|"
            ),
            "{md}"
        );
    }

    #[test]
    fn test_key_value_builder_rejects_leading_value_only_prose() {
        let items = vec![
            make_char(
                "3rd Party Authorization documenting the reason for the hardship.",
                260.0,
                714.0,
                9.0,
                310.0,
            ),
            make_char("Borrower", 80.0, 696.0, 9.0, 44.0),
            make_char(
                "Homeowner has adequate income to support modified payments.",
                260.0,
                696.0,
                9.0,
                300.0,
            ),
            make_char("Servicer", 80.0, 678.0, 9.0, 42.0),
            make_char(
                "Collects documentation and reviews hardship status.",
                260.0,
                678.0,
                9.0,
                260.0,
            ),
        ];

        assert!(try_build_key_value_table_from_rows(&items, 1).is_none());
    }

    #[test]
    fn test_key_value_builder_recovers_edgar_tag_value_rows() {
        let items = vec![
            make_char("<S>", 70.0, 700.0, 9.0, 18.0),
            make_char("<C>", 240.0, 700.0, 9.0, 18.0),
            make_char("<PERIOD-TYPE>", 70.0, 684.0, 9.0, 78.0),
            make_char("3-MOS", 240.0, 684.0, 9.0, 30.0),
            make_char("<FISCAL-YEAR-END>", 70.0, 668.0, 9.0, 104.0),
            make_char("DEC-31-2000", 240.0, 668.0, 9.0, 66.0),
            make_char("<PERIOD-END>", 70.0, 652.0, 9.0, 76.0),
            make_char("MAR-31-2000", 240.0, 652.0, 9.0, 66.0),
            make_char("<CASH>", 70.0, 636.0, 9.0, 38.0),
            make_char("214", 240.0, 636.0, 9.0, 18.0),
            make_char("</TABLE>", 70.0, 620.0, 9.0, 46.0),
        ];

        let table = try_build_key_value_table_from_rows(&items, 1).unwrap();
        let md = table_to_markdown(&table);

        assert!(md.starts_with("|Field|Value|"), "{md}");
        assert!(md.contains("|<S>|<C>|"), "{md}");
        assert!(md.contains("|<FISCAL-YEAR-END>|DEC-31-2000|"), "{md}");
        assert!(md.contains("|<CASH>|214|"), "{md}");
        assert!(!md.contains("</TABLE>"), "{md}");
    }

    #[test]
    fn test_key_value_builder_rejects_split_prose() {
        let items = vec![
            make_char(
                "This paragraph describes an operational process and continues without a field label.",
                70.0,
                700.0,
                10.0,
                350.0,
            ),
            make_char(
                "It was split only because the text wrapped across a wide line.",
                455.0,
                700.0,
                10.0,
                300.0,
            ),
            make_char(
                "Another sentence explains background context rather than a measurable property.",
                70.0,
                680.0,
                10.0,
                350.0,
            ),
            make_char(
                "The neighboring phrase is not a value and should not form a table.",
                455.0,
                680.0,
                10.0,
                300.0,
            ),
            make_char(
                "Finally, this narrative line keeps flowing with normal prose content.",
                70.0,
                660.0,
                10.0,
                350.0,
            ),
            make_char(
                "It has punctuation and complete sentences on both sides of the gap.",
                455.0,
                660.0,
                10.0,
                300.0,
            ),
        ];

        assert!(try_build_key_value_table_from_rows(&items, 1).is_none());
    }

    #[test]
    fn test_body_font_table_detected() {
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

        let tables = detect_tables(&items, 10.0, false);
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

        let tables = detect_tables(&items, 10.0, false);
        assert_eq!(
            tables.len(),
            0,
            "Single-column paragraph must not be detected as table"
        );
    }

    #[test]
    fn test_word_level_paragraph_not_detected_as_table() {
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

        let tables = detect_tables(&items, 10.0, false);
        assert_eq!(
            tables.len(),
            0,
            "Word-level paragraph text must not be detected as table"
        );
    }

    #[test]
    fn test_large_data_table_not_rejected() {
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

        let tables = detect_tables(&items, 10.0, false);
        assert_eq!(tables.len(), 1, "Large data table should not be rejected");
        assert!(
            tables[0].rows.len() >= 40,
            "Large table should preserve most rows, got {}",
            tables[0].rows.len()
        );
    }

    #[test]
    fn test_uniform_spacing_rows_not_merged() {
        let companies = [
            "SC Priority LLC",
            "Craft Roofing Co",
            "Alpha Roofing Inc",
            "Beta Construction",
            "Gamma Builders",
            "Delta Roofing",
            "Epsilon Contractors",
        ];

        let mut items = Vec::new();

        // Header row at y=800
        items.push(make_item("No.", 50.0, 800.0, 8.0));
        items.push(make_item("Company", 120.0, 800.0, 8.0));
        items.push(make_item("Bid Amount", 350.0, 800.0, 8.0));

        // 7 data rows, each 10pt apart (exactly the old threshold)
        for (i, company) in companies.iter().enumerate() {
            let y = 790.0 - (i as f32 * 10.0);
            items.push(make_item(&format!("{}", i + 1), 50.0, y, 8.0));
            items.push(make_item(company, 120.0, y, 8.0));
            items.push(make_item(&format!("${},000", 100 + i * 10), 350.0, y, 8.0));
        }

        let tables = detect_tables(&items, 12.0, false);
        assert_eq!(tables.len(), 1, "Should detect one table");
        assert_eq!(
            tables[0].rows.len(),
            8,
            "Each company must be on its own row, got {} rows instead of 8",
            tables[0].rows.len()
        );
    }

    #[test]
    fn test_merge_adjacent_items() {
        let items = vec![
            make_char("J", 310.0, 532.0, 13.3, 4.0),
            make_char("u", 314.0, 532.0, 13.3, 4.4),
            make_char("n", 318.4, 532.0, 13.3, 4.4),
            make_char("e", 322.8, 532.0, 13.3, 3.5),
            // word gap (2pt)
            make_char("3", 328.3, 532.0, 13.3, 4.0),
            make_char("0", 332.3, 532.0, 13.3, 4.0),
            make_char(",", 336.3, 532.0, 13.3, 2.0),
            // large column gap (40pt)
            make_char("M", 378.3, 532.0, 13.3, 7.5),
            make_char("a", 385.8, 532.0, 13.3, 4.0),
            make_char("r", 389.8, 532.0, 13.3, 3.5),
        ];

        let (merged, map) = detect_heuristic::merge_adjacent_items(&items);

        assert_eq!(
            merged.len(),
            2,
            "Should produce 2 merged items, got {}",
            merged.len()
        );
        assert!(
            merged[0].text.contains("June") && merged[0].text.contains("30"),
            "First merged item should be 'June 30,' but got {:?}",
            merged[0].text
        );
        assert_eq!(merged[1].text, "Mar");

        assert_eq!(
            map[0].len(),
            7,
            "First merged item should map to 7 original chars"
        );
        assert_eq!(
            map[1].len(),
            3,
            "Second merged item should map to 3 original chars"
        );
    }

    #[test]
    fn test_per_char_financial_table_detected() {
        let mut items = Vec::new();

        // Per-character header row
        for (i, c) in "Col1".chars().enumerate() {
            items.push(make_char(
                &c.to_string(),
                300.0 + i as f32 * 5.0,
                540.0,
                13.0,
                5.0,
            ));
        }
        for (i, c) in "Col2".chars().enumerate() {
            items.push(make_char(
                &c.to_string(),
                400.0 + i as f32 * 5.0,
                540.0,
                13.0,
                5.0,
            ));
        }
        for (i, c) in "Col3".chars().enumerate() {
            items.push(make_char(
                &c.to_string(),
                500.0 + i as f32 * 5.0,
                540.0,
                13.0,
                5.0,
            ));
        }

        // Data rows with multi-word items
        let data = [
            ("Revenue", 520.0, "1,000", "2,000", "3,000"),
            ("Expenses", 505.0, "500", "800", "1,200"),
            ("Net Income", 490.0, "500", "1,200", "1,800"),
            ("Taxes", 475.0, "100", "200", "300"),
        ];

        for (label, y, v1, v2, v3) in &data {
            items.push(make_item(label, 50.0, *y, 12.0));
            items.push(make_item(v1, 310.0, *y, 12.0));
            items.push(make_item(v2, 410.0, *y, 12.0));
            items.push(make_item(v3, 510.0, *y, 12.0));
        }

        let tables = detect_tables(&items, 13.0, false);
        assert!(
            !tables.is_empty(),
            "Per-character financial table should be detected"
        );
    }

    #[test]
    fn test_short_subheader_not_merged_as_continuation() {
        // Simulate a table with section sub-headers (like month names) that have
        // an empty first column and short text in a single other column.
        // These should NOT be merged into the previous row as continuation text.
        let table = Table {
            columns: vec![50.0, 150.0, 300.0, 450.0],
            rows: vec![500.0, 480.0, 460.0, 440.0, 420.0, 400.0],
            cells: vec![
                // Header row
                vec!["No.".into(), "Date".into(), "Title".into(), "Amount".into()],
                // Sub-header: month name in 1 column, rest empty
                vec!["".into(), "JAN".into(), "".into(), "".into()],
                // Data row
                vec!["1".into(), "8/1".into(), "Item A".into(), "100".into()],
                vec!["2".into(), "15/1".into(), "Item B".into(), "200".into()],
                // Another sub-header
                vec!["".into(), "FEB".into(), "".into(), "".into()],
                // Data row
                vec!["3".into(), "5/2".into(), "Item C".into(), "300".into()],
            ],
            item_indices: vec![],
            kind: TableKind::Data,
        };

        let md = table_to_markdown(&table);
        // JAN and FEB should be on their own rows, not merged into adjacent rows
        assert!(
            md.contains("|JAN|"),
            "JAN should be on its own row, got:\n{}",
            md
        );
        assert!(
            md.contains("|FEB|"),
            "FEB should be on its own row, got:\n{}",
            md
        );
        // Verify they're NOT merged into data rows
        assert!(
            !md.contains("15/1 FEB"),
            "FEB should not be merged into data row, got:\n{}",
            md
        );
        assert!(
            !md.contains("8/1 JAN"),
            "JAN should not be merged into data row, got:\n{}",
            md
        );
    }

    // ── Rect-guided table builder tests ─────────────────────────────

    #[test]
    fn rect_guided_basic() {
        // 7 column boundaries (like days of week), items "1"-"7" at matching X
        let col_xs: Vec<f32> = (0..7).map(|i| 50.0 + i as f32 * 30.0).collect();
        let cluster_rects: Vec<(f32, f32, f32, f32)> =
            col_xs.iter().map(|&x| (x, 100.0, 28.0, 15.0)).collect();
        let items: Vec<TextItem> = (1..=7)
            .map(|i| make_item(&i.to_string(), col_xs[i - 1] + 2.0, 110.0, 7.0))
            .collect();

        let table = try_build_rect_guided_table(&items, &cluster_rects);
        assert!(table.is_some(), "Should produce a table from 7 columns");
        let table = table.unwrap();
        assert_eq!(table.columns.len(), 7);
        assert_eq!(table.rows.len(), 1);
        for (i, cell) in table.cells[0].iter().enumerate() {
            assert_eq!(cell, &(i + 1).to_string());
        }
    }

    #[test]
    fn rect_guided_split_merged() {
        // One merged item "10 11 12" spanning 3 column boundaries
        let col_xs: Vec<f32> = (0..7).map(|i| 50.0 + i as f32 * 30.0).collect();
        let cluster_rects: Vec<(f32, f32, f32, f32)> =
            col_xs.iter().map(|&x| (x, 100.0, 28.0, 15.0)).collect();
        // Single items for cols 0-3, merged "4 5 6" spanning cols 4-6
        let mut items = vec![
            make_item("1", col_xs[0] + 2.0, 110.0, 7.0),
            make_item("2", col_xs[1] + 2.0, 110.0, 7.0),
            make_item("3", col_xs[2] + 2.0, 110.0, 7.0),
        ];
        // Merged item spanning from col 3 to col 5 (width covers 3 columns)
        let mut merged = make_item("4 5 6", col_xs[3], 110.0, 7.0);
        merged.width = 3.0 * 30.0; // spans 3 column widths
        items.push(merged);

        let table = try_build_rect_guided_table(&items, &cluster_rects);
        assert!(table.is_some(), "Should handle merged number items");
        let table = table.unwrap();
        // Check that "4", "5", "6" ended up in separate columns
        let row = &table.cells[0];
        assert!(
            row.contains(&"4".to_string()),
            "Should have '4' in a cell: {:?}",
            row
        );
        assert!(
            row.contains(&"5".to_string()),
            "Should have '5' in a cell: {:?}",
            row
        );
        assert!(
            row.contains(&"6".to_string()),
            "Should have '6' in a cell: {:?}",
            row
        );
    }

    #[test]
    fn rect_guided_with_annotations() {
        // Day numbers on one row, annotations on a second row
        let col_xs: Vec<f32> = (0..7).map(|i| 50.0 + i as f32 * 30.0).collect();
        let cluster_rects: Vec<(f32, f32, f32, f32)> =
            col_xs.iter().map(|&x| (x, 100.0, 28.0, 15.0)).collect();
        let mut items: Vec<TextItem> = (1..=7)
            .map(|i| make_item(&i.to_string(), col_xs[i - 1] + 2.0, 115.0, 7.0))
            .collect();
        // Add annotation "Holiday" under day 4
        items.push(make_item("Holiday", col_xs[3] + 2.0, 105.0, 6.0));

        let table = try_build_rect_guided_table(&items, &cluster_rects);
        assert!(table.is_some());
        let table = table.unwrap();
        assert_eq!(
            table.rows.len(),
            2,
            "Should have 2 rows (days + annotations)"
        );
        // The annotation row should have "Holiday" in column 3
        assert_eq!(table.cells[1][3], "Holiday");
    }

    #[test]
    fn rect_guided_too_few_columns() {
        // Only 3 column boundaries → should return None (need ≥ 5)
        let cluster_rects = vec![
            (50.0, 100.0, 28.0, 15.0),
            (80.0, 100.0, 28.0, 15.0),
            (110.0, 100.0, 28.0, 15.0),
        ];
        let items = vec![
            make_item("A", 52.0, 110.0, 7.0),
            make_item("B", 82.0, 110.0, 7.0),
            make_item("C", 112.0, 110.0, 7.0),
        ];
        let table = try_build_rect_guided_table(&items, &cluster_rects);
        assert!(table.is_none(), "Should reject fewer than 5 columns");
    }

    #[test]
    fn split_merged_numbers_single_token() {
        let col_boundaries = vec![50.0, 80.0, 110.0, 140.0, 170.0];
        let item = make_item("Holiday", 52.0, 110.0, 7.0);
        let result = split_merged_numbers(&item, &col_boundaries);
        assert_eq!(result.len(), 1, "Single-token item should not be split");
        assert_eq!(result[0].text, "Holiday");
    }

    #[test]
    fn split_leading_numbers_with_annotation() {
        // "11 Veterans Day" → "11" split off, "Veterans Day" as annotation
        let col_boundaries = vec![50.0, 80.0, 110.0, 140.0, 170.0];
        let mut item = make_item("11 Veterans Day", 110.0, 110.0, 7.0);
        item.width = 90.0; // spans 3 tokens
        let result = split_merged_numbers(&item, &col_boundaries);
        assert_eq!(result.len(), 2, "Should split into number + annotation");
        assert_eq!(result[0].text, "11");
        assert_eq!(result[1].text, "Veterans Day");
    }

    #[test]
    fn split_multiple_leading_numbers_with_annotation() {
        // "24 25 Memorial Day" → "24", "25" split, "Memorial Day" trails
        let col_xs: Vec<f32> = (0..7).map(|i| 50.0 + i as f32 * 30.0).collect();
        let mut item = make_item("24 25 Memorial Day", col_xs[3], 110.0, 7.0);
        item.width = 4.0 * 30.0; // spans 4 tokens
        let result = split_merged_numbers(&item, &col_xs);
        assert_eq!(result.len(), 3, "Should split into 2 numbers + annotation");
        assert_eq!(result[0].text, "24");
        assert_eq!(result[1].text, "25");
        assert_eq!(result[2].text, "Memorial Day");
    }

    #[test]
    fn split_no_leading_numbers() {
        // "Memorial Day" → no leading numeric, returned as-is
        let col_boundaries = vec![50.0, 80.0, 110.0, 140.0, 170.0];
        let item = make_item("Memorial Day", 52.0, 110.0, 7.0);
        let result = split_merged_numbers(&item, &col_boundaries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "Memorial Day");
    }

    #[test]
    fn rect_guided_tilde_cleanup() {
        // Items with tilde noise should have it stripped
        let col_xs: Vec<f32> = (0..7).map(|i| 50.0 + i as f32 * 30.0).collect();
        let cluster_rects: Vec<(f32, f32, f32, f32)> =
            col_xs.iter().map(|&x| (x, 100.0, 28.0, 15.0)).collect();
        let mut items: Vec<TextItem> = (1..=7)
            .map(|i| make_item(&i.to_string(), col_xs[i - 1] + 2.0, 110.0, 7.0))
            .collect();
        // Day 7 has tilde-leader legend text bleeding in
        items[6] = make_item("7 ~~~~~~~ Legend text here", col_xs[6] + 2.0, 110.0, 7.0);

        let table = try_build_rect_guided_table(&items, &cluster_rects).unwrap();
        assert_eq!(table.cells[0][6], "7", "Tilde noise should be stripped");
    }
}
