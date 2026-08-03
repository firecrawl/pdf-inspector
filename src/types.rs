//! Shared types used across the extraction and markdown pipelines.
//!
//! Centralises `TextItem`, `TextLine`, `PdfRect`, font-width / encoding
//! type aliases, and the `ItemType` enum so that every module can import
//! them from one place.

use std::collections::HashMap;

use crate::text_utils::should_join_items;

/// Result tuple returned by page-level text extraction: text items, rectangles, line segments,
/// and whether fonts with unresolvable gid-encoded glyphs were encountered.
pub(crate) type PageExtraction = (Vec<TextItem>, Vec<PdfRect>, Vec<PdfLine>);

// ── Font types (crate-internal) ──────────────────────────────────────

/// Font encoding map: maps byte codes to Unicode characters
pub(crate) type FontEncodingMap = HashMap<u8, char>;

/// All font encodings for a page
pub(crate) type PageFontEncodings = HashMap<String, FontEncodingMap>;

/// Font width information extracted from PDF font dictionaries
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct FontWidthInfo {
    /// Glyph widths: maps character code to width in font units
    pub(crate) widths: HashMap<u16, u16>,
    /// Default width for glyphs not in the widths table
    pub(crate) default_width: u16,
    /// Width of the space character (code 32) if known
    pub(crate) space_width: u16,
    /// Whether this is a CID font (2-byte character codes)
    pub(crate) is_cid: bool,
    /// Scale factor to convert font units to text space units.
    /// For Type1/TrueType: 0.001 (widths in 1000ths of em)
    /// For Type3: FontMatrix[0] (e.g., 0.00048828125 for 2048-unit grid)
    pub(crate) units_scale: f32,
    /// Writing mode: 0 = horizontal (default), 1 = vertical
    pub(crate) wmode: u8,
}

/// All font width info for a page, keyed by font resource name
pub(crate) type PageFontWidths = HashMap<String, FontWidthInfo>;

// ── Public types ─────────────────────────────────────────────────────

/// Type of extracted item
#[derive(Debug, Clone, Default)]
pub enum ItemType {
    /// Regular text content
    #[default]
    Text,
    /// Image placeholder
    Image,
    /// Hyperlink (with URL)
    Link(String),
    /// Form field (name: value)
    FormField,
}

/// Layout complexity analysis result.
///
/// Callers can use this to decide whether the extracted markdown is reliable
/// or whether the PDF should be routed to an OCR pipeline instead.
#[derive(Debug, Clone, Default)]
pub struct LayoutComplexity {
    /// True if any page has tables or multi-column text.
    pub is_complex: bool,
    /// 1-indexed pages where table borders were detected (rect count > 6).
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where 2+ text columns were detected.
    pub pages_with_columns: Vec<u32>,
}

/// A line segment from PDF path operators (`m`/`l`/`S`).
#[derive(Debug, Clone)]
pub struct PdfLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub page: u32,
}

/// A rectangle from a PDF `re` operator (cell boundary, border, etc.)
#[derive(Debug, Clone)]
pub struct PdfRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub page: u32,
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
    /// Whether the text is underlined (drawn rule/thin rect under the
    /// baseline — PDFs have no underline font flag, so this is detected
    /// geometrically after extraction; see `extractor::underline`).
    pub is_underline: bool,
    /// Whether the text is struck out (drawn rule/thin rect crossing the
    /// glyphs at mid x-height). Same geometric detection as underline,
    /// different vertical window; see `extractor::underline`.
    pub is_strikeout: bool,
    /// Type of item (text, image, link)
    pub item_type: ItemType,
    /// Marked Content ID from the content stream's BDC/BMC operator.
    /// Used to link this item to the PDF structure tree for tagged PDFs.
    pub mcid: Option<i64>,
}

/// A line of text (grouped text items)
#[derive(Debug, Clone)]
pub struct TextLine {
    pub items: Vec<TextItem>,
    pub y: f32,
    pub page: u32,
    /// Adaptive join threshold from page-level letter-spacing detection.
    /// Default 0.10 for normal PDFs; higher for Canva-style PDFs.
    #[doc(hidden)]
    pub adaptive_threshold: f32,
}

impl TextLine {
    pub fn text(&self) -> String {
        self.text_with_formatting(false, false, false)
    }

