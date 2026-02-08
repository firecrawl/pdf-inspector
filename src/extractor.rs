//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

use crate::PdfError;
use lopdf::{Document, Object, ObjectId};
use std::path::Path;

/// A text item with position information
#[derive(Debug, Clone)]
pub struct TextItem {
    /// The text content
    pub text: String,
    /// X position on page
    pub x: f32,
    /// Y position on page (PDF coordinates, origin at bottom-left)
    pub y: f32,
    /// Width of text
    pub width: f32,
    /// Height (approximated from font size)
    pub height: f32,
    /// Font name
    pub font: String,
    /// Font size
    pub font_size: f32,
    /// Page number (1-indexed)
    pub page: u32,
}

/// A line of text (grouped text items)
#[derive(Debug, Clone)]
pub struct TextLine {
    pub items: Vec<TextItem>,
    pub y: f32,
    pub page: u32,
}

impl TextLine {
    pub fn text(&self) -> String {
        self.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Extract text from PDF file as plain string
pub fn extract_text<P: AsRef<Path>>(path: P) -> Result<String, PdfError> {
    let doc = Document::load(path)?;
    extract_text_from_doc(&doc)
}

/// Extract text from PDF memory buffer
pub fn extract_text_mem(buffer: &[u8]) -> Result<String, PdfError> {
    let doc = Document::load_mem(buffer)?;
    extract_text_from_doc(&doc)
}

/// Extract text from loaded document
fn extract_text_from_doc(doc: &Document) -> Result<String, PdfError> {
    let pages = doc.get_pages();
    let page_nums: Vec<u32> = pages.keys().cloned().collect();

    doc.extract_text(&page_nums)
        .map_err(|e| PdfError::Parse(e.to_string()))
}

/// Extract text with position information from PDF file
pub fn extract_text_with_positions<P: AsRef<Path>>(path: P) -> Result<Vec<TextItem>, PdfError> {
    let doc = Document::load(path)?;
    extract_positioned_text_from_doc(&doc)
}

/// Extract text with positions from memory buffer
pub fn extract_text_with_positions_mem(buffer: &[u8]) -> Result<Vec<TextItem>, PdfError> {
    let doc = Document::load_mem(buffer)?;
    extract_positioned_text_from_doc(&doc)
}

/// Extract positioned text from loaded document
fn extract_positioned_text_from_doc(doc: &Document) -> Result<Vec<TextItem>, PdfError> {
    let pages = doc.get_pages();
    let mut all_items = Vec::new();

    for (page_num, &page_id) in pages.iter() {
        let items = extract_page_text_items(doc, page_id, *page_num)?;
        all_items.extend(items);
    }

    Ok(all_items)
}

/// Multiply two 2D transformation matrices
/// Matrix format: [a, b, c, d, e, f] representing:
/// | a  b  0 |
/// | c  d  0 |
/// | e  f  1 |
fn multiply_matrices(m1: &[f32; 6], m2: &[f32; 6]) -> [f32; 6] {
    [
        m1[0] * m2[0] + m1[1] * m2[2],
        m1[0] * m2[1] + m1[1] * m2[3],
        m1[2] * m2[0] + m1[3] * m2[2],
        m1[2] * m2[1] + m1[3] * m2[3],
        m1[4] * m2[0] + m1[5] * m2[2] + m2[4],
        m1[4] * m2[1] + m1[5] * m2[3] + m2[5],
    ]
}

/// Extract text items from a single page
fn extract_page_text_items(
    doc: &Document,
    page_id: ObjectId,
    page_num: u32,
) -> Result<Vec<TextItem>, PdfError> {
    use lopdf::content::Content;

    let mut items = Vec::new();

    // Get fonts for encoding
    let fonts = doc.get_page_fonts(page_id).unwrap_or_default();

    // Get content
    let content_data = doc
        .get_page_content(page_id)
        .map_err(|e| PdfError::Parse(e.to_string()))?;

    let content = Content::decode(&content_data).map_err(|e| PdfError::Parse(e.to_string()))?;

    // Graphics state tracking
    let mut ctm = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0]; // Current Transformation Matrix
    let mut ctm_stack: Vec<[f32; 6]> = Vec::new();

    // Text state tracking
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut line_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text_block = false;

    for op in &content.operations {
        match op.operator.as_str() {
            "q" => {
                // Save graphics state
                ctm_stack.push(ctm);
            }
            "Q" => {
                // Restore graphics state
                if let Some(saved) = ctm_stack.pop() {
                    ctm = saved;
                }
            }
            "cm" => {
                // Concatenate matrix to CTM
                if op.operands.len() >= 6 {
                    let new_matrix = [
                        get_number(&op.operands[0]).unwrap_or(1.0),
                        get_number(&op.operands[1]).unwrap_or(0.0),
                        get_number(&op.operands[2]).unwrap_or(0.0),
                        get_number(&op.operands[3]).unwrap_or(1.0),
                        get_number(&op.operands[4]).unwrap_or(0.0),
                        get_number(&op.operands[5]).unwrap_or(0.0),
                    ];
                    ctm = multiply_matrices(&new_matrix, &ctm);
                }
            }
            "BT" => {
                // Begin text block
                in_text_block = true;
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                line_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            }
            "ET" => {
                // End text block
                in_text_block = false;
            }
            "Tf" => {
                // Set font and size
                if op.operands.len() >= 2 {
                    if let Ok(name) = op.operands[0].as_name() {
                        current_font = String::from_utf8_lossy(name).to_string();
                    }
                    if let Ok(size) = op.operands[1].as_f32() {
                        current_font_size = size;
                    } else if let Ok(size) = op.operands[1].as_i64() {
                        current_font_size = size as f32;
                    }
                }
            }
            "Td" | "TD" => {
                // Move text position
                if op.operands.len() >= 2 {
                    let tx = get_number(&op.operands[0]).unwrap_or(0.0);
                    let ty = get_number(&op.operands[1]).unwrap_or(0.0);
                    line_matrix[4] += tx;
                    line_matrix[5] += ty;
                    text_matrix = line_matrix;
                }
            }
            "Tm" => {
                // Set text matrix
                if op.operands.len() >= 6 {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        text_matrix[i] =
                            get_number(operand).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                    }
                    line_matrix = text_matrix;
                }
            }
            "T*" => {
                // Move to start of next line
                line_matrix[5] -= current_font_size * 1.2; // Approximate line height
                text_matrix = line_matrix;
            }
            "Tj" => {
                // Show text string
                if in_text_block && !op.operands.is_empty() {
                    if let Some(text) =
                        extract_text_from_operand(&op.operands[0], doc, &fonts, &current_font)
                    {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width: 0.0, // Would need glyph widths
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                            });
                        }
                    }
                }
            }
            "TJ" => {
                // Show text with positioning
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        let mut combined_text = String::new();
                        for item in array {
                            if let Some(text) =
                                extract_text_from_operand(item, doc, &fonts, &current_font)
                            {
                                combined_text.push_str(&text);
                            }
                        }
                        if !combined_text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            items.push(TextItem {
                                text: combined_text,
                                x,
                                y,
                                width: 0.0,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                            });
                        }
                    }
                }
            }
            "'" => {
                // Move to next line and show text
                line_matrix[5] -= current_font_size * 1.2;
                text_matrix = line_matrix;
                if !op.operands.is_empty() {
                    if let Some(text) =
                        extract_text_from_operand(&op.operands[0], doc, &fonts, &current_font)
                    {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width: 0.0,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(items)
}

/// Helper to get f32 from Object
fn get_number(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// Compute effective font size from base size and text matrix
/// Text matrix is [a, b, c, d, tx, ty] where a,d are scale factors
fn effective_font_size(base_size: f32, text_matrix: &[f32; 6]) -> f32 {
    // The scale factor is typically the magnitude of the transformation
    // For most PDFs, text_matrix[0] (a) is the horizontal scale
    // and text_matrix[3] (d) is the vertical scale
    let scale_x = (text_matrix[0].powi(2) + text_matrix[1].powi(2)).sqrt();
    let scale_y = (text_matrix[2].powi(2) + text_matrix[3].powi(2)).sqrt();
    // Use the larger of the two scales (usually they're equal for non-rotated text)
    let scale = scale_x.max(scale_y);
    base_size * scale
}

/// Extract text from a text operand, handling encoding
fn extract_text_from_operand(
    obj: &Object,
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
    current_font: &str,
) -> Option<String> {
    if let Object::String(bytes, _) = obj {
        // Try to decode using font encoding
        if let Some(font_dict) = fonts.get(current_font.as_bytes()) {
            if let Ok(encoding) = font_dict.get_font_encoding(doc) {
                if let Ok(text) = Document::decode_text(&encoding, bytes) {
                    return Some(text);
                }
            }
        }

        // Fallback: try UTF-16BE then Latin-1
        if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
            let utf16: Vec<u16> = bytes[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect();
            return Some(String::from_utf16_lossy(&utf16));
        }

        // Latin-1 fallback
        Some(bytes.iter().map(|&b| b as char).collect())
    } else {
        None
    }
}

/// Represents a column region on a page
#[derive(Debug, Clone)]
struct ColumnRegion {
    x_min: f32,
    x_max: f32,
}

/// Detect column boundaries on a page based on X-position gaps
fn detect_columns(items: &[TextItem], page: u32) -> Vec<ColumnRegion> {
    // Get items for this page
    let page_items: Vec<&TextItem> = items.iter().filter(|i| i.page == page).collect();

    if page_items.is_empty() {
        return vec![];
    }

    // Find page bounds
    let x_min = page_items.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let x_max = page_items
        .iter()
        .map(|i| i.x + i.width.max(50.0)) // Estimate right edge
        .fold(f32::NEG_INFINITY, f32::max);

    let page_width = x_max - x_min;
    if page_width < 200.0 {
        // Page too narrow for multi-column, single column
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Need enough items to reliably detect columns
    if page_items.len() < 20 {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Collect all X positions (left edge of each text item)
    let mut x_positions: Vec<f32> = page_items.iter().map(|i| i.x).collect();
    x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Find gaps in X positions
    // A gap > 20% of page width suggests column boundary
    let gap_threshold = page_width * 0.20;
    let mut column_boundaries = vec![x_min];

    for window in x_positions.windows(2) {
        let gap = window[1] - window[0];
        if gap > gap_threshold {
            // Found a column boundary - use midpoint of gap
            let boundary = (window[0] + window[1]) / 2.0;
            column_boundaries.push(boundary);
        }
    }
    column_boundaries.push(x_max + 1.0);

    // Convert boundaries to column regions
    let mut columns = Vec::new();
    for i in 0..column_boundaries.len() - 1 {
        columns.push(ColumnRegion {
            x_min: column_boundaries[i],
            x_max: column_boundaries[i + 1],
        });
    }

    // Only use multi-column if we have exactly 2 columns
    // (most common case; 3+ columns are rare and error-prone)
    if columns.len() == 2 {
        // Verify both columns have substantial content
        let col_counts: Vec<usize> = columns
            .iter()
            .map(|col| {
                page_items
                    .iter()
                    .filter(|i| i.x >= col.x_min && i.x < col.x_max)
                    .count()
            })
            .collect();

        // Each column should have at least 20% of the content
        let total: usize = col_counts.iter().sum();
        let min_threshold = total / 5;
        if col_counts.iter().all(|&c| c >= min_threshold) {
            return columns;
        }
    }

    // For 3+ detected columns, try merging adjacent small columns
    if columns.len() > 2 {
        let col_counts: Vec<usize> = columns
            .iter()
            .map(|col| {
                page_items
                    .iter()
                    .filter(|i| i.x >= col.x_min && i.x < col.x_max)
                    .count()
            })
            .collect();

        // Find the largest gap between columns that have substantial content
        let total: usize = col_counts.iter().sum();
        let min_items = total / 5; // 20% minimum

        // Find first and last columns with enough content
        let first_substantial = col_counts.iter().position(|&c| c >= min_items);
        let last_substantial = col_counts.iter().rposition(|&c| c >= min_items);

        if let (Some(first), Some(last)) = (first_substantial, last_substantial) {
            if first != last {
                // Create two columns: merge everything before the gap and after
                return vec![
                    ColumnRegion {
                        x_min: columns[0].x_min,
                        x_max: columns[first].x_max,
                    },
                    ColumnRegion {
                        x_min: columns[last].x_min,
                        x_max: columns[columns.len() - 1].x_max,
                    },
                ];
            }
        }
    }

    // Default to single column
    vec![ColumnRegion { x_min, x_max }]
}

/// Group text items into lines, with multi-column support
pub fn group_into_lines(items: Vec<TextItem>) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Get unique pages
    let mut pages: Vec<u32> = items.iter().map(|i| i.page).collect();
    pages.sort();
    pages.dedup();

    let mut all_lines = Vec::new();

    for page in pages {
        let page_items: Vec<TextItem> = items.iter().filter(|i| i.page == page).cloned().collect();

        // Detect columns for this page
        let columns = detect_columns(&page_items, page);

        if columns.len() <= 1 {
            // Single column - use simple sorting
            let lines = group_single_column(page_items);
            all_lines.extend(lines);
        } else {
            // Multi-column - process each column separately, then concatenate
            for column in &columns {
                let col_items: Vec<TextItem> = page_items
                    .iter()
                    .filter(|i| i.x >= column.x_min && i.x < column.x_max)
                    .cloned()
                    .collect();

                let lines = group_single_column(col_items);
                all_lines.extend(lines);
            }
        }
    }

    all_lines
}

/// Group items from a single column into lines
/// Preserves PDF stream order (which is typically reading order) and only groups
/// consecutive items on the same line by their X position.
fn group_single_column(items: Vec<TextItem>) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // DO NOT sort by Y - preserve PDF stream order which is usually reading order
    // Only merge consecutive items that are on the same line (same Y within tolerance)
    let mut lines: Vec<TextLine> = Vec::new();
    let y_tolerance = 3.0;

    for item in items {
        // Only check the most recent line for merging (to preserve stream order)
        let should_merge = lines.last().map_or(false, |last_line| {
            last_line.page == item.page && (last_line.y - item.y).abs() < y_tolerance
        });

        if should_merge {
            // Add to the most recent line
            lines.last_mut().unwrap().items.push(item);
        } else {
            // Create new line
            let y = item.y;
            let page = item.page;
            lines.push(TextLine {
                items: vec![item],
                y,
                page,
            });
        }
    }

    // Sort items within each line by X position (left to right)
    for line in &mut lines {
        line.items
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_into_lines() {
        let items = vec![
            TextItem {
                text: "Hello".into(),
                x: 100.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
            },
            TextItem {
                text: "World".into(),
                x: 160.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
            },
            TextItem {
                text: "Next line".into(),
                x: 100.0,
                y: 680.0,
                width: 80.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
            },
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "Hello World");
        assert_eq!(lines[1].text(), "Next line");
    }
}
