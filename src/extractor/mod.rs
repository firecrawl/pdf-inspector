//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

pub(crate) mod content_stream;
mod fonts;
mod layout;
mod links;
mod xobjects;

use crate::text_utils::is_rtl_text;
use crate::tounicode::FontCMaps;
use crate::types::{PageExtraction, TextItem};
use crate::PdfError;
use log::debug;
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use content_stream::extract_page_text_items;
use links::{extract_form_fields, extract_page_links};

// Re-export public types so existing `crate::extractor::X` paths keep working.
pub use crate::text_utils::{is_bold_font, is_italic_font};
pub use crate::types::{ItemType, TextLine};
pub(crate) use layout::detect_columns;
pub use layout::group_into_lines;
pub(crate) use layout::group_into_lines_with_thresholds;
pub(crate) use layout::is_newspaper_layout;
pub(crate) use layout::ColumnRegion;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract text from PDF file as plain string
pub fn extract_text<P: AsRef<Path>>(path: P) -> Result<String, PdfError> {
    crate::validate_pdf_file(&path)?;
    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_with_password(&path, "")?
        }
        Err(e) => return Err(e.into()),
    };
    extract_text_from_doc(&doc)
}

/// Extract text from PDF memory buffer
pub fn extract_text_mem(buffer: &[u8]) -> Result<String, PdfError> {
    crate::validate_pdf_bytes(buffer)?;
    let doc = match Document::load_mem(buffer) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_mem_with_options(buffer, lopdf::LoadOptions::with_password(""))?
        }
        Err(e) => return Err(e.into()),
    };
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
    extract_text_with_positions_pages(path, None)
}

/// Extract text with positions from a file, limited to specific pages.
///
/// `page_filter` is an optional set of 1-indexed page numbers to process.
/// When `None`, all pages are processed.
pub fn extract_text_with_positions_pages<P: AsRef<Path>>(
    path: P,
    page_filter: Option<&HashSet<u32>>,
) -> Result<Vec<TextItem>, PdfError> {
    let (items, _rects, _lines) = extract_text_with_positions_and_rects(path, page_filter)?;
    Ok(items)
}

/// Extract text with positions and rectangles from a file.
pub(crate) fn extract_text_with_positions_and_rects<P: AsRef<Path>>(
    path: P,
    page_filter: Option<&HashSet<u32>>,
) -> Result<PageExtraction, PdfError> {
    crate::validate_pdf_file(&path)?;
    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_with_password(&path, "")?
        }
        Err(e) => return Err(e.into()),
    };
    let font_cmaps = FontCMaps::from_doc(&doc);
    let (extraction, _thresholds, _gid_pages) =
        extract_positioned_text_from_doc(&doc, &font_cmaps, page_filter)?;
    Ok(extraction)
}

/// Extract text with positions from memory buffer
pub fn extract_text_with_positions_mem(buffer: &[u8]) -> Result<Vec<TextItem>, PdfError> {
    extract_text_with_positions_mem_pages(buffer, None)
}

/// Extract text with positions from memory buffer, limited to specific pages.
pub fn extract_text_with_positions_mem_pages(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
) -> Result<Vec<TextItem>, PdfError> {
    let (items, _rects, _lines) = extract_text_with_positions_mem_and_rects(buffer, page_filter)?;
    Ok(items)
}

/// Extract text with positions and rectangles from memory buffer.
pub(crate) fn extract_text_with_positions_mem_and_rects(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
) -> Result<PageExtraction, PdfError> {
    crate::validate_pdf_bytes(buffer)?;
    let doc = match Document::load_mem(buffer) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_mem_with_options(buffer, lopdf::LoadOptions::with_password(""))?
        }
        Err(e) => return Err(e.into()),
    };
    let font_cmaps = FontCMaps::from_doc(&doc);
    let (extraction, _thresholds, _gid_pages) =
        extract_positioned_text_from_doc(&doc, &font_cmaps, page_filter)?;
    Ok(extraction)
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Per-page adaptive join thresholds from Canva-style letter-spacing detection.
pub(crate) type PageThresholds = HashMap<u32, f32>;

