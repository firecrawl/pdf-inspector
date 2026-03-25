//! Table-to-markdown formatting and cell cleanup.

use super::Table;

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

    // Compact format: no padding, minimal separators. Optimized for token
    // efficiency — AI agents are the primary consumer, not human eyes.
    for (row_idx, row) in cleaned_cells.iter().enumerate() {
        output.push('|');
        for cell in row.iter() {
            output.push_str(cell);
            output.push('|');
        }
        output.push('\n');

        // Add separator after header row
        if row_idx == 0 {
            output.push('|');
            for _ in 0..num_cols {
                output.push_str("---|");
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

        // Check if this is a continuation row (first column is empty but others have content).
        // A row with only 1 short non-empty cell (besides the first) is more likely a
        // section sub-header (e.g. "JAN", "FEB") than overflow text — don't merge it.
        // A row with content in many columns is a real data row with a merged/spanning
        // first-column cell (e.g. n₂ in a statistical table), not text overflow.
        let non_first_cells: Vec<&str> = row
            .iter()
            .skip(1)
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        let is_short_subheader = non_first_cells.len() == 1 && non_first_cells[0].len() <= 5;
        // Rows with multiple short-valued cells (e.g. numeric data in a lookup
        // table) are data rows with a merged/spanning first column, not text
        // overflow from the previous row.  Continuation rows typically have
        // longer descriptive text; data rows have short numeric values.
        let avg_cell_len = if non_first_cells.is_empty() {
            0.0
        } else {
            non_first_cells.iter().map(|c| c.len()).sum::<usize>() as f32
                / non_first_cells.len() as f32
        };
        let numeric_cells = non_first_cells
            .iter()
            .filter(|c| {
                c.chars().all(|ch| {
                    ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == ',' || ch == ' '
                })
            })
            .count();
        let looks_like_data_row = non_first_cells.len() >= 2
            && avg_cell_len <= 10.0
            && numeric_cells > non_first_cells.len() / 2;
        // Classic continuation: first cell empty, content in other cells
        let is_classic_continuation = first_cell.is_empty()
            && !non_first_cells.is_empty()
            && !is_short_subheader
            && !looks_like_data_row
            && cleaned.len() > 1;

        // Wrapped-cell continuation: row has fewer filled cells than the header
        // row, suggesting it's overflow text from the previous row's cells.
        // Only trigger when the previous row has significantly more filled cells.
        let num_cols = row.len();
        let filled_cells = row.iter().filter(|c| !c.trim().is_empty()).count();
        let prev_filled = cleaned
            .last()
            .map(|r| r.iter().filter(|c| !c.trim().is_empty()).count())
            .unwrap_or(0);
        let header_filled = cleaned
            .first()
            .map(|r| r.iter().filter(|c| !c.trim().is_empty()).count())
            .unwrap_or(num_cols);
        // Merge when the row has significantly fewer filled cells than header.
        // For wide tables (5+ cols), require ≤50% of header cells.
        // For narrow tables (2-4 cols), require fewer than header cells.
        // This prevents merging normal data rows in wide tables (6_KE_Chart)
        // while allowing continuation merging in narrow tables (178).
        let max_filled_for_merge = if header_filled >= 5 {
            header_filled / 2
        } else {
            header_filled.saturating_sub(1)
        };
        let is_wrapped_continuation = cleaned.len() > 1
            && filled_cells <= max_filled_for_merge
            && prev_filled > filled_cells
            && !looks_like_data_row
            && !is_short_subheader;

        let is_continuation = is_classic_continuation || is_wrapped_continuation;

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

    // --- is_footnote_row ---

    #[test]
    fn test_is_footnote_row_parenthesized_number() {
        assert!(is_footnote_row("(1)"));
        assert!(is_footnote_row("(23)"));
    }

    #[test]
    fn test_is_footnote_row_number_paren() {
        assert!(is_footnote_row("1)"));
        assert!(is_footnote_row("12)"));
    }

    #[test]
    fn test_is_footnote_row_note_colon() {
        assert!(is_footnote_row("Note: some text"));
        assert!(is_footnote_row("note: lowercase"));
    }

    #[test]
    fn test_is_footnote_row_notes_colon() {
        assert!(is_footnote_row("Notes: multiple"));
        assert!(is_footnote_row("NOTES: uppercase"));
    }

    #[test]
    fn test_is_footnote_row_plain_text_false() {
        assert!(!is_footnote_row("Regular cell text"));
        assert!(!is_footnote_row("Amount"));
    }

    #[test]
    fn test_is_footnote_row_empty_false() {
        assert!(!is_footnote_row(""));
    }

    // --- clean_table_cells ---

    #[test]
    fn test_clean_table_cells_empty_rows_removed() {
        let cells = vec![
            vec!["A".into(), "B".into()],
            vec!["".into(), "".into()],
            vec!["C".into(), "D".into()],
        ];
        let (cleaned, _) = clean_table_cells(&cells);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0], vec!["A", "B"]);
        assert_eq!(cleaned[1], vec!["C", "D"]);
    }

    #[test]
    fn test_clean_table_cells_footnote_extracted() {
        let cells = vec![
            vec!["Header".into(), "Value".into()],
            vec!["Data".into(), "100".into()],
            vec!["(1)".into(), "See appendix".into()],
        ];
        let (cleaned, footnotes) = clean_table_cells(&cells);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(footnotes.len(), 1);
        assert!(footnotes[0].contains("(1)"));
        assert!(footnotes[0].contains("See appendix"));
    }

    #[test]
    fn test_clean_table_cells_continuation_row_merged() {
        let cells = vec![
            vec!["Header".into(), "Col2".into()],
            vec!["Row1".into(), "Short".into()],
            vec!["".into(), "continued text here".into()],
        ];
        let (cleaned, _) = clean_table_cells(&cells);
        // The continuation row should merge into the previous row
        assert_eq!(cleaned.len(), 2);
        assert!(cleaned[1][1].contains("Short"));
        assert!(cleaned[1][1].contains("continued text here"));
    }

    #[test]
    fn test_clean_table_cells_short_subheader_not_merged() {
        let cells = vec![
            vec!["Header".into(), "Col2".into()],
            vec!["Row1".into(), "Data".into()],
            vec!["".into(), "JAN".into()],
        ];
        let (cleaned, _) = clean_table_cells(&cells);
        // Short subheader (<=5 chars, single non-empty cell) should not merge
        assert_eq!(cleaned.len(), 3);
    }

    #[test]
    fn test_clean_table_cells_numeric_data_row_not_merged() {
        let cells = vec![
            vec!["Header".into(), "A".into(), "B".into(), "C".into()],
            vec!["Row1".into(), "10".into(), "20".into(), "30".into()],
            vec!["".into(), "40".into(), "50".into(), "60".into()],
        ];
        let (cleaned, _) = clean_table_cells(&cells);
        // Numeric data row with empty first col should not merge
        assert_eq!(cleaned.len(), 3);
    }

    #[test]
    fn test_clean_table_cells_header_row_not_merged() {
        // Continuation requires cleaned.len() > 1 (don't merge into header)
        let cells = vec![
            vec!["Header".into(), "Col2".into()],
            vec!["".into(), "continuation text goes here".into()],
        ];
        let (cleaned, _) = clean_table_cells(&cells);
        // Should not merge into first row (header)
        assert_eq!(cleaned.len(), 2);
    }

    #[test]
    fn test_clean_table_cells_all_empty() {
        let cells = vec![vec!["".into(), "".into()], vec!["  ".into(), "".into()]];
        let (cleaned, footnotes) = clean_table_cells(&cells);
        assert!(cleaned.is_empty());
        assert!(footnotes.is_empty());
    }

    #[test]
    fn test_clean_table_cells_mixed_scenario() {
        let cells = vec![
            vec!["Name".into(), "Score".into()],
            vec!["Alice".into(), "95".into()],
            vec!["".into(), "".into()],
            vec!["Bob".into(), "87".into()],
            vec!["Note: graded on curve".into(), "".into()],
        ];
        let (cleaned, footnotes) = clean_table_cells(&cells);
        assert_eq!(cleaned.len(), 3); // header + Alice + Bob (empty row removed)
        assert_eq!(footnotes.len(), 1);
        assert!(footnotes[0].contains("Note:"));
    }

    // --- table_to_markdown ---

    #[test]
    fn test_table_to_markdown_basic() {
        let table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0, 460.0],
            cells: vec![
                vec!["Name".into(), "Age".into()],
                vec!["Alice".into(), "30".into()],
                vec!["Bob".into(), "25".into()],
            ],
            item_indices: vec![],
        };
        let md = table_to_markdown(&table);
        assert!(md.contains("|Name|"));
        assert!(md.contains("|---|"));
        assert!(md.contains("|Alice|"));
        assert!(md.contains("|Bob|"));
    }

    #[test]
    fn test_table_to_markdown_single_row() {
        let table = Table {
            columns: vec![100.0],
            rows: vec![500.0],
            cells: vec![vec!["Only".into(), "Row".into()]],
            item_indices: vec![],
        };
        let md = table_to_markdown(&table);
        assert!(md.contains("|Only|"));
        assert!(md.contains("|---|"));
    }

    #[test]
    fn test_table_to_markdown_empty_table() {
        let table = Table {
            columns: vec![],
            rows: vec![],
            cells: vec![],
            item_indices: vec![],
        };
        assert_eq!(table_to_markdown(&table), "");
    }

    #[test]
    fn test_table_to_markdown_footnotes_appended() {
        let table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0, 460.0],
            cells: vec![
                vec!["Header".into(), "Value".into()],
                vec!["Data".into(), "100".into()],
                vec!["(1)".into(), "Footnote text".into()],
            ],
            item_indices: vec![],
        };
        let md = table_to_markdown(&table);
        assert!(md.contains("(1) Footnote text"));
    }

    #[test]
    fn test_table_to_markdown_unicode_content() {
        let table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0],
            cells: vec![
                vec!["名前".into(), "年齢".into()],
                vec!["太郎".into(), "25".into()],
            ],
            item_indices: vec![],
        };
        let md = table_to_markdown(&table);
        assert!(md.contains("名前"));
        assert!(md.contains("太郎"));
    }

    #[test]
    fn test_table_to_markdown_empty_first_row() {
        let table = Table {
            columns: vec![100.0],
            rows: vec![500.0],
            cells: vec![vec![]],
            item_indices: vec![],
        };
        assert_eq!(table_to_markdown(&table), "");
    }
}
