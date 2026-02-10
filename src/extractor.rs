//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

use crate::tounicode::FontCMaps;
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
    /// Whether the font is bold
    pub is_bold: bool,
    /// Whether the font is italic
    pub is_italic: bool,
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
        self.text_with_formatting(false, false)
    }

    /// Get text with optional bold/italic markdown formatting
    pub fn text_with_formatting(&self, format_bold: bool, format_italic: bool) -> String {
        if !format_bold && !format_italic {
            return self.text_plain();
        }

        let mut result = String::new();
        let mut current_bold = false;
        let mut current_italic = false;

        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            let text_trimmed = text.trim();

            // Skip empty items
            if text_trimmed.is_empty() {
                continue;
            }

            // Determine spacing
            let needs_space = if i == 0 || result.is_empty() {
                false
            } else {
                let prev_item = &self.items[i - 1];
                self.needs_space_between(prev_item, item, &result)
            };

            // Check for style changes
            let item_bold = format_bold && item.is_bold;
            let item_italic = format_italic && item.is_italic;

            // Close previous styles if they change
            if current_italic && !item_italic {
                result.push('*');
                current_italic = false;
            }
            if current_bold && !item_bold {
                result.push_str("**");
                current_bold = false;
            }

            // Add space after closing markers if needed
            if needs_space {
                result.push(' ');
            }

            // Open new styles
            if item_bold && !current_bold {
                result.push_str("**");
                current_bold = true;
            }
            if item_italic && !current_italic {
                result.push('*');
                current_italic = true;
            }

            result.push_str(text_trimmed);
        }

        // Close any remaining open styles
        if current_italic {
            result.push('*');
        }
        if current_bold {
            result.push_str("**");
        }

        result
    }

    /// Get plain text without formatting
    fn text_plain(&self) -> String {
        let mut result = String::new();
        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            if i == 0 {
                result.push_str(text);
            } else {
                let prev_item = &self.items[i - 1];
                if self.needs_space_between(prev_item, item, &result) {
                    result.push(' ');
                }
                result.push_str(text);
            }
        }
        result
    }

    /// Determine if a space is needed between two items
    fn needs_space_between(&self, prev_item: &TextItem, item: &TextItem, result: &str) -> bool {
        let text = item.text.as_str();

        // Don't add space before/after hyphens for hyphenated words
        let prev_ends_with_hyphen = result.ends_with('-');
        let curr_is_hyphen = text.trim() == "-";
        let curr_starts_with_hyphen = text.starts_with('-');

        // Detect subscript/superscript: smaller font size and/or Y offset
        let font_ratio = item.font_size / prev_item.font_size;
        let reverse_font_ratio = prev_item.font_size / item.font_size;
        let y_diff = (item.y - prev_item.y).abs();

        let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
        let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

        // Use position-based spacing detection
        let should_join = should_join_items(prev_item, item);

        // Check if space already exists
        let prev_ends_with_space = result.ends_with(' ');
        let curr_starts_with_space = text.starts_with(' ');
        let space_already_exists = prev_ends_with_space || curr_starts_with_space;

        // Add space unless one of these conditions applies
        !(prev_ends_with_hyphen
            || curr_is_hyphen
            || curr_starts_with_hyphen
            || is_sub_super
            || was_sub_super
            || should_join
            || space_already_exists)
    }
}