/// Extract positioned text, rectangles, and line segments from a pre-loaded document.
///
/// Also returns per-page adaptive join thresholds for Canva-style pages.
pub(crate) fn extract_positioned_text_from_doc(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    extract_positioned_text_impl(doc, font_cmaps, page_filter, false)
}

/// Extract with option to include invisible (Tr=3) text.
/// Used for Mixed/template PDFs where the OCR text layer is invisible.
pub(crate) fn extract_positioned_text_include_invisible(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    extract_positioned_text_impl(doc, font_cmaps, page_filter, true)
}

fn extract_positioned_text_impl(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    let pages = doc.get_pages();
    let mut all_items = Vec::new();
    let mut all_rects = Vec::new();
    let mut all_lines = Vec::new();
    let mut page_thresholds: PageThresholds = HashMap::new();
    let mut gid_encoded_pages: HashSet<u32> = HashSet::new();

    // Build page ObjectId → page number map for form field extraction
    let page_id_to_num: HashMap<ObjectId, u32> =
        pages.iter().map(|(num, &id)| (id, *num)).collect();

    for (page_num, &page_id) in pages.iter() {
        if let Some(filter) = page_filter {
            if !filter.contains(page_num) {
                continue;
            }
        }
        let ((mut items, rects, lines), has_gid_fonts) =
            extract_page_text_items(doc, page_id, *page_num, font_cmaps, include_invisible)?;
        if has_gid_fonts {
            gid_encoded_pages.insert(*page_num);
        }
        let threshold = crate::text_utils::fix_letterspaced_items(&mut items);
        if threshold > 0.10 {
            page_thresholds.insert(*page_num, threshold);
        }
        debug!(
            "page {}: {} text items, {} rects, {} lines{}",
            page_num,
            items.len(),
            rects.len(),
            lines.len(),
            if has_gid_fonts {
                " [gid-encoded fonts]"
            } else {
                ""
            }
        );
        if log::log_enabled!(log::Level::Trace) {
            for item in &items {
                log::trace!(
                    "  p={} x={:7.1} y={:7.1} w={:7.1} fs={:5.1} font={:6} {:?}",
                    page_num,
                    item.x,
                    item.y,
                    item.width,
                    item.font_size,
                    item.font,
                    if item.text.len() > 80 {
                        &item.text[..80]
                    } else {
                        &item.text
                    }
                );
            }
        }
        all_items.extend(items);
        all_rects.extend(rects);
        all_lines.extend(lines);

        // Extract hyperlinks from page annotations
        let links = extract_page_links(doc, page_id, *page_num);
        all_items.extend(links);
    }

    // Extract AcroForm field values
    let form_items = extract_form_fields(doc, &page_id_to_num);
    all_items.extend(form_items);

    Ok((
        (all_items, all_rects, all_lines),
        page_thresholds,
        gid_encoded_pages,
    ))
}

// ---------------------------------------------------------------------------
// Shared helpers (used by submodules via `super::`)
// ---------------------------------------------------------------------------

/// Multiply two 2D transformation matrices
/// Matrix format: [a, b, c, d, e, f] representing:
/// | a  b  0 |
/// | c  d  0 |
/// | e  f  1 |
pub(crate) fn multiply_matrices(m1: &[f32; 6], m2: &[f32; 6]) -> [f32; 6] {
    [
        m1[0] * m2[0] + m1[1] * m2[2],
        m1[0] * m2[1] + m1[1] * m2[3],
        m1[2] * m2[0] + m1[3] * m2[2],
        m1[2] * m2[1] + m1[3] * m2[3],
        m1[4] * m2[0] + m1[5] * m2[2] + m2[4],
        m1[4] * m2[1] + m1[5] * m2[3] + m2[5],
    ]
}