    /// Get text with optional bold/italic/decorative markdown formatting.
    ///
    /// `format_decorations` enables both geometrically detected source
    /// decorations: underline (`<u>`) and strikeout (`<s>`).
    pub fn text_with_formatting(
        &self,
        format_bold: bool,
        format_italic: bool,
        format_decorations: bool,
    ) -> String {
        if !format_bold && !format_italic && !format_decorations {
            return self.text_plain();
        }

        let single_char_threshold = self.adaptive_threshold;

        let mut result = String::new();
        let mut current_bold = false;
        let mut current_italic = false;
        let mut current_underline = false;
        let mut current_strikeout = false;

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
                self.needs_space_between(prev_item, item, &result, single_char_threshold)
            };

            // Preserve leading whitespace from the item text.
            // Items like " means any person" have a leading space that indicates
            // a word boundary. needs_space_between returns false for these (because
            // space_already_exists), but we still need to emit the space since
            // we push text_trimmed below (which strips it).
            let has_leading_space = text.starts_with(' ');

            // Check for style changes. Source decorations are exclusive:
            // `<u>`/`<s>` content stays free of `**`/`*` markers — consumers
            // (and the eval harnesses this feeds) match tag content literally,
            // and mixed nesting breaks that. A struck-and-underlined item is
            // emitted as struck text because deletion is the stronger semantic
            // distinction in redline documents.
            let item_strikeout = format_decorations && item.is_strikeout;
            let item_underline = format_decorations && item.is_underline && !item_strikeout;
            let item_bold = format_bold && item.is_bold && !item_underline && !item_strikeout;
            let item_italic = format_italic && item.is_italic && !item_underline && !item_strikeout;

            // Close previous styles if they change
            if current_italic && !item_italic {
                result.push('*');
                current_italic = false;
            }
            if current_bold && !item_bold {
                result.push_str("**");
                current_bold = false;
            }
            if current_underline && !item_underline {
                result.push_str("</u>");
                current_underline = false;
            }
            if current_strikeout && !item_strikeout {
                result.push_str("</s>");
                current_strikeout = false;
            }

            // Add space: either from spacing logic or preserved from item text
            if needs_space || (has_leading_space && !result.is_empty() && !result.ends_with(' ')) {
                result.push(' ');
            }

            // Open new styles
            if item_underline && !current_underline {
                result.push_str("<u>");
                current_underline = true;
            }
            if item_strikeout && !current_strikeout {
                result.push_str("<s>");
                current_strikeout = true;
            }
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
        if current_underline {
            result.push_str("</u>");
        }
        if current_strikeout {
            result.push_str("</s>");
        }

        result
    }

    /// Get plain text without formatting
    fn text_plain(&self) -> String {
        let single_char_threshold = self.adaptive_threshold;

        let mut result = String::new();
        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            if i == 0 {
                result.push_str(text);
            } else {
                let prev_item = &self.items[i - 1];
                if self.needs_space_between(prev_item, item, &result, single_char_threshold) {
                    result.push(' ');
                }
                result.push_str(text);
            }
        }
        result
    }

    /// Determine if a space is needed between two items
    fn needs_space_between(
        &self,
        prev_item: &TextItem,
        item: &TextItem,
        result: &str,
        single_char_threshold: f32,
    ) -> bool {
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
        let should_join = should_join_items(prev_item, item, single_char_threshold);

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

#[cfg(test)]
mod formatting_tests {
    use super::{ItemType, TextItem, TextLine};

    fn item(text: &str, x: f32, width: f32, strikeout: bool) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y: 100.0,
            width,
            height: 12.0,
            font: "F1".to_string(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: strikeout,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn line(items: Vec<TextItem>) -> TextLine {
        TextLine {
            items,
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.1,
        }
    }

    #[test]
    fn formatting_emits_semantic_strikeout() {
        let line = line(vec![item("deleted", 10.0, 42.0, true)]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted</s>"
        );
    }

    #[test]
    fn formatting_closes_strikeout_before_live_text() {
        let line = line(vec![
            item("keep", 10.0, 24.0, false),
            item("remove", 40.0, 42.0, true),
            item("keep", 88.0, 24.0, false),
        ]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "keep <s>remove</s> keep"
        );
    }

    #[test]
    fn formatting_coalesces_adjacent_struck_items() {
        let line = line(vec![
            item("deleted", 10.0, 42.0, true),
            item("words", 58.0, 30.0, true),
        ]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted words</s>"
        );
    }

    #[test]
    fn strikeout_takes_precedence_over_other_styles() {
        let mut decorated = item("deleted", 10.0, 42.0, true);
        decorated.is_bold = true;
        decorated.is_italic = true;
        decorated.is_underline = true;
        let line = line(vec![decorated]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted</s>"
        );
        assert_eq!(line.text(), "deleted");
    }
}
