//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

use crate::glyph_names::glyph_to_char;
use crate::tounicode::FontCMaps;
use crate::PdfError;
use lopdf::{Document, Object, ObjectId};
use std::collections::HashMap;
use std::path::Path;

/// Font encoding map: maps byte codes to Unicode characters
type FontEncodingMap = HashMap<u8, char>;

/// All font encodings for a page
type PageFontEncodings = HashMap<String, FontEncodingMap>;

/// Font width information extracted from PDF font dictionaries
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FontWidthInfo {
    /// Glyph widths: maps character code to width in font units
    widths: HashMap<u16, u16>,
    /// Default width for glyphs not in the widths table
    default_width: u16,
    /// Width of the space character (code 32) if known
    space_width: u16,
    /// Whether this is a CID font (2-byte character codes)
    is_cid: bool,
    /// Scale factor to convert font units to text space units.
    /// For Type1/TrueType: 0.001 (widths in 1000ths of em)
    /// For Type3: FontMatrix[0] (e.g., 0.00048828125 for 2048-unit grid)
    units_scale: f32,
}

/// All font width info for a page, keyed by font resource name
type PageFontWidths = HashMap<String, FontWidthInfo>;

/// Resolve a PDF object reference to an array
fn resolve_array<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Vec<Object>> {
    match obj {
        Object::Array(arr) => Some(arr),
        Object::Reference(r) => {
            if let Ok(Object::Array(arr)) = doc.get_object(*r) {
                Some(arr)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve a PDF object reference to a dictionary
fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a lopdf::Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        _ => None,
    }
}

/// Build font width info for all fonts on a page
fn build_font_widths(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
) -> PageFontWidths {
    let mut widths = PageFontWidths::new();

    for (font_name, font_dict) in fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Some(info) = parse_font_widths(doc, font_dict) {
            widths.insert(resource_name, info);
        }
    }

    widths
}

/// Parse font widths from a font dictionary, dispatching by Subtype
fn parse_font_widths(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<FontWidthInfo> {
    // Get the font subtype
    let subtype = font_dict.get(b"Subtype").ok()?;
    let subtype_name = subtype.as_name().ok()?;

    match subtype_name {
        b"Type0" => parse_type0_widths(doc, font_dict),
        b"Type1" | b"TrueType" | b"MMType1" | b"Type3" => parse_simple_font_widths(doc, font_dict),
        _ => None,
    }
}

/// Parse widths for simple fonts (Type1, TrueType, MMType1, Type3)
/// Reads FirstChar, LastChar, and Widths array.
/// For Type3 fonts, reads FontMatrix to determine the correct units_scale.
fn parse_simple_font_widths(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<FontWidthInfo> {
    let first_char = font_dict.get(b"FirstChar").ok().and_then(|o| match o {
        Object::Integer(n) => Some(*n as u16),
        Object::Reference(r) => doc.get_object(*r).ok().and_then(|o| {
            if let Object::Integer(n) = o {
                Some(*n as u16)
            } else {
                None
            }
        }),
        _ => None,
    })?;

    let last_char = font_dict.get(b"LastChar").ok().and_then(|o| match o {
        Object::Integer(n) => Some(*n as u16),
        Object::Reference(r) => doc.get_object(*r).ok().and_then(|o| {
            if let Object::Integer(n) = o {
                Some(*n as u16)
            } else {
                None
            }
        }),
        _ => None,
    })?;

    let widths_obj = font_dict.get(b"Widths").ok()?;
    let widths_array = resolve_array(doc, widths_obj)?;

    let mut widths = HashMap::new();
    let mut space_width: u16 = 0;

    for (i, w_obj) in widths_array.iter().enumerate() {
        let code = first_char + i as u16;
        if code > last_char {
            break;
        }
        let w = match w_obj {
            Object::Integer(n) => *n as u16,
            Object::Real(n) => *n as u16,
            Object::Reference(r) => {
                if let Ok(obj) = doc.get_object(*r) {
                    match obj {
                        Object::Integer(n) => *n as u16,
                        Object::Real(n) => *n as u16,
                        _ => continue,
                    }
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        if code == 32 {
            space_width = w;
        }
        widths.insert(code, w);
    }

    // If space width wasn't found in the table, use a reasonable default
    if space_width == 0 {
        space_width = 250;
    }

    // Determine units_scale: for Type3 fonts, use FontMatrix[0]; for others, use 1/1000
    let units_scale = if let Ok(fm) = font_dict.get(b"FontMatrix") {
        if let Some(arr) = resolve_array(doc, fm) {
            if !arr.is_empty() {
                match &arr[0] {
                    Object::Real(r) => r.abs(),
                    Object::Integer(i) => (*i as f32).abs(),
                    _ => 0.001,
                }
            } else {
                0.001
            }
        } else {
            0.001
        }
    } else {
        0.001 // Standard 1000-unit system
    };

    Some(FontWidthInfo {
        widths,
        default_width: 0,
        space_width,
        is_cid: false,
        units_scale,
    })
}

/// Parse widths for Type0 (composite/CID) fonts
/// Reads DescendantFonts → CIDFont → W array and DW value
fn parse_type0_widths(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<FontWidthInfo> {
    let desc_fonts_obj = font_dict.get(b"DescendantFonts").ok()?;
    let desc_fonts = resolve_array(doc, desc_fonts_obj)?;

    if desc_fonts.is_empty() {
        return None;
    }

    // Get the first descendant font dictionary
    let cid_font_dict = resolve_dict(doc, &desc_fonts[0])?;

    // Get DW (default width)
    let default_width = cid_font_dict
        .get(b"DW")
        .ok()
        .and_then(|o| match o {
            Object::Integer(n) => Some(*n as u16),
            Object::Real(n) => Some(*n as u16),
            _ => None,
        })
        .unwrap_or(1000);

    let mut widths = HashMap::new();

    // Parse W array if present
    if let Ok(w_obj) = cid_font_dict.get(b"W") {
        if let Some(w_array) = resolve_array(doc, w_obj) {
            parse_cid_w_array(doc, w_array, &mut widths);
        }
    }

    // Try to determine space width (CID 32 or CID 3 are common for space)
    let space_width = widths
        .get(&32)
        .or_else(|| widths.get(&3))
        .copied()
        .unwrap_or(if default_width > 0 {
            default_width / 4
        } else {
            250
        });

    Some(FontWidthInfo {
        widths,
        default_width,
        space_width,
        is_cid: true,
        units_scale: 0.001, // CID fonts use standard 1000-unit system
    })
}

/// Parse a CID W array into widths map
/// Format: [c [w1 w2 ...]] (consecutive from c) or [c_first c_last w] (range with same width)
fn parse_cid_w_array(doc: &Document, w_array: &[Object], widths: &mut HashMap<u16, u16>) {
    let mut i = 0;
    while i < w_array.len() {
        let start_cid = match &w_array[i] {
            Object::Integer(n) => *n as u16,
            Object::Real(n) => *n as u16,
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        if i >= w_array.len() {
            break;
        }

        // Check if next element is an array (consecutive widths) or integer (range)
        match &w_array[i] {
            Object::Array(arr) => {
                // [c [w1 w2 ...]] — consecutive widths starting at c
                for (j, w_obj) in arr.iter().enumerate() {
                    let w = match w_obj {
                        Object::Integer(n) => *n as u16,
                        Object::Real(n) => *n as u16,
                        _ => continue,
                    };
                    widths.insert(start_cid + j as u16, w);
                }
                i += 1;
            }
            Object::Reference(r) => {
                // Could be a reference to an array
                if let Ok(Object::Array(arr)) = doc.get_object(*r) {
                    for (j, w_obj) in arr.iter().enumerate() {
                        let w = match w_obj {
                            Object::Integer(n) => *n as u16,
                            Object::Real(n) => *n as u16,
                            _ => continue,
                        };
                        widths.insert(start_cid + j as u16, w);
                    }
                    i += 1;
                } else {
                    // Treat as c_first c_last w
                    i += 1; // skip this
                }
            }
            Object::Integer(end_cid) => {
                // [c_first c_last w] — range with uniform width
                let end = *end_cid as u16;
                i += 1;
                if i >= w_array.len() {
                    break;
                }
                let w = match &w_array[i] {
                    Object::Integer(n) => *n as u16,
                    Object::Real(n) => *n as u16,
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                for cid in start_cid..=end {
                    widths.insert(cid, w);
                }
                i += 1;
            }
            Object::Real(end_cid) => {
                let end = *end_cid as u16;
                i += 1;
                if i >= w_array.len() {
                    break;
                }
                let w = match &w_array[i] {
                    Object::Integer(n) => *n as u16,
                    Object::Real(n) => *n as u16,
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                for cid in start_cid..=end {
                    widths.insert(cid, w);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
}

/// Compute the width of a string in text space units,
/// given raw bytes and font width info.
/// Returns width in text space units (font_units * units_scale * font_size).
fn compute_string_width_ts(bytes: &[u8], font_info: &FontWidthInfo, font_size: f32) -> f32 {
    let mut total: f32 = 0.0;
    if font_info.is_cid {
        // 2-byte (big-endian) character codes
        let mut j = 0;
        while j + 1 < bytes.len() {
            let cid = u16::from_be_bytes([bytes[j], bytes[j + 1]]);
            let w = font_info
                .widths
                .get(&cid)
                .copied()
                .unwrap_or(font_info.default_width);
            total += w as f32;
            j += 2;
        }
    } else {
        // 1-byte character codes
        for &b in bytes {
            let code = b as u16;
            let w = font_info
                .widths
                .get(&code)
                .copied()
                .unwrap_or(font_info.default_width);
            total += w as f32;
        }
    }
    // Convert from font units to text space using the font's scale factor
    total * font_info.units_scale * font_size
}

/// Extract raw bytes from a PDF operand (String object)
fn get_operand_bytes(obj: &Object) -> Option<&[u8]> {
    if let Object::String(bytes, _) = obj {
        Some(bytes)
    } else {
        None
    }
}

/// Build encoding maps for all fonts on a page
fn build_font_encodings(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
) -> PageFontEncodings {
    let mut encodings = PageFontEncodings::new();

    for (font_name, font_dict) in fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();

        if let Some(encoding_map) = parse_font_encoding(doc, font_dict) {
            encodings.insert(resource_name, encoding_map);
        }
    }

    encodings
}

/// Parse font encoding from a font dictionary
fn parse_font_encoding(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<FontEncodingMap> {
    let encoding_obj = font_dict.get(b"Encoding").ok()?;

    // Encoding can be a name or a dictionary
    match encoding_obj {
        Object::Name(_name) => {
            // Standard encoding name (e.g., MacRomanEncoding, WinAnsiEncoding)
            // For standard encodings, we can use the standard tables
            // But we still need to check for Differences
            None // Let lopdf handle standard encodings
        }
        Object::Reference(obj_ref) => {
            // Reference to encoding dictionary
            if let Ok(enc_dict) = doc.get_dictionary(*obj_ref) {
                parse_encoding_dictionary(doc, enc_dict)
            } else {
                None
            }
        }
        Object::Dictionary(enc_dict) => parse_encoding_dictionary(doc, enc_dict),
        _ => None,
    }
}

/// Parse an encoding dictionary with Differences array
fn parse_encoding_dictionary(
    doc: &Document,
    enc_dict: &lopdf::Dictionary,
) -> Option<FontEncodingMap> {
    let differences = enc_dict.get(b"Differences").ok()?;

    let diff_array = match differences {
        Object::Array(arr) => arr.clone(),
        Object::Reference(obj_ref) => {
            if let Ok(Object::Array(arr)) = doc.get_object(*obj_ref) {
                arr.clone()
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let mut encoding_map = FontEncodingMap::new();
    let mut current_code: u8 = 0;

    for item in diff_array {
        match item {
            Object::Integer(n) => {
                // This sets the starting code for subsequent glyph names
                current_code = n as u8;
            }
            Object::Name(name) => {
                // Map current code to glyph name -> Unicode
                let glyph_name = String::from_utf8_lossy(&name).to_string();
                if let Some(ch) = glyph_to_char(&glyph_name) {
                    encoding_map.insert(current_code, ch);
                }
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if encoding_map.is_empty() {
        None
    } else {
        Some(encoding_map)
    }
}

/// Type of content item
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ItemType {
    /// Regular text content
    #[default]
    Text,
    /// Image placeholder
    Image,
    /// Hyperlink (with URL)
    Link(String),
}

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
    /// Type of item (text, image, link)
    pub item_type: ItemType,
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

    // When we have accurate width from font metrics, use a tight threshold
    if prev_item.width > 0.0 {
        let prev_end_x = prev_item.x + prev_item.width;
        let gap = curr_item.x - prev_end_x;
        let font_size = prev_item.font_size;
        // With accurate widths, a gap < 15% of font size means glyphs are
        // adjacent (same word). Anything larger is a deliberate space.
        return gap < font_size * 0.15;
    }

    // Fallback: estimate width from font size heuristics
    let char_width = prev_item.font_size * 0.45;

    let prev_text_len = prev_item.text.chars().count() as f32;
    let estimated_prev_width = prev_text_len * char_width;

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

        // Extract hyperlinks from page annotations
        let links = extract_page_links(doc, page_id, *page_num);
        all_items.extend(links);
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

    // Build font encoding maps from Differences arrays
    let font_encodings = build_font_encodings(doc, &fonts);

    // Build font width info for accurate text positioning
    let font_widths = build_font_widths(doc, &fonts);

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

    // Get XObjects (images) from page resources
    let xobjects = get_page_xobjects(doc, page_id);

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
                        &font_encodings,
                    ) {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            // Transform position through CTM
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Compute width from font widths if available
                            let width = if let Some(font_info) = font_widths.get(&current_font) {
                                if let Some(raw_bytes) = get_operand_bytes(&op.operands[0]) {
                                    let w_ts = compute_string_width_ts(
                                        raw_bytes,
                                        font_info,
                                        current_font_size,
                                    );
                                    // Advance text matrix by string width
                                    text_matrix[4] += w_ts * text_matrix[0];
                                    text_matrix[5] += w_ts * text_matrix[1];
                                    // Transform width through text matrix and CTM
                                    (w_ts * (text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2]))
                                        .abs()
                                } else {
                                    0.0
                                }
                            } else {
                                0.0
                            };
                            // Detect bold/italic from font name
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
                                item_type: ItemType::Text,
                            });
                        }
                    }
                }
            }
            "TJ" => {
                // Show text with positioning
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        let font_info = font_widths.get(&current_font);

                        // Compute space threshold based on font metrics when available
                        let space_threshold = if let Some(font_info) = font_info {
                            // Use 40% of the font's space width (in thousandths of text space)
                            let space_em = font_info.space_width as f32 * font_info.units_scale;
                            let threshold = space_em * 1000.0 * 0.4;
                            // Clamp to reasonable range: at least 80, at most 200
                            threshold.clamp(80.0, 200.0)
                        } else {
                            120.0 // fallback threshold
                        };

                        let mut combined_text = String::new();
                        let mut total_width_ts: f32 = 0.0;
                        for element in array {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    // Track displacement for total width
                                    total_width_ts += -n_val / 1000.0 * current_font_size;
                                    if n_val < -space_threshold
                                        && !combined_text.is_empty()
                                        && !combined_text.ends_with(' ')
                                    {
                                        combined_text.push(' ');
                                    }
                                    continue;
                                }
                                Object::Real(n) => {
                                    total_width_ts += -(*n) / 1000.0 * current_font_size;
                                    if *n < -space_threshold
                                        && !combined_text.is_empty()
                                        && !combined_text.ends_with(' ')
                                    {
                                        combined_text.push(' ');
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                            // Compute string width for total
                            if let Some(fi) = font_info {
                                if let Some(raw_bytes) = get_operand_bytes(element) {
                                    total_width_ts +=
                                        compute_string_width_ts(raw_bytes, fi, current_font_size);
                                }
                            }
                            if let Some(text) = extract_text_from_operand(
                                element,
                                doc,
                                &fonts,
                                &current_font,
                                font_cmaps,
                                &font_base_names,
                                &font_tounicode_refs,
                                &font_encodings,
                            ) {
                                combined_text.push_str(&text);
                            }
                        }
                        if !combined_text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Compute accurate width if font widths available
                            let width = if font_info.is_some() {
                                (total_width_ts
                                    * (text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2]))
                                    .abs()
                            } else {
                                0.0
                            };
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text: combined_text,
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
                                item_type: ItemType::Text,
                            });
                            // Advance text matrix by total width
                            if font_info.is_some() {
                                text_matrix[4] += total_width_ts * text_matrix[0];
                                text_matrix[5] += total_width_ts * text_matrix[1];
                            }
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
                        &font_encodings,
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
                                item_type: ItemType::Text,
                            });
                        }
                    }
                }
            }
            "Do" => {
                // XObject invocation - could be an image or form
                if !op.operands.is_empty() {
                    if let Ok(name) = op.operands[0].as_name() {
                        let xobj_name = String::from_utf8_lossy(name).to_string();

                        if let Some(xobj_type) = xobjects.get(&xobj_name) {
                            match xobj_type {
                                XObjectType::Image => {
                                    // Get position from CTM
                                    let (x, y) = (ctm[4], ctm[5]);
                                    // Get dimensions from CTM scale factors
                                    let width = ctm[0].abs();
                                    let height = ctm[3].abs();

                                    items.push(TextItem {
                                        text: format!("[Image: {}]", xobj_name),
                                        x,
                                        y,
                                        width,
                                        height,
                                        font: String::new(),
                                        font_size: 0.0,
                                        page: page_num,
                                        is_bold: false,
                                        is_italic: false,
                                        item_type: ItemType::Image,
                                    });
                                }
                                XObjectType::Form(form_id) => {
                                    // Extract text from Form XObject
                                    let form_items = extract_form_xobject_text(
                                        doc, *form_id, page_num, font_cmaps, &ctm,
                                    );
                                    items.extend(form_items);
                                }
                            }
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

/// Get XObject names that are images from page resources
/// XObject info - either Image or Form
#[derive(Debug)]
enum XObjectType {
    Image,
    Form(ObjectId),
}

/// Get XObjects from page resources, categorized by type
fn get_page_xobjects(
    doc: &Document,
    page_id: ObjectId,
) -> std::collections::HashMap<String, XObjectType> {
    let mut xobject_types = std::collections::HashMap::new();

    // Try to get the page dictionary
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        // Get Resources dictionary
        let resources = if let Ok(res_ref) = page_dict.get(b"Resources") {
            if let Ok(obj_ref) = res_ref.as_reference() {
                doc.get_dictionary(obj_ref).ok()
            } else {
                res_ref.as_dict().ok()
            }
        } else {
            None
        };

        if let Some(resources) = resources {
            // Get XObject dictionary from Resources
            if let Ok(xobjects_ref) = resources.get(b"XObject") {
                let xobjects = if let Ok(obj_ref) = xobjects_ref.as_reference() {
                    doc.get_dictionary(obj_ref).ok()
                } else {
                    xobjects_ref.as_dict().ok()
                };

                if let Some(xobjects) = xobjects {
                    for (name, value) in xobjects.iter() {
                        let name_str = String::from_utf8_lossy(name).to_string();

                        // Check XObject subtype
                        if let Ok(obj_ref) = value.as_reference() {
                            if let Ok(Object::Stream(stream)) = doc.get_object(obj_ref) {
                                if let Ok(subtype) = stream.dict.get(b"Subtype") {
                                    if let Ok(subtype_name) = subtype.as_name() {
                                        if subtype_name == b"Image" {
                                            xobject_types.insert(name_str, XObjectType::Image);
                                        } else if subtype_name == b"Form" {
                                            xobject_types
                                                .insert(name_str, XObjectType::Form(obj_ref));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    xobject_types
}

/// Extract text items from a Form XObject
fn extract_form_xobject_text(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
) -> Vec<TextItem> {
    use lopdf::content::Content;

    let mut items = Vec::new();

    // Get the Form XObject stream
    let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
        return items;
    };

    // Decompress the content stream
    let Ok(content_data) = stream.decompressed_content() else {
        return items;
    };

    // Decode the content stream
    let Ok(content) = Content::decode(&content_data) else {
        return items;
    };

    // Get fonts from the Form's Resources
    let form_fonts = get_form_fonts(doc, &stream.dict);
    let font_encodings = build_font_encodings(doc, &form_fonts);

    // Build font width info for the form
    let font_widths = build_font_widths(doc, &form_fonts);

    // Build font base names and ToUnicode refs for the form
    let mut font_base_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut font_tounicode_refs: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    for (font_name, font_dict) in &form_fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        if let Ok(tounicode) = font_dict.get(b"ToUnicode") {
            if let Ok(obj_ref) = tounicode.as_reference() {
                font_tounicode_refs.insert(resource_name, obj_ref.0);
            }
        }
    }

    // Process the content stream
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text_block = false;

    for op in &content.operations {
        match op.operator.as_str() {
            "BT" => {
                in_text_block = true;
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            }
            "ET" => {
                in_text_block = false;
            }
            "Tf" => {
                if op.operands.len() >= 2 {
                    if let Ok(name) = op.operands[0].as_name() {
                        current_font = String::from_utf8_lossy(name).to_string();
                    }
                    current_font_size = get_number(&op.operands[1]).unwrap_or(12.0);
                }
            }
            "Td" | "TD" => {
                if op.operands.len() >= 2 {
                    let tx = get_number(&op.operands[0]).unwrap_or(0.0);
                    let ty = get_number(&op.operands[1]).unwrap_or(0.0);
                    text_matrix[4] += tx;
                    text_matrix[5] += ty;
                }
            }
            "Tm" => {
                if op.operands.len() >= 6 {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        text_matrix[i] =
                            get_number(operand).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                    }
                }
            }
            "Tj" => {
                if in_text_block && !op.operands.is_empty() {
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        doc,
                        &form_fonts,
                        &current_font,
                        font_cmaps,
                        &font_base_names,
                        &font_tounicode_refs,
                        &font_encodings,
                    ) {
                        if !text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            let combined = multiply_matrices(&text_matrix, parent_ctm);
                            let (x, y) = (combined[4], combined[5]);
                            // Compute width from font widths if available
                            let width = if let Some(font_info) = font_widths.get(&current_font) {
                                if let Some(raw_bytes) = get_operand_bytes(&op.operands[0]) {
                                    let w_ts = compute_string_width_ts(
                                        raw_bytes,
                                        font_info,
                                        current_font_size,
                                    );
                                    text_matrix[4] += w_ts * text_matrix[0];
                                    text_matrix[5] += w_ts * text_matrix[1];
                                    (w_ts
                                        * (text_matrix[0] * parent_ctm[0]
                                            + text_matrix[1] * parent_ctm[2]))
                                        .abs()
                                } else {
                                    0.0
                                }
                            } else {
                                0.0
                            };
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text,
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
                                item_type: ItemType::Text,
                            });
                        }
                    }
                }
            }
            "TJ" => {
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        let font_info = font_widths.get(&current_font);

                        // Compute space threshold based on font metrics when available
                        let space_threshold = if let Some(fi) = font_info {
                            let space_em = fi.space_width as f32 * fi.units_scale;
                            let threshold = space_em * 1000.0 * 0.4;
                            threshold.clamp(80.0, 200.0)
                        } else {
                            120.0
                        };

                        let mut combined_text = String::new();
                        let mut total_width_ts: f32 = 0.0;
                        for element in array {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    total_width_ts += -n_val / 1000.0 * current_font_size;
                                    if n_val < -space_threshold
                                        && !combined_text.is_empty()
                                        && !combined_text.ends_with(' ')
                                    {
                                        combined_text.push(' ');
                                    }
                                    continue;
                                }
                                Object::Real(n) => {
                                    total_width_ts += -(*n) / 1000.0 * current_font_size;
                                    if *n < -space_threshold
                                        && !combined_text.is_empty()
                                        && !combined_text.ends_with(' ')
                                    {
                                        combined_text.push(' ');
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                            if let Some(fi) = font_info {
                                if let Some(raw_bytes) = get_operand_bytes(element) {
                                    total_width_ts +=
                                        compute_string_width_ts(raw_bytes, fi, current_font_size);
                                }
                            }
                            if let Some(text) = extract_text_from_operand(
                                element,
                                doc,
                                &form_fonts,
                                &current_font,
                                font_cmaps,
                                &font_base_names,
                                &font_tounicode_refs,
                                &font_encodings,
                            ) {
                                combined_text.push_str(&text);
                            }
                        }
                        if !combined_text.trim().is_empty() {
                            let rendered_size =
                                effective_font_size(current_font_size, &text_matrix);
                            let combined_mat = multiply_matrices(&text_matrix, parent_ctm);
                            let (x, y) = (combined_mat[4], combined_mat[5]);
                            let width = if font_info.is_some() {
                                (total_width_ts
                                    * (text_matrix[0] * parent_ctm[0]
                                        + text_matrix[1] * parent_ctm[2]))
                                    .abs()
                            } else {
                                0.0
                            };
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            items.push(TextItem {
                                text: combined_text,
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font),
                                is_italic: is_italic_font(base_font),
                                item_type: ItemType::Text,
                            });
                            if font_info.is_some() {
                                text_matrix[4] += total_width_ts * text_matrix[0];
                                text_matrix[5] += total_width_ts * text_matrix[1];
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Get fonts from a Form XObject's Resources
fn get_form_fonts<'a>(
    doc: &'a Document,
    form_dict: &lopdf::Dictionary,
) -> std::collections::BTreeMap<Vec<u8>, &'a lopdf::Dictionary> {
    let mut fonts = std::collections::BTreeMap::new();

    // Get Resources from Form dictionary
    let resources = if let Ok(res_ref) = form_dict.get(b"Resources") {
        if let Ok(obj_ref) = res_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            res_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(resources) = resources else {
        return fonts;
    };

    // Get Font dictionary
    let font_dict = if let Ok(font_ref) = resources.get(b"Font") {
        if let Ok(obj_ref) = font_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            font_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(font_dict) = font_dict else {
        return fonts;
    };

    // Collect fonts
    for (name, value) in font_dict.iter() {
        if let Ok(obj_ref) = value.as_reference() {
            if let Ok(dict) = doc.get_dictionary(obj_ref) {
                fonts.insert(name.clone(), dict);
            }
        }
    }

    fonts
}

/// Extract hyperlinks from page annotations
pub fn extract_page_links(doc: &Document, page_id: ObjectId, page_num: u32) -> Vec<TextItem> {
    let mut links = Vec::new();

    // Try to get the page dictionary
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        // Get Annots array
        let annots = if let Ok(annots_ref) = page_dict.get(b"Annots") {
            if let Ok(obj_ref) = annots_ref.as_reference() {
                doc.get_object(obj_ref)
                    .ok()
                    .and_then(|o| o.as_array().ok().cloned())
            } else {
                annots_ref.as_array().ok().cloned()
            }
        } else {
            None
        };

        if let Some(annots) = annots {
            for annot_ref in annots {
                // Get annotation dictionary
                let annot_dict = if let Ok(obj_ref) = annot_ref.as_reference() {
                    doc.get_dictionary(obj_ref).ok()
                } else {
                    annot_ref.as_dict().ok()
                };

                if let Some(annot_dict) = annot_dict {
                    // Check if this is a Link annotation
                    if let Ok(subtype) = annot_dict.get(b"Subtype") {
                        if let Ok(subtype_name) = subtype.as_name() {
                            if subtype_name != b"Link" {
                                continue;
                            }
                        }
                    }

                    // Get the Rect (position)
                    let rect = if let Ok(rect_obj) = annot_dict.get(b"Rect") {
                        if let Ok(rect_array) = rect_obj.as_array() {
                            if rect_array.len() >= 4 {
                                let x1 = get_number(&rect_array[0]).unwrap_or(0.0);
                                let y1 = get_number(&rect_array[1]).unwrap_or(0.0);
                                let x2 = get_number(&rect_array[2]).unwrap_or(0.0);
                                let y2 = get_number(&rect_array[3]).unwrap_or(0.0);
                                Some((x1, y1, x2 - x1, y2 - y1))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Get the action (A dictionary) or Dest
                    let uri = extract_link_uri(doc, annot_dict);

                    if let (Some((x, y, width, height)), Some(url)) = (rect, uri) {
                        links.push(TextItem {
                            text: url.clone(),
                            x,
                            y,
                            width,
                            height,
                            font: String::new(),
                            font_size: 0.0,
                            page: page_num,
                            is_bold: false,
                            is_italic: false,
                            item_type: ItemType::Link(url),
                        });
                    }
                }
            }
        }
    }

    links
}

/// Extract URI from a link annotation
fn extract_link_uri(doc: &Document, annot_dict: &lopdf::Dictionary) -> Option<String> {
    // Try to get the A (Action) dictionary
    if let Ok(action_ref) = annot_dict.get(b"A") {
        let action_dict = if let Ok(obj_ref) = action_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            action_ref.as_dict().ok()
        };

        if let Some(action_dict) = action_dict {
            // Check for URI action
            if let Ok(uri_obj) = action_dict.get(b"URI") {
                if let Ok(uri_str) = uri_obj.as_str() {
                    return Some(String::from_utf8_lossy(uri_str).to_string());
                }
            }
        }
    }

    // Try Dest (named destination) - less common for external links
    // We'll skip this for now as it requires looking up named destinations

    None
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
#[allow(clippy::too_many_arguments)]
fn extract_text_from_operand(
    obj: &Object,
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
    current_font: &str,
    font_cmaps: &FontCMaps,
    font_base_names: &std::collections::HashMap<String, String>,
    font_tounicode_refs: &std::collections::HashMap<String, u32>,
    font_encodings: &PageFontEncodings,
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

        // Try our custom encoding map from Differences arrays
        if let Some(encoding_map) = font_encodings.get(current_font) {
            let decoded: String = bytes
                .iter()
                .filter_map(|&b| encoding_map.get(&b).copied())
                .collect();
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

/// Estimate the width of a text item, falling back to a character-count heuristic when width is 0.
fn effective_width(item: &TextItem) -> f32 {
    if item.width > 0.0 {
        item.width
    } else {
        item.text.chars().count() as f32 * item.font_size * 0.5
    }
}

/// Represents a column region on a page
#[derive(Debug, Clone)]
struct ColumnRegion {
    x_min: f32,
    x_max: f32,
}

/// Detect column boundaries on a page using a horizontal projection profile.
///
/// Builds an occupancy histogram across the page width and finds empty valleys
/// (gutters) where no text exists. Validates valleys with vertical consistency
/// checks to avoid false positives.
fn detect_columns(items: &[TextItem], page: u32) -> Vec<ColumnRegion> {
    const BIN_WIDTH: f32 = 2.0;
    const MIN_GUTTER_WIDTH: f32 = 8.0;
    const MIN_VERTICAL_SPAN_RATIO: f32 = 0.30;
    const MIN_ITEMS_PER_COLUMN: usize = 10;
    const NOISE_FRACTION: f32 = 0.05;

    // Get items for this page
    let page_items: Vec<&TextItem> = items.iter().filter(|i| i.page == page).collect();

    if page_items.is_empty() {
        return vec![];
    }

    // Find page bounds
    let x_min = page_items.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let x_max = page_items
        .iter()
        .map(|i| i.x + effective_width(i))
        .fold(f32::NEG_INFINITY, f32::max);

    let page_width = x_max - x_min;
    if page_width < 200.0 {
        return vec![ColumnRegion { x_min, x_max }];
    }

    if page_items.len() < 20 {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Build occupancy histogram
    let num_bins = ((page_width / BIN_WIDTH).ceil() as usize).max(1);
    let mut histogram = vec![0u32; num_bins];

    for item in &page_items {
        let w = effective_width(item);
        let left = ((item.x - x_min) / BIN_WIDTH).floor() as usize;
        let right = (((item.x + w) - x_min) / BIN_WIDTH).ceil() as usize;
        let left = left.min(num_bins);
        let right = right.min(num_bins);
        for count in histogram.iter_mut().take(right).skip(left) {
            *count += 1;
        }
    }

    // Find the noise threshold: bins with count <= max_count * NOISE_FRACTION are "empty"
    let max_count = *histogram.iter().max().unwrap_or(&0);
    let noise_threshold = (max_count as f32 * NOISE_FRACTION) as u32;

    // Find empty valleys (consecutive runs of low-count bins)
    // Each valley is stored as (start_bin, end_bin)
    let mut valleys: Vec<(usize, usize)> = Vec::new();
    let mut valley_start: Option<usize> = None;

    for (i, &count) in histogram.iter().enumerate() {
        if count <= noise_threshold {
            if valley_start.is_none() {
                valley_start = Some(i);
            }
        } else if let Some(start) = valley_start {
            valleys.push((start, i));
            valley_start = None;
        }
    }
    // Close any valley that extends to the end
    if let Some(start) = valley_start {
        valleys.push((start, num_bins));
    }

    // Filter valleys: must be wide enough and not at page margins
    let margin_threshold = page_width * 0.05;
    let valleys: Vec<(usize, usize)> = valleys
        .into_iter()
        .filter(|&(start, end)| {
            let width_pts = (end - start) as f32 * BIN_WIDTH;
            if width_pts < MIN_GUTTER_WIDTH {
                return false;
            }
            // Valley center must not be within 5% of page edges
            let center_pts = ((start + end) as f32 / 2.0) * BIN_WIDTH;
            center_pts > margin_threshold && center_pts < (page_width - margin_threshold)
        })
        .collect();

    if valleys.is_empty() {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Compute Y range of the page
    let y_min = page_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let y_max = page_items
        .iter()
        .map(|i| i.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let y_range = y_max - y_min;

    // Validate each valley with vertical consistency
    let mut valid_valleys: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in &valleys {
        let gutter_left = x_min + start as f32 * BIN_WIDTH;
        let gutter_right = x_min + end as f32 * BIN_WIDTH;
        let gutter_center = (gutter_left + gutter_right) / 2.0;

        // Collect items on each side of the gutter
        let left_items: Vec<&&TextItem> = page_items
            .iter()
            .filter(|i| i.x + effective_width(i) <= gutter_center)
            .collect();
        let right_items: Vec<&&TextItem> =
            page_items.iter().filter(|i| i.x >= gutter_center).collect();

        if left_items.len() < MIN_ITEMS_PER_COLUMN || right_items.len() < MIN_ITEMS_PER_COLUMN {
            continue;
        }

        // Check vertical overlap
        if y_range > 0.0 {
            let left_y_min = left_items.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
            let left_y_max = left_items
                .iter()
                .map(|i| i.y)
                .fold(f32::NEG_INFINITY, f32::max);
            let right_y_min = right_items
                .iter()
                .map(|i| i.y)
                .fold(f32::INFINITY, f32::min);
            let right_y_max = right_items
                .iter()
                .map(|i| i.y)
                .fold(f32::NEG_INFINITY, f32::max);

            let overlap_min = left_y_min.max(right_y_min);
            let overlap_max = left_y_max.min(right_y_max);
            let overlap = (overlap_max - overlap_min).max(0.0);

            if overlap / y_range < MIN_VERTICAL_SPAN_RATIO {
                continue;
            }
        }

        valid_valleys.push((start, end));
    }

    if valid_valleys.is_empty() {
        return vec![ColumnRegion { x_min, x_max }];
    }

    // Limit to at most 3 gutters (4 columns) — keep the widest if more found
    if valid_valleys.len() > 3 {
        valid_valleys.sort_by(|a, b| {
            let wa = (a.1 - a.0) as f32;
            let wb = (b.1 - b.0) as f32;
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
        valid_valleys.truncate(3);
        // Re-sort by position (left to right)
        valid_valleys.sort_by_key(|v| v.0);
    }

    // Build column regions from gutter boundaries
    let mut columns = Vec::new();
    let mut col_start = x_min;
    for &(start, end) in &valid_valleys {
        let gutter_center = x_min + ((start + end) as f32 / 2.0) * BIN_WIDTH;
        columns.push(ColumnRegion {
            x_min: col_start,
            x_max: gutter_center,
        });
        col_start = gutter_center;
    }
    columns.push(ColumnRegion {
        x_min: col_start,
        x_max,
    });

    columns
}

/// Determines if a text item spans across multiple column regions (e.g. full-width headers/titles).
fn spans_multiple_columns(item: &TextItem, columns: &[ColumnRegion]) -> bool {
    let w = effective_width(item);
    let item_right = item.x + w;
    let overlap_count = columns
        .iter()
        .filter(|col| {
            let overlap_start = item.x.max(col.x_min);
            let overlap_end = item_right.min(col.x_max);
            let overlap = (overlap_end - overlap_start).max(0.0);
            overlap > (col.x_max - col.x_min) * 0.10 || overlap > 20.0
        })
        .count();
    overlap_count >= 2
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
            // Multi-column - separate spanning items from column items
            let mut spanning_items: Vec<TextItem> = Vec::new();
            let mut column_items: Vec<TextItem> = Vec::new();

            for item in &page_items {
                if spans_multiple_columns(item, &columns) {
                    spanning_items.push(item.clone());
                } else {
                    column_items.push(item.clone());
                }
            }

            // Process each column's items independently
            let mut column_lines: Vec<TextLine> = Vec::new();
            for column in &columns {
                let col_items: Vec<TextItem> = column_items
                    .iter()
                    .filter(|i| {
                        let center = i.x + effective_width(i) / 2.0;
                        center >= column.x_min && center < column.x_max
                    })
                    .cloned()
                    .collect();

                let lines = group_single_column(col_items);
                column_lines.extend(lines);
            }

            // Process spanning items as their own group
            let spanning_lines = group_single_column(spanning_items);

            // Merge: interleave spanning lines by Y position among column lines
            let mut merged: Vec<TextLine> = Vec::new();
            let mut span_idx = 0;
            let mut col_idx = 0;

            // Sort spanning lines by Y descending (top-first in PDF coords)
            let mut spanning_lines = spanning_lines;
            spanning_lines
                .sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));

            while span_idx < spanning_lines.len() && col_idx < column_lines.len() {
                // Higher Y = higher on page = comes first in reading order
                if spanning_lines[span_idx].y >= column_lines[col_idx].y {
                    merged.push(spanning_lines[span_idx].clone());
                    span_idx += 1;
                } else {
                    merged.push(column_lines[col_idx].clone());
                    col_idx += 1;
                }
            }
            while span_idx < spanning_lines.len() {
                merged.push(spanning_lines[span_idx].clone());
                span_idx += 1;
            }
            while col_idx < column_lines.len() {
                merged.push(column_lines[col_idx].clone());
                col_idx += 1;
            }

            all_lines.extend(merged);
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
            if last_line.page != item.page {
                return false;
            }
            let y_diff = (last_line.y - item.y).abs();
            if y_diff >= y_tolerance {
                return false;
            }
            // Check if this looks like a new line despite similar Y:
            // If items are at the same X position (left margin) but different Y,
            // they're vertically stacked lines, not the same line
            let has_y_change = y_diff > 0.5;
            if has_y_change {
                if let Some(first_item) = last_line.items.first() {
                    let at_same_x = (item.x - first_item.x).abs() < 5.0;
                    // If at same X (left margin) with Y change, it's likely a new line
                    if at_same_x {
                        return false;
                    }
                    // If new item starts significantly to the left with Y change,
                    // it's a new line (not just out-of-order items on same line)
                    if let Some(last_item) = last_line.items.last() {
                        if item.x < last_item.x - 10.0 {
                            return false;
                        }
                    }
                }
            }
            true
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
                item_type: ItemType::Text,
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
                item_type: ItemType::Text,
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
                item_type: ItemType::Text,
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