/// Merge adjacent text items on the same line into single items.
///
/// Groups items by (page, Y-position) with a 5pt tolerance, sorts within each
/// group by X, then merges consecutive items that share a similar font size
/// and are close horizontally.
/// Cap item width for merge-gap computation to guard against Tw inflation.
///
/// When PDF word-spacing (Tw) is large (used for text justification), the
/// advance width of strings containing spaces extends far past the visible
/// glyph extent.  This inflated width collapses inter-column gaps, making
/// `merge_text_items` incorrectly merge items from different table columns.
///
/// Only applies to non-CJK items whose text contains spaces (where Tw
/// contributes) and whose average width-per-character is abnormally high.
fn effective_merge_width(item: &TextItem) -> f32 {
    use crate::text_utils::is_cjk_char;

    if item.width <= 0.0 || item.font_size <= 0.0 {
        return item.width;
    }
    // Tw only inflates strings that contain space characters.
    if !item.text.contains(' ') {
        return item.width;
    }
    // CJK characters are naturally ~1.0× font_size wide; skip the cap.
    if item.text.chars().any(is_cjk_char) {
        return item.width;
    }
    let char_count = item.text.chars().count();
    if char_count == 0 {
        return item.width;
    }
    let avg = item.width / char_count as f32;
    // Normal proportional text: ~0.5× font_size per char.
    // Monospace: ~0.6×.  Threshold at 0.85× catches Tw inflation.
    if avg > item.font_size * 0.85 {
        let capped = char_count as f32 * item.font_size * 0.6;
        capped.min(item.width)
    } else {
        item.width
    }
}

pub(crate) fn merge_text_items(items: Vec<TextItem>) -> Vec<TextItem> {
    if items.is_empty() {
        return items;
    }

    // Group items by (page, Y position) with 5pt tolerance
    let y_tolerance = 5.0;
    let mut line_groups: Vec<(u32, f32, Vec<&TextItem>)> = Vec::new();

    for item in &items {
        let found = line_groups
            .iter_mut()
            .find(|(pg, y, _)| *pg == item.page && (item.y - *y).abs() < y_tolerance);
        if let Some((_, _, group)) = found {
            group.push(item);
        } else {
            line_groups.push((item.page, item.y, vec![item]));
        }
    }

    // Sort each group by X position (direction-aware)
    for (_, _, group) in &mut line_groups {
        let rtl = is_rtl_text(group.iter().map(|i| &i.text));
        if rtl {
            group.sort_by(|a, b| b.x.total_cmp(&a.x));
        } else {
            group.sort_by(|a, b| a.x.total_cmp(&b.x));
        }
    }

    // Sort groups by page then Y descending (top of page first)
    line_groups.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.total_cmp(&a.1)));

    let mut merged = Vec::new();

    for (_, _, group) in &line_groups {
        let mut i = 0;
        while i < group.len() {
            let first = group[i];
            let mut text = first.text.clone();
            let mut end_x = first.x + effective_merge_width(first);
            let x_gap_max = first.font_size * 0.5;

            let mut j = i + 1;
            while j < group.len() {
                let next = group[j];
                // Must be similar font size (within 20%)
                if (next.font_size - first.font_size).abs() > first.font_size * 0.20 {
                    break;
                }
                let gap = next.x - end_x;
                if gap > x_gap_max {
                    break;
                }
                if gap < -first.font_size * 0.5 {
                    break;
                }
                // Insert space at word boundaries.
                // Base threshold 0.08; raised to 0.13 for lowercase→lowercase
                // junctions to accommodate Tc/Tw character-spacing adjustments
                // that shift advance widths relative to Td positioning.
                let threshold = {
                    let prev_last = text.trim_end().chars().last();
                    let next_first = next.text.trim_start().chars().next();
                    // Never insert space before joining punctuation
                    if next_first.is_some_and(|c| matches!(c, '.' | ',' | ';' | ')' | ']' | '}')) {
                        first.font_size * 0.25
                    } else if prev_last.is_some_and(|c| c.is_lowercase())
                        && next_first.is_some_and(|c| c.is_lowercase())
                    {
                        // Lowercase→lowercase: likely mid-word, use wider threshold
                        first.font_size * 0.13
                    } else {
                        first.font_size * 0.08
                    }
                };
                if gap > threshold {
                    text.push(' ');
                }
                text.push_str(&next.text);
                end_x = next.x + effective_merge_width(next);
                j += 1;
            }

            merged.push(TextItem {
                text,
                x: first.x,
                y: first.y,
                width: end_x - first.x,
                height: first.height,
                font: first.font.clone(),
                font_size: first.font_size,
                page: first.page,
                is_bold: first.is_bold,
                is_italic: first.is_italic,
                item_type: first.item_type.clone(),
                mcid: first.mcid,
            });

            i = j;
        }
    }

    merged
}

