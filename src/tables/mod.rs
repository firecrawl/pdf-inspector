//! Table detection and formatting.
//!
//! Detects tabular data in PDF text items and converts to markdown tables.

mod detect_heuristic;
mod detect_lines;
mod detect_rects;
mod financial;
mod format;
mod grid;

pub use detect_heuristic::detect_tables;
pub use detect_lines::detect_tables_from_lines;
pub(crate) use detect_rects::cluster_rects;
pub use detect_rects::{detect_tables_from_rects, RectHintRegion};
pub use format::table_to_markdown;

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
    x_lefts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
        spacings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
    y_values.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)); // descending
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

    Some(Table {
        columns: col_boundaries,
        rows: row_boundaries,
        cells,
        item_indices: used_indices,
    })
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
        };

        let md = table_to_markdown(&table);
        assert!(md.contains("|Header 1|"));
        assert!(md.contains("|---|"));
        assert!(md.contains("|Cell 1|"));
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