/// Determine if two adjacent text items should be joined without a space
/// based on their physical positions on the page and character case.
/// Uses a hybrid approach: position-based with case-aware thresholds.
fn should_join_items(prev_item: &TextItem, curr_item: &TextItem) -> bool {
    // If either text explicitly has leading/trailing spaces, respect them
    if prev_item.text.ends_with(' ') || curr_item.text.starts_with(' ') {
        return false;
    }

    // Get the last character of previous and first character of current
    let prev_last = prev_item.text.trim_end().chars().last();
    let curr_first = curr_item.text.trim_start().chars().next();

    // Always join if current starts with punctuation that typically follows without space
    // e.g., "www" + ".com" → "www.com", not "www .com"
    if let Some(c) = curr_first {
        if matches!(c, '.' | ',' | ';' | '!' | '?' | ')' | ']' | '}' | '\'') {
            return true;
        }
    }

    // After colons, add space if followed by alphanumeric (typical label:value pattern)
    // e.g., "Clave:" + "T9N2I6" → "Clave: T9N2I6"
    if let (Some(p), Some(c)) = (prev_last, curr_first) {
        if p == ':' && c.is_alphanumeric() {
            return false;
        }
    }

    // Estimate the average character width from font size
    // Use a conservative estimate (0.45) since fonts vary
    let char_width = prev_item.font_size * 0.45;

    // Estimate the width of the previous text
    let prev_text_len = prev_item.text.chars().count() as f32;
    let estimated_prev_width = if prev_item.width > 0.0 {
        prev_item.width // Use actual width if available
    } else {
        prev_text_len * char_width
    };

    // Calculate expected end position of previous item
    let prev_end_x = prev_item.x + estimated_prev_width;

    // Calculate gap between items
    let gap = curr_item.x - prev_end_x;

    // Use different thresholds based on character case
    // Same-case sequences (ALL CAPS or all lowercase) are more likely to be
    // word fragments that got split. Mixed case suggests word boundaries.
    match (prev_last, curr_first) {
        (Some(p), Some(c)) if p.is_alphabetic() && c.is_alphabetic() => {
            let same_case =
                (p.is_uppercase() && c.is_uppercase()) || (p.is_lowercase() && c.is_lowercase());
            if same_case {
                // Same case: use generous threshold (likely same word fragment)
                // e.g., "CONST" + "ANCIA" → "CONSTANCIA"
                gap < char_width * 0.8
            } else if p.is_lowercase() && c.is_uppercase() {
                // Lowercase to uppercase transition (e.g., "presente" → "CONSTANCIA")
                // This is typically a word boundary. In Spanish/English, words don't
                // transition from lowercase to uppercase mid-word.
                // Always add a space for this case, regardless of position.
                false
            } else {
                // Uppercase to lowercase (e.g., "REGISTRO" → "para")
                // Use stricter threshold (likely word boundary)
                gap < char_width * 0.3
            }
        }
        _ => {
            // Non-alphabetic: use moderate threshold
            gap < char_width * 0.5
        }
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
    // Read the raw PDF bytes for ToUnicode extraction
    let pdf_bytes = std::fs::read(path.as_ref())?;
    let font_cmaps = FontCMaps::from_pdf_bytes(&pdf_bytes);

    let doc = Document::load_mem(&pdf_bytes)?;
    extract_positioned_text_from_doc(&doc, &font_cmaps)
}

/// Extract text with positions from memory buffer
pub fn extract_text_with_positions_mem(buffer: &[u8]) -> Result<Vec<TextItem>, PdfError> {
    // Extract ToUnicode CMaps from raw PDF bytes
    let font_cmaps = FontCMaps::from_pdf_bytes(buffer);

    let doc = Document::load_mem(buffer)?;
    extract_positioned_text_from_doc(&doc, &font_cmaps)
}

/// Extract positioned text from loaded document
fn extract_positioned_text_from_doc(
    doc: &Document,
    font_cmaps: &FontCMaps,
) -> Result<Vec<TextItem>, PdfError> {
    let pages = doc.get_pages();
    let mut all_items = Vec::new();

    for (page_num, &page_id) in pages.iter() {
        let items = extract_page_text_items(doc, page_id, *page_num, font_cmaps)?;
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
    font_cmaps: &FontCMaps,
) -> Result<Vec<TextItem>, PdfError> {
    use lopdf::content::Content;

    let mut items = Vec::new();

    // Get fonts for encoding
    let fonts = doc.get_page_fonts(page_id).unwrap_or_default();

    // Build maps of font resource names to their base font names and ToUnicode object refs
    let mut font_base_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut font_tounicode_refs: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for (font_name, font_dict) in &fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        // Track ToUnicode object reference
        if let Ok(tounicode) = font_dict.get(b"ToUnicode") {
            if let Ok(obj_ref) = tounicode.as_reference() {
                font_tounicode_refs.insert(resource_name, obj_ref.0);
            }
        }
    }

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
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        doc,
                        &fonts,
                        &current_font,
                        font_cmaps,
                        &font_base_names,
                        &font_tounicode_refs,
                    ) {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Detect bold/italic from font name
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width: 0.0, // Would need glyph widths
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
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
                            if let Some(text) = extract_text_from_operand(
                                item,
                                doc,
                                &fonts,
                                &current_font,
                                font_cmaps,
                                &font_base_names,
                                &font_tounicode_refs,
                            ) {
                                combined_text.push_str(&text);
                            }
                        }
                        if !combined_text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Detect bold/italic from font name
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text: combined_text,
                                x,
                                y,
                                width: 0.0,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
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
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        doc,
                        &fonts,
                        &current_font,
                        font_cmaps,
                        &font_base_names,
                        &font_tounicode_refs,
                    ) {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Detect bold/italic from font name
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width: 0.0,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
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

/// Detect if a font name indicates bold style
/// Common patterns: "Bold", "Bd", "Black", "Heavy", "Demi", "Semi" (semi-bold)
pub fn is_bold_font(font_name: &str) -> bool {
    let lower = font_name.to_lowercase();

    // Check for common bold indicators
    // Note: Need to be careful with "Oblique" not matching "Obl" + false positive for bold
    lower.contains("bold")
        || lower.contains("-bd")
        || lower.contains("_bd")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("demibold")
        || lower.contains("semibold")
        || lower.contains("demi-bold")
        || lower.contains("semi-bold")
        || lower.contains("extrabold")
        || lower.contains("ultrabold")
        || lower.contains("medium") && !lower.contains("mediumitalic") // Some fonts use Medium for semi-bold
}

/// Detect if a font name indicates italic/oblique style
/// Common patterns: "Italic", "It", "Oblique", "Obl", "Slant", "Inclined"
pub fn is_italic_font(font_name: &str) -> bool {
    let lower = font_name.to_lowercase();

    // Check for common italic indicators
    lower.contains("italic")
        || lower.contains("oblique")
        || lower.contains("-it")
        || lower.contains("_it")
        || lower.contains("slant")
        || lower.contains("inclined")
        || lower.contains("kursiv") // German for italic
}

/// Extract text from a text operand, handling encoding
fn extract_text_from_operand(
    obj: &Object,
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
    current_font: &str,
    font_cmaps: &FontCMaps,
    font_base_names: &std::collections::HashMap<String, String>,
    font_tounicode_refs: &std::collections::HashMap<String, u32>,
) -> Option<String> {
    if let Object::String(bytes, _) = obj {
        // First, try to look up CMap by ToUnicode object reference (most reliable)
        // This handles cases where multiple fonts have the same BaseFont but different ToUnicode
        if let Some(&obj_num) = font_tounicode_refs.get(current_font) {
            if let Some(cmap) = font_cmaps.get_by_obj(obj_num) {
                let decoded = cmap.decode_cids(bytes);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }

        // Fall back to base name lookup with object number
        if let (Some(base_name), Some(&obj_num)) = (
            font_base_names.get(current_font),
            font_tounicode_refs.get(current_font),
        ) {
            if let Some(cmap) = font_cmaps.get_with_obj(base_name, obj_num) {
                let decoded = cmap.decode_cids(bytes);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }

        // Try base name only (legacy fallback)
        if let Some(base_name) = font_base_names.get(current_font) {
            if let Some(cmap) = font_cmaps.get(base_name) {
                let decoded = cmap.decode_cids(bytes);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }

        // Also try looking up by resource name directly
        if let Some(cmap) = font_cmaps.get(current_font) {
            let decoded = cmap.decode_cids(bytes);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }

        // Try to decode using font encoding from lopdf
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
    // A gap > 30% of page width suggests column boundary
    // (increased from 20% to reduce false positives from text with varying indentation)
    let gap_threshold = page_width * 0.30;
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

/// Check if a text item is likely a page number
fn is_page_number(item: &TextItem) -> bool {
    let text = item.text.trim();

    // Must be 1-4 digits only
    if text.is_empty() || text.len() > 4 {
        return false;
    }
    if !text.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    // Must be at top (y > 800) or bottom (y < 100) of page
    // These thresholds work for standard page sizes
    item.y > 800.0 || item.y < 100.0
}

/// Group text items into lines, with multi-column support
pub fn group_into_lines(items: Vec<TextItem>) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Filter out page numbers (standalone numbers at top/bottom of page)
    let items: Vec<TextItem> = items
        .into_iter()
        .filter(|item| !is_page_number(item))
        .collect();

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

/// Determine if Y-sorting should be used instead of stream order.
/// Returns true if the stream order appears chaotic (items jump around in Y position).
fn should_use_y_sorting(items: &[TextItem]) -> bool {
    if items.len() < 5 {
        return false; // Not enough items to judge
    }

    // Sample Y positions from stream order
    let y_positions: Vec<f32> = items.iter().map(|i| i.y).collect();

    // Count "order violations" - cases where Y increases (going up) when it should decrease
    // In proper reading order, Y should generally decrease (top to bottom)
    let mut large_jumps_up = 0;
    let mut large_jumps_down = 0;
    let jump_threshold = 50.0; // Significant Y jump

    for window in y_positions.windows(2) {
        let delta = window[1] - window[0];
        if delta > jump_threshold {
            large_jumps_up += 1; // Y increased significantly (jumped up on page)
        } else if delta < -jump_threshold {
            large_jumps_down += 1; // Y decreased significantly (normal reading direction)
        }
    }

    // If there are many upward jumps relative to downward jumps, order is chaotic
    // A well-ordered document should have mostly downward progression
    let total_jumps = large_jumps_up + large_jumps_down;
    if total_jumps < 3 {
        return false; // Not enough jumps to judge
    }

    // If more than 40% of large jumps are upward, use Y-sorting
    let chaos_ratio = large_jumps_up as f32 / total_jumps as f32;
    chaos_ratio > 0.4
}

/// Group items from a single column into lines
/// Uses heuristics to decide between PDF stream order and Y-position sorting.
fn group_single_column(items: Vec<TextItem>) -> Vec<TextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    // Decide whether to use stream order or Y-sorting
    let use_y_sorting = should_use_y_sorting(&items);

    let items = if use_y_sorting {
        // Sort by Y descending (top to bottom in PDF coords)
        let mut sorted = items;
        sorted.sort_by(|a, b| {
            b.y.partial_cmp(&a.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        sorted
    } else {
        items
    };

    // Group items into lines
    let mut lines: Vec<TextLine> = Vec::new();
    let y_tolerance = 3.0;

    for item in items {
        // Only check the most recent line for merging
        let should_merge = lines.last().is_some_and(|last_line| {
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
                is_bold: false,
                is_italic: false,
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
                is_bold: false,
                is_italic: false,
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
                is_bold: false,
                is_italic: false,
            },
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "Hello World");
        assert_eq!(lines[1].text(), "Next line");
    }

    #[test]
    fn test_bold_italic_detection() {
        // Test bold detection
        assert!(is_bold_font("Arial-Bold"));
        assert!(is_bold_font("TimesNewRoman-Bold"));
        assert!(is_bold_font("Helvetica-BoldOblique"));
        assert!(is_bold_font("ABCDEF+ArialMT-Bold"));
        assert!(is_bold_font("NotoSans-Black"));
        assert!(is_bold_font("Roboto-SemiBold"));
        assert!(!is_bold_font("Arial"));
        assert!(!is_bold_font("TimesNewRoman-Italic"));

        // Test italic detection
        assert!(is_italic_font("Arial-Italic"));
        assert!(is_italic_font("TimesNewRoman-Italic"));
        assert!(is_italic_font("Helvetica-Oblique"));
        assert!(is_italic_font("ABCDEF+ArialMT-Italic"));
        assert!(is_italic_font("Helvetica-BoldOblique"));
        assert!(!is_italic_font("Arial"));
        assert!(!is_italic_font("TimesNewRoman-Bold"));

        // Test bold-italic detection
        assert!(is_bold_font("Arial-BoldItalic"));
        assert!(is_italic_font("Arial-BoldItalic"));
        assert!(is_bold_font("Helvetica-BoldOblique"));
        assert!(is_italic_font("Helvetica-BoldOblique"));
    }
}