/// Merge subscript/superscript items into their adjacent parent items.
///
/// Subscripts (e.g. "2" in H₂O) are rendered as separate text items with a
/// much smaller font size and a slight Y offset. This pass finds such items
/// and absorbs them into the preceding normal-sized item so that downstream
/// table detection and line grouping see complete text (e.g. "H2O" not "H"+"2"+"O").
pub(crate) fn merge_subscript_items(items: Vec<TextItem>) -> Vec<TextItem> {
    if items.len() < 2 {
        return items;
    }

    // Group items by (page, approximate Y) with generous tolerance to capture
    // both the parent line and the subscript/superscript offset.
    let y_tolerance = 5.0;
    let mut line_groups: Vec<(u32, f32, Vec<TextItem>)> = Vec::new();

    for item in items {
        let found = line_groups
            .iter_mut()
            .find(|(pg, y, _)| *pg == item.page && (item.y - *y).abs() < y_tolerance);
        if let Some((_, _, group)) = found {
            group.push(item);
        } else {
            let page = item.page;
            let y = item.y;
            line_groups.push((page, y, vec![item]));
        }
    }

    let mut result = Vec::new();

    for (_, _, mut group) in line_groups {
        // Sort by X position
        group.sort_by(|a, b| a.x.total_cmp(&b.x));

        // Find the dominant (most common) font size in this group
        let max_fs = group.iter().map(|i| i.font_size).fold(0.0_f32, f32::max);

        if max_fs < 1.0 {
            result.extend(group);
            continue;
        }

        let sub_threshold = max_fs * 0.75;

        // Walk through items and merge subscripts into their preceding parent
        let mut merged: Vec<TextItem> = Vec::new();
        for item in group {
            if item.font_size < sub_threshold
                && item.font_size > 0.0
                && item.text.len() <= 4
                && item.text.chars().all(|c| c.is_ascii_digit())
            {
                // This is a candidate numeric subscript/superscript (e.g. "2" in H₂O).
                // Only merge purely numeric text to avoid false positives with small
                // bullets, ordinal indicators, or letter-based labels.
                if let Some(parent) = merged.last_mut() {
                    // Only merge into a parent that is normal-sized, not another subscript,
                    // and whose text ends with a letter. This prevents merging into numbers
                    // (e.g. "33" + "1" in "33 1/3%") or punctuation, while preserving
                    // chemical formulas (NH + "3") and footnote refs (word + "2").
                    let ends_with_letter = parent
                        .text
                        .chars()
                        .last()
                        .is_some_and(|c| c.is_alphabetic());
                    if parent.font_size >= sub_threshold && ends_with_letter {
                        let parent_right = parent.x + parent.width;
                        let gap = item.x - parent_right;
                        // Subscripts must be tightly adjacent (within ~1pt)
                        if gap < parent.font_size * 0.2 && gap > -parent.font_size * 0.3 {
                            parent.text.push_str(&item.text);
                            parent.width = (item.x + item.width) - parent.x;
                            continue;
                        }
                    }
                }
            }
            merged.push(item);
        }
        result.extend(merged);
    }

    result
}

