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

    // Text state tracking
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut line_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text_block = false;

    for op in &content.operations {
        match op.operator.as_str() {
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
                            items.push(TextItem {
                                text,
                                x: text_matrix[4],
                                y: text_matrix[5],
                                width: 0.0, // Would need glyph widths
                                height: current_font_size,
                                font: current_font.clone(),
                                font_size: current_font_size,
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
                            items.push(TextItem {
                                text: combined_text,
                                x: text_matrix[4],
                                y: text_matrix[5],
                                width: 0.0,
                                height: current_font_size,
                                font: current_font.clone(),
                                font_size: current_font_size,
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
                            items.push(TextItem {
                                text,
                                x: text_matrix[4],
                                y: text_matrix[5],
                                width: 0.0,
                                height: current_font_size,
                                font: current_font.clone(),
                                font_size: current_font_size,
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

/// Group text items into lines based on Y position
pub fn group_into_lines(items: Vec<TextItem>) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Sort by page, then by Y (descending for PDF coords), then by X
    let mut sorted = items;
    sorted.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then(b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut lines = Vec::new();
    let mut current_line: Option<TextLine> = None;
    let y_tolerance = 3.0; // Tolerance for same-line grouping

    for item in sorted {
        match &mut current_line {
            Some(line) if line.page == item.page && (line.y - item.y).abs() < y_tolerance => {
                // Same line
                line.items.push(item);
            }
            _ => {
                // New line
                if let Some(line) = current_line.take() {
                    lines.push(line);
                }
                let y = item.y;
                let page = item.page;
                current_line = Some(TextLine {
                    items: vec![item],
                    y,
                    page,
                });
            }
        }
    }

    if let Some(line) = current_line {
        lines.push(line);
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
