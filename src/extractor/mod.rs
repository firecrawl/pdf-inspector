//! Text extraction from PDF using lopdf
//!
//! This module extracts text with position information for structure detection.

mod base14;
mod clip_boundaries;
pub(crate) mod content_decode;
pub(crate) mod content_stream;
pub(crate) mod display_frame;
pub(crate) mod fonts;
pub(crate) mod geometry;
mod layout;
mod links;
pub(crate) mod page_box;
mod reading_order;
mod scripts;
mod text_paint;
pub(crate) mod underline;
pub(crate) mod word_gaps;
mod xobjects;

use crate::text_utils::{is_cjk_char, is_rtl_text};
use crate::tounicode::FontCMaps;
use crate::types::{CMapCoverageByFont, PageExtraction, PdfLine, PdfRect, RunCoverage, TextItem};
use crate::PdfError;
use log::debug;
use lopdf::{Document, Object, ObjectId};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use content_stream::extract_page_text_items_with_options;
pub(crate) use content_stream::TextExtractionOptions;
pub(crate) use display_frame::DisplayPage;
pub use display_frame::PositionFrame;

/// Options of the positioned-text and region APIs, taken by their
/// `_with_options` variants ([`extract_text_with_positions_mem_with_options`],
/// [`extract_text_with_positions_and_rotations_mem_with_options`],
/// [`extract_text_in_regions_mem_with_options`](crate::extract_text_in_regions_mem_with_options),
/// [`extract_tables_in_regions_mem_with_options`](crate::extract_tables_in_regions_mem_with_options)).
/// The default is what the plain and `_in_frame` variants do.
///
/// ```
/// use pdf_inspector::{PositionFrame, PositionOptions};
///
/// let options = PositionOptions::new()
///     .frame(PositionFrame::Display)
///     .bold_from_weight(true)
///     .bold_weight_threshold(700);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PositionOptions {
    /// Coordinate frame items are reported in and region rects are read
    /// in; [`PositionFrame::Sheet`] by default.
    pub frame: PositionFrame,
    /// Read bold from the font's weight class as well. When set,
    /// `TextItem::is_bold` is also `true` for items whose
    /// `TextItem::font_weight` is `bold_weight_threshold` (600, SemiBold,
    /// by default) or more, `TextItem::bold_source` says so, and adjacent
    /// runs are merged by that verdict: a run the weight makes bold stays
    /// apart from its plain neighbours, so a heavier run inside a lighter
    /// paragraph keeps its own item, while runs whose weights differ but
    /// agree on bold — both below the threshold, or both at or above it —
    /// merge as usual. Off by default: `is_bold` and item merging are then
    /// exactly what they were before the option existed, and `font_weight`
    /// is reported either way.
    pub bold_from_weight: bool,
    /// The weight class from which `bold_from_weight` reads bold, on the
    /// 100..=900 scale: 600 by default, so SemiBold and heavier faces are
    /// bold. It matters only when `bold_from_weight` is set; a value outside
    /// the scale is clamped into it either way.
    pub bold_weight_threshold: u16,
}

impl Default for PositionOptions {
    fn default() -> Self {
        Self {
            frame: PositionFrame::default(),
            bold_from_weight: false,
            bold_weight_threshold: content_stream::DEFAULT_BOLD_WEIGHT_THRESHOLD,
        }
    }
}

impl PositionOptions {
    /// The defaults: sheet frame, bold not read from the weight class, a
    /// threshold of 600 for when it is.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the coordinate frame.
    pub fn frame(mut self, frame: PositionFrame) -> Self {
        self.frame = frame;
        self
    }

    /// Set whether bold is also read from the weight class.
    pub fn bold_from_weight(mut self, bold_from_weight: bool) -> Self {
        self.bold_from_weight = bold_from_weight;
        self
    }

    /// Set the weight class from which bold is read when `bold_from_weight`
    /// is on: 600 by default, valid values 100..=900.
    pub fn bold_weight_threshold(mut self, threshold: u16) -> Self {
        self.bold_weight_threshold = threshold;
        self
    }

    /// The content-stream switches these options ask for.
    pub(crate) fn text_extraction(self, include_invisible: bool) -> TextExtractionOptions {
        TextExtractionOptions {
            include_invisible,
            bold_from_weight: self.bold_from_weight,
            bold_weight_threshold: self.bold_weight_threshold.clamp(100, 900),
            // The position readers report no CMap coverage.
            cmap_coverage: false,
        }
    }
}
use links::{extract_form_fields, extract_page_links};
pub(crate) use page_box::{visible_page_box, PageBox};

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
pub(crate) use scripts::merge_subscript_items;
pub(crate) use xobjects::FormWalkBudget;

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

/// Extract text with position information from a PDF file.
///
/// # Coordinate frame
///
/// Every positioned item (text, image placeholder, link, form field) is
/// reported in PDF points relative to the page's **visible page box**:
/// `CropBox ∩ MediaBox` when the page has a CropBox, else the MediaBox. The
/// box's lower-left corner is the origin and `y` grows upward. A renderer's
/// page image and the region APIs ([`crate::extract_text_in_regions_mem`]
/// and friends) use the same box from its top-left corner with `y` growing
/// downward, so flipping `y` by the box height moves between the two. Raw
/// content-stream coordinates differ whenever the CropBox or MediaBox origin
/// is not `(0, 0)`. Pages whose text is drawn rotated by 90° are normalized
/// into a synthetic landscape frame before the shift; `/Rotate` is not
/// applied. See [`TextItem`] for the full note.
pub fn extract_text_with_positions<P: AsRef<Path>>(path: P) -> Result<Vec<TextItem>, PdfError> {
    extract_text_with_positions_pages(path, None)
}

/// Extract text with positions from a file, limited to specific pages.
///
/// `page_filter` is an optional set of 1-indexed page numbers to process.
/// When `None`, all pages are processed. Coordinates: see
/// [`extract_text_with_positions`].
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
/// When `None`, all pages are processed. Coordinates: see
/// [`extract_text_with_positions`].
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
    let (extraction, _thresholds, _gid_pages, _page_rotations, _cmap_coverage) =
        extract_positioned_text_from_doc_in_page_box(
            &doc,
            &font_cmaps,
            page_filter,
            PositionOptions::default(),
        )?;
    Ok(extraction)
}

/// Extract text with positions from a memory buffer. Coordinates: see
/// [`extract_text_with_positions`].
pub fn extract_text_with_positions_mem(buffer: &[u8]) -> Result<Vec<TextItem>, PdfError> {
    extract_text_with_positions_mem_pages(buffer, None)
}

/// Extract text with positions from a memory buffer, limited to specific
/// pages. Coordinates: see [`extract_text_with_positions`].
pub fn extract_text_with_positions_mem_pages(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
) -> Result<Vec<TextItem>, PdfError> {
    extract_text_with_positions_mem_in_frame(buffer, page_filter, PositionFrame::Sheet)
}

/// Extract text with positions from a memory buffer in the given coordinate
/// frame, limited to specific pages.
///
/// [`PositionFrame::Sheet`] is the frame of [`extract_text_with_positions`]:
/// the visible page box as laid out in the content stream, `/Rotate` not
/// applied, with predominantly rotated pages turned so their text reads
/// left-to-right. [`PositionFrame::Display`] reports every item — text and
/// image placeholders, links and form fields alike — in the rendered page's
/// frame instead: the visible box turned clockwise by the page's inheritable
/// `/Rotate`, lower-left origin, `y` up, with the turn of a rotated page
/// undone first. Baseline angles (`TextItem::rotation`) are expressed in the
/// same frame, so text that renders horizontally reads as `0`.
pub fn extract_text_with_positions_mem_in_frame(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
    frame: PositionFrame,
) -> Result<Vec<TextItem>, PdfError> {
    extract_text_with_positions_mem_with_options(
        buffer,
        page_filter,
        PositionOptions::new().frame(frame),
    )
}

/// [`extract_text_with_positions_mem_in_frame`] with every option of the
/// positioned-text APIs given as a [`PositionOptions`]: the frame, and
/// whether bold is also read from the font's weight class
/// (`bold_from_weight`). The default options are that function with
/// [`PositionFrame::Sheet`].
pub fn extract_text_with_positions_mem_with_options(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
    options: PositionOptions,
) -> Result<Vec<TextItem>, PdfError> {
    let (items, _page_rotations) =
        extract_text_with_positions_and_rotations_mem_with_options(buffer, page_filter, options)?;
    Ok(items)
}

/// Extract text with positions from a memory buffer, together with the
/// coordinate frame of every page whose text was predominantly rotated.
///
/// Such pages are re-based so their dominant runs read left-to-right (see
/// [`PageRotation`](crate::PageRotation)); their items live in the turned
/// frame, and region boxes given in page coordinates must be turned the same
/// way with [`collect_text_in_region_in_frame`](crate::collect_text_in_region_in_frame).
/// Pages absent from the map are upright. Keys are 1-indexed like
/// `TextItem::page`.
pub fn extract_text_with_positions_and_rotations_mem(
    buffer: &[u8],
) -> Result<(Vec<TextItem>, HashMap<u32, geometry::PageRotation>), PdfError> {
    extract_text_with_positions_and_rotations_mem_in_frame(buffer, None, PositionFrame::Sheet)
}

/// [`extract_text_with_positions_and_rotations_mem`] limited to specific
/// pages and reporting items in the given coordinate frame (see
/// [`extract_text_with_positions_mem_in_frame`]).
///
/// The returned map names the pages whose text was predominantly rotated
/// whatever the frame: in the display frame their turn has already been
/// undone, so the map is informational there.
pub fn extract_text_with_positions_and_rotations_mem_in_frame(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
    frame: PositionFrame,
) -> Result<(Vec<TextItem>, HashMap<u32, geometry::PageRotation>), PdfError> {
    extract_text_with_positions_and_rotations_mem_with_options(
        buffer,
        page_filter,
        PositionOptions::new().frame(frame),
    )
}

/// [`extract_text_with_positions_and_rotations_mem_in_frame`] with every
/// option given as a [`PositionOptions`] — see
/// [`extract_text_with_positions_mem_with_options`].
pub fn extract_text_with_positions_and_rotations_mem_with_options(
    buffer: &[u8],
    page_filter: Option<&HashSet<u32>>,
    options: PositionOptions,
) -> Result<(Vec<TextItem>, HashMap<u32, geometry::PageRotation>), PdfError> {
    crate::validate_pdf_bytes(buffer)?;
    let (doc, _) = crate::load_document_from_mem(buffer)?;
    let font_cmaps = FontCMaps::from_doc(&doc);
    let ((mut items, _rects, _lines), _thresholds, _gid_pages, page_rotations, _cmap_coverage) =
        extract_positioned_text_from_doc_in_page_box(&doc, &font_cmaps, page_filter, options)?;
    if options.frame == PositionFrame::Display {
        display_frame::document_items_to_display_frame(&doc, &mut items, &page_rotations);
    }
    Ok((items, page_rotations))
}

/// One page's geometry in the visible-page-box frame, from
/// [`extract_page_text_items_in_page_box`].
pub(crate) struct PageBoxExtraction {
    pub(crate) items: Vec<TextItem>,
    pub(crate) rects: Vec<PdfRect>,
    pub(crate) lines: Vec<PdfLine>,
    /// Fonts with unresolvable gid-encoded glyphs were encountered.
    pub(crate) has_gid_fonts: bool,
    /// How the page's frame was turned so its text reads left-to-right
    /// (see `content_stream::correct_rotated_page`); `Upright` when it was
    /// not.
    pub(crate) coords_rotated: geometry::PageRotation,
    /// Invisible (Tr 3) text was skipped and could be recovered with
    /// `include_invisible`.
    pub(crate) skipped_invisible: bool,
    /// The visible page box the geometry was shifted into; its height is the
    /// flip height for top-left-origin region inputs.
    pub(crate) page_box: PageBox,
}

/// Page-scoped extraction for the region APIs: `extract_page_text_items`
/// followed by the visible-page-box shift every one of them must apply, so
/// no caller can translate items but forget rects, or skip the rotated
/// shift. Region bounds then flip `y` with `page_box.height()`.
pub(crate) fn extract_page_text_items_in_page_box(
    doc: &Document,
    page_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    include_invisible: bool,
    style_cache: &mut FontStyleCache,
    form_budget: &mut FormWalkBudget,
) -> Result<PageBoxExtraction, PdfError> {
    extract_page_text_items_in_page_box_with_options(
        doc,
        page_id,
        page_num,
        font_cmaps,
        TextExtractionOptions {
            include_invisible,
            ..TextExtractionOptions::default()
        },
        style_cache,
        form_budget,
    )
}