/// Helper to get f32 from Object
pub(crate) fn get_number(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_utils::{is_cjk_char, is_rtl_char, is_rtl_text, sort_line_items};
    use crate::types::{ItemType, TextLine};
    use layout::{detect_columns, is_newspaper_layout, ColumnRegion};

    fn make_merge_item(text: &str, x: f32, width: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y: 700.0,
            width,
            height: 12.0,
            font: "F1".into(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn merge_items_no_space_before_period() {
        // Simulate Tc/Tw-adjusted width: "date" width is smaller than the gap
        // to "." due to negative Tc, but period should still join without space.
        let items = vec![
            make_merge_item("date", 227.25, 89.25), // end = 316.50
            make_merge_item(".", 318.00, 3.0),      // gap = 1.50 (0.125 × fs)
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "date.");
    }

    #[test]
    fn merge_items_lowercase_join_with_tc() {
        // Lowercase→lowercase junction: "deve" + "lopers" with Tc-affected gap
        // Gap of 0.12 × font_size should merge without space
        let items = vec![
            make_merge_item("deve", 100.0, 30.0),    // end = 130.0
            make_merge_item("lopers", 131.44, 40.0), // gap = 1.44 (0.12 × 12)
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "developers");
    }

    #[test]
    fn merge_items_space_at_word_boundary() {
        // Word boundary gap (> 0.13 × font_size) should insert space
        let items = vec![
            make_merge_item("hello", 100.0, 30.0),
            make_merge_item("world", 132.0, 30.0), // gap = 2.0 (0.167 × 12)
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "hello world");
    }

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
                mcid: None,
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
                mcid: None,
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
                mcid: None,
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

    #[test]
    fn test_word_level_items_get_spaces() {
        // Simulate CID font per-word items touching with gap=0
        let items = vec![
            TextItem {
                text: "the".into(),
                x: 100.0,
                y: 500.0,
                width: 19.5,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "Prague".into(),
                x: 119.5,
                y: 500.0,
                width: 42.0,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "Rules".into(),
                x: 161.5,
                y: 500.0,
                width: 35.0,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "the Prague Rules");
    }

    #[test]
    fn test_single_char_items_still_join() {
        // Per-glyph positioning: single chars should join into words
        let items = vec![
            TextItem {
                text: "N".into(),
                x: 100.0,
                y: 500.0,
                width: 8.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "A".into(),
                x: 108.0,
                y: 500.0,
                width: 8.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "V".into(),
                x: 116.0,
                y: 500.0,
                width: 8.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "NAV");
    }

    #[test]
    fn test_per_glyph_word_boundaries() {
        // Per-character PDF rendering (e.g. SEC filings): each glyph is a
        // separate TextItem. Intra-word gaps are ≈ 0, word gaps ≈ 2.0 at
        // font_size 13.3 (ratio 0.15). Must detect word boundaries correctly.
        fn char_item(ch: &str, x: f32, width: f32) -> TextItem {
            TextItem {
                text: ch.into(),
                x,
                y: 719.3,
                width,
                height: 13.3,
                font: "F4".into(),
                font_size: 13.3,
                page: 1,
                is_bold: true,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            }
        }

        // "Item 2" — gap of 2.0 between 'm' and '2' at font_size 13.3
        let items = vec![
            char_item("I", 24.3, 3.1),
            char_item("t", 27.5, 2.7),
            char_item("e", 30.1, 3.5),
            char_item("m", 33.7, 6.7),
            char_item("2", 42.3, 4.0), // gap = 42.3 - 40.4 = 1.9
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Item 2");
    }

    #[test]
    fn test_per_glyph_words_not_merged() {
        // Verify multiple words from per-character rendering get spaces between them
        fn char_item(ch: &str, x: f32, width: f32) -> TextItem {
            TextItem {
                text: ch.into(),
                x,
                y: 705.5,
                width,
                height: 13.3,
                font: "F5".into(),
                font_size: 13.3,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            }
        }

        // "of the" — three words, each with ~2px word gaps
        let items = vec![
            char_item("o", 100.0, 4.0),
            char_item("f", 104.0, 2.7),
            // word gap: 108.7 → 110.7 (gap = 4.0)
            char_item("t", 110.7, 2.7),
            char_item("h", 113.4, 4.4),
            char_item("e", 117.8, 3.5),
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "of the");
    }

    #[test]
    fn test_cjk_items_join_without_spaces() {
        // Japanese text items touching at gap=0 should join without spaces
        let items = vec![
            TextItem {
                text: "である".into(),
                x: 100.0,
                y: 500.0,
                width: 24.0,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "履行義務".into(),
                x: 124.0,
                y: 500.0,
                width: 32.0,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "を識別す".into(),
                x: 156.0,
                y: 500.0,
                width: 32.0,
                height: 12.0,
                font: "C2_0".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];

        let lines = group_into_lines(items);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "である履行義務を識別す");
    }

    fn make_item(text: &str, x: f32, y: f32, width: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width,
            height: 12.0,
            font: "F1".into(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn test_detect_two_columns() {
        let mut items = Vec::new();
        // Left column at x=72, right column at x=350, gutter ~278-350
        for i in 0..30 {
            let y = 700.0 - (i as f32) * 14.0;
            items.push(make_item("Left text here", 72.0, y, 200.0));
            items.push(make_item("Right text here", 350.0, y, 200.0));
        }
        let cols = detect_columns(&items, 1, false);
        assert_eq!(cols.len(), 2, "Expected 2 columns, got {:?}", cols);
        assert!(cols[0].x_min < cols[1].x_min);
    }

    #[test]
    fn test_detect_three_columns() {
        let mut items = Vec::new();
        // Three columns at x=50, x=220, x=390
        for i in 0..30 {
            let y = 700.0 - (i as f32) * 14.0;
            items.push(make_item("Col one", 50.0, y, 140.0));
            items.push(make_item("Col two", 220.0, y, 140.0));
            items.push(make_item("Col three", 390.0, y, 140.0));
        }
        let cols = detect_columns(&items, 1, false);
        assert_eq!(cols.len(), 3, "Expected 3 columns, got {:?}", cols);
    }

    #[test]
    fn test_width_bleed_tolerance() {
        let mut items = Vec::new();
        // Two columns with a clear gutter
        for i in 0..30 {
            let y = 700.0 - (i as f32) * 14.0;
            items.push(make_item("Left text", 72.0, y, 200.0));
            items.push(make_item("Right text", 350.0, y, 200.0));
        }
        // Add a few items that bleed across the gutter
        for i in 0..3 {
            let y = 700.0 - (i as f32) * 14.0;
            items.push(make_item("wide", 72.0, y, 320.0));
        }
        let cols = detect_columns(&items, 1, false);
        assert!(
            cols.len() >= 2,
            "Width bleed should not prevent column detection, got {:?}",
            cols
        );
    }

    #[test]
    fn test_single_column_no_false_split() {
        let mut items = Vec::new();
        // Single column: items spanning full width
        for i in 0..30 {
            let y = 700.0 - (i as f32) * 14.0;
            items.push(make_item(
                "This is a full-width paragraph of text",
                72.0,
                y,
                468.0,
            ));
        }
        let cols = detect_columns(&items, 1, false);
        assert!(
            cols.len() <= 1,
            "Full-width text should not be split into columns, got {:?}",
            cols
        );
    }

    #[test]
    fn test_is_rtl_char() {
        // Hebrew alef
        assert!(is_rtl_char('\u{05D0}'));
        // Arabic alif
        assert!(is_rtl_char('\u{0627}'));
        // Latin 'A' is not RTL
        assert!(!is_rtl_char('A'));
        // CJK is not RTL
        assert!(!is_rtl_char('\u{4E00}'));
    }

    #[test]
    fn test_is_rtl_text() {
        // Majority Hebrew with digits → RTL
        assert!(is_rtl_text(["\u{05E9}\u{05DC}\u{05D5}\u{05DD} 123"].iter()));
        // Majority Latin → not RTL
        assert!(!is_rtl_text(["Hello world"].iter()));
        // Empty → not RTL
        assert!(!is_rtl_text(std::iter::empty::<&str>()));
    }

    #[test]
    fn test_rtl_line_sorting() {
        let mut items = vec![
            TextItem {
                text: "\u{05D0}".into(), // alef at x=100
                x: 100.0,
                y: 700.0,
                width: 10.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "\u{05D1}".into(), // bet at x=200 (rightmost)
                x: 200.0,
                y: 700.0,
                width: 10.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];
        sort_line_items(&mut items);
        // RTL: rightmost (higher X) comes first
        assert_eq!(items[0].x, 200.0);
        assert_eq!(items[1].x, 100.0);
    }

    #[test]
    fn test_ltr_unaffected() {
        let mut items = vec![
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
                mcid: None,
            },
            TextItem {
                text: "World".into(),
                x: 200.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];
        sort_line_items(&mut items);
        // LTR: leftmost comes first
        assert_eq!(items[0].x, 100.0);
        assert_eq!(items[1].x, 200.0);
    }

    #[test]
    fn test_hangul_is_cjk() {
        // Hangul Jamo
        assert!(is_cjk_char('\u{1100}'));
        // Hangul Compatibility Jamo
        assert!(is_cjk_char('\u{3131}'));
        // Hangul Syllable '가'
        assert!(is_cjk_char('\u{AC00}'));
        // Latin is not CJK
        assert!(!is_cjk_char('A'));
    }

    #[test]
    fn test_newspaper_layout_detection() {
        // Two dense columns (>15 lines each) with matching Y positions → newspaper
        let make_line = |y: f32, x: f32, page: u32| TextLine {
            y,
            page,
            adaptive_threshold: 0.10,
            items: vec![TextItem {
                text: "text".into(),
                x,
                y,
                width: 100.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            }],
        };

        let col1: Vec<TextLine> = (0..20)
            .map(|i| make_line(700.0 - i as f32 * 14.0, 50.0, 1))
            .collect();
        let col2: Vec<TextLine> = (0..20)
            .map(|i| make_line(700.0 - i as f32 * 14.0, 350.0, 1))
            .collect();

        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        assert!(is_newspaper_layout(&[col1, col2], &cols));
    }

    #[test]
    fn test_newspaper_layout_misaligned_baselines() {
        // Two dense balanced columns with non-aligned Y positions (e.g. government gazettes
        // where columns are independently typeset) → should still be newspaper
        let make_line = |y: f32, x: f32, page: u32| TextLine {
            y,
            page,
            adaptive_threshold: 0.10,
            items: vec![TextItem {
                text: "text".into(),
                x,
                y,
                width: 100.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            }],
        };

        // Col1 starts at Y=700, col2 starts at Y=685 (15pt offset — no Y-collision)
        let col1: Vec<TextLine> = (0..20)
            .map(|i| make_line(700.0 - i as f32 * 14.0, 50.0, 1))
            .collect();
        let col2: Vec<TextLine> = (0..20)
            .map(|i| make_line(685.0 - i as f32 * 14.0, 350.0, 1))
            .collect();

        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        assert!(is_newspaper_layout(&[col1, col2], &cols));
    }

    #[test]
    fn test_tabular_layout_detection() {
        // Sparse columns (<15 lines) → tabular, not newspaper
        let make_line = |y: f32, x: f32, page: u32| TextLine {
            y,
            page,
            adaptive_threshold: 0.10,
            items: vec![TextItem {
                text: "text".into(),
                x,
                y,
                width: 100.0,
                height: 12.0,
                font: "F1".into(),
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                item_type: ItemType::Text,
                mcid: None,
            }],
        };

        let col1: Vec<TextLine> = (0..5)
            .map(|i| make_line(700.0 - i as f32 * 14.0, 50.0, 1))
            .collect();
        let col2: Vec<TextLine> = (0..5)
            .map(|i| make_line(700.0 - i as f32 * 14.0, 350.0, 1))
            .collect();

        let cols = vec![
            ColumnRegion {
                x_min: 0.0,
                x_max: 300.0,
            },
            ColumnRegion {
                x_min: 300.0,
                x_max: 600.0,
            },
        ];
        assert!(!is_newspaper_layout(&[col1, col2], &cols));
    }

    fn make_item_fs(text: &str, x: f32, y: f32, width: f32, font_size: f32) -> TextItem {
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
    fn test_merge_subscript_items_chemical_formula() {
        // NH₃: "NH" at fs=8 followed by subscript "3" at fs=4.7
        let items = vec![
            make_item_fs("NH", 78.0, 499.0, 12.0, 8.0),
            make_item_fs("3", 90.0, 496.0, 2.3, 4.7),
            make_item_fs("Cl", 100.0, 499.0, 7.0, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "NH3");
        assert_eq!(merged[1].text, "Cl");
    }

    #[test]
    fn test_merge_subscript_items_h2o() {
        // H₂O: "H" then subscript "2" then "O"
        let items = vec![
            make_item_fs("H", 250.0, 499.0, 5.0, 8.0),
            make_item_fs("2", 255.0, 496.0, 2.3, 4.7),
            make_item_fs("O", 257.5, 499.0, 6.0, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "H2");
        assert_eq!(merged[1].text, "O");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_far_gap() {
        // Subscript-sized item that's far from the parent should NOT merge
        let items = vec![
            make_item_fs("Text", 78.0, 499.0, 20.0, 8.0),
            make_item_fs("▶", 120.0, 498.0, 3.0, 3.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "Text");
        assert_eq!(merged[1].text, "▶");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_long_text() {
        // Long subscript-sized text should NOT merge (not a true subscript)
        let items = vec![
            make_item_fs("Title", 78.0, 499.0, 30.0, 8.0),
            make_item_fs("footnote", 108.0, 496.0, 20.0, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_subscript_items_no_merge_same_font_size() {
        // Same font size items should NOT be treated as subscripts
        let items = vec![
            make_item_fs("NH", 78.0, 499.0, 12.0, 8.0),
            make_item_fs("3", 90.0, 496.0, 2.3, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_subscript_items_no_merge_non_numeric() {
        // Non-numeric subscript text (e.g. "sol", "º", "vf") should NOT merge
        let items = vec![
            make_item_fs("∆", 200.0, 639.0, 5.5, 8.0),
            make_item_fs("sol", 205.8, 636.9, 5.7, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "∆");
        assert_eq!(merged[1].text, "sol");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_parent_ends_with_digit() {
        // "33" + "1" in "33 1/3%" — parent ends with digit, should NOT merge
        let items = vec![
            make_item_fs("33", 78.0, 499.0, 10.0, 8.0),
            make_item_fs("1", 88.0, 496.0, 2.3, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "33");
        assert_eq!(merged[1].text, "1");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_parent_ends_with_space() {
        // "Health " + "1" — parent ends with space (table credit), should NOT merge
        let items = vec![
            make_item_fs("Health ", 78.0, 499.0, 30.0, 8.0),
            make_item_fs("1", 108.0, 496.0, 2.3, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }
}
