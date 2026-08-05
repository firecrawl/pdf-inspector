//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

mod base14;
pub(crate) mod content_stream;
mod fonts;
mod layout;
mod links;
mod reading_order;
pub(crate) mod underline;
mod xobjects;

use crate::text_utils::{is_cjk_char, is_rtl_text};
use crate::tounicode::FontCMaps;
use crate::types::{PageExtraction, PdfLine, PdfRect, TextItem};
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
pub(crate) use fonts::FontStyleCache;
pub(crate) use layout::detect_columns;
#[cfg(test)]
use layout::filter_markdown_page_numbers;
pub(crate) use layout::filter_markdown_page_numbers_with_removed_pages;
pub(crate) use layout::group_into_lines_with_thresholds;
pub(crate) use layout::group_prefiltered_items_into_lines_with_thresholds_and_charts;
pub(crate) use layout::group_prefiltered_items_into_lines_with_thresholds_and_regions;
pub(crate) use layout::is_newspaper_layout;
pub(crate) use layout::ColumnRegion;
pub use layout::{group_into_lines, group_into_lines_preserving_all_text};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub(crate) fn trace_text_preview(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

/// Extract text from PDF file as plain string
pub fn extract_text<P: AsRef<Path>>(path: P) -> Result<String, PdfError> {
    crate::validate_pdf_file(&path)?;
    let (doc, _) = crate::load_document_from_path(&path)?;
    extract_text_from_doc(&doc)
}

/// Extract text from PDF memory buffer
pub fn extract_text_mem(buffer: &[u8]) -> Result<String, PdfError> {
    crate::validate_pdf_bytes(buffer)?;
    let (doc, _) = crate::load_document_from_mem(buffer)?;
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
    let (items, _rects, _lines) =
        extract_text_with_positions_and_rects_with_password(path, page_filter, None)?;
    Ok(items)
}

/// Extract text with positions from a file, limited to specific pages and
/// decrypting with `password` when the PDF is encrypted.
///
/// `page_filter` is an optional set of 1-indexed page numbers to process.
/// When `None`, all pages are processed.
pub fn extract_text_with_positions_pages_with_password<P: AsRef<Path>>(
    path: P,
    page_filter: Option<&HashSet<u32>>,
    password: Option<&str>,
) -> Result<Vec<TextItem>, PdfError> {
    let (items, _rects, _lines) =
        extract_text_with_positions_and_rects_with_password(path, page_filter, password)?;
    Ok(items)
}

pub(crate) fn extract_text_with_positions_and_rects_with_password<P: AsRef<Path>>(
    path: P,
    page_filter: Option<&HashSet<u32>>,
    password: Option<&str>,
) -> Result<PageExtraction, PdfError> {
    crate::validate_pdf_file(&path)?;
    let (doc, _) = crate::load_document_from_path_with_password(&path, password)?;
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
    let (doc, _) = crate::load_document_from_mem(buffer)?;
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
    extract_positioned_text_impl(doc, font_cmaps, page_filter, false, None)
}

/// Extract selected pages and gather document-wide folio evidence only when a
/// selected page contains an ambiguous contextual page-edge number. Errors on
/// selected pages remain fatal; errors on context-only pages are skipped.
pub(crate) fn extract_positioned_text_with_folio_context(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    extract_positioned_text_with_folio_context_impl(doc, font_cmaps, page_filter, false)
}

/// Invisible-text variant of [`extract_positioned_text_with_folio_context`].
pub(crate) fn extract_positioned_text_include_invisible_with_folio_context(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    extract_positioned_text_with_folio_context_impl(doc, font_cmaps, page_filter, true)
}

fn extract_positioned_text_with_folio_context_impl(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    let Some(required_pages) = page_filter else {
        return extract_positioned_text_impl(doc, font_cmaps, None, include_invisible, None);
    };

    let (
        (mut selected_items, mut selected_rects, mut selected_lines),
        mut page_thresholds,
        mut gid_encoded_pages,
    ) = extract_positioned_text_impl(
        doc,
        font_cmaps,
        Some(required_pages),
        include_invisible,
        None,
    )?;
    if !layout::needs_document_page_number_context(&selected_items, doc.get_pages().len()) {
        return Ok((
            (selected_items, selected_rects, selected_lines),
            page_thresholds,
            gid_encoded_pages,
        ));
    }

    let context_pages: HashSet<u32> = doc
        .get_pages()
        .keys()
        .copied()
        .filter(|page| !required_pages.contains(page))
        .collect();
    let ((context_items, context_rects, context_lines), context_thresholds, context_gid_pages) =
        extract_positioned_text_impl(
            doc,
            font_cmaps,
            Some(&context_pages),
            include_invisible,
            Some(required_pages),
        )?;
    selected_items.extend(context_items);
    selected_rects.extend(context_rects);
    selected_lines.extend(context_lines);
    page_thresholds.extend(context_thresholds);
    gid_encoded_pages.extend(context_gid_pages);
    Ok((
        (selected_items, selected_rects, selected_lines),
        page_thresholds,
        gid_encoded_pages,
    ))
}

/// Extract all pages for document-wide analysis while allowing malformed
/// unselected pages to be skipped. Any requested page still fails normally.
pub(crate) fn extract_positioned_text_for_document_analysis(
    doc: &Document,
    font_cmaps: &FontCMaps,
    required_pages: &HashSet<u32>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    extract_positioned_text_impl(doc, font_cmaps, None, false, Some(required_pages))
}

fn extract_positioned_text_impl(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
    required_pages: Option<&HashSet<u32>>,
) -> Result<(PageExtraction, PageThresholds, HashSet<u32>), PdfError> {
    let pages = doc.get_pages();
    let mut all_items = Vec::new();
    let mut all_rects = Vec::new();
    let mut all_lines = Vec::new();
    let mut page_thresholds: PageThresholds = HashMap::new();
    let mut gid_encoded_pages: HashSet<u32> = HashSet::new();
    // Embedded-font style flags are document-scoped: the same font program
    // is shared across pages, so parse it once, not once per page.
    let mut style_cache = FontStyleCache::new();

    // Build page ObjectId → page number map for form field extraction
    let page_id_to_num: HashMap<ObjectId, u32> =
        pages.iter().map(|(num, &id)| (id, *num)).collect();

    for (page_num, &page_id) in pages.iter() {
        if let Some(filter) = page_filter {
            if !filter.contains(page_num) {
                continue;
            }
        }
        let page_result = extract_page_text_items(
            doc,
            page_id,
            *page_num,
            font_cmaps,
            include_invisible,
            &mut style_cache,
        );
        let ((mut items, mut rects, mut lines), has_gid_fonts, coords_rotated) = match page_result {
            Ok(extraction) => extraction,
            Err(error) if required_pages.is_some_and(|required| !required.contains(page_num)) => {
                debug!(
                    "page {}: skipping context-only extraction error: {}",
                    page_num, error
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        // Clip to the visible page box: single-page extracts and imposed
        // spreads keep neighboring pages' content in the stream, positioned
        // outside the CropBox. Extracting it interleaves invisible text into
        // the page and poisons font statistics. Rotated pages are left alone
        // — their item coordinates are already transformed out of box space.
        let mut clipped_box: Option<(f32, f32, f32, f32)> = None;
        if !coords_rotated {
            if let Some((bx0, by0, bx1, by1)) = get_page_box(doc, page_id) {
                const TOL: f32 = 6.0;
                let outside = |it: &TextItem| {
                    let cx = it.x + it.width / 2.0;
                    !(cx >= bx0 - TOL && cx <= bx1 + TOL && it.y >= by0 - TOL && it.y <= by1 + TOL)
                };
                // Only clip when the off-page material reads as coherent text
                // (neighboring-page paragraphs). Curved/rotated display text
                // leaves short glyph fragments with artifact coordinates
                // outside the box, and those must stay.
                let off: Vec<&TextItem> = items.iter().filter(|it| outside(it)).collect();
                // Judge by character mass: paragraphs are dominated by long
                // word runs even when interleaved with short math fragments,
                // while glyph-confetti is short items through and through.
                let total_chars: usize = off.iter().map(|it| it.text.trim().chars().count()).sum();
                let wordy_chars: usize = off
                    .iter()
                    .map(|it| it.text.trim().chars().count())
                    .filter(|&n| n >= 4)
                    .sum();
                // Genuine neighboring-page content is cleanly separated from
                // on-page text. When an off-page item continues an on-page
                // line (same baseline, near-adjacent x), the coordinates are
                // artifacts of transforms we mis-model — don't clip those.
                let straddles = off.iter().any(|o| {
                    items.iter().any(|i| {
                        !outside(i)
                            && (i.y - o.y).abs() <= 2.0
                            && (o.x - (i.x + i.width)).abs() <= 10.0
                    })
                });
                let coherent =
                    off.len() >= 10 && wordy_chars * 2 >= total_chars.max(1) && !straddles;
                if bx1 - bx0 >= 72.0 && by1 - by0 >= 72.0 && coherent {
                    let before = items.len();
                    items.retain(|it| !outside(it));
                    if items.len() < before {
                        debug!(
                            "page {}: clipped {} items outside page box ({:.0},{:.0})-({:.0},{:.0})",
                            page_num,
                            before - items.len(),
                            bx0,
                            by0,
                            bx1,
                            by1
                        );
                        // Only prune off-page geometry when off-page text
                        // existed — same neighboring-page content.
                        let overlaps = |x: f32, y: f32, w: f32, h: f32| {
                            let (x0, x1) = if w < 0.0 { (x + w, x) } else { (x, x + w) };
                            let (y0, y1) = if h < 0.0 { (y + h, y) } else { (y, y + h) };
                            x0 < bx1 + TOL && x1 > bx0 - TOL && y0 < by1 + TOL && y1 > by0 - TOL
                        };
                        rects.retain(|r| overlaps(r.x, r.y, r.width, r.height));
                        clipped_box = Some((bx0, by0, bx1, by1));
                        lines.retain(|l| {
                            overlaps(
                                l.x1.min(l.x2),
                                l.y1.min(l.y2),
                                (l.x2 - l.x1).abs(),
                                (l.y2 - l.y1).abs(),
                            )
                        });
                    }
                }
            }
        }
        if has_gid_fonts {
            gid_encoded_pages.insert(*page_num);
        }
        let threshold = crate::text_utils::fix_letterspaced_items(&mut items);
        if threshold > 0.10 {
            page_thresholds.insert(*page_num, threshold);
        }
        suppress_table_underlines(&mut items, &rects, &lines, *page_num);
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
                    trace_text_preview(&item.text, 80)
                );
            }
        }
        all_items.extend(items);
        all_rects.extend(rects);
        all_lines.extend(lines);

        // Extract hyperlinks from page annotations
        let mut links = extract_page_links(doc, page_id, *page_num);
        // Annotations from the neighboring page are off-box too.
        if let Some((bx0, by0, bx1, by1)) = clipped_box {
            links.retain(|it| {
                let cx = it.x + it.width / 2.0;
                // Center-y, not it.y: link items carry an annotation rect,
                // so y is a box edge — unlike text items, where y is a
                // baseline and testing it directly is the natural semantics.
                let cy = it.y + it.height / 2.0;
                cx >= bx0 - 6.0 && cx <= bx1 + 6.0 && cy >= by0 - 6.0 && cy <= by1 + 6.0
            });
        }
        all_items.extend(links);
    }

    // Extract AcroForm field values
    let form_items = extract_form_fields(doc, &page_id_to_num)
        .into_iter()
        .filter(|item| page_filter.is_none_or(|filter| filter.contains(&item.page)));
    all_items.extend(form_items);

    Ok((
        (all_items, all_rects, all_lines),
        page_thresholds,
        gid_encoded_pages,
    ))
}

fn suppress_table_underlines(
    items: &mut [TextItem],
    rects: &[PdfRect],
    lines: &[PdfLine],
    page: u32,
) {
    if !items
        .iter()
        .any(|item| item.is_underline || item.is_strikeout)
    {
        return;
    }

    let mut table_item_indices: HashSet<usize> = HashSet::new();
    // A detected "table" that swallows nearly every text item on the page
    // is a detection artifact (prose pages with boxed callouts or stacked
    // underline rules read as one giant grid), not a real table — letting
    // it through here erased every legitimate underline on the page
    // (text_dense__underline: rect detection claimed 52/52 items). Real
    // ruled tables share the page with headings, captions, and body text.
    let plausible = |table: &crate::tables::Table| {
        // Content sanity gate: prose pages with boxed callouts and stacked
        // underline rules can detect as a structurally rich "table" that
        // swallows every item on the page (text_dense__underline: a 4x8
        // grid claiming 52/52 items, one "cell" holding 806 chars of body
        // text) — suppressing there erased every legitimate underline on
        // the page. Real data-table cells are short values; a cell with
        // hundreds of characters means the grid captured flowing prose.
        let lens: Vec<usize> = table
            .cells
            .iter()
            .flatten()
            .filter(|cell| !cell.trim().is_empty())
            .map(|cell| cell.chars().count())
            .collect();
        if lens.is_empty() {
            return false;
        }
        let long = lens.iter().filter(|&&n| n > 100).count();
        (long as f32) < (lens.len() as f32) * 0.3
    };

    if !rects.is_empty() {
        let (rect_tables, _) = crate::tables::detect_tables_from_rects(items, rects, page);
        for table in rect_tables.iter().filter(|t| plausible(t)) {
            table_item_indices.extend(table.item_indices.iter().copied());
        }
    }

    if !lines.is_empty() {
        for table in crate::tables::detect_tables_from_lines(items, lines, page)
            .iter()
            .filter(|t| plausible(t))
        {
            table_item_indices.extend(table.item_indices.iter().copied());
        }
    }

    for index in table_item_indices {
        if let Some(item) = items.get_mut(index) {
            item.is_underline = false;
            item.is_strikeout = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (used by submodules via `super::`)
// ---------------------------------------------------------------------------

/// Return true when this item should participate in text-layout
/// heuristics (column detection, table grid detection, line grouping).
///
/// Image XObjects emit a positional placeholder via
/// `extract_text_with_positions` (so layout-aware callers can crop +
/// caption figures), but their bboxes don't carry text glyphs and would
/// skew column/row clustering if they reached the heuristics. Hyperlinks
/// and form fields *do* participate — the existing logic treats them as
/// text-like and we keep that.
pub(crate) fn is_text_layout_item(item: &crate::types::TextItem) -> bool {
    !matches!(item.item_type, crate::types::ItemType::Image)
}

/// Map a (u, v) point in unit-square coordinates through the 6-element CTM
/// to page-space. CTM format is `[a, b, c, d, e, f]` per
/// [`multiply_matrices`].
fn apply_ctm_point(ctm: &[f32; 6], u: f32, v: f32) -> (f32, f32) {
    (
        u * ctm[0] + v * ctm[2] + ctm[4],
        u * ctm[1] + v * ctm[3] + ctm[5],
    )
}

/// Compute the page-space axis-aligned bounding box of an Image XObject
/// invoked under the given CTM.
///
/// Per the PDF spec, an image XObject is always rendered into a unit
/// square `(0,0)–(1,1)` in its local coordinate system, and the `Do`
/// operator applies the current CTM to position/scale/rotate that square
/// onto the page. For the common axis-aligned case (no rotation/shear),
/// the CTM reduces to `[w, 0, 0, h, x, y]` and the bbox is just
/// `(x, y, w, h)`. For rotated/sheared images we transform all four
/// corners and return their axis-aligned bbox so the caller always gets
/// an upright rectangle.
///
/// Coordinates are PDF user space (origin at bottom-left, y-up). Width
/// and height are non-negative.
pub(crate) fn image_bbox_from_ctm(ctm: &[f32; 6]) -> (f32, f32, f32, f32) {
    let corners = [
        apply_ctm_point(ctm, 0.0, 0.0),
        apply_ctm_point(ctm, 1.0, 0.0),
        apply_ctm_point(ctm, 1.0, 1.0),
        apply_ctm_point(ctm, 0.0, 1.0),
    ];
    let (mut x_min, mut x_max) = (corners[0].0, corners[0].0);
    let (mut y_min, mut y_max) = (corners[0].1, corners[0].1);
    for (cx, cy) in corners.iter().skip(1) {
        if *cx < x_min {
            x_min = *cx;
        }
        if *cx > x_max {
            x_max = *cx;
        }
        if *cy < y_min {
            y_min = *cy;
        }
        if *cy > y_max {
            y_max = *cy;
        }
    }
    (x_min, y_min, x_max - x_min, y_max - y_min)
}

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

fn is_standalone_bullet_text(text: &str) -> bool {
    matches!(text.trim(), "•" | "○" | "●" | "◦")
}

fn first_text_char(text: &str) -> Option<char> {
    text.trim_start().chars().next()
}

fn is_short_alpha_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    (1..=4).contains(&char_count) && trimmed.chars().all(char::is_alphabetic)
}

fn has_phrase_continuation_shape(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed
        .chars()
        .take(24)
        .any(|ch| ch.is_whitespace() || matches!(ch, '-'))
}

fn should_preserve_overlapping_stream_order(group: &[&TextItem]) -> bool {
    if group.len() < 3 {
        return false;
    }

    let Some(first) = group.iter().find(|item| !item.text.trim().is_empty()) else {
        return false;
    };
    if group.iter().all(|item| item.mcid.is_none()) {
        return false;
    }

    let mut nonempty_count = 0;
    let mut saw_backtrack = false;
    let mut nonspace_chars = 0;
    let mut math_symbol_chars = 0;
    let mut max_font_size = first.font_size;

    for item in group {
        if !item.text.trim().is_empty() {
            nonempty_count += 1;
        }
        if (item.font_size - first.font_size).abs() > first.font_size * 0.25 {
            return false;
        }
        max_font_size = max_font_size.max(item.font_size);
        for ch in item.text.chars().filter(|ch| !ch.is_whitespace()) {
            nonspace_chars += 1;
            if matches!(
                ch,
                '*' | 'ˆ' | '^' | '=' | '+' | '_' | '[' | ']' | '{' | '}' | '|' | '<' | '>'
            ) {
                math_symbol_chars += 1;
            }
        }
    }

    if nonempty_count < 2 {
        return false;
    }
    if nonspace_chars > 0 && math_symbol_chars * 4 > nonspace_chars {
        return false;
    }

    let mut sorted_by_x = group.to_vec();
    sorted_by_x.sort_by(|a, b| a.x.total_cmp(&b.x));
    let cluster_start = sorted_by_x[0].x;
    let mut cluster_end = cluster_start + effective_merge_width(sorted_by_x[0]);
    for item in sorted_by_x.iter().skip(1) {
        let gap = item.x - cluster_end;
        if gap > max_font_size * 2.5 {
            return false;
        }
        cluster_end = cluster_end.max(item.x + effective_merge_width(item));
    }
    if cluster_end - cluster_start > max_font_size * 36.0 {
        return false;
    }

    for index in 0..group.len() - 1 {
        let previous = group[index];
        let next = group[index + 1];
        let font_size = previous.font_size.max(next.font_size);
        let backtrack_threshold = font_size * 0.25;
        let previous_start = previous.x;
        let next_start = next.x;
        let next_end = next.x + effective_merge_width(next);
        if next_start < previous_start - backtrack_threshold
            && next_end > previous_start + backtrack_threshold
        {
            let has_near_prefix = group[..=index].iter().rev().take(4).any(|item| {
                is_short_alpha_fragment(&item.text)
                    && item.x >= next_start - font_size * 0.5
                    && item.x <= next_start + font_size * 4.0
            });
            let starts_lowercase = first_text_char(&next.text).is_some_and(char::is_lowercase);
            let phrase_continuation = has_phrase_continuation_shape(&next.text);
            let has_near_bullet = group[..=index]
                .iter()
                .position(|item| {
                    is_standalone_bullet_text(&item.text) && next_start <= item.x + font_size * 3.0
                })
                .is_some_and(|bullet_index| {
                    if bullet_index >= index {
                        return false;
                    }
                    group[bullet_index + 1..=index]
                        .iter()
                        .rev()
                        .find(|item| !item.text.trim().is_empty())
                        .is_some_and(|item| {
                            item.text.trim().chars().count() <= 8
                                && has_phrase_continuation_shape(&next.text)
                        })
                });
            if (has_near_prefix && starts_lowercase && phrase_continuation) || has_near_bullet {
                saw_backtrack = true;
                break;
            }
        }
    }

    saw_backtrack
}

/// Detect a tracked (letter-spaced) run of single-glyph items and derive its
/// run-local space floor.
///
/// Display type set with tracking renders one glyph per show op; the merge
/// loop's fixed thresholds (0.08-0.13 em) then read every letter gap as a
/// word boundary and emit "H O W" instead of "HOW". Within such a run the
/// gaps carry the real signal: letter gaps cluster tightly just above the
/// fixed threshold, word gaps sit clearly higher. Returns (run_end_index,
/// space_floor) when the run starting at `start` is tracked — spaces are
/// then inserted only at gaps above the floor (infinity = single word).
/// Normal text (multi-char items, or single-char runs with sub-threshold
/// gaps) returns None and keeps the existing behavior.
/// Han/Kana scripts write without inter-word spaces. Hangul (Korean) DOES
/// space between words and deliberately stays out of this set — a Korean
/// tracked run keeps normal word-boundary handling.
fn is_spaceless_cjk(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303F}'   // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{FF00}'..='\u{FFEF}' // Halfwidth and Fullwidth Forms
    )
}

fn tracked_run_space_floor(group: &[&TextItem], start: usize) -> Option<(usize, f32)> {
    const MIN_GAPS: usize = 4;
    let first = group[start];
    if first.text.trim().chars().count() != 1 {
        return None;
    }
    let fs = first.font_size;
    if fs <= 0.0 {
        return None;
    }

    // Walk the run under the SAME break conditions as the merge loop
    // (size band, style equality, mergeable gap) so indices stay aligned.
    let mut gaps: Vec<f32> = Vec::new();
    let mut end_x = first.x + effective_merge_width(first);
    let mut end = start;
    for (offset, next) in group[start + 1..].iter().enumerate() {
        if next.text.trim().chars().count() != 1 {
            break;
        }
        if (next.font_size - fs).abs() > fs * 0.20 {
            break;
        }
        if next.is_bold != first.is_bold
            || next.is_italic != first.is_italic
            || next.is_underline != first.is_underline
            || next.is_strikeout != first.is_strikeout
        {
            break;
        }
        let gap = next.x - end_x;
        if gap > fs * 0.5 || gap < -fs * 0.5 {
            break;
        }
        gaps.push(gap / fs);
        end_x = next.x + effective_merge_width(next);
        end = start + 1 + offset;
    }
    if gaps.len() < 2 {
        return None;
    }

    // Tracked signature: the run's TYPICAL gap clears the fixed space
    // threshold (0.08) — the merge loop would break almost every letter
    // pair into "words". Short runs (2-3 gaps: "H O W") demand a stricter
    // shape — clearly wide, uniform, ALL-CAPS — because a genuine spaced
    // sequence of single letters ("x y z" variables) has the same gap
    // count; display tracking is a caps convention.
    let mut sorted = gaps.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted[sorted.len() / 2];
    // Typographic convention gate, both tiers: display tracking is an
    // all-caps convention, and Han/Kana never space between glyphs. Mixed-
    // or lowercase Latin runs keep their boundaries because geometry alone
    // cannot distinguish spaced singles ("A b c d e") from a tracked
    // title-case word ("B u f f a l o").
    let run_chars = || {
        group[start..=end]
            .iter()
            .flat_map(|it| it.text.trim().chars())
    };
    let spaceless_cjk = run_chars().all(|c| is_spaceless_cjk(c) || !c.is_alphanumeric())
        && run_chars().any(is_spaceless_cjk);
    let all_caps = run_chars().all(|c| c.is_uppercase() || is_cjk_char(c) || !c.is_alphabetic());
    if !(spaceless_cjk || all_caps) {
        return None;
    }

    if gaps.len() >= MIN_GAPS {
        if median <= 0.075 {
            return None;
        }
    } else {
        let uniform = sorted[sorted.len() - 1] <= sorted[0].max(0.01) * 1.4;
        if median < 0.09 || !uniform {
            return None;
        }
    }

    // Han/Kana: no inter-glyph spaces, period — a nonuniform gap
    // distribution (punctuation spacing, justification) must not
    // manufacture word boundaries.
    if spaceless_cjk {
        return Some((end, f32::INFINITY));
    }

    // Word gaps, if present, form a second mode above the letter-gap
    // cluster: split at the largest relative jump. Unimodal → one word.
    let mut best_jump = 1.0f32;
    let mut floor = f32::INFINITY;
    for pair in sorted.windows(2) {
        let (lo, hi) = (pair[0].max(0.01), pair[1].max(0.01));
        let jump = hi / lo;
        if jump > best_jump {
            best_jump = jump;
            floor = (lo + hi) / 2.0;
        }
    }
    if best_jump < 1.4 {
        floor = f32::INFINITY;
    }
    Some((end, floor * fs))
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

    let mut ordered_line_groups: Vec<(u32, f32, Vec<&TextItem>, bool)> = Vec::new();

    // Sort each group by X position (direction-aware), except for lines whose
    // content stream intentionally backtracks to overlay ActualText fragments.
    for (page, y, mut group) in line_groups {
        let rtl = is_rtl_text(group.iter().map(|i| &i.text));
        let preserve_stream_order = !rtl && should_preserve_overlapping_stream_order(&group);
        if rtl {
            group.sort_by(|a, b| b.x.total_cmp(&a.x));
        } else if !preserve_stream_order {
            group.sort_by(|a, b| a.x.total_cmp(&b.x));
        }
        ordered_line_groups.push((page, y, group, preserve_stream_order));
    }

    // Sort groups by page then Y descending (top of page first)
    ordered_line_groups.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.total_cmp(&a.1)));

    let mut merged = Vec::new();

    for (_, _, group, preserve_stream_order) in &ordered_line_groups {
        let mut i = 0;
        while i < group.len() {
            let first = group[i];
            let mut text = first.text.clone();
            let mut end_x = first.x + effective_merge_width(first);

            // Tracked display text: run-local space floor overrides the
            // fixed thresholds for this run's junctions (see helper).
            let tracked = if *preserve_stream_order {
                None
            } else {
                tracked_run_space_floor(group, i)
            };

            let mut j = i + 1;
            while j < group.len() {
                let next = group[j];
                // Must be similar font size (within 20%)
                if (next.font_size - first.font_size).abs() > first.font_size * 0.20 {
                    break;
                }
                // Never merge across style boundaries: the merged item
                // carries `first`'s flags, so absorbing a styled run into a
                // plain neighbor (or vice versa) silently erases the styling
                // that markdown emission and downstream inline-styling need —
                // and OR-ing underline instead would stretch `<u>` spans over
                // neighboring plain text.
                if next.is_bold != first.is_bold
                    || next.is_italic != first.is_italic
                    || next.is_underline != first.is_underline
                    || next.is_strikeout != first.is_strikeout
                {
                    break;
                }
                let gap = next.x - end_x;
                let x_gap_max = if *preserve_stream_order && is_standalone_bullet_text(&text) {
                    first.font_size * 1.2
                } else {
                    first.font_size * 0.5
                };
                if gap > x_gap_max {
                    break;
                }
                if gap < -first.font_size * 0.5 && !preserve_stream_order {
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
                let needs_bullet_space = *preserve_stream_order
                    && is_standalone_bullet_text(&text)
                    && !next.text.trim().is_empty();
                let effective_threshold = match tracked {
                    Some((run_end, floor)) if j <= run_end => floor,
                    _ => threshold,
                };
                if needs_bullet_space || gap > effective_threshold {
                    text.push(' ');
                }
                text.push_str(&next.text);
                let next_end = next.x + effective_merge_width(next);
                end_x = if *preserve_stream_order {
                    end_x.max(next_end)
                } else {
                    next_end
                };
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
                is_underline: first.is_underline,
                is_strikeout: first.is_strikeout,
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
                    // Strikeout boundaries block the merge (a struck word
                    // must not extend its strike over a live footnote digit,
                    // and a struck digit must not lose its own mark). An
                    // underlined parent with an unmarked digit DOES merge:
                    // the drawn rule easily misses the tiny digit's overlap
                    // window, and refusing costs the whole subscript token
                    // ("b"+"2" staying split). Visually the rule spans both.
                    let marks_ok = parent.is_strikeout == item.is_strikeout
                        && (parent.is_underline == item.is_underline
                            || (parent.is_underline && !item.is_underline));
                    if parent.font_size >= sub_threshold && ends_with_letter && marks_ok {
                        let parent_right = parent.x + parent.width;
                        let gap = item.x - parent_right;
                        // Subscripts must be tightly adjacent (within ~1pt)
                        if gap < parent.font_size * 0.2 && gap > -parent.font_size * 0.3 {
                            // Preserve the script when absorbing it: map the
                            // digits to Unicode sub/superscript forms so the
                            // raised/lowered rendering survives in extracted
                            // text ("H"+"2" → "H₂", "word"+"2" → "word²").
                            // NFKC/NFKD normalization folds these back to
                            // plain digits, so text matching downstream is
                            // unaffected. Direction from the baseline offset
                            // (y-up here): raised → superscript (footnote
                            // refs), lowered/level → subscript (chemistry).
                            let raised = item.y > parent.y + parent.font_size * 0.1;
                            parent.text.push_str(&map_script_digits(&item.text, raised));
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

/// Map ASCII digits to their Unicode superscript (`raised`) or subscript
/// forms. Callers guarantee digit-only input (see `merge_subscript_items`);
/// anything else passes through unchanged.
fn map_script_digits(text: &str, raised: bool) -> String {
    const SUP: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    const SUB: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
    text.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) if raised => SUP[d as usize],
            Some(d) => SUB[d as usize],
            None => c,
        })
        .collect()
}

/// Helper to get f32 from Object
pub(crate) fn get_number(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// Visible page box: CropBox if present, else MediaBox, walking page-tree
/// inheritance (both attributes are inheritable). Returns normalized
/// (x0, y0, x1, y1) in PDF space.
fn get_page_box(doc: &Document, page_id: ObjectId) -> Option<(f32, f32, f32, f32)> {
    fn find_box(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Vec<f32>> {
        let mut id = page_id;
        for _ in 0..32 {
            let dict = doc.get_dictionary(id).ok()?;
            if let Ok(obj) = dict.get(key) {
                let arr = match obj {
                    Object::Array(a) => Some(a.clone()),
                    Object::Reference(r) => match doc.get_object(*r) {
                        Ok(Object::Array(a)) => Some(a.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(arr) = arr {
                    let vals: Vec<f32> = arr.iter().filter_map(get_number).collect();
                    if vals.len() >= 4 {
                        return Some(vals);
                    }
                }
            }
            match dict.get(b"Parent") {
                Ok(Object::Reference(p)) => id = *p,
                _ => return None,
            }
        }
        None
    }
    let v = find_box(doc, page_id, b"CropBox").or_else(|| find_box(doc, page_id, b"MediaBox"))?;
    Some((
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_utils::{is_cjk_char, is_rtl_char, is_rtl_text, sort_line_items};
    use crate::types::{ItemType, PdfLine, TextLine};
    use layout::{detect_columns, is_newspaper_layout, ColumnRegion};

    /// Glyph-per-item run at `fs`=12 with the given inter-glyph gap (pt).
    fn glyph_run(chars: &str, start_x: f32, glyph_w: f32, gap: f32) -> Vec<TextItem> {
        let mut x = start_x;
        let mut out = Vec::new();
        for c in chars.chars() {
            out.push(make_merge_item(&c.to_string(), x, glyph_w));
            x += glyph_w + gap;
        }
        out
    }

    #[test]
    fn tracked_caps_run_collapses_to_word() {
        // Display tracking: every letter gap (0.19 em) clears the fixed
        // space threshold — without the run-local floor this reads "H O W".
        let items = glyph_run("HOW", 100.0, 10.0, 2.3);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "HOW");
    }

    #[test]
    fn tracked_run_keeps_word_gaps_bimodal() {
        // Letters at 0.19 em, word gaps at 0.42 em (below the 0.5 em item
        // break): the split must land between the modes. Needs >=4 gaps to
        // enter the bimodal tier — short runs use the strict uniform gate.
        let mut items = glyph_run("ITISOK", 100.0, 8.0, 2.3);
        for i in 2..6 {
            items[i].x += 2.8; // word gap at T|I
        }
        for i in 4..6 {
            items[i].x += 2.8; // word gap at S|O
        }
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "IT IS OK");
    }

    #[test]
    fn lowercase_spaced_singles_stay_words() {
        // "x y z" variables: same gap shape but lowercase — the short-run
        // caps requirement keeps genuine spaced singles apart.
        let items = glyph_run("xyz", 100.0, 6.0, 2.3);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "x y z");
    }

    #[test]
    fn kerned_singles_unaffected() {
        // Tiny kerning gaps never triggered spaces before and still don't.
        let items = glyph_run("WORD", 100.0, 8.0, 0.3);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "WORD");
    }

    #[test]
    fn long_lowercase_spaced_singles_keep_boundaries() {
        // Review: a 5+ single-letter lowercase list has the tracked gap
        // shape at any length — the convention gate must protect it in
        // the >=4-gap tier too.
        let items = glyph_run("abcde", 100.0, 6.0, 2.3);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "a b c d e");
    }

    #[test]
    fn han_run_with_nonuniform_gaps_never_gains_spaces() {
        // Review: a bimodal gap distribution (justification, punctuation
        // spacing) must not manufacture word boundaries in Han text.
        let mut items = glyph_run("北京时事快报", 100.0, 12.0, 1.4);
        for item in items.iter_mut().skip(3) {
            item.x += 3.0; // wide gap after the third glyph
        }
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "北京时事快报");
    }

    #[test]
    fn uppercase_leading_spaced_singles_keep_boundaries() {
        // "A b c d e" is indistinguishable from a title-case tracked word
        // without reliable tracking metadata, so preserve its boundaries.
        let items = glyph_run("Abcde", 100.0, 7.0, 2.3);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "A b c d e");
    }

    #[test]
    fn cjk_glyph_run_collapses_without_spaces() {
        // CJK sets one glyph per item with loose gaps; CJK uses no spaces,
        // and the non-alphabetic run passes the caps gate.
        let items = glyph_run("北京时事", 100.0, 12.0, 1.4);
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "北京时事");
    }

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
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn with_mcid(mut item: TextItem) -> TextItem {
        item.mcid = Some(1);
        item
    }

    fn make_line(x1: f32, y1: f32, x2: f32, y2: f32) -> PdfLine {
        PdfLine {
            x1,
            y1,
            x2,
            y2,
            page: 1,
        }
    }

    #[test]
    fn trace_text_preview_truncates_on_char_boundary() {
        let text = format!("{}{}tail", "a".repeat(79), '\u{FFFD}');
        let preview = trace_text_preview(&text, 80);

        assert_eq!(preview.chars().count(), 80);
        assert!(text.is_char_boundary(preview.len()));
        assert!(preview.ends_with('\u{FFFD}'));
    }

    #[test]
    fn merge_items_breaks_at_style_boundaries() {
        // A styled run adjacent to plain text must stay a separate item —
        // merging would erase the flags (italic) or stretch the span
        // (underline) before markdown emission sees them.
        let mut italic = make_merge_item("emphasis", 150.0, 40.0);
        italic.is_italic = true;
        let mut underlined = make_merge_item("term", 195.0, 20.0);
        underlined.is_underline = true;
        let items = vec![
            make_merge_item("plain lead", 100.0, 48.0),
            italic,
            underlined,
            make_merge_item("plain tail", 218.0, 45.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 4);
        assert!(merged[1].is_italic && !merged[1].is_underline);
        assert!(merged[2].is_underline && !merged[2].is_italic);
        assert!(!merged[3].is_underline && !merged[3].is_italic);
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
    fn merge_items_preserves_underline_from_later_fragment() {
        // Fragments with differing underline stay separate items — OR-merging
        // would stretch the eventual `<u>` span over the plain fragment.
        // Line-level text assembly still joins them without a space (tight
        // gap), so the rendered word is unchanged: `pre<u>fix</u>`.
        let mut items = vec![
            make_merge_item("pre", 100.0, 18.0),
            make_merge_item("fix", 119.0, 18.0),
        ];
        items[1].is_underline = true;

        let merged = merge_text_items(items);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "pre");
        assert!(!merged[0].is_underline);
        assert_eq!(merged[1].text, "fix");
        assert!(merged[1].is_underline);
    }

    #[test]
    fn merge_items_preserves_stream_order_for_backtracking_heading() {
        // Some tagged PDFs emit first-letter ActualText fragments, then reset
        // the text matrix and draw the rest of the word from the line start.
        let items = vec![
            with_mcid(make_merge_item("F", 79.4, 4.5)),
            with_mcid(make_merge_item("r", 83.9, 3.3)),
            with_mcid(make_merge_item("om tables to data-", 79.4, 89.7)),
            with_mcid(make_merge_item("", 168.9, 33.9)),
            with_mcid(make_merge_item("analytics-", 168.9, 75.5)),
            with_mcid(make_merge_item("ready content", 210.5, 60.8)),
        ];

        let merged = merge_text_items(items);

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text,
            "From tables to data-analytics-ready content"
        );
    }

    #[test]
    fn merge_items_preserves_stream_order_for_reset_word_prefix() {
        let items = vec![
            with_mcid(make_merge_item("N", 68.0, 7.0)),
            with_mcid(make_merge_item("e", 75.1, 4.0)),
            with_mcid(make_merge_item("w fields created", 68.0, 82.0)),
        ];

        let merged = merge_text_items(items);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "New fields created");
    }

    #[test]
    fn merge_items_uses_x_order_for_untagged_backtracking_text() {
        let items = vec![
            make_merge_item("N", 68.0, 7.0),
            make_merge_item("e", 75.1, 4.0),
            make_merge_item("w fields created", 68.2, 82.0),
        ];

        let merged = merge_text_items(items);

        let texts: Vec<_> = merged.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, vec!["N", "w fields created", "e"]);
    }

    #[test]
    fn merge_items_preserves_bullet_stream_order_with_backtracking() {
        let items = vec![
            with_mcid(make_merge_item("•", 79.4, 5.0)),
            with_mcid(make_merge_item("The MS", 91.0, 32.6)),
            with_mcid(make_merge_item("A LoS project", 84.4, 70.0)),
        ];

        let merged = merge_text_items(items);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "• The MSA LoS project");
    }

    #[test]
    fn merge_items_keeps_normal_bullet_gap_limit_without_stream_order() {
        let items = vec![
            make_merge_item("•", 79.4, 5.0),
            make_merge_item("Distant item", 91.0, 60.0),
        ];

        let merged = merge_text_items(items);

        let texts: Vec<_> = merged.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, vec!["•", "Distant item"]);
    }

    #[test]
    fn suppress_table_underlines_clears_line_detected_table_items() {
        let mut items = vec![
            make_merge_item("H1", 125.0, 20.0),
            make_merge_item("H2", 225.0, 20.0),
            make_merge_item("A", 125.0, 20.0),
            make_merge_item("B", 225.0, 20.0),
        ];
        items[0].y = 490.0;
        items[1].y = 490.0;
        items[2].y = 470.0;
        items[3].y = 470.0;
        for item in &mut items {
            item.is_underline = true;
            item.is_strikeout = true;
        }
        let lines = vec![
            make_line(100.0, 500.0, 300.0, 500.0),
            make_line(100.0, 480.0, 300.0, 480.0),
            make_line(100.0, 460.0, 300.0, 460.0),
            make_line(100.0, 460.0, 100.0, 500.0),
            make_line(200.0, 460.0, 200.0, 500.0),
            make_line(300.0, 460.0, 300.0, 500.0),
        ];

        suppress_table_underlines(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
        assert!(items.iter().all(|item| !item.is_strikeout));
    }

    #[test]
    fn subscript_digit_with_different_marks_is_not_absorbed() {
        // A struck-out word followed by an unmarked footnote digit: merging
        // would widen the parent's strikeout claim over the digit (and the
        // reverse would drop the digit's own mark). Style boundaries break
        // the merge, as in merge_text_items.
        let mut word = make_merge_item("word", 100.0, 24.0);
        word.font_size = 10.0;
        word.is_strikeout = true;
        let mut digit = make_merge_item("2", 124.5, 4.0);
        digit.font_size = 6.0;
        digit.y = word.y + 3.0;

        let merged = merge_subscript_items(vec![word.clone(), digit.clone()]);
        assert_eq!(merged.len(), 2);

        // Same marks still merge (footnote ref inside the strike).
        digit.is_strikeout = true;
        let merged = merge_subscript_items(vec![word, digit]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].text.starts_with("word"));
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
    fn preserving_all_text_keeps_numeric_page_footer() {
        let mut page_number = make_merge_item("42", 100.0, 12.0);
        page_number.y = 50.0;

        assert!(group_into_lines(vec![page_number.clone()]).is_empty());

        let lines = group_into_lines_preserving_all_text(vec![page_number]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "42");
    }

    #[test]
    fn inline_numeric_run_near_page_edge_is_not_removed() {
        let mut items = vec![
            make_merge_item("Total", 100.0, 30.0),
            make_merge_item("730", 136.0, 18.0),
            make_merge_item("seats", 160.0, 30.0),
        ];
        for item in &mut items {
            item.y = 780.0;
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Total 730 seats");
    }

    #[test]
    fn numeric_page_footer_separated_from_label_is_removed() {
        let mut page_number = make_merge_item("42", 25.0, 12.0);
        page_number.y = 50.0;
        let mut footer_label = make_merge_item("DOCUMENT FOOTER", 60.0, 100.0);
        footer_label.y = 50.0;

        let lines = group_into_lines(vec![page_number, footer_label]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "DOCUMENT FOOTER");
    }

    #[test]
    fn decorative_marker_does_not_contextualize_numeric_page_footer() {
        let mut marker = make_merge_item("•", 19.0, 6.0);
        marker.y = 30.0;
        let mut page_number = make_merge_item("42", 37.0, 10.0);
        page_number.y = 30.0;
        let mut footer_label = make_merge_item("Company report footer", 68.0, 120.0);
        footer_label.y = 30.0;

        let lines = group_into_lines(vec![marker, page_number, footer_label]);

        assert!(lines.iter().all(|line| !line.text().contains("42")));
        assert!(lines
            .iter()
            .any(|line| line.text().contains("Company report footer")));
    }

    #[test]
    fn labeled_page_number_is_removed_in_a_short_document() {
        let mut label = make_merge_item("Page", 25.0, 28.0);
        label.y = 50.0;
        let mut page_number = make_merge_item("42", 57.0, 12.0);
        page_number.y = 50.0;

        let lines = group_into_lines(vec![label, page_number]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Page");
    }

    #[test]
    fn labeled_page_number_with_running_header_suffix_is_removed() {
        let mut items = vec![
            make_merge_item("Page", 25.0, 28.0),
            make_merge_item("42", 57.0, 12.0),
            make_merge_item("of", 73.0, 12.0),
            make_merge_item("100", 89.0, 18.0),
            make_merge_item("Report header", 111.0, 78.0),
        ];
        for item in &mut items {
            item.y = 50.0;
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Report header");
    }

    #[test]
    fn page_of_total_expression_is_removed_without_leaving_fragments() {
        let mut items = vec![
            make_merge_item("Page", 482.0, 27.0),
            make_merge_item("1", 513.0, 6.0),
            make_merge_item("of", 523.0, 10.0),
            make_merge_item("15", 537.0, 12.0),
        ];
        for item in &mut items {
            item.y = 46.0;
        }

        let lines = group_into_lines(items);

        assert!(lines.is_empty());
    }

    #[test]
    fn document_folio_filter_survives_per_page_layout_splitting() {
        let mut items = Vec::new();
        for (page, value) in [(1, "42"), (2, "43"), (3, "44")] {
            let mut label = make_merge_item("Page", 25.0, 28.0);
            label.page = page;
            label.y = 50.0;
            let mut page_number = make_merge_item(value, 57.0, 12.0);
            page_number.page = page;
            page_number.y = 50.0;
            items.extend([label, page_number]);
        }

        let filtered = filter_markdown_page_numbers(items, 3);
        assert!(filtered
            .iter()
            .all(|item| !matches!(item.text.as_str(), "42" | "43" | "44")));
        let mut lines = Vec::new();
        for page in 1..=3 {
            let page_items = filtered
                .iter()
                .filter(|item| item.page == page)
                .cloned()
                .collect();
            lines.extend(
                group_prefiltered_items_into_lines_with_thresholds_and_charts(
                    page_items,
                    &HashMap::new(),
                    &HashSet::new(),
                    &HashMap::new(),
                ),
            );
        }

        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.text() == "Page"));
    }

    #[test]
    fn numeric_only_page_edge_runs_do_not_contextualize_folios() {
        let mut page_number = make_merge_item("42", 25.0, 12.0);
        page_number.y = 50.0;
        let mut long_number = make_merge_item("12345", 43.0, 30.0);
        long_number.y = 50.0;

        let lines = group_into_lines(vec![page_number, long_number]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "12345");
    }

    #[test]
    fn structured_and_dense_numeric_page_edge_runs_are_preserved() {
        let mut list_marker = make_merge_item("11)", 25.0, 18.0);
        list_marker.y = 50.0;
        let mut chapter = make_merge_item("13", 47.0, 12.0);
        chapter.y = 50.0;

        let mut isbn_prefix = make_merge_item("9", 25.0, 6.0);
        isbn_prefix.page = 2;
        isbn_prefix.y = 50.0;
        let mut isbn_mid = make_merge_item("780113", 35.0, 36.0);
        isbn_mid.page = 2;
        isbn_mid.y = 50.0;
        let mut isbn_end = make_merge_item("227426", 75.0, 36.0);
        isbn_end.page = 2;
        isbn_end.y = 50.0;

        let lines = group_into_lines(vec![list_marker, chapter, isbn_prefix, isbn_mid, isbn_end]);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "11) 13");
        assert_eq!(lines[1].text(), "9 780113 227426");
    }

    #[test]
    fn incrementing_numeric_body_column_is_not_treated_as_a_folio() {
        let mut items = Vec::new();
        for (page, value) in [(1, "13"), (2, "14"), (3, "15")] {
            let mut row_number = make_merge_item(value, 72.0, 12.0);
            row_number.page = page;
            row_number.y = 730.0;
            let mut name = make_merge_item("Person", 90.0, 42.0);
            name.page = page;
            name.y = 730.0;
            items.extend([row_number, name]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "13 Person");
        assert_eq!(lines[1].text(), "14 Person");
        assert_eq!(lines[2].text(), "15 Person");
    }

    #[test]
    fn advancing_number_in_repeated_deep_margin_footer_is_removed() {
        let mut items = Vec::new();
        for (page, value) in [(1, "2"), (2, "4"), (3, "6"), (4, "8")] {
            let mut page_number = make_merge_item(value, 25.0, 12.0);
            page_number.page = page;
            page_number.y = 30.0;
            let mut footer = make_merge_item("Company report footer", 41.0, 120.0);
            footer.page = page;
            footer.y = 30.0;
            items.extend([page_number, footer]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 4);
        assert!(lines
            .iter()
            .all(|line| line.text() == "Company report footer"));
    }

    #[test]
    fn repeated_substantive_page_number_prose_is_preserved() {
        let mut items = Vec::new();
        for (page, value) in [(1, "42"), (2, "43"), (3, "44"), (4, "45")] {
            let mut page_label = make_merge_item("Page", 25.0, 28.0);
            page_label.page = page;
            page_label.y = 30.0;
            let mut number = make_merge_item(value, 57.0, 12.0);
            number.page = page;
            number.y = 30.0;
            let mut explanation = make_merge_item("explains the result", 73.0, 108.0);
            explanation.page = page;
            explanation.y = 30.0;
            items.extend([page_label, number, explanation]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 4);
        for (line, value) in lines.iter().zip(["42", "43", "44", "45"]) {
            assert_eq!(line.text(), format!("Page {value} explains the result"));
        }
    }

    #[test]
    fn numeric_candidates_do_not_bridge_lexical_context() {
        let mut report = make_merge_item("Report", 25.0, 40.0);
        report.y = 30.0;
        let mut year = make_merge_item("2026", 69.0, 24.0);
        year.y = 30.0;
        let mut folio = make_merge_item("42", 97.0, 12.0);
        folio.y = 30.0;

        let lines = group_into_lines(vec![report, year, folio]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Report 2026");
    }

    #[test]
    fn centered_folio_delimiters_are_removed_with_the_number() {
        let mut left = make_merge_item("-", 270.0, 6.0);
        left.y = 30.0;
        let mut number = make_merge_item("42", 280.0, 12.0);
        number.y = 30.0;
        let mut right = make_merge_item("-", 296.0, 6.0);
        right.y = 30.0;

        let lines = group_into_lines(vec![left, number, right]);

        assert!(lines.is_empty());
    }

    #[test]
    fn centered_delimiters_inside_substantive_text_are_preserved() {
        let mut items = vec![
            make_merge_item("Result", 240.0, 36.0),
            make_merge_item("-", 280.0, 6.0),
            make_merge_item("42", 290.0, 12.0),
            make_merge_item("-", 306.0, 6.0),
            make_merge_item("approved", 316.0, 48.0),
        ];
        for item in &mut items {
            item.y = 30.0;
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Result-42-approved");
    }

    #[test]
    fn changing_year_in_repeated_deep_margin_header_is_preserved() {
        let mut items = Vec::new();
        for (page, year) in [(1, "2020"), (2, "2021"), (3, "2022"), (4, "2023")] {
            let mut year = make_merge_item(year, 25.0, 24.0);
            year.page = page;
            year.y = 780.0;
            let mut header = make_merge_item("Annual report", 53.0, 78.0);
            header.page = page;
            header.y = 780.0;
            items.extend([year, header]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].text(), "2020 Annual report");
        assert_eq!(lines[3].text(), "2023 Annual report");
    }

    #[test]
    fn sparse_repeated_margin_numbers_do_not_meet_the_folio_evidence_floor() {
        let mut items = Vec::new();
        for page in 1..=4 {
            if page <= 2 {
                let value = if page == 1 { "2" } else { "4" };
                let mut number = make_merge_item(value, 25.0, 12.0);
                number.page = page;
                number.y = 30.0;
                let mut footer = make_merge_item("Company report footer", 41.0, 120.0);
                footer.page = page;
                footer.y = 30.0;
                items.extend([number, footer]);
            } else {
                let mut body = make_merge_item("Body text", 72.0, 54.0);
                body.page = page;
                body.y = 400.0;
                items.push(body);
            }
        }

        let lines = group_into_lines(items);

        assert!(lines
            .iter()
            .any(|line| line.text() == "2 Company report footer"));
        assert!(lines
            .iter()
            .any(|line| line.text() == "4 Company report footer"));
    }

    #[test]
    fn sparse_document_pages_count_toward_repeated_folio_coverage() {
        let mut items = Vec::new();
        for (page, value) in [(1, "1"), (10, "10"), (19, "19"), (28, "28")] {
            let mut number = make_merge_item(value, 25.0, 12.0);
            number.page = page;
            number.y = 30.0;
            let mut footer = make_merge_item("Company report footer", 41.0, 120.0);
            footer.page = page;
            footer.y = 30.0;
            items.extend([number, footer]);
        }

        let lines = group_into_lines(items);

        assert!(lines
            .iter()
            .any(|line| line.text() == "1 Company report footer"));
        assert!(lines
            .iter()
            .any(|line| line.text() == "28 Company report footer"));
    }

    #[test]
    fn trailing_blank_pages_count_toward_repeated_folio_coverage() {
        let mut items = Vec::new();
        for (page, value) in [(1, "1"), (2, "2"), (3, "3"), (4, "4")] {
            let mut number = make_merge_item(value, 25.0, 12.0);
            number.page = page;
            number.y = 30.0;
            let mut footer = make_merge_item("Company report footer", 41.0, 120.0);
            footer.page = page;
            footer.y = 30.0;
            items.extend([number, footer]);
        }

        let filtered = filter_markdown_page_numbers(items, 20);

        assert!(filtered.iter().any(|item| item.text == "1"));
        assert!(filtered.iter().any(|item| item.text == "4"));
    }

    #[test]
    fn prefiltered_contextual_number_survives_layout_partitioning() {
        let mut items = vec![
            make_merge_item("Total", 100.0, 30.0),
            make_merge_item("730", 136.0, 18.0),
            make_merge_item("seats", 160.0, 30.0),
        ];
        for item in &mut items {
            item.y = 780.0;
        }

        let filtered = filter_markdown_page_numbers(items, 1);
        let partitioned_number: Vec<TextItem> = filtered
            .into_iter()
            .filter(|item| item.text == "730")
            .collect();
        let lines = group_prefiltered_items_into_lines_with_thresholds_and_charts(
            partitioned_number,
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "730");
    }

    #[test]
    fn numeric_only_partition_does_not_define_columns() {
        let mut items = Vec::new();
        for row in 0..20 {
            let y = 90.0 - row as f32 * 4.0;
            let mut left = make_merge_item(&(row + 1).to_string(), 50.0, 20.0);
            left.y = y;
            let mut right = make_merge_item(&(row + 101).to_string(), 350.0, 20.0);
            right.y = y;
            items.extend([left, right]);
        }
        assert_eq!(detect_columns(&items, 1, false).len(), 2);

        let lines = group_prefiltered_items_into_lines_with_thresholds_and_charts(
            items,
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        assert_eq!(lines.len(), 20);
        assert!(lines.iter().all(|line| line.items.len() == 2));
    }

    #[test]
    fn separated_content_is_not_treated_as_a_spread_folio_pair() {
        let mut value = make_merge_item("12", 100.0, 12.0);
        value.y = 30.0;
        let mut label = make_merge_item("Total", 116.0, 30.0);
        label.y = 30.0;
        let mut unrelated_number = make_merge_item("13", 300.0, 12.0);
        unrelated_number.y = 30.0;

        let lines = group_into_lines(vec![value, label, unrelated_number]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "12 Total");
    }

    #[test]
    fn repeated_folio_uses_the_full_page_edge_band() {
        let mut items = Vec::new();
        for (page, value) in [(1, "2"), (2, "4"), (3, "6"), (4, "8")] {
            let mut page_number = make_merge_item(value, 25.0, 12.0);
            page_number.page = page;
            page_number.y = 80.0;
            let mut footer = make_merge_item("Company report footer", 41.0, 120.0);
            footer.page = page;
            footer.y = 80.0;
            items.extend([page_number, footer]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 4);
        assert!(lines
            .iter()
            .all(|line| line.text() == "Company report footer"));
    }

    #[test]
    fn contextual_folio_on_facing_page_spread_is_removed() {
        let mut marker = make_merge_item("•", 19.0, 6.0);
        marker.y = 30.0;
        let mut left_folio = make_merge_item("326", 35.0, 17.0);
        left_folio.y = 30.0;
        let mut footer = make_merge_item("Company report footer", 61.0, 120.0);
        footer.y = 30.0;
        let mut right_folio = make_merge_item("327", 1148.0, 17.0);
        right_folio.y = 30.0;

        let lines = group_into_lines(vec![marker, left_folio, footer, right_folio]);

        assert!(lines
            .iter()
            .all(|line| !line.text().contains("326") && !line.text().contains("327")));
        assert!(lines
            .iter()
            .any(|line| line.text().contains("Company report footer")));
    }

    #[test]
    fn contextual_folios_alternating_across_pages_are_removed() {
        let headers = [
            "Letter to shareholders",
            "Corporate governance report",
            "Business environment overview",
            "Consolidated financial statements",
        ];
        let mut items = Vec::new();
        for page in 1..=8 {
            let mut body = make_merge_item("Body text", 50.0, 500.0);
            body.page = page;
            body.y = 400.0;
            items.push(body);

            let mut folio = make_merge_item(&(page + 22).to_string(), 0.0, 14.0);
            folio.page = page;
            folio.y = 780.0;
            if page % 2 == 0 {
                folio.x = 50.0;
                items.push(folio);
            } else {
                let mut header = make_merge_item(headers[(page / 2) as usize], 350.0, 180.0);
                header.page = page;
                header.y = 780.0;
                folio.x = 536.0;
                items.extend([header, folio]);
            }
        }

        let filtered = filter_markdown_page_numbers(items, 8);

        assert!(filtered.iter().all(|item| {
            !matches!(
                item.text.as_str(),
                "23" | "24" | "25" | "26" | "27" | "28" | "29" | "30"
            )
        }));
        assert!(headers
            .iter()
            .all(|header| filtered.iter().any(|item| item.text == *header)));
    }

    #[test]
    fn one_isolated_neighbor_does_not_remove_contextual_number() {
        let mut body_one = make_merge_item("Body text", 50.0, 500.0);
        body_one.y = 400.0;
        let mut label = make_merge_item("Report", 450.0, 70.0);
        label.y = 780.0;
        let mut contextual = make_merge_item("1", 526.0, 7.0);
        contextual.y = 780.0;

        let mut body_two = body_one.clone();
        body_two.page = 2;
        let mut isolated = make_merge_item("2", 50.0, 7.0);
        isolated.page = 2;
        isolated.y = 780.0;

        let filtered =
            filter_markdown_page_numbers(vec![body_one, label, contextual, body_two, isolated], 2);

        assert!(filtered.iter().any(|item| item.text == "Report"));
        assert!(filtered.iter().any(|item| item.text == "1"));
        assert!(filtered.iter().all(|item| item.text != "2"));
    }

    #[test]
    fn narrow_content_span_does_not_establish_adjacent_page_edges() {
        let mut body_one = make_merge_item("Body text", 100.0, 120.0);
        body_one.y = 400.0;
        let mut label = make_merge_item("Report", 170.0, 60.0);
        label.y = 780.0;
        let mut contextual = make_merge_item("1", 235.0, 7.0);
        contextual.y = 780.0;

        let mut body_two = body_one.clone();
        body_two.page = 2;
        let mut isolated_two = make_merge_item("2", 100.0, 7.0);
        isolated_two.page = 2;
        isolated_two.y = 780.0;

        let mut body_four = body_one.clone();
        body_four.page = 4;
        let mut isolated_four = make_merge_item("4", 100.0, 7.0);
        isolated_four.page = 4;
        isolated_four.y = 780.0;

        let filtered = filter_markdown_page_numbers(
            vec![
                body_one,
                label,
                contextual,
                body_two,
                isolated_two,
                body_four,
                isolated_four,
            ],
            4,
        );

        assert!(filtered.iter().any(|item| item.text == "Report"));
        assert!(filtered.iter().any(|item| item.text == "1"));
    }

    #[test]
    fn same_edge_number_on_an_adjacent_page_is_not_folio_evidence() {
        let mut body_one = make_merge_item("Body text", 50.0, 500.0);
        body_one.y = 400.0;
        let mut isolated = make_merge_item("42", 50.0, 14.0);
        isolated.y = 780.0;

        let mut body_two = body_one.clone();
        body_two.page = 2;
        let mut contextual = make_merge_item("43", 50.0, 14.0);
        contextual.page = 2;
        contextual.y = 780.0;
        let mut label = make_merge_item("cases reviewed", 70.0, 90.0);
        label.page = 2;
        label.y = 780.0;

        let filtered =
            filter_markdown_page_numbers(vec![body_one, isolated, body_two, contextual, label], 2);

        assert!(filtered.iter().any(|item| item.text == "43"));
        assert!(filtered.iter().any(|item| item.text == "cases reviewed"));
    }

    #[test]
    fn constant_number_in_repeated_deep_margin_header_is_preserved() {
        let mut items = Vec::new();
        for page in 1..=4 {
            let mut year = make_merge_item("2026", 25.0, 24.0);
            year.page = page;
            year.y = 780.0;
            let mut header = make_merge_item("Annual report", 53.0, 78.0);
            header.page = page;
            header.y = 780.0;
            items.extend([year, header]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|line| line.text() == "2026 Annual report"));
    }

    #[test]
    fn page_number_prefix_does_not_remove_substantive_text_during_layout() {
        let mut items = vec![
            make_merge_item("Page", 25.0, 28.0),
            make_merge_item("42", 57.0, 12.0),
            make_merge_item("explains", 73.0, 44.0),
            make_merge_item("the result", 121.0, 55.0),
        ];
        for item in &mut items {
            item.y = 50.0;
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Page 42 explains the result");
    }

    #[test]
    fn repeated_page_number_prefix_with_substantive_text_is_preserved_during_layout() {
        let mut items = Vec::new();
        for (page, value, chapter) in [(1, "42", "Chapter 1"), (2, "43", "Chapter 2")] {
            let mut label = make_merge_item("Page", 25.0, 28.0);
            label.page = page;
            label.y = 50.0;
            let mut page_number = make_merge_item(value, 57.0, 12.0);
            page_number.page = page;
            page_number.y = 50.0;
            let mut suffix = make_merge_item(chapter, 73.0, 58.0);
            suffix.page = page;
            suffix.y = 50.0;
            items.extend([label, page_number, suffix]);
        }

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "Page 42 Chapter 1");
        assert_eq!(lines[1].text(), "Page 43 Chapter 2");
    }

    #[test]
    fn page_number_phrase_in_the_page_body_is_preserved() {
        let items = vec![
            make_merge_item("Page", 25.0, 28.0),
            make_merge_item("42", 57.0, 12.0),
        ];

        let lines = group_into_lines(items);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Page 42");
    }

    #[test]
    fn short_numeric_context_near_page_edge_is_preserved() {
        let mut chapter = make_merge_item("Chapter", 100.0, 45.0);
        chapter.y = 760.0;
        let mut chapter_number = make_merge_item("1", 151.0, 6.0);
        chapter_number.y = 760.0;

        let chapter_lines = group_into_lines(vec![chapter, chapter_number]);
        assert_eq!(chapter_lines.len(), 1);
        assert_eq!(chapter_lines[0].text(), "Chapter 1");

        let mut year = make_merge_item("2026", 100.0, 24.0);
        year.y = 760.0;
        let mut report = make_merge_item("Report", 130.0, 36.0);
        report.y = 760.0;

        let report_lines = group_into_lines(vec![year, report]);
        assert_eq!(report_lines.len(), 1);
        assert_eq!(report_lines[0].text(), "2026 Report");

        let mut chapter = make_merge_item("Chapter", 100.0, 45.0);
        chapter.y = 760.0;
        let mut chapter_number = make_merge_item("1", 151.0, 6.0);
        chapter_number.y = 760.0;
        let mut edition_year = make_merge_item("2026", 163.0, 24.0);
        edition_year.y = 760.0;

        let chained_lines = group_into_lines(vec![chapter, chapter_number, edition_year]);
        assert_eq!(chained_lines.len(), 1);
        assert_eq!(chained_lines[0].text(), "Chapter 1 2026");
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
            is_underline: false,
            is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
                is_underline: false,
                is_strikeout: false,
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
            is_underline: false,
            is_strikeout: false,
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
        // Lowered baseline → Unicode subscript form (NFKC folds back to "NH3")
        assert_eq!(merged[0].text, "NH₃");
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
        assert_eq!(merged[0].text, "H₂");
        assert_eq!(merged[1].text, "O");
    }

    #[test]
    fn test_merge_subscript_items_raised_marker_becomes_superscript() {
        // Footnote reference: "word" followed by a RAISED small "2" → word²
        let mut marker = make_item_fs("2", 90.0, 502.5, 2.3, 4.7);
        marker.y = 502.5; // raised above the 499.0 parent baseline
        let items = vec![make_item_fs("word", 78.0, 499.0, 12.0, 8.0), marker];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "word²");
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