/// [`extract_page_text_items_in_page_box`] with every content-stream switch
/// given.
pub(crate) fn extract_page_text_items_in_page_box_with_options(
    doc: &Document,
    page_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    options: TextExtractionOptions,
    style_cache: &mut FontStyleCache,
    form_budget: &mut FormWalkBudget,
) -> Result<PageBoxExtraction, PdfError> {
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    // The region APIs read one page at a time and report no CMap coverage.
    let (
        (mut items, mut rects, mut lines),
        has_gid_fonts,
        coords_rotated,
        skipped_invisible,
        _run_coverage,
    ) = extract_page_text_items_with_options(
        doc,
        page_id,
        page_num,
        font_cmaps,
        options,
        style_cache,
        form_budget,
    )?;
    page_box.translate_page(&mut items, &mut rects, &mut lines, coords_rotated);
    Ok(PageBoxExtraction {
        items,
        rects,
        lines,
        has_gid_fonts,
        coords_rotated,
        skipped_invisible,
        page_box,
    })
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Per-page adaptive join thresholds from Canva-style letter-spacing detection.
pub(crate) type PageThresholds = HashMap<u32, f32>;

/// Pages (1-indexed, like `TextItem::page`) whose coordinate frame was turned
/// because their text is predominantly rotated; pages absent from the map
/// are upright.
pub(crate) type PageRotations = HashMap<u32, geometry::PageRotation>;

/// What a document-level extraction returns: the text, rectangles and lines
/// of the extracted pages, their join thresholds, the pages with gid-encoded
/// fonts, their frame rotations and, per font, the two-byte codes shown
/// through the font's CMap and how many of them the CMap had no entry for.
pub(crate) type DocumentExtraction = (
    PageExtraction,
    PageThresholds,
    HashSet<u32>,
    PageRotations,
    CMapCoverageByFont,
);

/// Extract positioned text, rectangles, and line segments from a pre-loaded document.
///
/// Also returns per-page adaptive join thresholds for Canva-style pages.
pub(crate) fn extract_positioned_text_from_doc(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<DocumentExtraction, PdfError> {
    extract_positioned_text_impl(
        doc,
        font_cmaps,
        page_filter,
        TextExtractionOptions::default(),
        None,
        CoordinateFrame::UserSpace,
    )
}

/// [`extract_positioned_text_from_doc`] with every page's geometry shifted
/// into the visible-page-box frame — what the public position APIs return.
/// A page whose frame was turned (see `PageRotation`) gets the equivalently
/// turned shift, so its items, links and form fields agree with region
/// bounds computed from the visible box.
pub(crate) fn extract_positioned_text_from_doc_in_page_box(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    options: PositionOptions,
) -> Result<DocumentExtraction, PdfError> {
    extract_positioned_text_impl(
        doc,
        font_cmaps,
        page_filter,
        options.text_extraction(false),
        None,
        CoordinateFrame::VisiblePageBox,
    )
}

/// Coordinate frame of the geometry a positioned-text extraction returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinateFrame {
    /// Raw PDF user space, as written in the content stream. The markdown
    /// pipeline works here; its page-edge heuristics never leave it.
    UserSpace,
    /// The visible page box ([`PageBox`]) with its lower-left corner as the
    /// origin — the frame of a rendered page image. Public position APIs
    /// return this so items can be intersected with rendered regions.
    VisiblePageBox,
}

/// Extract selected pages and gather document-wide folio evidence only when a
/// selected page contains an ambiguous contextual page-edge number. Errors on
/// selected pages remain fatal; errors on context-only pages are skipped.
pub(crate) fn extract_positioned_text_with_folio_context(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<DocumentExtraction, PdfError> {
    extract_positioned_text_with_folio_context_impl(doc, font_cmaps, page_filter, false)
}

/// Invisible-text variant of [`extract_positioned_text_with_folio_context`].
pub(crate) fn extract_positioned_text_include_invisible_with_folio_context(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
) -> Result<DocumentExtraction, PdfError> {
    extract_positioned_text_with_folio_context_impl(doc, font_cmaps, page_filter, true)
}

fn extract_positioned_text_with_folio_context_impl(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
) -> Result<DocumentExtraction, PdfError> {
    // The selected pages report their CMap coverage; the context pages
    // gathered below for their folio evidence do not.
    let options = TextExtractionOptions {
        include_invisible,
        cmap_coverage: true,
        ..TextExtractionOptions::default()
    };
    let Some(required_pages) = page_filter else {
        return extract_positioned_text_impl(
            doc,
            font_cmaps,
            None,
            options,
            None,
            CoordinateFrame::UserSpace,
        );
    };

    let (
        (mut selected_items, mut selected_rects, mut selected_lines),
        mut page_thresholds,
        mut gid_encoded_pages,
        mut page_rotations,
        cmap_coverage,
    ) = extract_positioned_text_impl(
        doc,
        font_cmaps,
        Some(required_pages),
        options,
        None,
        CoordinateFrame::UserSpace,
    )?;
    if !layout::needs_document_page_number_context(&selected_items, doc.get_pages().len()) {
        return Ok((
            (selected_items, selected_rects, selected_lines),
            page_thresholds,
            gid_encoded_pages,
            page_rotations,
            cmap_coverage,
        ));
    }

    let context_pages: HashSet<u32> = doc
        .get_pages()
        .keys()
        .copied()
        .filter(|page| !required_pages.contains(page))
        .collect();
    // The context pages' items are dropped again once the folios are
    // decided; their fonts' coverage is no part of the selected pages'
    // either, so it is left out here.
    let (
        (context_items, context_rects, context_lines),
        context_thresholds,
        context_gid_pages,
        context_rotations,
        _context_coverage,
    ) = extract_positioned_text_impl(
        doc,
        font_cmaps,
        Some(&context_pages),
        TextExtractionOptions {
            cmap_coverage: false,
            ..options
        },
        Some(required_pages),
        CoordinateFrame::UserSpace,
    )?;
    selected_items.extend(context_items);
    selected_rects.extend(context_rects);
    selected_lines.extend(context_lines);
    page_thresholds.extend(context_thresholds);
    gid_encoded_pages.extend(context_gid_pages);
    page_rotations.extend(context_rotations);
    Ok((
        (selected_items, selected_rects, selected_lines),
        page_thresholds,
        gid_encoded_pages,
        page_rotations,
        cmap_coverage,
    ))
}

/// Extract all pages for document-wide analysis while allowing malformed
/// unselected pages to be skipped. Any requested page still fails normally.
pub(crate) fn extract_positioned_text_for_document_analysis(
    doc: &Document,
    font_cmaps: &FontCMaps,
    required_pages: &HashSet<u32>,
) -> Result<DocumentExtraction, PdfError> {
    extract_positioned_text_impl(
        doc,
        font_cmaps,
        None,
        TextExtractionOptions::default(),
        Some(required_pages),
        CoordinateFrame::UserSpace,
    )
}

fn extract_positioned_text_impl(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    options: TextExtractionOptions,
    required_pages: Option<&HashSet<u32>>,
    frame: CoordinateFrame,
) -> Result<DocumentExtraction, PdfError> {
    let pages = doc.get_pages();
    let mut all_items = Vec::new();
    let mut all_rects = Vec::new();
    let mut all_lines = Vec::new();
    let mut page_thresholds: PageThresholds = HashMap::new();
    let mut gid_encoded_pages: HashSet<u32> = HashSet::new();
    // Per font, the codes shown through its CMap over every extracted page.
    let mut cmap_coverage = CMapCoverageByFont::new();
    // Embedded-font style flags are document-scoped: the same font program
    // is shared across pages, so parse it once, not once per page.
    let mut style_cache = FontStyleCache::new();
    // Pages whose coordinate frame was turned (see `PageRotation`): the
    // document-level annotation items appended below must follow.
    let mut page_rotations: PageRotations = HashMap::new();
    // Visible page box per extracted page, for the form-field shift below.
    let mut page_boxes: HashMap<u32, PageBox> = HashMap::new();

    // Build page ObjectId → page number map for form field extraction
    let page_id_to_num: HashMap<ObjectId, u32> =
        pages.iter().map(|(num, &id)| (id, *num)).collect();

    for (page_num, &page_id) in pages.iter() {
        if let Some(filter) = page_filter {
            if !filter.contains(page_num) {
                continue;
            }
        }
        let page_result = extract_page_text_items_with_options(
            doc,
            page_id,
            *page_num,
            font_cmaps,
            options,
            &mut style_cache,
            &mut FormWalkBudget::new(),
        );
        let (
            (mut items, mut rects, mut lines),
            has_gid_fonts,
            coords_rotated,
            _skipped_invisible,
            mut run_coverage,
        ) = match page_result {
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
        if coords_rotated != geometry::PageRotation::Upright {
            page_rotations.insert(*page_num, coords_rotated);
        }
        // Clip to the visible page box: single-page extracts and imposed
        // spreads keep neighboring pages' content in the stream, positioned
        // outside the CropBox. Extracting it interleaves invisible text into
        // the page and poisons font statistics. On a turned page the box is
        // turned the same way, so the test runs in the items' own frame.
        let page_box = visible_page_box(doc, page_id);
        let mut clipped_box: Option<(f32, f32, f32, f32)> = None;
        let clip_box = page_box.map(|b| {
            let (mut x, mut y, mut w, mut h) = (b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0);
            coords_rotated.rotate_box(&mut x, &mut y, &mut w, &mut h);
            (x, y, x + w, y + h)
        });
        if let Some((bx0, by0, bx1, by1)) = clip_box {
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
                    !outside(i) && (i.y - o.y).abs() <= 2.0 && (o.x - (i.x + i.width)).abs() <= 10.0
                })
            });
            let coherent = off.len() >= 10 && wordy_chars * 2 >= total_chars.max(1) && !straddles;
            if bx1 - bx0 >= 72.0 && by1 - by0 >= 72.0 && coherent {
                let before = items.len();
                items.retain(|it| !outside(it));
                // The runs left out take their CMap coverage with them; a
                // run without a position (see `RunCoverage`) stays.
                run_coverage.retain(|run| match run.position {
                    Some((x, y, width)) => {
                        let cx = x + width / 2.0;
                        cx >= bx0 - TOL && cx <= bx1 + TOL && y >= by0 - TOL && y <= by1 + TOL
                    }
                    None => true,
                });
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
        if has_gid_fonts {
            gid_encoded_pages.insert(*page_num);
        }
        for RunCoverage { font, stats, .. } in run_coverage {
            match cmap_coverage.get_mut(&*font) {
                Some(total) => total.add(stats),
                None => {
                    cmap_coverage.insert(font.to_string(), stats);
                }
            }
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
        // Public position APIs report geometry relative to the visible page
        // box; the markdown pipeline stays in raw user space.
        let frame_box = page_box.unwrap_or(PageBox::LETTER);
        if frame == CoordinateFrame::VisiblePageBox {
            frame_box.translate_page(&mut items, &mut rects, &mut lines, coords_rotated);
        }
        page_boxes.insert(*page_num, frame_box);
        all_items.extend(items);
        all_rects.extend(rects);
        all_lines.extend(lines);

        // Extract hyperlinks from page annotations. A turned page turns its
        // annotation boxes too, so links stay on the text they cover and in
        // the regions that text is assigned to.
        let mut links = extract_page_links(doc, page_id, *page_num);
        for link in &mut links {
            coords_rotated.rotate_box(&mut link.x, &mut link.y, &mut link.width, &mut link.height);
        }
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
        if frame == CoordinateFrame::VisiblePageBox {
            // The link rects were turned with the page above, so they take
            // the same turned shift as the page's items.
            frame_box.translate_items(&mut links, coords_rotated);
        }
        all_items.extend(links);
    }

    // Extract AcroForm field values. Widgets on a turned page turn with it,
    // like links, so positional consumers keep them on the text they cover;
    // in the visible-box frame they take the page's (turned) shift as well.
    let form_items: Vec<TextItem> = extract_form_fields(doc, &page_id_to_num)
        .into_iter()
        .filter(|item| page_filter.is_none_or(|filter| filter.contains(&item.page)))
        .map(|mut item| {
            let rotation = page_rotations
                .get(&item.page)
                .copied()
                .unwrap_or(geometry::PageRotation::Upright);
            rotation.rotate_box(&mut item.x, &mut item.y, &mut item.width, &mut item.height);
            if frame == CoordinateFrame::VisiblePageBox {
                let frame_box = page_boxes.get(&item.page).copied().unwrap_or_else(|| {
                    pages
                        .get(&item.page)
                        .and_then(|&id| visible_page_box(doc, id))
                        .unwrap_or(PageBox::LETTER)
                });
                frame_box.translate_items(std::slice::from_mut(&mut item), rotation);
            }
            item
        })
        .collect();
    all_items.extend(form_items);

    Ok((
        (all_items, all_rects, all_lines),
        page_thresholds,
        gid_encoded_pages,
        page_rotations,
        cmap_coverage,
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
/// Groups items into lines ([`group_fragments_into_lines`]: on one page,
/// within 5 pt of the line's first fragment and within a window that follows
/// the type size of a fragment already on the line), sorts within
/// each line by X, then merges consecutive items that share a similar font
/// size and are close horizontally.
/// Cap item width for merge-gap computation to guard against Tw inflation.
///
/// When PDF word-spacing (Tw) is large (used for text justification), the
/// advance width of strings containing spaces extends far past the visible
/// glyph extent.  This inflated width collapses inter-column gaps, making
/// `merge_text_items` incorrectly merge items from different table columns.
///
/// Only applies to non-CJK items whose text contains spaces (where Tw
/// contributes) and whose average width-per-character is abnormally high.
/// Extent used to detect overlapping (backtracking) paint order: the merge
/// width for measured runs, the estimated box for width-less ones — an
/// estimate is still evidence of where a fragment ends, and taking it as zero
/// would let a tagged widthless ActualText line be x-sorted out of order.
fn order_extent(item: &TextItem) -> f32 {
    if item.advance_known {
        effective_merge_width(item)
    } else {
        item.width
    }
}

fn effective_merge_width(item: &TextItem) -> f32 {
    use crate::text_utils::is_cjk_char;

    // An estimated box (font without widths) is the best extent there is for
    // word-gap decisions — as it was when `effective_width` estimated it here;
    // the Tw cap below only makes sense for measured widths.
    if !item.advance_known {
        return item.width;
    }
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

/// Width, as a share of the font size, under which a measured run is a
/// glyph without advance.
const ZERO_WIDTH_MARK_EM: f32 = 0.01;

/// Most characters a dependent sign shown as a run of its own decodes to:
/// a sign, or one that decodes to two code points. A longer run without
/// advance is hidden text, not a sign.
const ZERO_WIDTH_MARK_MAX_CHARS: usize = 2;

/// Whether `item` is a dependent sign shown as a run of its own: a glyph
/// without advance — a vowel sign, a subscript letter, an accent — by its
/// measured width, or a run of nothing but combining marks, of a character
/// or two either way. Such a sign is drawn over the glyph before it, behind
/// the pen, and the line's right edge does not move for it.
fn is_zero_width_mark(item: &TextItem) -> bool {
    let text = item.text.trim();
    if !(1..=ZERO_WIDTH_MARK_MAX_CHARS).contains(&text.chars().count())
        || !matches!(item.item_type, crate::types::ItemType::Text)
    {
        return false;
    }
    (item.advance_known && item.width.abs() <= item.font_size.abs() * ZERO_WIDTH_MARK_EM)
        || text.chars().all(crate::bidi::is_combining_mark)
}

/// Whether `x` lies inside the advance of `item`: from its origin to short
/// of its end, whichever way it reads.
fn inside_advance(item: &TextItem, x: f32) -> bool {
    let end = item.x + item.width;
    x >= item.x.min(end) && x < item.x.max(end)
}

/// Sort a line's fragments along +x, keeping a dependent sign right after
/// the base it was shown on. A zero-width sign whose origin lies inside
/// the advance of the fragment shown before it — over that fragment, short
/// of its end — takes that fragment's x as its key and follows it; by its
/// own x it would land after the next base when that base is kerned in
/// ahead of the pen.
fn sort_along_x_keeping_marks(group: &mut [&TextItem]) {
    let mut keys: Vec<f32> = Vec::with_capacity(group.len());
    // The fragment the one before this sorts with: itself, or for a sign
    // kept after its base, that base.
    let mut previous_base: Option<usize> = None;
    for (index, item) in group.iter().enumerate() {
        let base = previous_base
            .filter(|&base| is_zero_width_mark(item) && inside_advance(group[base], item.x));
        keys.push(base.map_or(item.x, |base| keys[base]));
        previous_base = Some(base.unwrap_or(index));
    }
    let mut order: Vec<usize> = (0..group.len()).collect();
    order.sort_by(|&a, &b| keys[a].total_cmp(&keys[b]));
    let sorted: Vec<&TextItem> = order.iter().map(|&index| group[index]).collect();
    group.copy_from_slice(&sorted);
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

/// The fraction of an em within which two baselines belong to one line
/// (see [`baseline_tolerance`]).
const LINE_BASELINE_TOLERANCE_EM: f32 = 0.6;

/// The widest the line window gets, in points: the fixed window that
/// grouped lines before the window followed the type, kept as its ceiling
/// so that a junction of two fragments of ordinary size groups exactly as
/// it did.
const LINE_BASELINE_TOLERANCE_MAX_PT: f32 = 5.0;

/// The type size at which [`baseline_tolerance`] reaches its ceiling: the
/// size a fragment without one is given, so that it keeps the window of old.
const LINE_BASELINE_LEGACY_EM: f32 = LINE_BASELINE_TOLERANCE_MAX_PT / LINE_BASELINE_TOLERANCE_EM;

/// A fragment's type size for the line test: its rendered em size — the
/// box height of an unrotated run, the font size of a rotated one, whose
/// box mixes the advance into its height — else its font size, else the
/// size that keeps the 5 pt window of old, which an image gets too, its
/// height being the image's and not a type size.
fn line_em(item: &TextItem) -> f32 {
    let em = item.cross_extent();
    if matches!(item.item_type, crate::types::ItemType::Image) {
        LINE_BASELINE_LEGACY_EM
    } else if em > 0.0 {
        em
    } else if item.font_size > 0.0 {
        item.font_size
    } else {
        LINE_BASELINE_LEGACY_EM
    }
}

/// How far apart two baselines may lie for fragments of type sizes `a` and
/// `b` to sit on one line: `LINE_BASELINE_TOLERANCE_EM` of the larger of
/// the two, no farther than the smaller, and never more than
/// `LINE_BASELINE_TOLERANCE_MAX_PT` — the fixed window of old, which two
/// fragments of 8⅓ pt and above therefore keep. Below that the window
/// follows the type. A raised or lowered mark is displaced by less than
/// its own em — a superscript of six tenths of its line's type rises a
/// third of that type, half its own size — so every such mark stays with
/// its line, while type of any size cannot pull a fragment of smaller type
/// farther than that fragment's em: a line of 4 pt labels beside a column
/// of 11 pt text 5 pt lower keeps its own line, and two lines of small
/// type on a pitch under 5 pt — a stacked table header at 4.7 pt — stay
/// apart, where the fixed window put them in one line and, shown glyph by
/// glyph as small kerned type is, interleaved their glyphs along the
/// baseline.
fn baseline_tolerance(a: f32, b: f32) -> f32 {
    (LINE_BASELINE_TOLERANCE_EM * a.max(b))
        .min(a.min(b))
        .min(LINE_BASELINE_TOLERANCE_MAX_PT)
}

/// How far apart two baselines must lie to be recorded separately on a
/// line, in points.
const LINE_BASELINE_RECORD_SPACING: f32 = 0.05;

/// The most distinct baselines a line records: as many as the record
/// spacing admits inside the fixed window either side of the line's first
/// fragment, so no fragment inside that window is ever measured against a
/// stale baseline — glyph-by-glyph type set on a slight slope records one
/// per glyph — while the walk over them stays bounded.
const LINE_BASELINES_MAX: usize =
    (2.0 * LINE_BASELINE_TOLERANCE_MAX_PT / LINE_BASELINE_RECORD_SPACING) as usize;

/// A line as the fragments are grouped: its page, the baseline of the
/// fragment it was seeded with, the distinct baselines its fragments sit
/// on (at most `LINE_BASELINES_MAX` of them, each with the largest type
/// size seen on it, so that a small glyph shown first on a baseline — a
/// bullet, a mark — does not narrow the window for the text that follows
/// it), and the fragments.
struct FragmentLine {
    page: u32,
    y: f32,
    baselines: Vec<(f32, f32)>,
    /// Indices into the fragments being grouped, in stream order.
    fragments: Vec<usize>,
}

impl FragmentLine {
    /// Whether a fragment at baseline `y` with type size `em` belongs to
    /// this line: within the fixed window of the line's first fragment, as
    /// always, and within [`baseline_tolerance`] of a baseline a fragment
    /// of the line already sits on, each at its own type size — so a
    /// subscript joins the base text it hangs from though the line's first
    /// fragment lies a little higher, a mark joins the text it is raised
    /// over though a smaller fragment sits nearer to it, and a line of
    /// small type a whole pitch away does not join. The fixed window is
    /// tested first, so only the baselines of lines that start within it
    /// are walked.
    fn admits(&self, page: u32, y: f32, em: f32) -> bool {
        page == self.page
            && (y - self.y).abs() < LINE_BASELINE_TOLERANCE_MAX_PT
            && self
                .baselines
                .iter()
                .any(|&(line_y, line_em)| (y - line_y).abs() < baseline_tolerance(em, line_em))
    }

    fn push(&mut self, index: usize, item: &TextItem, em: f32) {
        let known = self
            .baselines
            .iter()
            .position(|&(line_y, _)| (line_y - item.y).abs() < LINE_BASELINE_RECORD_SPACING);
        match known {
            Some(i) => self.baselines[i].1 = self.baselines[i].1.max(em),
            None if self.baselines.len() < LINE_BASELINES_MAX => {
                self.baselines.push((item.y, em));
            }
            None => {}
        }
        self.fragments.push(index);
    }
}

/// The lines the fragments of `items` fall into — by page, then by
/// baseline (see [`FragmentLine::admits`]) — as the fragments' indices,
/// lines in the order their first fragment was walked and fragments in
/// stream order within each. The walk is one pass in stream order, as it
/// always was: the fragment that seeds a line anchors its fixed window,
/// and a baseline's type size grows as larger type lands on it, so text
/// shown after a small glyph on its baseline still gets its full window.
/// The subscript pass buckets its rough lines with it too, so the order
/// it fixes agrees with the lines made here.
fn group_indices_into_lines(items: &[TextItem]) -> Vec<Vec<usize>> {
    let mut lines: Vec<FragmentLine> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let em = line_em(item);
        match lines
            .iter_mut()
            .find(|line| line.admits(item.page, item.y, em))
        {
            Some(line) => line.push(index, item, em),
            None => lines.push(FragmentLine {
                page: item.page,
                y: item.y,
                baselines: vec![(item.y, em)],
                fragments: vec![index],
            }),
        }
    }
    lines.into_iter().map(|line| line.fragments).collect()
}

/// [`group_indices_into_lines`] as (page, baseline of the first fragment,
/// fragments).
fn group_fragments_into_lines(items: &[TextItem]) -> Vec<(u32, f32, Vec<&TextItem>)> {
    group_indices_into_lines(items)
        .into_iter()
        .map(|indices| {
            let first = &items[indices[0]];
            (
                first.page,
                first.y,
                indices.iter().map(|&i| &items[i]).collect(),
            )
        })
        .collect()
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
    let mut cluster_end = cluster_start + order_extent(sorted_by_x[0]);
    for item in sorted_by_x.iter().skip(1) {
        let gap = item.x - cluster_end;
        if gap > max_font_size * 2.5 {
            return false;
        }
        cluster_end = cluster_end.max(item.x + order_extent(item));
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
        let next_end = next.x + order_extent(next);
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
pub(crate) fn is_spaceless_cjk(c: char) -> bool {
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
        let next_end = next.x + effective_merge_width(next);
        // A dependent sign over the letter before it — of a character or
        // two — is no letter of the run, and the line's right edge does
        // not move for it (as in the merge loop): the next letter's gap
        // is measured from the pen the letter under the sign left.
        if is_zero_width_mark(next) {
            end_x = end_x.max(next_end);
            end = start + 1 + offset;
            continue;
        }
        if next.text.trim().chars().count() != 1 {
            break;
        }
        let gap = next.x - end_x;
        if gap > fs * 0.5 || gap < -fs * 0.5 {
            break;
        }
        gaps.push(gap / fs);
        end_x = next_end;
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

/// Fractional font-size band within which `merge_text_items` treats two runs as
/// the same size. Shared with `is_small_caps_continuation`, which exists only to
/// rescue junctions this band would otherwise break.
const MERGE_FONT_SIZE_BAND: f32 = 0.20;

/// Detect a small-caps continuation: typesetters render small caps as a
/// full-size capital immediately followed by shrunken capitals in the same
/// font (`(R) Tj` at 9.98pt, then `(OLANDO) Tj` at 6.74pt). Those runs are one
/// word, but the font-size band in `merge_text_items` would split them,
/// leaving "R" and "OLANDO" as separate items — which then read as separate
/// table columns, since column boundaries cluster on item start positions.
///
/// Gated tightly so it cannot absorb the other reasons a smaller run follows a
/// larger one:
///   - runs the size band already accepts — excluded by requiring the junction
///     to *cross* the band, so within-band pairs keep the normal word-spacing
///     logic instead of having their space suppressed
///   - superscripts / footnote markers — excluded by requiring an uppercase
///     *letter* on both sides, so digits never qualify
///   - drop caps — excluded because the body text that follows is mixed case
///   - adjacent table cells or separate words — excluded by requiring the runs
///     to be visually contiguous (essentially no gap)
fn is_small_caps_continuation(
    text_so_far: &str,
    first: &TextItem,
    next: &TextItem,
    gap: f32,
) -> bool {
    // Must shrink. Real small caps sit near 0.7-0.8 of the full cap height;
    // anything smaller is a superscript or a different run entirely.
    if first.font_size <= 0.0 || next.font_size >= first.font_size {
        return false;
    }
    // Only rescue junctions the size band would have broken. Within-band pairs
    // merge on their own, and suppressing their space would swallow real word
    // gaps between two similarly-sized uppercase words.
    if (next.font_size - first.font_size).abs() <= first.font_size * MERGE_FONT_SIZE_BAND {
        return false;
    }
    if next.font_size / first.font_size < 0.55 {
        return false;
    }
    // Visually contiguous: the capital and its small caps touch. A real word
    // space or a column gap disqualifies.
    if !(-first.font_size * 0.2..=first.font_size * 0.15).contains(&gap) {
        return false;
    }
    // The continuation must be all-uppercase letters (digits and lowercase
    // both disqualify), and must contain at least one letter.
    let mut saw_letter = false;
    for ch in next.text.chars() {
        if ch.is_alphabetic() {
            saw_letter = true;
            if !ch.is_uppercase() {
                return false;
            }
        } else if ch.is_numeric() {
            return false;
        }
    }
    if !saw_letter {
        return false;
    }
    // What we are continuing must itself end in a capital. Check the actual
    // trailing character rather than skipping back to the nearest letter: after
    // "ANGELA M. MAZZARELLI1" the run to continue is the footnote marker, not
    // the "I" before it.
    let trimmed = text_so_far.trim_end();
    if trimmed.chars().last().is_some_and(|c| c.is_numeric()) {
        // One legitimate exception: an ordinal suffix set as a smaller run,
        // e.g. "JULY 4" + "TH". Only the four English suffixes qualify —
        // anything else after a digit is a footnote marker or numeric suffix.
        return matches!(trimmed_suffix(next), "TH" | "ST" | "ND" | "RD");
    }
    trimmed
        .chars()
        .rev()
        .find(|c| c.is_alphabetic())
        .is_some_and(|c| c.is_uppercase())
}

/// The continuation run's text, trimmed — used to spot ordinal suffixes.
fn trimmed_suffix(next: &TextItem) -> &str {
    next.text.trim()
}

/// The combining mark a spacing accent stands for. These are the accents
/// the standard Latin encodings carry as glyphs of their own, each with an
/// advance: macron, acute, grave, circumflex, tilde, dieresis, caron,
/// breve, ring, cedilla, ogonek, dot accent and double acute. `None` for
/// any other character.
fn combining_mark_of_spacing_accent(c: char) -> Option<char> {
    Some(match c {
        '\u{00AF}' => '\u{0304}', // macron
        '\u{00B4}' => '\u{0301}', // acute
        '\u{0060}' => '\u{0300}', // grave
        '\u{02C6}' => '\u{0302}', // circumflex
        '\u{02DC}' => '\u{0303}', // tilde
        '\u{00A8}' => '\u{0308}', // dieresis
        '\u{02C7}' => '\u{030C}', // caron
        '\u{02D8}' => '\u{0306}', // breve
        '\u{02DA}' => '\u{030A}', // ring
        '\u{00B8}' => '\u{0327}', // cedilla
        '\u{02DB}' => '\u{0328}', // ogonek
        '\u{02D9}' => '\u{0307}', // dot accent
        '\u{02DD}' => '\u{030B}', // double acute
        _ => return None,
    })
}

/// The combining mark of a fragment that is one spacing accent and nothing
/// else: a show operator of the single accent glyph.
fn lone_spacing_accent(item: &TextItem) -> Option<char> {
    let mut chars = item.text.chars();
    let accent = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    combining_mark_of_spacing_accent(accent)
}

/// The end of a run a detached accent is matched against: the glyph that
/// begins the run or the glyph that closes it.
#[derive(Clone, Copy)]
enum RunEnd {
    First,
    Last,
}

/// The glyph at `end` of `text`: its character, the byte range it takes in
/// the text and the number of whitespace characters between it and that
/// end of the run. `None` for a run of nothing but whitespace.
fn end_glyph(text: &str, end: RunEnd) -> Option<(char, std::ops::Range<usize>, usize)> {
    let found = match end {
        RunEnd::First => text
            .char_indices()
            .enumerate()
            .find(|(_, (_, c))| !c.is_whitespace()),
        RunEnd::Last => text
            .char_indices()
            .rev()
            .enumerate()
            .find(|(_, (_, c))| !c.is_whitespace()),
    };
    found.map(|(skipped, (offset, c))| (c, offset..offset + c.len_utf8(), skipped))
}

/// How much of a detached accent lies over the glyph at `end` of `run`, in
/// points along the baseline; `None` when it does not stand over that
/// glyph. Both must be text runs of one page on a level baseline (a
/// `rotation` of exactly 0: the axis-aligned box of an oblique run is no
/// baseline), with measured advances and without right-to-left letters — a
/// run of those may have been painted under a mirrored matrix, which reads
/// as a level run whose glyphs advance leftwards, so which glyph stands at
/// which end of its box is not known from the item, and none of the accents
/// composes with such a letter anyway — their baselines within 0.3 em of
/// each other (an accent over a capital is set a little higher than one
/// over a small letter). The item keeps no advance per glyph, so the
/// glyph's window along the baseline is estimated: each whitespace
/// character is counted at 0.28 em (at the run's uniform advance for a
/// fixed-pitch face), the rest of the width is shared equally among the
/// other characters, and the window is that share or 0.6 em, whichever is
/// wider, so a capital W or M under the accent is covered although the
/// run's letters average half of it. The accent stands over the glyph when
/// its centre falls within the window, with a quarter of the accent's own
/// width of play beyond the run's edge for an accent overhanging a narrow
/// letter. An accent shown beside a glyph rather than over it, a circumflex
/// or grave that is text of its own, has its centre at least half its width
/// beyond that edge and is never matched.
fn accent_overlap(accent: &TextItem, run: &TextItem, end: RunEnd) -> Option<f32> {
    let level_text = |item: &TextItem| {
        matches!(item.item_type, crate::types::ItemType::Text)
            && item.rotation == 0.0
            && item.advance_known
            && item.width > 0.0
            && !item.text.chars().any(crate::text_utils::is_rtl_char)
    };
    if run.page != accent.page
        || !level_text(run)
        || !level_text(accent)
        || run.font_size <= 0.0
        || (accent.y - run.y).abs() > run.font_size * 0.3
    {
        return None;
    }
    let (_, _, skipped) = end_glyph(&run.text, end)?;
    let em = run.font_size;
    let chars = run.text.chars().count();
    let spaces = run.text.chars().filter(|c| c.is_whitespace()).count();
    let space_advance = if run.fixed_pitch == Some(true) {
        run.width / chars as f32
    } else {
        em * 0.28
    };
    let ink = run.width - space_advance * spaces as f32;
    if ink <= 0.0 {
        return None;
    }
    // `end_glyph` found a glyph, so the run has more characters than spaces.
    let advance = ink / (chars - spaces) as f32;
    let window = advance.max(em * 0.6);
    let (left, right) = match end {
        RunEnd::First => {
            let left = run.x + space_advance * skipped as f32;
            (left, left + window)
        }
        RunEnd::Last => {
            let right = run.x + run.width - space_advance * skipped as f32;
            (right - window, right)
        }
    };
    let centre = accent.x + accent.width / 2.0;
    let play = accent.width * 0.25;
    if centre < left - play || centre > right + play {
        return None;
    }
    Some(((accent.x + accent.width).min(right) - accent.x.max(left)).max(0.0))
}

/// Composes a spacing accent shown as a text object of its own with the
/// letter it is painted over.
///
/// Some producers set an accented letter as three show operators: the run
/// up to the letter, one glyph of a spacing accent placed by its own text
/// matrix over the letter, and the run from the letter on. The standard
/// Latin encodings carry these accents (`macron`, `acute`, `caron` and
/// their kin) as glyphs with an advance of their own, and the producer
/// spells the letter by overprinting one on the other. Rendered, the accent
/// lands on the letter; read back, it is a fragment of one glyph that
/// starts a fraction of a point to the right of the run it decorates, and
/// the line's fragments sorted along the baseline put it after that whole
/// run: a stray accent a word on, and a letter without its mark.
///
/// So, before the line is ordered, a fragment that is nothing but a spacing
/// accent is matched against the fragments shown just before and just
/// after it: it stands over the last glyph of the one or the first glyph of
/// the other when their baselines lie within 0.3 em and the accent's centre
/// falls within that glyph's advance (`accent_overlap`, which also asks for
/// level, measured runs without right-to-left letters; the glyph with the
/// larger overlap wins when both qualify). That glyph and the accent's
/// combining mark are then replaced by their canonical composition, when
/// Unicode has one character for the pair, and the accent fragment is
/// dropped; a dotless i or j under the accent composes as the dotted letter,
/// the dot being what the accent replaces. An accent over neither
/// neighbour, or over a glyph its mark does not compose with, is left
/// exactly as shown: a lone circumflex or grave in code or mathematics has
/// no letter under it and stays a character of its own.
///
/// `clips` runs parallel to `items` (see `merge_text_items_with_clips`);
/// the entries of dropped fragments go with them. `replaced_text` marks,
/// parallel to `items` too, the runs whose text is a producer's ActualText
/// replacement rather than their glyphs' decoding: such a run's characters
/// need not stand for its glyphs one by one, so it neither gives nor takes
/// an accent.
fn compose_detached_spacing_accents<'a>(
    mut items: Vec<TextItem>,
    clips: &'a [Option<clip_boundaries::ClipRect>],
    replaced_text: &[bool],
) -> (Vec<TextItem>, Cow<'a, [Option<clip_boundaries::ClipRect>]>) {
    let replaced = |index: usize| replaced_text.get(index).copied().unwrap_or(false);
    let mut dropped: Vec<usize> = Vec::new();
    for i in 0..items.len() {
        if replaced(i) {
            continue;
        }
        let Some(mark) = lone_spacing_accent(&items[i]) else {
            continue;
        };
        let neighbours = [
            (i.checked_sub(1), RunEnd::Last),
            (
                Some(i + 1).filter(|&next| next < items.len()),
                RunEnd::First,
            ),
        ];
        // The neighbour whose end glyph the accent lies over and composes
        // with; when it lies over both, the one it overlaps more. A glyph
        // that has no composition with the mark is no candidate, so an
        // accent over a `t` and an `o` composes with the `o` however the
        // overlaps compare.
        let mut best: Option<(usize, std::ops::Range<usize>, char, f32)> = None;
        for (index, end) in neighbours {
            let Some(index) = index else {
                continue;
            };
            if replaced(index) || lone_spacing_accent(&items[index]).is_some() {
                continue;
            }
            let Some(overlap) = accent_overlap(&items[i], &items[index], end) else {
                continue;
            };
            let Some((glyph, range, _)) = end_glyph(&items[index].text, end) else {
                continue;
            };
            // A dotless i or j is the form a typesetter puts an accent over,
            // the dot being what the accent replaces: it composes as the
            // dotted letter.
            let base = match glyph {
                '\u{0131}' => 'i',
                '\u{0237}' => 'j',
                other => other,
            };
            let Some(composed) = unicode_normalization::char::compose(base, mark) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(_, _, _, other)| overlap > *other)
            {
                best = Some((index, range, composed, overlap));
            }
        }
        let Some((index, range, composed, _)) = best else {
            continue;
        };
        items[index]
            .text
            .replace_range(range, composed.encode_utf8(&mut [0; 4]));
        dropped.push(i);
    }
    if dropped.is_empty() {
        return (items, Cow::Borrowed(clips));
    }
    let mut kept = vec![true; items.len()];
    for &index in &dropped {
        kept[index] = false;
    }
    let clips: Vec<Option<clip_boundaries::ClipRect>> = clips
        .iter()
        .zip(&kept)
        .filter(|(_, &keep)| keep)
        .map(|(clip, _)| *clip)
        .collect();
    let mut index = 0;
    items.retain(|_| {
        let keep = kept[index];
        index += 1;
        keep
    });
    (items, Cow::Owned(clips))
}

#[cfg(test)]
pub(crate) fn merge_text_items(items: Vec<TextItem>) -> Vec<TextItem> {
    merge_text_items_with_clips(items, &[], false, &[])
}

/// The separators a number is written with: point, comma, colon, slash
/// and the Arabic decimal and thousands separators.
fn is_number_separator(c: char) -> bool {
    matches!(c, '.' | ',' | ':' | '/' | '\u{066B}' | '\u{066C}')
}

/// A fragment of a number: digits of any script, at least one, with the
/// separators that join them and nothing else.
fn numeric_fragment(text: &str) -> bool {
    let text = text.trim();
    text.chars().any(char::is_numeric)
        && text
            .chars()
            .all(|c| c.is_numeric() || is_number_separator(c))
}

/// A fragment of nothing but number separators: a comma or point shown
/// apart from its digits.
fn separator_fragment(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().all(is_number_separator)
}

/// Whether the junction of two neighbouring fragments lies inside a
/// number: both are number material and at least one holds a digit — a
/// separator shown apart from its digits belongs to the number beside it,
/// while two lone separators make no number.
fn inside_number(a: &str, b: &str) -> bool {
    let material = |text: &str| numeric_fragment(text) || separator_fragment(text);
    material(a) && material(b) && (numeric_fragment(a) || numeric_fragment(b))
}

/// Word-gap floor for a line of right-to-left text shown one glyph per
/// item, from its own gaps: the letter gaps of such a line cluster below
/// its word gaps. Declared advance widths are often off for these fonts,
/// so the fixed em fractions that serve word-by-word runs would put a space
/// after every narrow letter. The gaps are given and the floor returned in
/// em — of the smaller font beside each gap, so a word gap next to a
/// footnote mark or a run of a smaller font is measured by the glyphs it
/// separates. They are split into two classes where the variance between
/// them is largest (Otsu's threshold), and the floor sits between the
/// classes when the upper one is a space's worth apart from the lower —
/// glyphs of a second font with true widths, digits at zero gap beside
/// letters at a small one, form no such class. With one class of gaps the
/// floor is infinite when even the wide ones are mid-word gaps (under the
/// 0.13 em a junction inside a word may open to), so a line of one word
/// holds together; gaps of one class that are wider than that could as
/// well be the word gaps of one-letter words, and the fixed thresholds
/// decide them: `None`.
fn glyph_run_word_gap_floor(gaps: &[f32]) -> Option<f32> {
    if gaps.len() < 3 {
        return None;
    }
    let mut sorted: Vec<f32> = gaps.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let total: f32 = sorted.iter().sum();
    let n = sorted.len() as f32;
    let mut best: Option<(f32, f32, f32)> = None; // (between-class variance, low mean, high mean)
    let mut low_sum = 0.0f32;
    for (k, &value) in sorted.iter().enumerate().take(sorted.len() - 1) {
        low_sum += value;
        let low_n = (k + 1) as f32;
        let high_n = n - low_n;
        let low_mean = low_sum / low_n;
        let high_mean = (total - low_sum) / high_n;
        let variance = low_n * high_n * (high_mean - low_mean).powi(2);
        if best.is_none_or(|(v, _, _)| variance > v) {
            best = Some((variance, low_mean, high_mean));
        }
    }
    let upper_quartile = sorted[sorted.len() * 3 / 4];
    match best {
        Some((_, low_mean, high_mean)) if high_mean - low_mean >= 0.12 && high_mean >= 0.2 => {
            Some((low_mean + high_mean) / 2.0)
        }
        _ if upper_quartile <= 0.13 => Some(f32::INFINITY),
        _ => None,
    }
}

/// Bold and plain runs are kept apart whatever said they were bold: with
/// `PositionOptions::bold_from_weight` the weight class has already had its
/// say in `is_bold` (see `content_stream::read_bold_from_weight`), so runs
/// of different weight split only where the bold verdict changes.
/// `visual_rtl` says the page's right-to-left runs are stored in visual
/// order (see `text_utils::fix_visual_order_rtl`): the lines holding them
/// are then read back into logical order here, as they merge.
/// `replaced_text` marks, parallel to `items` like `clips`, the runs whose
/// text is a producer's ActualText replacement rather than their glyphs'
/// decoding (see `compose_detached_spacing_accents`).
fn merge_text_items_with_clips(
    items: Vec<TextItem>,
    clips: &[Option<clip_boundaries::ClipRect>],
    visual_rtl: bool,
    replaced_text: &[bool],
) -> Vec<TextItem> {
    if items.is_empty() {
        return items;
    }

    // A spacing accent shown as a fragment of its own over a letter of the
    // run before or after it is composed with that letter first, so the
    // line is ordered without it.
    let (items, owned_clips) = compose_detached_spacing_accents(items, clips, replaced_text);
    let clips: &[Option<clip_boundaries::ClipRect>] = &owned_clips;

    // References into `items` remain stable throughout grouping and sorting.
    // Keep clipping provenance private rather than changing the public item type.
    let clip_by_item: HashMap<*const TextItem, clip_boundaries::ClipRect> = items
        .iter()
        .zip(clips)
        .filter_map(|(item, clip)| clip.map(|rect| (item as *const TextItem, rect)))
        .collect();

    let line_groups = group_fragments_into_lines(&items);

    // Each page's own direction: a line with right-to-left letters on a
    // right-to-left page reads right to left even when its Latin letters
    // outnumber them.
    let page_rtl: HashMap<u32, bool> = items
        .iter()
        .map(|item| item.page)
        .collect::<std::collections::HashSet<u32>>()
        .into_iter()
        .map(|page| {
            let rtl = is_rtl_text(items.iter().filter(|i| i.page == page).map(|i| &i.text));
            (page, rtl)
        })
        .collect();

    /// One line's fragments, in the order they are walked.
    struct LineGroup<'a> {
        page: u32,
        y: f32,
        group: Vec<&'a TextItem>,
        /// Each fragment's text as it reads: its own, or on a visual-order
        /// page the stretch of the line's logical text that is its.
        texts: Vec<Cow<'a, str>>,
        preserve_stream_order: bool,
        /// Holds right-to-left letters: walked in reading order, not along +x.
        bidi: bool,
        /// For a `bidi` line, each fragment's position in screen order.
        display_index: Vec<usize>,
        /// For a `bidi` line, the gap between each pair of screen
        /// neighbours, in points, by screen position: from the line's right
        /// edge so far, which a dependent sign does not move.
        display_gaps: Vec<f32>,
        /// For a `bidi` line shown one glyph per fragment, the word-gap
        /// floor its own gaps give (`glyph_run_word_gap_floor`), in em of
        /// the smaller font at a junction.
        glyph_floor: Option<f32>,
    }
    let mut ordered_line_groups: Vec<LineGroup<'_>> = Vec::new();

    // Sort each group by X position (direction-aware), except for lines whose
    // content stream intentionally backtracks to overlay ActualText fragments.
    for (page, y, mut group) in line_groups {
        // A line with right-to-left letters, whichever direction dominates
        // it, is walked in reading order below.
        let bidi = group
            .iter()
            .any(|i| i.text.chars().any(crate::text_utils::is_rtl_char));
        let preserve_stream_order = !bidi && should_preserve_overlapping_stream_order(&group);
        let texts: Vec<Cow<'_, str>>;
        let mut display_index: Vec<usize> = Vec::new();
        let mut display_gaps: Vec<f32> = Vec::new();
        let mut glyph_floor: Option<f32> = None;
        if bidi {
            // Screen order first; the fragments are then taken in the reading
            // order the Unicode Bidirectional Algorithm gives the line, so an
            // embedded Latin phrase or number keeps its own order when the
            // concatenation below bakes the order in. On a visual-order page
            // the fragments' glyphs are the display line itself, and each
            // fragment gets its stretch of the logical text back.
            let rtl_base = crate::text_utils::rtl_line_base(
                &group,
                |i| *i,
                page_rtl.get(&page).copied().unwrap_or(false),
            );
            // Screen order, with a dependent sign kept after the letter it
            // was shown on, as on a left-to-right line.
            sort_along_x_keeping_marks(&mut group);
            // The gaps between screen neighbours, taken once here: two
            // glyphs at one x (a mark over its letter) sort either way, and
            // the reading order below must index the same sequence. A
            // dependent sign sits behind the pen and does not move the
            // line's right edge: the fragment after it is measured from
            // where the letter under it left the pen.
            display_gaps = Vec::with_capacity(group.len().saturating_sub(1));
            let mut right_edge: Option<f32> = None;
            for item in &group {
                let end = item.x + effective_merge_width(item);
                if let Some(edge) = right_edge {
                    display_gaps.push(item.x - edge);
                }
                right_edge = Some(match right_edge {
                    Some(edge) if is_zero_width_mark(item) => edge.max(end),
                    _ => end,
                });
            }
            // Glyph-by-glyph positioned RTL text clusters into words by the
            // line's own gaps: adjacent glyphs abut, word gaps do not, with
            // the floor below where declared widths are off. Junctions
            // inside a number are never word gaps and say nothing about the
            // letters' gaps either (digits of another font keep true
            // widths), so they stay out of the sample, and so do the
            // junctions at a dependent sign, which is no letter of the
            // line. The same floor separates the words for the bidi
            // analysis and, below, for the merge.
            let single_glyphs = group
                .iter()
                .filter(|i| i.text.trim().chars().count() == 1)
                .count();
            if group.len() >= 4 && single_glyphs * 10 >= group.len() * 7 {
                let gaps: Vec<f32> = group
                    .windows(2)
                    .zip(&display_gaps)
                    .filter(|(pair, _)| {
                        !inside_number(&pair[0].text, &pair[1].text)
                            && !is_zero_width_mark(pair[0])
                            && !is_zero_width_mark(pair[1])
                    })
                    .map(|(pair, gap)| gap / pair[0].font_size.min(pair[1].font_size).max(1.0))
                    .collect();
                glyph_floor = glyph_run_word_gap_floor(&gaps);
            }
            let order = crate::bidi::logical_line_order(
                &group,
                |i| i.text.as_str(),
                |i| (i.x, i.width),
                |i| i.font_size,
                |_| visual_rtl,
                glyph_floor,
                rtl_base,
            );
            let reordered: Vec<&TextItem> = order.iter().map(|&(index, _)| group[index]).collect();
            display_index = order.iter().map(|&(index, _)| index).collect();
            texts = order
                .into_iter()
                .map(|(index, logical)| {
                    if visual_rtl {
                        Cow::Owned(logical)
                    } else {
                        Cow::Borrowed(group[index].text.as_str())
                    }
                })
                .collect();
            group = reordered;
        } else {
            if !preserve_stream_order {
                sort_along_x_keeping_marks(&mut group);
            }
            texts = group
                .iter()
                .map(|i| Cow::Borrowed(i.text.as_str()))
                .collect();
        }
        ordered_line_groups.push(LineGroup {
            page,
            y,
            group,
            texts,
            preserve_stream_order,
            bidi,
            display_index,
            display_gaps,
            glyph_floor,
        });
    }

    // Sort groups by page then Y descending (top of page first)
    ordered_line_groups.sort_by(|a, b| a.page.cmp(&b.page).then_with(|| b.y.total_cmp(&a.y)));

    let mut merged = Vec::new();

    for line in &ordered_line_groups {
        let LineGroup {
            group,
            texts,
            preserve_stream_order,
            bidi,
            display_index,
            display_gaps,
            glyph_floor,
            ..
        } = line;
        // A line of right-to-left text is walked in reading order, which
        // runs leftwards through its RTL words and rightwards through an
        // embedded Latin phrase or number, but its word gaps are gaps between
        // neighbours on the page. The gap at a junction of the walk is the
        // gap between the two fragments when they are neighbours in screen
        // order; where the walk jumps into or out of an embedded run, it is
        // the gap between that run's near end and the fragment it turned
        // from — the wider of the two screen gaps that flank the jump, the
        // other being inside a run.
        let junction_gap = |from: usize, to: usize| -> f32 {
            let (a, b) = (display_index[from], display_index[to]);
            if a.abs_diff(b) == 1 {
                return display_gaps[a.min(b)];
            }
            let towards = |at: usize, other: usize| {
                if other > at {
                    display_gaps.get(at).copied()
                } else {
                    at.checked_sub(1).and_then(|k| display_gaps.get(k).copied())
                }
            };
            match (towards(a, b), towards(b, a)) {
                (Some(x), Some(y)) => x.max(y),
                (Some(x), None) | (None, Some(x)) => x,
                (None, None) => 0.0,
            }
        };
        let glyph_floor = *glyph_floor;
        let mut i = 0;
        while i < group.len() {
            let first = group[i];
            let mut text = texts[i].to_string();
            let mut legacy_symbol_rewrite = first.legacy_symbol_rewrite;
            let mut end_x = first.x + effective_merge_width(first);
            let mut box_right = first.x + first.width;
            let mut box_left = first.x;

            // Tracked display text: run-local space floor overrides the
            // fixed thresholds for this run's junctions (see helper).
            let tracked = if *preserve_stream_order || *bidi {
                None
            } else {
                tracked_run_space_floor(group, i)
            };

            let mut j = i + 1;
            while j < group.len() {
                let next = group[j];
                let next_text: &str = &texts[j];
                let gap = if *bidi {
                    junction_gap(j - 1, j)
                } else {
                    next.x - end_x
                };
                // A small-caps junction is mid-word: it both survives the
                // font-size band below and must never take a space.
                let small_caps_join = is_small_caps_continuation(&text, first, next, gap);
                // Must be similar font size, except for genuine small-caps
                // runs, where the shrunken capitals are the same word as the
                // full-size initial (see helper).
                if (next.font_size - first.font_size).abs() > first.font_size * MERGE_FONT_SIZE_BAND
                    && !small_caps_join
                {
                    break;
                }
                // Preserve the existing join behavior at non-bold style
                // boundaries, including italic fragments within formulas.
                if next.is_italic != first.is_italic
                    || next.is_underline != first.is_underline
                    || next.is_strikeout != first.is_strikeout
                {
                    break;
                }
                // Merging walks along +x, which is only the reading direction
                // of upright runs. A vertical stamp whose box bottom shares a
                // baseline with a body line must not be glued onto it, two
                // side-by-side vertical runs are separate lines, and the
                // fragments of an upside-down run read towards -x, so an
                // ascending walk would concatenate them reversed.
                if !first.is_upright() || !next.is_upright() {
                    break;
                }
                // A merged item carries one `advance_known`: never fold a
                // measured run and an estimated one into the same item.
                if next.advance_known != first.advance_known {
                    break;
                }
                let x_gap_max = if *preserve_stream_order && is_standalone_bullet_text(&text) {
                    first.font_size * 1.2
                } else {
                    first.font_size * 0.5
                };
                if gap > x_gap_max {
                    break;
                }
                // A dependent sign is drawn over the glyph before it, as far
                // behind the pen as that glyph is wide: it stays with it.
                if gap < -first.font_size * 0.5
                    && !preserve_stream_order
                    && !is_zero_width_mark(next)
                {
                    break;
                }
                let previous = group[j - 1];
                if clip_boundaries::separated_runs(
                    previous,
                    clip_by_item.get(&(previous as *const TextItem)),
                    next,
                    clip_by_item.get(&(next as *const TextItem)),
                ) {
                    break;
                }
                // Vertically stacked DIGITS at different baselines — the
                // numerator over the denominator of a case fraction ("1"
                // over "3") — are not one number even though they share the
                // 5pt band and overlap in x. Letters keep merging: rotated
                // running headers and diagram labels stack letters too, and
                // splitting those only scatters fragments into body text.
                let digits = |t: &str| !t.is_empty() && t.chars().all(char::is_numeric);
                if !preserve_stream_order
                    && (next.y - first.y).abs() > first.font_size * 0.3
                    && gap < -effective_merge_width(next) * 0.5
                    && digits(text.trim())
                    && digits(next_text.trim())
                {
                    break;
                }
                // Insert space at word boundaries.
                // Base threshold 0.08; raised to 0.13 for lowercase→lowercase
                // junctions to accommodate Tc/Tw character-spacing adjustments
                // that shift advance widths relative to Td positioning.
                let threshold = {
                    let prev_last = text.trim_end().chars().last();
                    let next_first = next_text.trim_start().chars().next();
                    // Never insert space before joining punctuation
                    if next_first.is_some_and(|c| matches!(c, '.' | ',' | ';' | ')' | ']' | '}')) {
                        first.font_size * 0.25
                    } else if prev_last.is_some_and(|c| c.is_lowercase())
                        && next_first.is_some_and(|c| c.is_lowercase())
                    {
                        // Lowercase→lowercase: likely mid-word, use wider threshold
                        first.font_size * 0.13
                    } else if prev_last.is_some_and(crate::text_utils::is_rtl_char)
                        && next_first.is_some_and(crate::text_utils::is_rtl_char)
                    {
                        // Two pieces of one Hebrew or Arabic word, split where
                        // the producer kerned or rejoined a glyph run: as
                        // mid-word as a lowercase junction.
                        first.font_size * 0.13
                    } else {
                        first.font_size * 0.08
                    }
                };
                let needs_bullet_space = *preserve_stream_order
                    && is_standalone_bullet_text(&text)
                    && !next_text.trim().is_empty();
                let effective_threshold = match (tracked, glyph_floor) {
                    (Some((run_end, floor)), _) if j <= run_end => floor,
                    (_, Some(floor_em)) => {
                        floor_em * group[j - 1].font_size.min(next.font_size).max(1.0)
                    }
                    _ => threshold,
                };
                let bold_boundary = next.is_bold != first.is_bold;
                let explicit_bold_space = bold_boundary
                    && (text.ends_with(char::is_whitespace)
                        || next_text.starts_with(char::is_whitespace));
                // Numeric fragments have their own joining thresholds in
                // line assembly. Injecting a word space here would split a
                // number whose decimal point or digits use a bold font.
                let numeric_boundary = bold_boundary
                    && match (text.chars().last(), next_text.chars().next()) {
                        (Some(p), Some(c)) if p.is_ascii_digit() => {
                            c.is_ascii_digit() || matches!(c, '.' | ',' | '%')
                        }
                        (Some('.' | ','), Some(c)) if c.is_ascii_digit() => {
                            let prefix = &text[..text.len() - 1];
                            // A separate decimal glyph can start a fractional
                            // number even without a preceding integer run.
                            prefix.trim().is_empty()
                                || prefix.chars().last().is_some_and(|c| c.is_ascii_digit())
                        }
                        (Some('+' | '-'), Some(c)) => c.is_ascii_digit(),
                        _ => false,
                    };
                if !small_caps_join
                    && (needs_bullet_space || (gap > effective_threshold && !numeric_boundary))
                    && !explicit_bold_space
                    && !text.ends_with(char::is_whitespace)
                {
                    text.push(' ');
                }
                // Keep bold runs separate, but preserve the same word-space
                // decision as an unstyled merge. Otherwise the later line
                // assembler's wider joining threshold can glue words when
                // newly recovered font flags split a previously merged run.
                if bold_boundary {
                    break;
                }
                text.push_str(next_text);
                legacy_symbol_rewrite |= next.legacy_symbol_rewrite;
                box_right = box_right.max(next.x + next.width);
                box_left = box_left.min(next.x);
                let next_end = next.x + effective_merge_width(next);
                // The line's right edge does not move for a dependent sign
                // drawn behind the pen: the fragment after the sign is
                // measured from where the glyph under it left the pen, not
                // from the sign's origin.
                end_x = if *preserve_stream_order || is_zero_width_mark(next) {
                    end_x.max(next_end)
                } else {
                    next_end
                };
                j += 1;
            }

            // Hebrew and Arabic presentation forms stand for letters; now
            // that the text reads in logical order, a ligature's letters
            // come out in reading order, and the joiners that held the
            // characters of one glyph together through the read-back have
            // done their work.
            let mut text = crate::bidi::normalize_presentation_forms(&text).into_owned();
            crate::bidi::strip_glyph_joiners(&mut text);

            merged.push(TextItem {
                text,
                // An estimated run's item is the union of the estimated boxes
                // it merged, including any fragment that backtracked in x; so
                // is a run with right-to-left text, whose fragments were
                // walked in reading order rather than along +x.
                x: if first.advance_known && !*bidi {
                    first.x
                } else {
                    box_left
                },
                y: first.y,
                width: if first.advance_known && !*bidi {
                    end_x - first.x
                } else {
                    box_right - box_left
                },
                height: first.height,
                font: first.font.clone(),
                font_tag: first.font_tag.clone(),
                legacy_symbol_rewrite,
                font_size: first.font_size,
                page: first.page,
                is_bold: first.is_bold,
                is_italic: first.is_italic,
                font_weight: first.font_weight,
                bold_source: first.bold_source,
                fixed_pitch: first.fixed_pitch,
                fill_color: first.fill_color,
                stroke_color: first.stroke_color,
                render_mode: first.render_mode,
                is_underline: first.is_underline,
                is_strikeout: first.is_strikeout,
                rotation: first.rotation,
                advance_known: first.advance_known,
                item_type: first.item_type.clone(),
                mcid: first.mcid,
                baseline_shift: 0.0,
            });

            i = j;
        }
    }

    merged
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
    use crate::types::{ItemType, PdfLine, TextLine};
    use layout::{detect_columns, is_newspaper_layout, ColumnRegion};

    /// Glyph-per-item run at `fs`=12 with the given inter-glyph gap (pt).
    #[test]
    fn numeric_fragments_are_digits_of_any_script_with_their_separators() {
        assert!(numeric_fragment("12"));
        assert!(numeric_fragment(" 1,234.5 "));
        assert!(numeric_fragment("\u{0662}\u{0664}"));
        assert!(numeric_fragment("\u{0663}\u{066B}\u{0665}"));
        assert!(!numeric_fragment(""));
        assert!(!numeric_fragment("a1"));
        assert!(!numeric_fragment("\u{05D0}"));
        assert!(!numeric_fragment("-"));
        // A separator on its own is no number, but belongs to the number
        // beside it.
        assert!(!numeric_fragment(","));
        assert!(separator_fragment(","));
        assert!(inside_number("21", ","));
        assert!(inside_number(",", "847"));
        assert!(inside_number("1", "2"));
        assert!(!inside_number(".", "."));
        assert!(!inside_number("a", "1"));
        assert!(!inside_number("1", "\u{05D0}"));
    }

    #[test]
    fn glyph_run_word_gap_floor_splits_letter_gaps_from_word_gaps() {
        // Letter gaps under a tenth of an em and two word gaps of a third:
        // the floor sits between the classes.
        let gaps = [0.06, 0.08, 0.07, 0.34, 0.09, 0.06, 0.32, 0.08];
        let floor = glyph_run_word_gap_floor(&gaps).expect("two classes");
        assert!(floor > 0.09 && floor < 0.32, "{floor}");
    }

    #[test]
    fn glyph_run_word_gap_floor_holds_one_word_together() {
        // One class of gaps, all mid-word sized: the line is one word.
        let gaps = [0.06, 0.09, 0.11, 0.07, 0.10];
        assert_eq!(glyph_run_word_gap_floor(&gaps), Some(f32::INFINITY));
    }

    #[test]
    fn glyph_run_word_gap_floor_leaves_uniform_wide_gaps_to_the_thresholds() {
        // One class of gaps as wide as word spaces (one-letter words, or a
        // short run whose gaps do not tell): no floor of its own.
        let gaps = [0.24, 0.26, 0.25, 0.25];
        assert_eq!(glyph_run_word_gap_floor(&gaps), None);
        // Too few gaps to read a distribution from.
        assert_eq!(glyph_run_word_gap_floor(&[0.05, 0.3]), None);
    }

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
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    #[test]
    fn explicit_trailing_space_is_not_doubled_across_a_word_gap() {
        // "for " already carries its space run; the 4pt gap (0.33 em) that
        // run left clears the word threshold but must not add a second one.
        let items = vec![
            make_merge_item("for ", 100.0, 21.6),
            make_merge_item("the", 125.6, 21.6),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "for the");
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

    /// `text` shown glyph by glyph at `size`, each glyph its own fragment
    /// with the advance of Helvetica-Bold, as small kerned type is shown.
    fn glyph_fragments(text: &str, x0: f32, y: f32, size: f32) -> Vec<TextItem> {
        let advance = |ch: char| -> f32 {
            let units = match ch {
                'A' | 'U' => 722.0,
                'E' | 'P' | 'S' => 667.0,
                'M' => 833.0,
                'O' => 778.0,
                'W' => 944.0,
                'd' | 'g' | 'n' | 'o' | 'p' => 611.0,
                'i' | 'l' => 278.0,
                'r' => 389.0,
                't' => 333.0,
                _ => 556.0,
            };
            units * size / 1000.0
        };
        let mut items = Vec::new();
        let mut x = x0;
        for ch in text.chars() {
            let w = advance(ch);
            if ch != ' ' {
                let mut item = make_merge_item(&ch.to_string(), x, w);
                item.y = y;
                item.height = size;
                item.font_size = size;
                items.push(item);
            }
            x += if ch == ' ' { 278.0 * size / 1000.0 } else { w };
        }
        items
    }

    /// The merged items' letters along each baseline, top line first,
    /// spaces left out: what a line reads as, whatever its word breaks.
    fn letters_by_baseline(merged: &[TextItem]) -> Vec<(f32, String)> {
        let mut lines: Vec<(f32, Vec<&TextItem>)> = Vec::new();
        for item in merged {
            match lines.iter_mut().find(|(y, _)| (*y - item.y).abs() < 0.01) {
                Some((_, line)) => line.push(item),
                None => lines.push((item.y, vec![item])),
            }
        }
        lines.sort_by(|a, b| b.0.total_cmp(&a.0));
        lines
            .into_iter()
            .map(|(y, mut line)| {
                line.sort_by(|a, b| a.x.total_cmp(&b.x));
                (y, line.iter().map(|i| i.text.replace(' ', "")).collect())
            })
            .collect()
    }

    fn sized_item(text: &str, x: f32, width: f32, y: f32, size: f32) -> TextItem {
        let mut item = make_merge_item(text, x, width);
        item.y = y;
        item.height = size;
        item.font_size = size;
        item
    }

    /// Two lines of 4.7 pt type on a 4.5 pt pitch, each shown glyph by
    /// glyph, keep their own lines and read in order; on a 6 pt pitch they
    /// always did.
    #[test]
    fn small_lines_under_five_points_apart_keep_their_own_lines() {
        let mut items = glyph_fragments("Apples Picked", 100.0, 700.0, 4.7);
        items.extend(glyph_fragments("Oranges Sold", 100.6, 695.5, 4.7));
        assert_eq!(
            letters_by_baseline(&merge_text_items(items)),
            vec![
                (700.0, "ApplesPicked".to_string()),
                (695.5, "OrangesSold".to_string())
            ]
        );
        let mut items = glyph_fragments("Water Usage", 100.0, 650.0, 4.7);
        items.extend(glyph_fragments("Energy Mix", 101.2, 644.0, 4.7));
        assert_eq!(
            letters_by_baseline(&merge_text_items(items)),
            vec![
                (650.0, "WaterUsage".to_string()),
                (644.0, "EnergyMix".to_string())
            ]
        );
    }

    /// The line window follows the type where either fragment is small and
    /// is the 5 pt of old between two fragments of ordinary size: markers
    /// raised a third of an em stay with their line, a run raised 5 pt over
    /// 12 pt type keeps its own line as it always did, a line of small type
    /// under larger type keeps its own, a small glyph shown first does not
    /// narrow the window for the text after it, sloped glyph-by-glyph type
    /// stays one line past the baselines a line records, images keep the
    /// window of old without pulling small type, and fragments without a
    /// type size keep the 5 pt window.
    #[test]
    fn the_line_window_follows_the_type_size() {
        // A 7.97 pt affiliation marker raised 4.3 pt over an 11.96 pt name.
        let name = sized_item("Huo", 100.0, 20.0, 700.0, 11.96);
        let marker = sized_item("1", 120.5, 4.0, 704.3, 7.97);
        assert_eq!(group_fragments_into_lines(&[name, marker]).len(), 1);
        // A 5.5 pt marker raised 3 pt over 8 pt type.
        let word = sized_item("word", 100.0, 16.0, 700.0, 8.0);
        let mark = sized_item("2", 116.2, 3.0, 703.0, 5.5);
        assert_eq!(group_fragments_into_lines(&[word, mark]).len(), 1);
        // A run raised 5 pt over 12 pt type is not within the window,
        // exactly as before.
        let base = sized_item("base", 100.0, 24.0, 500.0, 12.0);
        let raised = sized_item("super", 124.0, 30.0, 505.0, 12.0);
        assert_eq!(group_fragments_into_lines(&[base, raised]).len(), 2);
        // A 7 pt reference mark raised 4 pt over 12 pt text stays with it.
        let word = sized_item("word", 100.0, 24.0, 700.0, 12.0);
        let mark = sized_item("1", 124.2, 3.5, 704.0, 7.0);
        assert_eq!(group_fragments_into_lines(&[word, mark]).len(), 1);
        // A 10 pt line 16 pt under a 40 pt heading, a 4 pt line 4.5 pt
        // under a 7 pt one, and a 4 pt line 4.5 pt under a 12 pt one: type
        // of any size pulls a fragment of smaller type no farther than that
        // fragment's em.
        let heading = sized_item("Title", 100.0, 100.0, 700.0, 40.0);
        let body = sized_item("body", 100.0, 20.0, 684.0, 10.0);
        assert_eq!(group_fragments_into_lines(&[heading, body]).len(), 2);
        let line = sized_item("line", 100.0, 14.0, 700.0, 7.0);
        let tiny = sized_item("tiny", 100.0, 8.0, 695.5, 4.0);
        assert_eq!(group_fragments_into_lines(&[line, tiny]).len(), 2);
        let line = sized_item("line", 100.0, 24.0, 700.0, 12.0);
        let tiny = sized_item("tiny", 100.0, 8.0, 695.5, 4.0);
        assert_eq!(group_fragments_into_lines(&[line, tiny]).len(), 2);
        // Forty 10 pt glyphs on a slope of a tenth of a point each are one
        // line, past the baselines the line records.
        let sloped: Vec<TextItem> = (0..40)
            .map(|i| {
                sized_item(
                    "g",
                    100.0 + 6.0 * i as f32,
                    6.0,
                    700.0 + 0.1 * i as f32,
                    10.0,
                )
            })
            .collect();
        assert_eq!(group_fragments_into_lines(&sloped).len(), 1);
        // An image keeps the window of old: a 2 pt tall one 3 pt under a
        // 10 pt line groups with it, as before, and a tall one 0.4 pt from
        // a line of 0.55 pt type does not pull a second such line, 0.4 pt
        // beyond the first, into the same line.
        let text = sized_item("text", 100.0, 20.0, 700.0, 10.0);
        let mut rule = sized_item("[Image]", 100.0, 200.0, 697.0, 2.0);
        rule.item_type = ItemType::Image;
        rule.font_size = 0.0;
        assert_eq!(group_fragments_into_lines(&[text, rule]).len(), 1);
        let mut figure = sized_item("[Image]", 28.7, 564.0, 435.1, 317.5);
        figure.item_type = ItemType::Image;
        figure.font_size = 0.0;
        let upper = sized_item("materials", 87.9, 20.0, 434.68, 0.55);
        let lower = sized_item("respective", 29.4, 20.0, 434.27, 0.55);
        let beside_figure = [figure, upper, lower];
        let lines = group_fragments_into_lines(&beside_figure);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].2.len(), 1);
        // A 3 pt bullet shown first on the baseline does not narrow the
        // window for the 10 pt text after it: its 6 pt marker raised 3.5 pt
        // still joins.
        let bullet = sized_item("•", 90.0, 3.0, 700.0, 3.0);
        let text = sized_item("text", 100.0, 20.0, 700.0, 10.0);
        let marker = sized_item("2", 120.2, 3.0, 703.5, 6.0);
        assert_eq!(group_fragments_into_lines(&[bullet, text, marker]).len(), 1);
        // A mark joins the text it is raised over though a smaller fragment
        // sits nearer to it: a 4 pt mark 3.8 pt over 12 pt text, with a 4 pt
        // glyph hanging 1 pt below that text.
        let text = sized_item("word", 100.0, 24.0, 700.0, 12.0);
        let hanging = sized_item("n", 125.0, 2.0, 701.0, 4.0);
        let mark = sized_item("1", 124.2, 2.0, 703.8, 4.0);
        assert_eq!(group_fragments_into_lines(&[text, hanging, mark]).len(), 1);
        // Forty-eight glyphs of 4.7 pt type on a slope of a tenth of a point each
        // stay one line as far as the fixed window reaches: the line records
        // every baseline inside it.
        let sloped: Vec<TextItem> = (0..48)
            .map(|i| {
                sized_item(
                    "g",
                    100.0 + 3.0 * i as f32,
                    3.0,
                    700.0 + 0.1 * i as f32,
                    4.7,
                )
            })
            .collect();
        assert_eq!(group_fragments_into_lines(&sloped).len(), 1);
        // Two 4.7 pt lines 4.5 pt apart, and two 6 pt apart.
        let a = sized_item("a", 100.0, 3.0, 700.0, 4.7);
        let b = sized_item("b", 100.0, 3.0, 695.5, 4.7);
        let c = sized_item("c", 100.0, 3.0, 694.0, 4.7);
        assert_eq!(group_fragments_into_lines(&[a.clone(), b]).len(), 2);
        assert_eq!(group_fragments_into_lines(&[a, c]).len(), 2);
        // The same two lines set 3° off level: each run's box is 10 pt
        // tall, but its type is still 4.7 pt.
        let mut a = sized_item("Apples Picked", 100.0, 100.0, 700.0, 4.7);
        let mut b = sized_item("Oranges Sold", 100.0, 100.0, 695.5, 4.7);
        for run in [&mut a, &mut b] {
            run.rotation = 3.0;
            run.height = 10.0;
        }
        assert_eq!(group_fragments_into_lines(&[a, b]).len(), 2);
        // A 4 pt subscript 2.3 pt under its 7 pt base text, on a line
        // whose first fragment sits 2 pt above that text: it joins by the
        // base text's baseline, though the first fragment is 4.3 pt away.
        let first = sized_item("and time horizons", 56.7, 56.0, 332.92, 7.0);
        let base = sized_item("tons of CO", 133.2, 130.0, 330.92, 7.0);
        let sub = sized_item("2", 262.7, 2.2, 328.63, 4.06);
        assert_eq!(group_fragments_into_lines(&[first, base, sub]).len(), 1);
        // No type size: the 5 pt window of old.
        let p = sized_item("p", 100.0, 5.0, 700.0, 0.0);
        let q = sized_item("q", 110.0, 5.0, 695.1, 0.0);
        let r = sized_item("r", 100.0, 5.0, 694.9, 0.0);
        assert_eq!(group_fragments_into_lines(&[p.clone(), q]).len(), 1);
        assert_eq!(group_fragments_into_lines(&[p, r]).len(), 2);
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
    fn merge_follows_the_bold_verdict_not_the_weight_class() {
        use crate::types::BoldSource;

        // A medium-weight label leading a light paragraph: neither is bold,
        // so the runs merge into one item whatever their weight classes,
        // and the item carries its first run's weight.
        let mut label = make_merge_item("Label:", 100.0, 36.0);
        label.font_weight = Some(500);
        let mut body = make_merge_item("body", 137.2, 24.0);
        body.font_weight = Some(300);
        let mut more = make_merge_item("text", 163.6, 24.0);
        more.font_weight = Some(300);
        let items = vec![label.clone(), body.clone(), more.clone()];
        let merged = merge_text_items_with_clips(items, &[], false, &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Label: body text");
        assert_eq!(merged[0].font_weight, Some(500));

        // Once the weight class has made the label bold (the option's pass,
        // `read_bold_from_weight`), the label stays its own item, and the
        // merge still decides the word space the way an unstyled merge
        // would, so the later line assembler does not glue "Label:" onto
        // "body".
        let mut items = vec![label, body, more];
        super::content_stream::read_bold_from_weight(&mut items, 500);
        let apart = merge_text_items_with_clips(items, &[], false, &[]);
        assert_eq!(apart.len(), 2);
        assert_eq!(apart[0].text, "Label: ");
        assert!(apart[0].is_bold);
        assert_eq!(apart[0].bold_source, Some(BoldSource::WeightClass));
        assert_eq!(apart[1].text, "body text");
        assert!(!apart[1].is_bold);
        assert_eq!(apart[1].bold_source, None);

        // Runs of different weight that agree on bold merge: a 700 face
        // beside a 400 face whose name says bold are one item, which keeps
        // the first run's weight class and bold source.
        let mut heavy = make_merge_item("Heavy", 100.0, 30.0);
        heavy.font_weight = Some(700);
        let mut named = make_merge_item("named", 132.0, 30.0);
        named.font_weight = Some(400);
        named.is_bold = true;
        named.bold_source = Some(BoldSource::FontName);
        let mut items = vec![heavy, named];
        super::content_stream::read_bold_from_weight(&mut items, 600);
        let merged = merge_text_items_with_clips(items, &[], false, &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Heavy named");
        assert_eq!(merged[0].font_weight, Some(700));
        assert_eq!(merged[0].bold_source, Some(BoldSource::WeightClass));

        // Runs that know no weight merge as before.
        let unknown = vec![
            make_merge_item("no", 100.0, 12.0),
            make_merge_item("weight", 113.2, 36.0),
        ];
        assert_eq!(
            merge_text_items_with_clips(unknown, &[], false, &[]).len(),
            1
        );
    }

    #[test]
    fn read_bold_from_weight_credits_the_weight_class_after_name_and_flags() {
        use crate::types::BoldSource;

        let mut named = make_merge_item("named", 0.0, 10.0);
        named.font_weight = Some(700);
        named.is_bold = true;
        named.bold_source = Some(BoldSource::FontName);
        let mut painted = make_merge_item("painted", 20.0, 10.0);
        painted.font_weight = Some(650);
        painted.is_bold = true;
        painted.bold_source = Some(BoldSource::Painted);
        let mut light = make_merge_item("light", 40.0, 10.0);
        light.font_weight = Some(300);
        let unknown = make_merge_item("unknown", 60.0, 10.0);
        let mut items = vec![named, painted, light, unknown];
        super::content_stream::read_bold_from_weight(&mut items, 600);
        // The name outranks the weight class; the weight class outranks the
        // paint, since the face itself is heavy.
        assert_eq!(items[0].bold_source, Some(BoldSource::FontName));
        assert_eq!(items[1].bold_source, Some(BoldSource::WeightClass));
        assert!(!items[2].is_bold && items[2].bold_source.is_none());
        assert!(!items[3].is_bold && items[3].bold_source.is_none());

        // The threshold is inclusive and honoured as given.
        let mut items = vec![make_merge_item("x", 0.0, 10.0)];
        items[0].font_weight = Some(500);
        super::content_stream::read_bold_from_weight(&mut items, 500);
        assert!(items[0].is_bold);
        assert_eq!(items[0].bold_source, Some(BoldSource::WeightClass));
    }

    #[test]
    fn recovered_bold_preserves_word_spacing_at_style_boundary() {
        // A 0.1-em gap is a word boundary in merging, but the later line
        // assembler joins gaps below 0.15 em. Recovering bold must not lose
        // the space that the original all-plain merge would have emitted.
        let plain = vec![
            make_merge_item("KEY", 100.0, 24.0),
            make_merge_item("Body", 125.2, 24.0),
        ];
        let original = merge_text_items(plain.clone());
        let mut styled = plain;
        styled[0].is_bold = true;
        let recovered = merge_text_items(styled);
        assert_eq!(original[0].text, "KEY Body");
        assert_eq!(recovered.len(), 2);
        assert!(recovered[0].is_bold && !recovered[1].is_bold);
        assert_eq!(recovered[0].text, "KEY ");
        assert_eq!(recovered[1].text, "Body");
        assert_eq!(
            recovered
                .iter()
                .map(|item| item.text.as_str())
                .collect::<String>(),
            original[0].text
        );
        let line = TextLine {
            items: recovered,
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.1,
        };
        assert_eq!(line.text(), "KEY Body");
        assert_eq!(
            line.text_with_formatting(true, false, false),
            "**KEY** Body"
        );
    }

    #[test]
    fn recovered_bold_keeps_zero_gap_word_fragments_joined() {
        let plain = vec![
            make_merge_item("un", 100.0, 12.0),
            make_merge_item("known", 112.0, 30.0),
        ];
        let original = merge_text_items(plain.clone());
        let mut styled = plain;
        styled[0].is_bold = true;
        let recovered = merge_text_items(styled);
        assert_eq!(original[0].text, "unknown");
        assert_eq!(recovered.len(), 2);
        assert!(recovered[0].is_bold && !recovered[1].is_bold);
        assert_eq!(
            recovered
                .iter()
                .map(|item| item.text.as_str())
                .collect::<String>(),
            original[0].text
        );
        let line = TextLine {
            items: recovered,
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.1,
        };
        assert_eq!(line.text(), "unknown");
        assert_eq!(line.text_with_formatting(true, false, false), "**un**known");
    }

    #[test]
    fn bold_numeric_fragments_keep_line_assembly_spacing() {
        for (parts, gap, expected) in [
            (
                vec![("0", false), (".", true), ("86", false)],
                1.2,
                "0**.**86",
            ),
            (vec![("0.", true), ("86", false)], 1.2, "**0.**86"),
            (vec![(".", true), ("42", false)], 1.2, "**.**42"),
            (
                vec![("12 ", false), (".", true), ("55", false)],
                1.2,
                "12 **.**55",
            ),
            (
                vec![("12", false), (" .", true), ("55", false)],
                1.2,
                "12 **.**55",
            ),
            (vec![("1", false), ("23", true)], 1.2, "1**23**"),
            (vec![("12", true), ("%", false)], 1.2, "**12**%"),
            (vec![("+", true), ("12", false)], 1.2, "**+**12"),
            (
                vec![("1", false), (",", true), ("25", false)],
                1.2,
                "1**,**25",
            ),
            (vec![("Done.", true), ("2", false)], 1.2, "**Done.** 2"),
            (vec![("0. ", true), ("86", false)], 1.2, "**0.** 86"),
            (vec![("0.", true), ("86", false)], 4.8, "**0.** 86"),
            (vec![("KEY ", true), ("Body", false)], 1.2, "**KEY** Body"),
            (vec![("KEY", true), (" Body", false)], 1.2, "**KEY** Body"),
        ] {
            let mut x = 100.0;
            let items = parts
                .into_iter()
                .map(|(text, bold)| {
                    let width = text.len() as f32 * 6.0;
                    let mut item = make_merge_item(text, x, width);
                    item.is_bold = bold;
                    x += width + gap;
                    item
                })
                .collect();
            let line = TextLine {
                items: merge_text_items(items),
                y: 100.0,
                page: 1,
                adaptive_threshold: 0.1,
            };
            assert_eq!(line.text_with_formatting(true, false, false), expected);
            assert_eq!(line.text(), expected.replace("**", ""));
        }
    }

    #[test]
    fn non_bold_style_boundaries_keep_existing_spacing() {
        for style in 0..3 {
            let mut first = make_merge_item("KEY", 100.0, 24.0);
            match style {
                0 => first.is_italic = true,
                1 => first.is_underline = true,
                _ => first.is_strikeout = true,
            }
            let merged = merge_text_items(vec![first, make_merge_item("Body", 125.2, 24.0)]);
            assert_eq!(merged.len(), 2);
            assert_eq!(merged[0].text, "KEY");
            assert_eq!(merged[1].text, "Body");
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
    fn test_group_into_lines() {
        let items = vec![
            TextItem {
                text: "Hello".into(),
                x: 100.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "World".into(),
                x: 160.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "Next line".into(),
                x: 100.0,
                y: 680.0,
                width: 80.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "Prague".into(),
                x: 119.5,
                y: 500.0,
                width: 42.0,
                height: 12.0,
                font: "C2_0".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "Rules".into(),
                x: 161.5,
                y: 500.0,
                width: 35.0,
                height: 12.0,
                font: "C2_0".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "A".into(),
                x: 108.0,
                y: 500.0,
                width: 8.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "V".into(),
                x: 116.0,
                y: 500.0,
                width: 8.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 13.3,
                page: 1,
                is_bold: true,
                is_italic: false,
                font_weight: None,
                bold_source: Some(crate::types::BoldSource::FontName),
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 13.3,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "履行義務".into(),
                x: 124.0,
                y: 500.0,
                width: 32.0,
                height: 12.0,
                font: "C2_0".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "を識別す".into(),
                x: 156.0,
                y: 500.0,
                width: 32.0,
                height: 12.0,
                font: "C2_0".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
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
    fn test_is_rtl_text_weak_chars_do_not_vote() {
        // Arabic-Indic digits (U+0660-0669) are bidi class AN, not strong RTL:
        // a digits-only line must stay neutral, like ASCII-digit lines.
        assert!(!is_rtl_text(["\u{0661}\u{0662}\u{0663}"].iter()));
        // Extended Arabic-Indic digits (U+06F0-06F9) likewise
        assert!(!is_rtl_text(["\u{06F1}\u{06F2}\u{06F3}"].iter()));
        // Arabic decimal/thousands separators (U+066B/U+066C) with digits
        assert!(!is_rtl_text(
            ["\u{0661}\u{066B}\u{0662}\u{0663}\u{066C}\u{0664}"].iter()
        ));
        // Arabic letters alongside Arabic-Indic digits → still RTL
        assert!(is_rtl_text(
            ["\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0661}\u{0662}"].iter()
        ));
        // Arabic letters with combining marks (NSM) → still RTL
        assert!(is_rtl_text(["\u{0645}\u{064E}\u{0631}\u{064D}"].iter()));
        // Combining marks alone are NSM, not strong RTL, even though they are
        // Other_Alphabetic: harakat-only and niqqud-only lines stay neutral
        assert!(!is_rtl_text(["\u{064E}\u{064F}\u{0650}\u{0651}"].iter()));
        assert!(!is_rtl_text(["\u{05B8}\u{05B4}\u{05BC}"].iter()));
        // Marks + Arabic-Indic digits (the full weak-only mix) → still neutral
        assert!(!is_rtl_text(["\u{0661}\u{064E}\u{0662}"].iter()));
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "\u{05D1}".into(), // bet at x=200 (rightmost)
                x: 200.0,
                y: 700.0,
                width: 10.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
        ];
        sort_line_items(&mut items, false);
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
            TextItem {
                text: "World".into(),
                x: 200.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                font: "F1".into(),
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
            },
        ];
        sort_line_items(&mut items, false);
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
                font_tag: String::new(),
                legacy_symbol_rewrite: false,
                font_size: 12.0,
                page,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                fill_color: None,
                stroke_color: None,
                render_mode: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    #[test]
    fn stacked_fraction_glyphs_are_not_merged_into_one_run() {
        // "1" over "3" (a TeX case fraction): same x, baselines 8pt apart,
        // both inside the body line's 5pt band. They are two items; the
        // script detector then sees a superscript and a subscript.
        let items = vec![
            make_item_fs("about 3", 91.9, 511.3, 103.2, 10.0),
            make_item_fs("1", 196.3, 515.2, 3.7, 7.4),
            make_item_fs("3", 196.3, 507.3, 3.7, 7.4),
            make_item_fs("bits", 201.5, 511.3, 18.0, 10.0),
        ];
        let merged = merge_text_items(items);
        let texts: Vec<&str> = merged.iter().map(|i| i.text.as_str()).collect();
        assert!(texts.contains(&"1") && texts.contains(&"3"), "{texts:?}");
        // Side-by-side same-size raised symbol still joins its word.
        let items = vec![
            make_item_fs("Freon", 100.0, 500.0, 26.0, 10.0),
            make_item_fs("®", 126.0, 502.0, 6.0, 10.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Freon®");
        // Stacked LETTERS in the same band (a rotated label beside body
        // text) keep merging as before — only digit pairs are fractions.
        let items = vec![
            make_item_fs("x", 91.9, 511.3, 5.0, 10.0),
            make_item_fs("a", 96.9, 515.2, 3.7, 7.4),
            make_item_fs("r", 96.9, 507.3, 3.7, 7.4),
        ];
        let merged = merge_text_items(items);
        assert!(
            merged.iter().any(|i| i.text.contains("ar")),
            "{:?}",
            merged.iter().map(|i| i.text.as_str()).collect::<Vec<_>>()
        );
    }

    /// Small caps as typesetters emit them: a full-size capital at 9.98pt
    /// immediately followed by shrunken capitals at 6.74pt, touching.
    /// Modelled on `199AD3d.pdf` p.5 ("ROLANDO T. ACOSTA, P.J.").
    #[test]
    fn small_caps_run_merges_into_one_word() {
        let items = vec![
            make_item_fs("R", 144.36, 581.84, 7.20, 9.98),
            make_item_fs("OLANDO", 151.56, 581.84, 30.56, 6.74),
            make_item_fs("T. A", 185.45, 581.84, 17.58, 9.98),
            make_item_fs("COSTA", 203.94, 581.84, 23.15, 6.74),
            make_item_fs(", P.J.", 227.09, 581.84, 22.56, 9.98),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "got {:?}", merged);
        assert_eq!(merged[0].text, "ROLANDO T. ACOSTA, P.J.");
    }

    /// The full two-column row: both names must merge independently and the
    /// 72pt column gap between them must survive as an item boundary.
    #[test]
    fn small_caps_merge_does_not_swallow_a_second_column() {
        let items = vec![
            // Column 1: "ROLANDO T. ACOSTA, P.J." ending at x=249.65
            make_item_fs("R", 144.36, 581.84, 7.20, 9.98),
            make_item_fs("OLANDO", 151.56, 581.84, 30.56, 6.74),
            make_item_fs("T. A", 185.45, 581.84, 17.58, 9.98),
            make_item_fs("COSTA", 203.94, 581.84, 23.15, 6.74),
            make_item_fs(", P.J.", 227.09, 581.84, 22.56, 9.98),
            // Column 2 starts at x=321.96 — a 72pt gap.
            make_item_fs("A", 321.96, 581.84, 7.20, 9.98),
            make_item_fs("NIL", 329.17, 581.84, 12.72, 6.74),
            make_item_fs("C. S", 345.04, 581.84, 19.59, 9.98),
            make_item_fs("INGH", 364.62, 581.84, 19.08, 6.74),
        ];
        let merged = merge_text_items(items);
        let texts: Vec<&str> = merged.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["ROLANDO T. ACOSTA, P.J.", "ANIL C. SINGH"],
            "column gap should keep the two names apart"
        );
    }

    #[test]
    fn small_caps_merge_keeps_word_space_between_same_size_capitals() {
        // Two uppercase words at sizes the merge band already accepts (9.98 and
        // 9.0, a 10% drop) separated by a real word gap. The small-caps path
        // must not claim this junction and swallow the space.
        let items = vec![
            make_item_fs("SEE", 100.0, 500.0, 18.0, 9.98),
            make_item_fs("ALSO", 119.2, 500.0, 24.0, 9.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "got {:?}", merged);
        assert_eq!(merged[0].text, "SEE ALSO");
    }

    #[test]
    fn trailing_digit_is_not_a_capital_awaiting_small_caps() {
        // "...MAZZARELLI1" ends in a footnote marker; the backward search for an
        // uppercase letter must not skip the digit and glue the next run.
        assert!(!is_small_caps_continuation(
            "ANGELA M. MAZZARELLI1",
            &make_item_fs("ANGELA", 100.0, 500.0, 40.0, 9.98),
            &make_item_fs("SHULMAN", 140.0, 500.0, 30.0, 6.74),
            0.0,
        ));
    }

    #[test]
    fn ordinal_suffix_after_a_digit_still_merges() {
        // "TUESDAY, JULY 4" + "TH" is one word in the source; the digit guard
        // must not block the four English ordinal suffixes.
        for suffix in ["TH", "ST", "ND", "RD"] {
            assert!(
                is_small_caps_continuation(
                    "TUESDAY, JULY 4",
                    &make_item_fs("JULY", 100.0, 500.0, 30.0, 12.0),
                    &make_item_fs(suffix, 130.0, 500.0, 8.0, 8.0),
                    0.0,
                ),
                "{suffix} should merge after a digit"
            );
        }
    }

    #[test]
    fn superscript_footnote_marker_is_not_a_small_caps_continuation() {
        // A digit must never qualify — otherwise footnote markers get glued on
        // without the superscript handling.
        assert!(!is_small_caps_continuation(
            "MAZZARELLI",
            &make_item_fs("MAZZARELLI", 100.0, 500.0, 50.0, 9.98),
            &make_item_fs("1", 150.0, 503.0, 3.0, 6.74),
            0.0,
        ));
    }

    #[test]
    fn drop_cap_is_not_a_small_caps_continuation() {
        // Mixed-case body text after a large initial is a drop cap, not small
        // caps.
        assert!(!is_small_caps_continuation(
            "T",
            &make_item_fs("T", 100.0, 500.0, 20.0, 30.0),
            &make_item_fs("he court held", 120.0, 500.0, 60.0, 10.0),
            0.0,
        ));
    }

    #[test]
    fn separate_word_is_not_a_small_caps_continuation() {
        // A real word space disqualifies even when both runs are uppercase.
        let first = make_item_fs("SEE", 100.0, 500.0, 20.0, 9.98);
        let next = make_item_fs("ALSO", 128.0, 500.0, 25.0, 6.74);
        assert!(!is_small_caps_continuation("SEE", &first, &next, 8.0));
    }

    #[test]
    fn lowercase_continuation_is_not_small_caps() {
        assert!(!is_small_caps_continuation(
            "SMALL",
            &make_item_fs("SMALL", 100.0, 500.0, 30.0, 9.98),
            &make_item_fs("caps", 130.0, 500.0, 20.0, 6.74),
            0.0,
        ));
    }

    #[test]
    fn too_small_a_ratio_is_not_small_caps() {
        // 0.4 ratio is a superscript/sub-run, outside the small-caps band.
        assert!(!is_small_caps_continuation(
            "A",
            &make_item_fs("A", 100.0, 500.0, 7.0, 10.0),
            &make_item_fs("BC", 107.0, 500.0, 8.0, 4.0),
            0.0,
        ));
    }

    #[test]
    fn vertical_run_never_merges_with_body_text_sharing_its_baseline() {
        // A 12pt margin stamp whose box bottom lands on a body line's
        // baseline, 2pt left of the body text: same y-group, tiny gap, same
        // font — every merge criterion but orientation says "join". The same
        // items set upright DO merge, so orientation is what blocks it.
        let mut stamp = make_merge_item("arXiv:2301.00001", 12.0, 12.0);
        stamp.height = 120.0;
        stamp.rotation = 90.0;
        let body = make_merge_item("Body text", 26.0, 54.0);

        let merged = merge_text_items(vec![stamp.clone(), body.clone()]);
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(merged[0].text, "arXiv:2301.00001");
        assert_eq!(merged[1].text, "Body text");

        stamp.height = 12.0;
        stamp.rotation = 0.0;
        stamp.width = 96.0;
        let body_after_upright_stamp = make_merge_item("Body text", 110.0, 54.0);
        let merged = merge_text_items(vec![stamp, body_after_upright_stamp]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "arXiv:2301.00001 Body text");
    }

    #[test]
    fn link_annotations_turn_with_a_rotated_page() {
        use crate::tounicode::FontCMaps;
        use lopdf::{dictionary, Object, Stream};

        // Two 90° runs make the page rotated; the link annotation covering
        // the first one (page box x 188..200, y 100..140) must land in the
        // same turned frame as the text: x = old y, y = -(old right edge).
        let mut doc = lopdf::Document::new();
        let widths: Vec<Object> = (0..=255).map(|_| 600.into()).collect();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        });
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (WORLD) Tj ET"
                .to_vec(),
        )));
        let link_id = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Link",
            "Rect" => vec![188.into(), 100.into(), 200.into(), 140.into()],
            "A" => dictionary! {
                "S" => "URI",
                "URI" => Object::string_literal("https://example.com/"),
            },
        });
        // An AcroForm text field drawn over the second run (page box
        // x 228..240, y 100..150) must turn the same way.
        let widget_id = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Widget",
            "FT" => "Tx",
            "T" => Object::string_literal("field"),
            "V" => Object::string_literal("value"),
            "Rect" => vec![228.into(), 100.into(), 240.into(), 150.into()],
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Annots" => vec![Object::Reference(link_id), Object::Reference(widget_id)],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages",
            "Count" => Object::Integer(1),
            "Kids" => vec![Object::Reference(page_id)],
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(widget_id)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, page_rotations, _) =
            extract_positioned_text_from_doc(&doc, &font_cmaps, None).unwrap();
        assert_eq!(page_rotations.get(&1), Some(&geometry::PageRotation::Ccw));
        let field = items
            .iter()
            .find(|i| matches!(i.item_type, ItemType::FormField))
            .expect("form field item");
        assert_eq!(
            (field.x, field.y, field.width, field.height),
            (100.0, -240.0, 50.0, 12.0)
        );
        let hello = items.iter().find(|i| i.text == "HELLO").unwrap();
        assert_eq!(hello.rotation, 0.0);
        assert!((hello.x - 100.0).abs() < 0.01 && (hello.y + 200.0).abs() < 0.01);
        let link = items
            .iter()
            .find(|i| matches!(i.item_type, ItemType::Link(_)))
            .expect("link item");
        assert_eq!(
            (link.x, link.y, link.width, link.height),
            (100.0, -200.0, 40.0, 12.0)
        );
        assert_eq!(link.rotation, 0.0);
    }

    #[test]
    fn upside_down_fragments_are_never_concatenated_reversed() {
        // A 180° run shown as two operators: "HELLO" is painted first, at
        // the right, and "WORLD" continues towards -x. Walking x ascending
        // would merge them as "WORLD HELLO"; they must stay separate.
        let mut hello = make_merge_item("HELLO", 310.0, 50.0);
        hello.rotation = 180.0;
        let mut world = make_merge_item("WORLD", 258.0, 50.0);
        world.rotation = 180.0;
        let merged = merge_text_items(vec![hello, world]);
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert!(merged.iter().any(|i| i.text == "HELLO"));
        assert!(merged.iter().any(|i| i.text == "WORLD"));
    }

    #[test]
    fn measured_and_estimated_runs_never_merge() {
        // A width-less font's run (estimated box) next to a measured one on
        // the same baseline: the pair must stay two items, whichever comes
        // first, so `advance_known` keeps describing each item truthfully.
        let measured = make_merge_item("known", 100.0, 30.0);
        let mut estimated = make_merge_item("guess", 132.0, 30.0);
        estimated.advance_known = false;
        assert_eq!(
            merge_text_items(vec![measured.clone(), estimated.clone()]).len(),
            2
        );
        assert_eq!(merge_text_items(vec![estimated, measured]).len(), 2);
    }

    #[test]
    fn merged_estimated_runs_keep_the_union_of_their_boxes() {
        // Two width-less runs positioned at their estimated advance (half an
        // em per glyph at 12pt) walk like measured ones — gap 0, no space —
        // and the merged item is the union of the two estimated boxes.
        let mut first = make_merge_item("ab", 100.0, 12.0);
        first.advance_known = false;
        let mut second = make_merge_item("cdefg", 112.0, 30.0);
        second.advance_known = false;
        let merged = merge_text_items(vec![first, second]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "abcdefg");
        assert!(!merged[0].advance_known);
        assert!((merged[0].x - 100.0).abs() < 1e-3, "x = {}", merged[0].x);
        assert!(
            (merged[0].width - 42.0).abs() < 1e-3,
            "width = {}",
            merged[0].width
        );
    }

    #[test]
    fn estimated_fragments_that_backtrack_keep_the_union_box() {
        // Stream order "abc" (x 100), "de" (x 94, a backtrack), "fg" (x 118):
        // whichever order the merge walks, the estimated item spans from the
        // leftmost fragment to the rightmost box edge.
        let mut a = make_merge_item("abc", 100.0, 18.0);
        let mut b = make_merge_item("de", 94.0, 10.0);
        let mut c = make_merge_item("fg", 118.0, 12.0);
        for item in [&mut a, &mut b, &mut c] {
            item.advance_known = false;
            item.mcid = Some(1);
        }
        let merged = merge_text_items(vec![a, b, c]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(!merged[0].advance_known);
        assert!((merged[0].x - 94.0).abs() < 1e-3, "x = {}", merged[0].x);
        assert!(
            (merged[0].width - 36.0).abs() < 1e-3,
            "width = {}",
            merged[0].width
        );
    }

    #[test]
    fn side_by_side_vertical_runs_stay_separate_lines() {
        // Two lines of a vertical stamp are adjacent em columns with the
        // same bottom: walking x would glue them into one "line".
        let mut first = make_merge_item("line one", 12.0, 12.0);
        first.height = 80.0;
        first.rotation = 90.0;
        let mut second = make_merge_item("line two", 24.0, 12.0);
        second.height = 80.0;
        second.rotation = 90.0;
        let merged = merge_text_items(vec![first, second]);
        assert_eq!(merged.len(), 2, "{merged:?}");
    }

    #[test]
    fn detached_accent_before_its_letter_composes_with_the_next_run() {
        // An accented letter set as three show operators: the run up to the
        // letter, the macron placed by its own text matrix over the "o"
        // that begins the next run (raised a fraction of a point, as
        // accents are), and the run from that letter on. Sorted along the
        // baseline the accent starts right of "ohoku" and would land after
        // it; composed, the line reads "Tōhoku".
        let mut accent = make_merge_item("\u{00AF}", 108.6, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("T", 100.0, 7.3),
            accent,
            make_merge_item("ohoku", 107.3, 33.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "T\u{014D}hoku");
    }

    #[test]
    fn detached_accent_after_its_letter_composes_with_the_previous_run() {
        // A run positioned glyph by glyph, the accent shown after the letter
        // it stands over: "m", "a", the macron over the "a", "jas".
        let mut accent = make_merge_item("\u{00AF}", 111.3, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("m", 100.0, 10.0),
            make_merge_item("a", 110.0, 6.7),
            accent,
            make_merge_item("jas", 116.7, 15.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "m\u{0101}jas");
    }

    #[test]
    fn detached_accent_over_the_last_glyph_of_a_longer_run_composes_with_it() {
        // "sta", the macron over its closing "a", "tus": the glyph's advance
        // is read as a third of the run's width.
        let mut accent = make_merge_item("\u{00AF}", 113.0, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("sta", 100.0, 18.0),
            accent,
            make_merge_item("tus", 118.0, 16.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "st\u{0101}tus");
    }

    #[test]
    fn spacing_accent_over_no_letter_stays_a_character_of_its_own() {
        // A grave shown a word gap away from the runs on either side is
        // text of its own, a quotation mark or a code delimiter, not an
        // accent.
        let items = vec![
            make_merge_item("code", 100.0, 24.0),
            make_merge_item("\u{0060}", 128.0, 4.0),
            make_merge_item("more", 136.0, 24.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "code \u{0060} more");
    }

    #[test]
    fn spacing_accent_shown_beside_a_letter_is_not_composed() {
        // "a", a circumflex whose advance follows the "a" exactly, "b": an
        // exponent operator set glyph by glyph. The accent stands beside
        // both letters and over neither, and keeps its place in "aˆb".
        let items = vec![
            make_merge_item("a", 100.0, 6.7),
            make_merge_item("\u{02C6}", 106.7, 4.0),
            make_merge_item("b", 110.7, 6.7),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "a\u{02C6}b");
    }

    #[test]
    fn accent_over_two_glyphs_composes_with_the_one_that_can() {
        // The macron overlaps the `t` before it more than the `o` after it;
        // `t` has no composition with a macron, `o` has.
        let mut accent = make_merge_item("\u{00AF}", 104.5, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("t", 100.0, 7.3),
            accent,
            make_merge_item("ohoku", 107.3, 33.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "t\u{014D}hoku");
    }

    #[test]
    fn accent_whose_pair_has_no_composed_character_is_left_as_shown() {
        // A macron over a "t": Unicode has no single character for the
        // pair, so nothing is rewritten and the accent fragment survives.
        let mut accent = make_merge_item("\u{00AF}", 107.0, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("u", 100.0, 6.7),
            accent,
            make_merge_item("tter", 106.7, 16.0),
        ];
        let merged = merge_text_items(items);
        assert!(merged.iter().any(|item| item.text == "utter"), "{merged:?}");
        assert!(
            merged.iter().any(|item| item.text == "\u{00AF}"),
            "{merged:?}"
        );
    }

    #[test]
    fn accent_on_another_baseline_is_not_composed() {
        // The same glyphs with the accent a third of an em above the
        // letters' baseline: not an accent over them.
        let mut accent = make_merge_item("\u{00AF}", 108.6, 4.0);
        accent.y = 704.0;
        let items = vec![
            make_merge_item("T", 100.0, 7.3),
            accent,
            make_merge_item("ohoku", 107.3, 33.0),
        ];
        let merged = merge_text_items(items);
        assert!(
            merged.iter().any(|item| item.text == "Tohoku"),
            "{merged:?}"
        );
        assert!(
            merged.iter().any(|item| item.text == "\u{00AF}"),
            "{merged:?}"
        );
    }

    #[test]
    fn detached_accent_over_a_dotless_i_composes_as_the_dotted_letter() {
        // "Garc", the acute over the dotless i that begins "ıa": the
        // typesetter's form of the letter under an accent reads "García".
        let mut accent = make_merge_item("\u{00B4}", 123.4, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("Garc", 100.0, 24.0),
            accent,
            make_merge_item("\u{0131}a", 124.0, 9.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "Garc\u{00ED}a");
    }

    #[test]
    fn composing_an_accent_drops_its_clip_entry_with_it() {
        let mut accent = make_merge_item("\u{00AF}", 108.6, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("T", 100.0, 7.3),
            accent,
            make_merge_item("ohoku", 107.3, 33.0),
        ];
        let (items, clips) = compose_detached_spacing_accents(items, &[None, None, None], &[]);
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(clips.len(), 2);
        assert_eq!(items[1].text, "\u{014D}hoku");
    }

    #[test]
    fn detached_accent_over_a_wide_capital_composes_with_it() {
        // "Herr", the circumflex centred over the W that begins "Willi": the
        // W is nearly an em wide where the run's letters average less than
        // half of one, so the endpoint window is held open to 0.6 em.
        let mut accent = make_merge_item("\u{02C6}", 130.37, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("Herr", 100.0, 23.34),
            accent,
            make_merge_item("Willi", 126.7, 22.01),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "Herr \u{0174}illi");
    }

    #[test]
    fn detached_accent_over_the_last_letter_before_trailing_spaces_composes() {
        // "Kovac" with its word gap written into the run as two spaces, the
        // caron over the "c", then "and": the spaces are counted at their
        // own advance, not at the run's average, so the "c" is found where
        // it stands. The run keeps its own spaces.
        let mut accent = make_merge_item("\u{02C7}", 128.34, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("Kovac  ", 100.0, 40.06),
            accent,
            make_merge_item("and", 140.06, 20.0),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "Kova\u{010D}  and");
    }

    #[test]
    fn accent_beside_a_replacement_text_run_is_not_composed() {
        // The run after the accent carries a producer's ActualText
        // replacement: its characters need not stand for its glyphs one by
        // one, so the accent is left as shown.
        let mut accent = make_merge_item("\u{00B4}", 108.6, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("T", 100.0, 7.3),
            accent,
            make_merge_item("eal", 107.3, 20.0),
        ];
        let merged = merge_text_items_with_clips(items, &[], false, &[false, false, true]);
        assert!(
            merged.iter().any(|item| item.text.contains('\u{00B4}')),
            "{merged:?}"
        );
        assert!(
            !merged.iter().any(|item| item.text.contains('\u{00E9}')),
            "{merged:?}"
        );
    }

    #[test]
    fn accent_beside_a_run_with_right_to_left_letters_is_not_composed() {
        // A run holding Hebrew letters may have been painted under a
        // mirrored matrix, so which of its glyphs stands at which end of its
        // box is not known from the item: even the Latin "a" at its start
        // under the accent is left alone.
        let mut accent = make_merge_item("\u{00B4}", 108.6, 4.0);
        accent.y = 700.2;
        let items = vec![
            make_merge_item("T", 100.0, 7.3),
            accent,
            make_merge_item("a\u{05D0}\u{05D1}", 107.3, 20.0),
        ];
        let merged = merge_text_items(items);
        assert!(
            merged.iter().any(|item| item.text.contains('\u{00B4}')),
            "{merged:?}"
        );
        assert!(
            !merged.iter().any(|item| item.text.contains('\u{00E1}')),
            "{merged:?}"
        );
    }

    #[test]
    fn accent_over_an_oblique_run_is_not_composed() {
        // A text matrix skewed by a degree and a half (a deskewed OCR layer)
        // makes an axis-aligned box that is no baseline: nothing is composed,
        // whichever of the two carries the skew.
        for skewed in 0..2 {
            let mut accent = make_merge_item("\u{00AF}", 108.6, 4.0);
            accent.y = 700.2;
            let mut run = make_merge_item("ohoku", 107.3, 33.0);
            if skewed == 0 {
                accent.rotation = 1.5;
            } else {
                run.rotation = 1.5;
            }
            let items = vec![make_merge_item("T", 100.0, 7.3), accent, run];
            let merged = merge_text_items(items);
            assert!(
                merged.iter().any(|item| item.text.contains('\u{00AF}')),
                "{merged:?}"
            );
            assert!(
                !merged.iter().any(|item| item.text.contains('\u{014D}')),
                "{merged:?}"
            );
        }
    }

    /// The six glyphs of a word in a script whose subscript letters and
    /// vowel signs have zero advance, shown one `Tm` and `Tj` per glyph at
    /// 12 pt: each sign sits about 0.23 em behind the pen, over the glyph
    /// before it, and the glyph after it starts where the pen was.
    fn signed_word_glyphs() -> Vec<TextItem> {
        vec![
            make_merge_item("\u{1789}\u{17D2}", 20.0, 11.496),
            make_merge_item("\u{1789}", 28.82, 0.0),
            make_merge_item("\u{179C}", 31.496, 4.128),
            make_merge_item("\u{178F}\u{17D2}", 35.624, 9.9),
            make_merge_item("\u{1790}", 42.572, 0.0),
            make_merge_item("\u{17BB}", 45.524, 3.312),
        ]
    }

    #[test]
    fn signs_behind_the_pen_open_no_gap_before_the_next_glyph() {
        // The glyph after a sign is measured from where the glyph under the
        // sign left the pen, not from the sign's origin 2.7 pt behind it;
        // the merged box ends where the last glyph does.
        let merged = merge_text_items(signed_word_glyphs());
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text,
            "\u{1789}\u{17D2}\u{1789}\u{179C}\u{178F}\u{17D2}\u{1790}\u{17BB}"
        );
        assert!((merged[0].x + merged[0].width - 48.836).abs() < 0.01);
    }

    #[test]
    fn a_word_gap_after_a_sign_is_still_a_space() {
        // The second half of the word moved 4 pt (a third of an em) on
        // from where the pen was: a word gap after the sign.
        let mut items = signed_word_glyphs();
        for item in &mut items[3..] {
            item.x += 4.0;
        }
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text,
            "\u{1789}\u{17D2}\u{1789}\u{179C} \u{178F}\u{17D2}\u{1790}\u{17BB}"
        );
    }

    #[test]
    fn a_sign_within_its_glyphs_advance_keeps_its_place_past_a_kerned_glyph() {
        // Shown as glyph, sign, glyph, with the second glyph kerned in
        // 0.5 pt ahead of the pen and the sign 0.2 pt behind it: by x
        // alone the sign would follow the second glyph.
        let items = vec![
            make_merge_item("\u{1780}", 20.0, 11.496),
            make_merge_item("\u{17BB}", 31.3, 0.0),
            make_merge_item("\u{1781}", 31.0, 4.128),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "\u{1780}\u{17BB}\u{1781}");

        // A sign shown before the glyph it is over sorts by its own x, as
        // any fragment does.
        let items = vec![
            make_merge_item("\u{1780}", 20.0, 11.496),
            make_merge_item("\u{17BB}", 40.0, 0.0),
            make_merge_item("\u{1781}", 31.496, 4.128),
            make_merge_item("\u{1782}", 35.624, 9.9),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "\u{1780}\u{1781}\u{1782}\u{17BB}");
    }

    #[test]
    fn a_two_character_sign_inside_a_tracked_run_keeps_the_run_tracked() {
        // Display tracking with a sign that decodes to two combining marks
        // over the `O`: the sign is no letter of the run, the run reads its
        // tracking across it, and the sign stays in its place without a
        // space on either side.
        let mut items = glyph_run("HOW", 100.0, 10.0, 2.3);
        items.insert(2, make_merge_item("\u{0301}\u{0300}", 118.0, 0.0));
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "HO\u{0301}\u{0300}W");
    }

    /// A pointed right-to-left word shown one glyph per item by a producer
    /// walking the line right to left: the first letter, then the second
    /// with its vowel sign 0.3 em behind the pen, then the third.
    fn pointed_rtl_word() -> Vec<TextItem> {
        vec![
            make_merge_item("\u{05D1}", 130.0, 6.0),
            make_merge_item("\u{05D0}", 124.0, 6.0),
            make_merge_item("\u{05B8}", 126.4, 0.0),
            make_merge_item("\u{05DC}", 118.0, 6.0),
        ]
    }

    #[test]
    fn a_sign_on_a_right_to_left_line_follows_its_letter_in_the_reading() {
        // The sign follows the letter it was shown on once the line is
        // read into logical order, with no space on either side, whether
        // the page stores its runs in visual or in logical order.
        for visual in [false, true] {
            let merged = merge_text_items_with_clips(pointed_rtl_word(), &[], visual, &[]);
            assert_eq!(merged.len(), 1, "visual={visual}: {merged:?}");
            assert_eq!(
                merged[0].text, "\u{05D1}\u{05D0}\u{05B8}\u{05DC}",
                "visual={visual}"
            );
        }
        // The next letter kerned in over the end of the pointed one, the
        // sign a hair past it in x: the sign still sorts after its letter.
        let mut items = pointed_rtl_word();
        items[0].x = 129.7;
        items[2].x = 129.8;
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "\u{05D1}\u{05D0}\u{05B8}\u{05DC}");
        // A quarter em from the sign's letter to the next is a word gap,
        // measured from the letter, not from the sign behind the pen.
        let mut items = pointed_rtl_word();
        items[3].x = 115.0;
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].text, "\u{05D1}\u{05D0}\u{05B8} \u{05DC}");
    }

    #[test]
    fn a_longer_run_without_advance_is_hidden_text_not_a_sign() {
        // Many zero-advance glyphs shown at the pen after a display-size
        // number, over which a line of body text starts: hidden text, not
        // a sign. It sorts by its own x, after the body text, and stays an
        // item of its own as before.
        let mut number = make_merge_item("24", 20.0, 120.0);
        number.font_size = 148.0;
        let mut hidden = make_merge_item("++3737++33", 139.0, 0.0);
        hidden.font_size = 148.0;
        let body = make_merge_item("tank with", 100.0, 60.0);
        let merged = merge_text_items(vec![number, hidden, body]);
        let texts: Vec<&str> = merged.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["24", "tank with", "++3737++33"]);
    }

    #[test]
    fn a_sign_deep_behind_the_pen_stays_with_its_glyph() {
        // A sign over the middle of a wide glyph, 0.55 em behind the pen:
        // the backward-gap break that ends an item does not apply to it,
        // and the glyph after it is measured from the pen.
        let items = vec![
            make_merge_item("\u{1780}", 20.0, 14.4),
            make_merge_item("\u{17BB}", 27.8, 0.0),
            make_merge_item("\u{1781}", 34.4, 4.128),
        ];
        let merged = merge_text_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "\u{1780}\u{17BB}\u{1781}");
    }
}
