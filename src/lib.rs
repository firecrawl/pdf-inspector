// Rust 1.95 introduced collapsible_match for `if` inside match arms.
// The content-stream parsers use this pattern extensively (match on operator
// name, then check `in_text_block && !op.operands.is_empty()`). Collapsing
// these into match guards would hurt readability. Allow crate-wide.
#![allow(clippy::collapsible_match)]

//! Smart PDF detection and text extraction using lopdf
//!
//! # Quick start
//!
//! ```no_run
//! // Full processing (detect + extract + markdown) with defaults
//! let result = pdf_inspector::process_pdf("document.pdf").unwrap();
//! println!("type: {:?}, pages: {}", result.pdf_type, result.page_count);
//! if let Some(md) = &result.markdown {
//!     println!("{md}");
//! }
//!
//! // Fast metadata-only detection (no text extraction)
//! let info = pdf_inspector::detect_pdf("document.pdf").unwrap();
//! println!("type: {:?}, pages: {}", info.pdf_type, info.page_count);
//!
//! // Custom options via builder
//! use pdf_inspector::{PdfOptions, ProcessMode};
//! let result = pdf_inspector::process_pdf_with_options(
//!     "document.pdf",
//!     PdfOptions::new().mode(ProcessMode::Analyze),
//! ).unwrap();
//! ```

#[cfg(feature = "python")]
pub mod python;

pub mod adobe_korea1;
pub mod detector;
pub mod extractor;
pub mod glyph_names;
pub mod markdown;
pub mod process_mode;
pub mod structure_tree;
pub mod tables;
pub mod text_utils;
pub mod tounicode;
pub mod types;

pub use detector::{
    detect_pdf_type, detect_pdf_type_mem, detect_pdf_type_mem_with_config,
    detect_pdf_type_with_config, DetectionConfig, PdfType, PdfTypeResult, ScanStrategy,
};
pub use extractor::{extract_text, extract_text_with_positions, extract_text_with_positions_pages};
pub use markdown::{
    to_markdown, to_markdown_from_items, to_markdown_from_items_with_rects, MarkdownOptions,
};
pub use process_mode::ProcessMode;
pub use types::{LayoutComplexity, PdfLine, PdfRect, TextItem};

use lopdf::Document;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use tounicode::FontCMaps;

// =========================================================================
// Result type
// =========================================================================

/// High-level PDF processing result.
#[derive(Debug)]
pub struct PdfProcessResult {
    /// The detected PDF type.
    pub pdf_type: PdfType,
    /// Markdown output (populated in [`ProcessMode::Full`], `None` otherwise).
    pub markdown: Option<String>,
    /// Page count.
    pub page_count: u32,
    /// Processing time in milliseconds.
    pub processing_time_ms: u64,
    /// 1-indexed page numbers that need OCR.
    pub pages_needing_ocr: Vec<u32>,
    /// Title from PDF metadata (if available).
    pub title: Option<String>,
    /// Detection confidence score (0.0–1.0).
    pub confidence: f32,
    /// Layout complexity analysis (tables, multi-column detection).
    pub layout: LayoutComplexity,
    /// `true` when broken font encodings are detected (garbled text,
    /// replacement characters). Clients should fall back to OCR.
    pub has_encoding_issues: bool,
}

// =========================================================================
// Options builder
// =========================================================================

/// Configuration for [`process_pdf_with_options`] and friends.
///
/// Use the builder methods to customise behaviour:
///
/// ```
/// use pdf_inspector::{PdfOptions, ProcessMode};
///
/// let opts = PdfOptions::new()
///     .mode(ProcessMode::Analyze)
///     .pages([1, 3, 5]);
/// ```
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// How far the pipeline should run (default: [`ProcessMode::Full`]).
    pub mode: ProcessMode,
    /// Detection configuration.
    pub detection: DetectionConfig,
    /// Markdown formatting options (only used in [`ProcessMode::Full`]).
    pub markdown: MarkdownOptions,
    /// Optional set of 1-indexed pages to process.  `None` = all pages.
    pub page_filter: Option<HashSet<u32>>,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            mode: ProcessMode::Full,
            detection: DetectionConfig::default(),
            markdown: MarkdownOptions::default(),
            page_filter: None,
        }
    }
}

impl PdfOptions {
    /// Create options with all defaults ([`ProcessMode::Full`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Shorthand for detect-only options.
    pub fn detect_only() -> Self {
        Self {
            mode: ProcessMode::DetectOnly,
            ..Self::default()
        }
    }

    /// Set the processing mode.
    pub fn mode(mut self, mode: ProcessMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set detection configuration.
    pub fn detection(mut self, config: DetectionConfig) -> Self {
        self.detection = config;
        self
    }

    /// Set markdown formatting options.
    pub fn markdown(mut self, options: MarkdownOptions) -> Self {
        self.markdown = options;
        self
    }

    /// Limit processing to specific 1-indexed pages.
    pub fn pages(mut self, pages: impl IntoIterator<Item = u32>) -> Self {
        self.page_filter = Some(pages.into_iter().collect());
        self
    }
}

// =========================================================================
// Public convenience functions
// =========================================================================

/// Process a PDF file with full extraction (detect → extract → markdown).
///
/// This is the most common entry point.  Equivalent to
/// `process_pdf_with_options(path, PdfOptions::new())`.
pub fn process_pdf<P: AsRef<Path>>(path: P) -> Result<PdfProcessResult, PdfError> {
    process_pdf_with_options(path, PdfOptions::new())
}

/// Fast metadata-only detection — no text extraction or markdown generation.
///
/// Equivalent to `process_pdf_with_options(path, PdfOptions::detect_only())`.
pub fn detect_pdf<P: AsRef<Path>>(path: P) -> Result<PdfProcessResult, PdfError> {
    process_pdf_with_options(path, PdfOptions::detect_only())
}

/// Process a PDF file with custom options.
///
/// The document is loaded **once** and shared between detection and extraction.
pub fn process_pdf_with_options<P: AsRef<Path>>(
    path: P,
    options: PdfOptions,
) -> Result<PdfProcessResult, PdfError> {
    let start = std::time::Instant::now();
    validate_pdf_file(&path)?;

    // Load the document once — shared by detection AND extraction.
    let (doc, page_count) = load_document_from_path(&path)?;

    process_document(doc, page_count, options, start)
}

/// Process a PDF from a memory buffer with full extraction.
pub fn process_pdf_mem(buffer: &[u8]) -> Result<PdfProcessResult, PdfError> {
    process_pdf_mem_with_options(buffer, PdfOptions::new())
}

/// Fast metadata-only detection from a memory buffer.
pub fn detect_pdf_mem(buffer: &[u8]) -> Result<PdfProcessResult, PdfError> {
    process_pdf_mem_with_options(buffer, PdfOptions::detect_only())
}

/// Process a PDF from a memory buffer with custom options.
///
/// The buffer is parsed **once** and shared between detection and extraction.
pub fn process_pdf_mem_with_options(
    buffer: &[u8],
    options: PdfOptions,
) -> Result<PdfProcessResult, PdfError> {
    let start = std::time::Instant::now();
    validate_pdf_bytes(buffer)?;

    let (doc, page_count) = load_document_from_mem(buffer)?;

    process_document(doc, page_count, options, start)
}

// =========================================================================
// Deprecated compat shims
// =========================================================================

/// Process a PDF file with custom detection and markdown configuration.
#[deprecated(since = "0.2.0", note = "Use process_pdf_with_options instead")]
pub fn process_pdf_with_config<P: AsRef<Path>>(
    path: P,
    config: DetectionConfig,
    markdown_options: MarkdownOptions,
) -> Result<PdfProcessResult, PdfError> {
    process_pdf_with_options(
        path,
        PdfOptions::new()
            .detection(config)
            .markdown(markdown_options),
    )
}

/// Process a PDF file with custom configuration and optional page filter.
#[deprecated(since = "0.2.0", note = "Use process_pdf_with_options instead")]
pub fn process_pdf_with_config_pages<P: AsRef<Path>>(
    path: P,
    config: DetectionConfig,
    markdown_options: MarkdownOptions,
    page_filter: Option<&HashSet<u32>>,
) -> Result<PdfProcessResult, PdfError> {
    let mut opts = PdfOptions::new()
        .detection(config)
        .markdown(markdown_options);
    opts.page_filter = page_filter.cloned();
    process_pdf_with_options(path, opts)
}

/// Process PDF from memory buffer with custom detection and markdown configuration.
#[deprecated(since = "0.2.0", note = "Use process_pdf_mem_with_options instead")]
pub fn process_pdf_mem_with_config(
    buffer: &[u8],
    config: DetectionConfig,
    markdown_options: MarkdownOptions,
) -> Result<PdfProcessResult, PdfError> {
    process_pdf_mem_with_options(
        buffer,
        PdfOptions::new()
            .detection(config)
            .markdown(markdown_options),
    )
}

// =========================================================================
// Region-based text extraction (for hybrid OCR pipelines)
// =========================================================================

/// Lightweight classification result for routing decisions.
#[derive(Debug)]
pub struct PdfClassification {
    /// The detected PDF type.
    pub pdf_type: PdfType,
    /// Total page count.
    pub page_count: u32,
    /// 0-indexed page numbers that need OCR (scanned/image pages).
    pub pages_needing_ocr: Vec<u32>,
    /// Detection confidence score (0.0–1.0).
    pub confidence: f32,
}

/// Classify a PDF from a memory buffer without extracting text.
/// Returns the PDF type and which pages need OCR (~10-50ms).
pub fn classify_pdf_mem(buffer: &[u8]) -> Result<PdfClassification, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, page_count) = load_document_from_mem(buffer)?;
    let detection = detector::detect_from_document(&doc, page_count, &DetectionConfig::default())?;
    Ok(PdfClassification {
        pdf_type: detection.pdf_type,
        page_count,
        // Convert from 1-indexed to 0-indexed for caller convenience
        pages_needing_ocr: detection.pages_needing_ocr.iter().map(|&p| p - 1).collect(),
        confidence: detection.confidence,
    })
}

// =========================================================================
// Per-page markdown extraction
// =========================================================================

/// Per-page markdown extraction result.
#[derive(Debug)]
pub struct PageMarkdown {
    /// 0-indexed page number.
    pub page: u32,
    /// Formatted markdown for this page.
    pub markdown: String,
    /// `true` when text on this page is unreliable (GID-encoded fonts,
    /// encoding issues, garbage text, or empty extraction).
    pub needs_ocr: bool,
}

/// Combined per-page markdown extraction and layout classification result.
#[derive(Debug)]
pub struct PagesExtractionResult {
    /// Per-page markdown results.
    pub pages: Vec<PageMarkdown>,
    /// 1-indexed pages where tables were detected.
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where multi-column layout was detected.
    pub pages_with_columns: Vec<u32>,
    /// 1-indexed pages that need OCR (scanned/image-based).
    pub pages_needing_ocr: Vec<u32>,
    /// True if any page has tables or columns.
    pub is_complex: bool,
}

/// Extract formatted markdown for pages of a PDF, with layout
/// classification metadata.
///
/// Unlike [`process_pdf_mem`] which returns one concatenated markdown string,
/// this returns per-page markdown so callers can mix direct extraction
/// (for simple text pages) with GPU OCR (for complex/scanned pages).
///
/// When `pages` is `None`, every page (0-indexed, in document order) is
/// returned. When `Some(&[...])`, only the listed 0-indexed pages are
/// returned, in the caller's order.
///
/// Font statistics are computed from the full document so header
/// detection thresholds are consistent regardless of which pages are
/// requested. Per-page `needs_ocr` is set when the page has GID-encoded
/// fonts, encoding issues, or garbage text.
///
/// Layout complexity (tables, columns) is computed from the full document
/// at near-zero cost since the items/rects/lines are already in memory.
pub fn extract_pages_markdown_mem(
    buffer: &[u8],
    pages: Option<&[u32]>,
) -> Result<PagesExtractionResult, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, page_count) = load_document_from_mem(buffer)?;
    let font_cmaps = FontCMaps::from_doc(&doc);

    // Extract ALL pages to get accurate, document-wide font stats.
    let ((all_items, all_rects, all_lines), page_thresholds, gid_pages) =
        extractor::extract_positioned_text_from_doc(&doc, &font_cmaps, None)?;

    // Compute layout complexity from full document (near-zero cost).
    let complexity = compute_layout_complexity(&all_items, &all_rects, &all_lines);

    // Compute font stats from full document (cross-page consistency).
    let font_stats = markdown::analysis::calculate_font_stats_from_items(&all_items);

    // When caller doesn't specify pages, return every page in document order.
    let all_pages: Vec<u32>;
    let pages_slice: &[u32] = match pages {
        Some(p) => p,
        None => {
            all_pages = (0..page_count).collect();
            &all_pages
        }
    };

    let mut results = Vec::with_capacity(pages_slice.len());
    let mut pages_needing_ocr = Vec::new();

    for &page_0idx in pages_slice {
        // Out-of-range pages → empty + needs_ocr
        if page_0idx >= page_count {
            pages_needing_ocr.push(page_0idx + 1);
            results.push(PageMarkdown {
                page: page_0idx,
                markdown: String::new(),
                needs_ocr: true,
            });
            continue;
        }

        let page_1idx = page_0idx + 1;

        // Filter items/rects for this page only
        let page_items: Vec<TextItem> = all_items
            .iter()
            .filter(|i| i.page == page_1idx)
            .cloned()
            .collect();

        let page_rects: Vec<PdfRect> = all_rects
            .iter()
            .filter(|r| r.page == page_1idx)
            .cloned()
            .collect();

        let has_gid = gid_pages.contains(&page_1idx);

        // Build markdown with document-wide font stats
        let options = MarkdownOptions {
            base_font_size: Some(font_stats.most_common_size),
            include_page_numbers: false,
            strip_headers_footers: false,
            ..MarkdownOptions::default()
        };

        let md = markdown::to_markdown_from_items_with_rects_and_lines(
            page_items,
            options,
            &page_rects,
            &[],
            &page_thresholds,
            None,
            &[],
        );

        let needs_ocr = md.trim().is_empty()
            || has_gid
            || is_garbage_text(&md)
            || is_cid_garbage(&md)
            || detect_encoding_issues(&md);

        if needs_ocr {
            pages_needing_ocr.push(page_1idx);
        }

        results.push(PageMarkdown {
            page: page_0idx,
            markdown: if needs_ocr { String::new() } else { md },
            needs_ocr,
        });
    }

    Ok(PagesExtractionResult {
        pages: results,
        pages_with_tables: complexity.pages_with_tables,
        pages_with_columns: complexity.pages_with_columns,
        pages_needing_ocr,
        is_complex: complexity.is_complex,
    })
}

/// Path-based wrapper for [`extract_pages_markdown_mem`].
///
/// Reads the PDF from disk and extracts per-page markdown. Pass `None` for
/// `pages` to return every page in document order, or `Some(&[...])` to
/// restrict to specific 0-indexed pages (in caller-supplied order).
pub fn extract_pages_markdown<P: AsRef<Path>>(
    path: P,
    pages: Option<&[u32]>,
) -> Result<PagesExtractionResult, PdfError> {
    validate_pdf_file(&path)?;
    let buffer = std::fs::read(path.as_ref())?;
    extract_pages_markdown_mem(&buffer, pages)
}

// =========================================================================
// Region-based text extraction (for hybrid OCR pipelines)
// =========================================================================

/// Result for a single region's text extraction.
#[derive(Debug)]
pub struct RegionText {
    /// Extracted text (may be empty if region has no text items).
    pub text: String,
    /// `true` when the text should not be trusted and OCR should be used instead.
    /// Set when: the region is empty, the page uses GID-encoded fonts, or the
    /// extracted text fails garbage/encoding checks.
    pub needs_ocr: bool,
}

/// Result for a page's region extractions.
#[derive(Debug)]
pub struct PageRegionResult {
    /// 0-indexed page number.
    pub page: u32,
    /// Per-region results, parallel to the input regions.
    pub regions: Vec<RegionText>,
}

/// Extract text within bounding-box regions from a PDF in memory.
///
/// This is designed for hybrid OCR pipelines: a layout model detects regions
/// in a rendered page image, and this function extracts the PDF text that
/// falls within each region — avoiding GPU OCR for text-based pages.
///
/// Each region result includes a `needs_ocr` flag that is set when extraction
/// quality is suspect (empty text, GID-encoded fonts, garbage/encoding issues).
///
/// # Arguments
///
/// * `buffer` — PDF file bytes
/// * `page_regions` — list of `(page_number_0indexed, Vec<[x1, y1, x2, y2]>)`.
///   Coordinates are in **PDF points** with **top-left origin** (matching typical
///   layout model output after coordinate conversion).
///
/// # Returns
///
/// A `Vec<PageRegionResult>` parallel to `page_regions`.
pub fn extract_text_in_regions_mem(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
) -> Result<Vec<PageRegionResult>, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();

    // Build a set of pages we need to extract (1-indexed for lopdf)
    let needed_pages: HashSet<u32> = page_regions.iter().map(|(p, _)| p + 1).collect();

    // Fast mode: skip expensive TrueType font fallback parsing.
    // Fonts that can't be decoded from ToUnicode alone will produce empty/garbage
    // text, triggering needs_ocr=true → GPU OCR fallback in the pipeline.
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed_pages));

    // Extract text items for needed pages only
    let mut items_by_page: HashMap<u32, Vec<TextItem>> = HashMap::new();
    let mut page_heights: HashMap<u32, f32> = HashMap::new();
    let mut gid_pages: HashSet<u32> = HashSet::new();
    let mut page_thresholds: HashMap<u32, f32> = HashMap::new();
    let mut rotated_pages: HashSet<u32> = HashSet::new();

    for (page_num, &page_id) in pages.iter() {
        if !needed_pages.contains(page_num) {
            continue;
        }

        // Get page height from MediaBox for coordinate flip
        let height = get_page_height(&doc, page_id).unwrap_or(792.0);
        page_heights.insert(*page_num, height);

        // Extract text items for this page
        let ((mut items, _rects, _lines), has_gid, coords_rotated) =
            extractor::content_stream::extract_page_text_items(
                &doc,
                page_id,
                *page_num,
                &font_cmaps,
                false,
            )?;
        let threshold = text_utils::fix_letterspaced_items(&mut items);
        if threshold > 0.10 {
            page_thresholds.insert(*page_num, threshold);
        }
        if has_gid {
            gid_pages.insert(*page_num);
        }
        if coords_rotated {
            rotated_pages.insert(*page_num);
        }
        items_by_page.insert(*page_num, items);
    }

    // For each page's regions, filter and assemble text
    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = items_by_page.get(&page_1idx);
        let page_h = page_heights.get(&page_1idx).copied().unwrap_or(792.0);
        let _page_has_gid = gid_pages.contains(&page_1idx);
        let adaptive_threshold = page_thresholds.get(&page_1idx).copied().unwrap_or(0.10);
        let coords = if rotated_pages.contains(&page_1idx) {
            RegionCoordSpace::Rotated90Ccw
        } else {
            RegionCoordSpace::Standard
        };

        let mut page_results = Vec::with_capacity(regions.len());

        for rect in regions {
            let [rx1, ry1, rx2, ry2] = *rect;

            let text = match items {
                Some(items) => collect_text_in_region_with_options(
                    items,
                    rx1,
                    ry1,
                    rx2,
                    ry2,
                    page_h,
                    coords,
                    adaptive_threshold,
                ),
                None => String::new(),
            };

            // Check per-region text quality instead of blanket page-level
            // GID rejection. A GID font in a logo elsewhere on the page
            // shouldn't force GPU OCR for clean text regions.
            let needs_ocr = text.trim().is_empty()
                || is_garbage_text(&text)
                || is_cid_garbage(&text)
                || detect_encoding_issues(&text);

            page_results.push(RegionText { text, needs_ocr });
        }

        results.push(PageRegionResult {
            page: *page_0idx,
            regions: page_results,
        });
    }

    Ok(results)
}

/// Extract tables within bounding-box regions from a PDF in memory.
///
/// Similar to [`extract_text_in_regions_mem`] but runs table detection on items
/// within each region and returns markdown pipe-tables instead of flat text.
///
/// When table structure is detected, `text` contains a markdown pipe-table and
/// `needs_ocr` is `false`. When no table is found (too few items, poor alignment,
/// GID fonts, etc.), `text` is empty and `needs_ocr` is `true` so the caller can
/// fall back to GPU OCR.
pub fn extract_tables_in_regions_mem(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
) -> Result<Vec<PageRegionResult>, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();

    let needed_pages: HashSet<u32> = page_regions.iter().map(|(p, _)| p + 1).collect();
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed_pages));

    let mut items_by_page: HashMap<u32, Vec<TextItem>> = HashMap::new();
    let mut page_heights: HashMap<u32, f32> = HashMap::new();
    let mut gid_pages: HashSet<u32> = HashSet::new();
    let mut page_thresholds: HashMap<u32, f32> = HashMap::new();
    let mut rotated_pages: HashSet<u32> = HashSet::new();

    for (page_num, &page_id) in pages.iter() {
        if !needed_pages.contains(page_num) {
            continue;
        }
        let height = get_page_height(&doc, page_id).unwrap_or(792.0);
        page_heights.insert(*page_num, height);

        let ((mut items, _rects, _lines), has_gid, coords_rotated) =
            extractor::content_stream::extract_page_text_items(
                &doc,
                page_id,
                *page_num,
                &font_cmaps,
                false,
            )?;
        let threshold = text_utils::fix_letterspaced_items(&mut items);
        if threshold > 0.10 {
            page_thresholds.insert(*page_num, threshold);
        }
        if has_gid {
            gid_pages.insert(*page_num);
        }
        if coords_rotated {
            rotated_pages.insert(*page_num);
        }
        items_by_page.insert(*page_num, items);
    }

    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = items_by_page.get(&page_1idx);
        let page_h = page_heights.get(&page_1idx).copied().unwrap_or(792.0);
        let _page_has_gid = gid_pages.contains(&page_1idx);
        let coords = if rotated_pages.contains(&page_1idx) {
            RegionCoordSpace::Rotated90Ccw
        } else {
            RegionCoordSpace::Standard
        };

        let mut page_results = Vec::with_capacity(regions.len());

        for rect in regions {
            let [rx1, ry1, rx2, ry2] = *rect;

            // Note: we intentionally DO NOT bail on page_has_gid here.
            // The GID flag means some font on the page uses unresolvable
            // glyph IDs, but that font may only appear in a logo or
            // header — not in the table region. Instead we let the
            // per-region text quality checks (is_garbage_text, is_cid_garbage,
            // detect_encoding_issues) reject based on the actual extracted
            // content. This avoids rejecting clean tables just because an
            // unrelated decorative font on the same page is GID-encoded.

            let matched: Vec<TextItem> = match items {
                Some(items) => {
                    let bounds = region_bounds(rx1, ry1, rx2, ry2, page_h, coords);
                    items
                        .iter()
                        .filter(|item| region_overlaps_item(item, bounds))
                        .cloned()
                        .collect()
                }
                None => Vec::new(),
            };

            if matched.is_empty() {
                page_results.push(RegionText {
                    text: String::new(),
                    needs_ocr: true,
                });
                continue;
            }

            // Compute base_font_size as most common font size in the region
            let base_font_size = {
                let mut freq: HashMap<i32, usize> = HashMap::new();
                for item in &matched {
                    *freq.entry((item.font_size * 10.0) as i32).or_default() += 1;
                }
                freq.into_iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(size, _)| size as f32 / 10.0)
                    .unwrap_or(12.0)
            };

            // Run heuristic table detection; skip_body_font = false since
            // the layout model already identified this region as a table.
            let detected = tables::detect_tables(&matched, base_font_size, false);

            if let Some(table) = detected.into_iter().next() {
                let md = tables::table_to_markdown(&table);
                if md.trim().is_empty() {
                    page_results.push(RegionText {
                        text: String::new(),
                        needs_ocr: true,
                    });
                } else {
                    // needs_ocr fires on any of:
                    //   - garbage text (non-alphanumeric heavy)
                    //   - CID/Latin-1 mojibake
                    //   - encoding issues (U+FFFD, dollar-as-space)
                    //   - structural giveaways that the table is partial /
                    //     mis-detected (numeric "header", empty header cells,
                    //     duplicate header cells). Caught GLM-OCR-as-baseline
                    //     scoring 0 TEDS on real prod tables in eval.
                    // Layout model already identified this region as a table,
                    // so use relaxed partial-table checks (layout_assisted=true).
                    let needs_ocr = is_garbage_text(&md)
                        || is_cid_garbage(&md)
                        || detect_encoding_issues(&md)
                        || looks_like_partial_table_ex(&md, true);
                    page_results.push(RegionText {
                        text: if needs_ocr { String::new() } else { md },
                        needs_ocr,
                    });
                }
            } else {
                page_results.push(RegionText {
                    text: String::new(),
                    needs_ocr: true,
                });
            }
        }

        results.push(PageRegionResult {
            page: *page_0idx,
            regions: page_results,
        });
    }

    Ok(results)
}

/// Region-scoped vector grid detection result for TSR-compatible callers.
#[derive(Debug, Clone)]
pub struct VectorGridDetection {
    /// HTML-like structure tokens consumed by the TSR path.
    pub structure_tokens: Vec<String>,
    /// One crop-pixel bbox per `<td>` token, in document order.
    pub cell_bboxes: Vec<Vec<f32>>,
}

#[derive(Clone, Copy)]
enum VectorGridSource {
    Rects,
    Lines,
}

/// Detect a vector ruled-line / rectangle grid inside one page region.
///
/// The returned shape intentionally matches [`TsrTableInput`]'s structure
/// fields so callers can hand it to `extract_tables_with_structure_*` and let
/// the existing PDF-text cell fill path populate contents.
pub fn detect_vector_grid_in_region_mem(
    buffer: &[u8],
    page_idx: u32,
    region_pdf_pt_bbox: [f32; 4],
    render_dpi: f32,
) -> Result<Option<VectorGridDetection>, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();

    let page_1idx = page_idx + 1;
    let Some(&page_id) = pages.get(&page_1idx) else {
        return Ok(None);
    };

    let needed_pages = HashSet::from([page_1idx]);
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed_pages));
    let page_h = get_page_height(&doc, page_id).unwrap_or(792.0);
    let ((mut items, rects, lines), _has_gid, coords_rotated) =
        extractor::content_stream::extract_page_text_items(
            &doc,
            page_id,
            page_1idx,
            &font_cmaps,
            false,
        )?;
    text_utils::fix_letterspaced_items(&mut items);

    let coords = if coords_rotated {
        RegionCoordSpace::Rotated90Ccw
    } else {
        RegionCoordSpace::Standard
    };
    if matches!(coords, RegionCoordSpace::Rotated90Ccw) {
        // TODO: add a rotated-page vector-grid fixture before enabling this.
        // The TSR crop contract is top-left page coordinates, while rotated
        // extraction normalizes vector geometry into a synthetic coordinate
        // space. Returning None is safer than emitting misleading bboxes.
        return Ok(None);
    }
    let [rx1, ry1, rx2, ry2] = region_pdf_pt_bbox;
    let bounds = region_bounds(rx1, ry1, rx2, ry2, page_h, coords);

    let items_in_region: Vec<TextItem> = items
        .iter()
        .filter(|item| region_overlaps_item(item, bounds))
        .cloned()
        .collect();
    if items_in_region.is_empty() {
        return Ok(None);
    }

    let rects_in_region: Vec<PdfRect> = rects
        .iter()
        .filter(|rect| region_overlaps_rect(rect, bounds))
        .cloned()
        .collect();
    let lines_in_region: Vec<PdfLine> = lines
        .iter()
        .filter(|line| region_overlaps_line(line, bounds))
        .cloned()
        .collect();

    // Match the existing geometry pipeline priority: rect-backed grids first,
    // then line-backed grids. The detector output is only the validity gate;
    // bboxes below are rebuilt from the filtered vector geometry because Table
    // stores centers for some rect paths and row starts for line paths.
    let (rect_tables, _) =
        tables::detect_tables_from_rects(&items_in_region, &rects_in_region, page_1idx);
    for table in rect_tables {
        if let Some(result) = vector_grid_result_from_table(
            &table,
            VectorGridSource::Rects,
            &rects_in_region,
            &lines_in_region,
            region_pdf_pt_bbox,
            render_dpi,
            page_h,
            coords,
        ) {
            return Ok(Some(result));
        }
    }

    let line_tables =
        tables::detect_tables_from_lines(&items_in_region, &lines_in_region, page_1idx);
    for table in line_tables {
        if let Some(result) = vector_grid_result_from_table(
            &table,
            VectorGridSource::Lines,
            &rects_in_region,
            &lines_in_region,
            region_pdf_pt_bbox,
            render_dpi,
            page_h,
            coords,
        ) {
            return Ok(Some(result));
        }
    }

    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn vector_grid_result_from_table(
    table: &tables::Table,
    source: VectorGridSource,
    rects: &[PdfRect],
    lines: &[PdfLine],
    crop_pdf_pt_bbox: [f32; 4],
    render_dpi: f32,
    page_height: f32,
    coord_space: RegionCoordSpace,
) -> Option<VectorGridDetection> {
    let num_rows = table.cells.len();
    let num_cols = table.cells.first().map_or(0, Vec::len);
    if num_rows == 0 || num_cols == 0 || table.cells.iter().any(|row| row.len() != num_cols) {
        return None;
    }

    let (x_edges, y_edges) = match source {
        VectorGridSource::Rects => rect_grid_edges(rects, num_cols, num_rows)
            .or_else(|| inferred_grid_edges(table, rects, lines, num_cols, num_rows))?,
        VectorGridSource::Lines => line_grid_edges(table, lines, num_cols, num_rows)
            .or_else(|| inferred_grid_edges(table, rects, lines, num_cols, num_rows))?,
    };

    if x_edges.len() != num_cols + 1 || y_edges.len() != num_rows + 1 {
        return None;
    }

    let mut structure_tokens = Vec::with_capacity(num_rows * (num_cols + 2) + 2);
    let mut cell_bboxes = Vec::with_capacity(num_rows * num_cols);
    structure_tokens.push("<table>".to_string());

    // TODO: refactor vector detectors to return normalized `(x_edges, y_edges)`.
    // Today `Table.columns` / `Table.rows` have detector-specific semantics,
    // so this export reconstructs edges from the validated table plus geometry.
    // Keep v1 structural output uniform: the downstream TSR text-fill path
    // does not require header semantics, and reliable header detection can be
    // layered later without changing the geometry contract.
    for r in 0..num_rows {
        structure_tokens.push("<tr>".to_string());
        for c in 0..num_cols {
            structure_tokens.push("<td></td>".to_string());
            let bbox_px = extracted_cell_to_crop_px(
                [x_edges[c], y_edges[r + 1], x_edges[c + 1], y_edges[r]],
                crop_pdf_pt_bbox,
                render_dpi,
                page_height,
                coord_space,
            )?;
            if !crop_px_bbox_is_plausible(bbox_px, crop_pdf_pt_bbox, render_dpi) {
                return None;
            }
            cell_bboxes.push(bbox_px.to_vec());
        }
        structure_tokens.push("</tr>".to_string());
    }

    structure_tokens.push("</table>".to_string());
    Some(VectorGridDetection {
        structure_tokens,
        cell_bboxes,
    })
}

fn crop_px_bbox_is_plausible(
    bbox_px: [f32; 4],
    crop_pdf_pt_bbox: [f32; 4],
    render_dpi: f32,
) -> bool {
    let ppi = if render_dpi > 0.0 {
        render_dpi / 72.0
    } else {
        1.0
    };
    let crop_w = (crop_pdf_pt_bbox[2] - crop_pdf_pt_bbox[0]).abs() * ppi;
    let crop_h = (crop_pdf_pt_bbox[3] - crop_pdf_pt_bbox[1]).abs() * ppi;
    let slack = 1.0;
    bbox_px[0] >= -slack
        && bbox_px[1] >= -slack
        && bbox_px[2] <= crop_w + slack
        && bbox_px[3] <= crop_h + slack
}

#[cfg(test)]
mod vector_grid_tests {
    use super::crop_px_bbox_is_plausible;

    /// Regression for `forecast_table_chart.pdf` (doc 128 from the
    /// opendataloader-bench corpus). The table has six visual columns, but
    /// text X-clustering in the cell-rect fallback previously split wide
    /// columns into ten spurious columns.
    #[test]
    fn forecast_table_chart_six_cols() {
        use crate::extractor::content_stream::extract_page_text_items;
        use crate::tables::detect_tables_from_rects;
        use crate::tounicode::FontCMaps;
        use lopdf::Document;
        use std::collections::HashSet;
        use std::fs;

        let path = "tests/fixtures/forecast_table_chart.pdf";
        let buf = fs::read(path).unwrap();
        let doc = Document::load_mem(&buf).unwrap();
        let pages = doc.get_pages();
        let &page_id = pages.get(&1).unwrap();
        let needed: HashSet<u32> = HashSet::from([1]);
        let cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed));
        let ((items, rects, _lines), _has_gid, _rotated) =
            extract_page_text_items(&doc, page_id, 1, &cmaps, false).unwrap();

        let (rect_tables, _) = detect_tables_from_rects(&items, &rects, 1);
        assert_eq!(rect_tables.len(), 1, "expected one rect-detected table");
        let t = &rect_tables[0];
        assert_eq!(
            t.columns.len(),
            6,
            "doc 128 has a 6-column table; got {} edges: {:?}",
            t.columns.len(),
            t.columns
        );
        assert!(
            t.rows.len() >= 14 && t.rows.len() <= 17,
            "row count drift: {}",
            t.rows.len()
        );
    }

    /// Helper: load a fixture PDF and run the rect-based table detector.
    fn detect_rect_tables_in_fixture(path: &str) -> Vec<crate::tables::Table> {
        use crate::extractor::content_stream::extract_page_text_items;
        use crate::tables::detect_tables_from_rects;
        use crate::tounicode::FontCMaps;
        use lopdf::Document;
        use std::collections::HashSet;
        use std::fs;

        let buf = fs::read(path).unwrap();
        let doc = Document::load_mem(&buf).unwrap();
        let pages = doc.get_pages();
        let &page_id = pages.get(&1).unwrap();
        let needed: HashSet<u32> = HashSet::from([1]);
        let cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed));
        let ((items, rects, _lines), _has_gid, _rotated) =
            extract_page_text_items(&doc, page_id, 1, &cmaps, false).unwrap();

        let (rect_tables, _) = detect_tables_from_rects(&items, &rects, 1);
        rect_tables
    }

    /// Regression for the prose-in-a-frame failure mode introduced by the
    /// shaded-header detection lift (PR #76). The accessory_building permit
    /// form has a paragraph of legal text laid out in a 2-column justified
    /// block; the new fill-priority + dedup changes start producing rects
    /// for it, and the rect detector then admits a 10×2 fake table where
    /// every cell holds a sentence fragment ("I agree to comply...", "I",
    /// "It is the property owner's responsibility..."). This test asserts
    /// the detector REJECTS that fake table — only the real 5×3 form data
    /// table (TYPE / SIZE / SETBACKS) should survive. See pdf-evals PR #30
    /// for the original score regression that surfaced this.
    #[test]
    fn accessory_building_rejects_prose_in_frame() {
        let tables = detect_rect_tables_in_fixture(
            "tests/fixtures/accessory_building_permit_prose_frame.pdf",
        );
        // Real form data table (TYPE / SIZE / SETBACKS) must still be detected.
        let data_table = tables.iter().find(|t| t.columns.len() == 3);
        assert!(
            data_table.is_some(),
            "expected to keep the 5×3 TYPE/SIZE/SETBACKS data table; got {:?}",
            tables
                .iter()
                .map(|t| (t.rows.len(), t.columns.len()))
                .collect::<Vec<_>>()
        );
        // Prose paragraph laid out in 2 cols must NOT be detected as a table.
        // If the rejection regresses, the 10×2 fake table reappears and
        // produces fragmented markdown like "I agree to comply..." | "I"
        // that fragments mid-sentence.
        let prose_table = tables.iter().find(|t| t.columns.len() == 2);
        assert!(
            prose_table.is_none(),
            "expected the 10×2 prose-in-frame block to be rejected; got rows×cols = {:?}",
            prose_table.map(|t| (t.rows.len(), t.columns.len()))
        );
    }

    /// Regression for `greencomp_competence.pdf` — a 2-column "Area / Competence"
    /// glossary with a green-shaded header row and plain (line-drawn) body cells.
    /// Mirrors the production failure cohort #1 (Contractions glossary) and #6
    /// (BIO 350 course header): a few colored header rects sit in a horizontal
    /// strip while body rows are drawn with `m`/`l` operators, so the rect
    /// cluster has only 2 Y-edges and `try_build_grid` rejects.
    ///
    /// IGNORED: lifting this shape required the exact-duplicate early-dedup
    /// (PR #76 first iteration), which had broad collateral damage on
    /// SEC 10-K TOCs and similar docs that draw rule-rects above + below
    /// section dividers (production diff: 0001104659-25-093871 lost its
    /// TOC structure, perf-graph data table, and qualifications matrix).
    /// Re-enable once a more surgical lift exists in `try_build_grid` or
    /// `snap_edges` that handles cell-border + inner-fill + text-bg rect
    /// triplets without page-wide dedup.
    #[test]
    #[ignore]
    fn greencomp_competence_two_cols() {
        let tables = detect_rect_tables_in_fixture("tests/fixtures/greencomp_competence.pdf");
        assert!(
            !tables.is_empty(),
            "expected at least one rect-detected table for shaded-header + plain-body shape"
        );
        let t = tables
            .iter()
            .max_by_key(|t| t.rows.len() * t.columns.len())
            .unwrap();
        assert_eq!(
            t.columns.len(),
            2,
            "GreenComp competence is a 2-column table; got {}: {:?}",
            t.columns.len(),
            t.columns
        );
        assert!(
            t.rows.len() >= 6,
            "expected at least 6 rows of competences; got {}",
            t.rows.len()
        );
    }

    /// Regression for `upstage_key_functions.pdf` — a 4-column "Service Stage /
    /// Function Name / Explanation / Expected Benefit" table with a blue-shaded
    /// header band plus alternating row backgrounds. Mirrors production crops
    /// #2 (Parameter / Value with alternating blue rows) and #7 (Spanish XML
    /// schema with shaded header). Currently `pdf2md` returns zero markdown
    /// table rows.
    #[test]
    fn upstage_key_functions_four_cols() {
        let tables = detect_rect_tables_in_fixture("tests/fixtures/upstage_key_functions.pdf");
        assert!(
            !tables.is_empty(),
            "expected at least one rect-detected table for shaded-header + alt-row shape"
        );
        let t = tables
            .iter()
            .max_by_key(|t| t.rows.len() * t.columns.len())
            .unwrap();
        assert_eq!(
            t.columns.len(),
            4,
            "Service Flow is a 4-column table; got {}: {:?}",
            t.columns.len(),
            t.columns
        );
        assert!(
            t.rows.len() >= 8,
            "expected at least 8 visible body rows; got {}",
            t.rows.len()
        );
    }

    #[test]
    fn test_crop_px_bbox_is_plausible_bounds() {
        let crop = [10.0, 20.0, 110.0, 220.0];

        assert!(crop_px_bbox_is_plausible(
            [0.0, 0.0, 100.0, 200.0],
            crop,
            72.0
        ));
        assert!(crop_px_bbox_is_plausible(
            [0.0, 0.0, 200.0, 400.0],
            crop,
            144.0
        ));
        assert!(!crop_px_bbox_is_plausible(
            [-2.0, 0.0, 50.0, 100.0],
            crop,
            72.0
        ));
        assert!(!crop_px_bbox_is_plausible(
            [0.0, -2.0, 50.0, 100.0],
            crop,
            72.0
        ));
        assert!(!crop_px_bbox_is_plausible(
            [0.0, 0.0, 102.0, 200.0],
            crop,
            72.0
        ));
        assert!(!crop_px_bbox_is_plausible(
            [0.0, 0.0, 100.0, 202.0],
            crop,
            72.0
        ));

        // Boundary values at the existing 1px slack should remain valid.
        assert!(crop_px_bbox_is_plausible(
            [-1.0, -1.0, 101.0, 201.0],
            crop,
            72.0
        ));

        // Non-positive DPI falls back to 1.0 ppi, so crop points equal pixels.
        assert!(crop_px_bbox_is_plausible(
            [0.0, 0.0, 100.0, 200.0],
            crop,
            0.0
        ));
        assert!(crop_px_bbox_is_plausible(
            [0.0, 0.0, 100.0, 200.0],
            crop,
            -144.0
        ));
        assert!(!crop_px_bbox_is_plausible(
            [0.0, 0.0, 102.0, 200.0],
            crop,
            -144.0
        ));
    }
}

fn line_grid_edges(
    table: &tables::Table,
    lines: &[PdfLine],
    num_cols: usize,
    num_rows: usize,
) -> Option<(Vec<f32>, Vec<f32>)> {
    if lines.is_empty() || table.columns.len() != num_cols + 1 || table.rows.len() != num_rows {
        return None;
    }

    let angle_tolerance = 2.0_f32.to_radians().tan();
    let mut ys = Vec::new();

    for line in lines {
        let dx = (line.x2 - line.x1).abs();
        let dy = (line.y2 - line.y1).abs();
        let length = (dx * dx + dy * dy).sqrt();
        if length < 20.0 {
            continue;
        }
        if dx > 0.01 && dy / dx <= angle_tolerance {
            ys.push((line.y1 + line.y2) * 0.5);
        }
    }

    let mut x_edges = table.columns.clone();
    x_edges.sort_by(|a, b| a.total_cmp(b));

    let snapped_y = snap_vector_edges(ys, true);
    let mut y_edges = Vec::with_capacity(num_rows + 1);
    for &row_top in &table.rows {
        let matched = snapped_y
            .iter()
            .copied()
            .find(|y| (*y - row_top).abs() <= 3.0)
            .unwrap_or(row_top);
        y_edges.push(matched);
    }

    let last_top = *y_edges.last()?;
    let bottom = snapped_y
        .iter()
        .copied()
        .filter(|y| *y < last_top - 3.0)
        .max_by(|a, b| a.total_cmp(b))?;
    y_edges.push(bottom);

    if x_edges.len() == num_cols + 1 && y_edges.len() == num_rows + 1 {
        Some((x_edges, y_edges))
    } else {
        None
    }
}

fn rect_grid_edges(
    rects: &[PdfRect],
    num_cols: usize,
    num_rows: usize,
) -> Option<(Vec<f32>, Vec<f32>)> {
    if rects.is_empty() {
        return None;
    }

    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for rect in rects {
        let (x1, y1, x2, y2) = normalized_rect_edges(rect);
        if (x2 - x1) < 5.0 || (y2 - y1) < 5.0 {
            continue;
        }
        xs.push(x1);
        xs.push(x2);
        ys.push(y1);
        ys.push(y2);
    }

    let x_edges = snap_vector_edges(xs, false);
    let y_edges = snap_vector_edges(ys, true);
    if x_edges.len() == num_cols + 1 && y_edges.len() == num_rows + 1 {
        Some((x_edges, y_edges))
    } else {
        None
    }
}

fn inferred_grid_edges(
    table: &tables::Table,
    rects: &[PdfRect],
    lines: &[PdfLine],
    num_cols: usize,
    num_rows: usize,
) -> Option<(Vec<f32>, Vec<f32>)> {
    let bounds = vector_geometry_bounds(rects, lines);
    let x_edges = if table.columns.len() == num_cols + 1 {
        let mut edges = table.columns.clone();
        edges.sort_by(|a, b| a.total_cmp(b));
        Some(edges)
    } else {
        infer_ascending_edges(&table.columns, num_cols, bounds.map(|b| (b.x_min, b.x_max)))
    }?;

    let y_edges = if table.rows.len() == num_rows + 1 {
        let mut edges = table.rows.clone();
        edges.sort_by(|a, b| b.total_cmp(a));
        Some(edges)
    } else {
        infer_descending_edges(&table.rows, num_rows, bounds.map(|b| (b.y_min, b.y_max)))
    }?;

    Some((x_edges, y_edges))
}

fn infer_ascending_edges(
    positions: &[f32],
    expected_centers: usize,
    bounds: Option<(f32, f32)>,
) -> Option<Vec<f32>> {
    if positions.len() != expected_centers || positions.is_empty() {
        return None;
    }
    let mut centers = positions.to_vec();
    centers.sort_by(|a, b| a.total_cmp(b));
    if centers.len() == 1 {
        return None;
    }

    let mut edges = Vec::with_capacity(centers.len() + 1);
    let first_gap = centers[1] - centers[0];
    let last_gap = centers[centers.len() - 1] - centers[centers.len() - 2];
    let left = bounds
        .map(|(min, _)| min)
        .filter(|min| min.is_finite() && *min < centers[0])
        .unwrap_or(centers[0] - first_gap * 0.5);
    let right = bounds
        .map(|(_, max)| max)
        .filter(|max| max.is_finite() && *max > *centers.last().unwrap())
        .unwrap_or(*centers.last().unwrap() + last_gap * 0.5);

    edges.push(left);
    for pair in centers.windows(2) {
        edges.push((pair[0] + pair[1]) * 0.5);
    }
    edges.push(right);

    strictly_ordered(&edges, false).then_some(edges)
}

fn infer_descending_edges(
    positions: &[f32],
    expected_centers: usize,
    bounds: Option<(f32, f32)>,
) -> Option<Vec<f32>> {
    if positions.len() != expected_centers || positions.is_empty() {
        return None;
    }
    let mut centers = positions.to_vec();
    centers.sort_by(|a, b| b.total_cmp(a));
    if centers.len() == 1 {
        return None;
    }

    let mut edges = Vec::with_capacity(centers.len() + 1);
    let first_gap = centers[0] - centers[1];
    let last_gap = centers[centers.len() - 2] - centers[centers.len() - 1];
    let top = bounds
        .map(|(_, max)| max)
        .filter(|max| max.is_finite() && *max > centers[0])
        .unwrap_or(centers[0] + first_gap * 0.5);
    let bottom = bounds
        .map(|(min, _)| min)
        .filter(|min| min.is_finite() && *min < *centers.last().unwrap())
        .unwrap_or(*centers.last().unwrap() - last_gap * 0.5);

    edges.push(top);
    for pair in centers.windows(2) {
        edges.push((pair[0] + pair[1]) * 0.5);
    }
    edges.push(bottom);

    strictly_ordered(&edges, true).then_some(edges)
}

fn snap_vector_edges(mut values: Vec<f32>, descending: bool) -> Vec<f32> {
    values.retain(|v| v.is_finite());
    values.sort_by(|a, b| a.total_cmp(b));

    let mut snapped: Vec<f32> = Vec::new();
    let mut cluster: Vec<f32> = Vec::new();
    for value in values {
        if cluster
            .last()
            .is_some_and(|last| (value - *last).abs() <= 3.0)
        {
            cluster.push(value);
        } else {
            if !cluster.is_empty() {
                snapped.push(cluster.iter().sum::<f32>() / cluster.len() as f32);
            }
            cluster = vec![value];
        }
    }
    if !cluster.is_empty() {
        snapped.push(cluster.iter().sum::<f32>() / cluster.len() as f32);
    }
    if descending {
        snapped.sort_by(|a, b| b.total_cmp(a));
    }
    snapped
}

fn strictly_ordered(values: &[f32], descending: bool) -> bool {
    values.windows(2).all(|pair| {
        pair[0].is_finite()
            && pair[1].is_finite()
            && if descending {
                pair[0] > pair[1]
            } else {
                pair[0] < pair[1]
            }
    })
}

fn normalized_rect_edges(rect: &PdfRect) -> (f32, f32, f32, f32) {
    let x2 = rect.x + rect.width;
    let y2 = rect.y + rect.height;
    (
        rect.x.min(x2),
        rect.y.min(y2),
        rect.x.max(x2),
        rect.y.max(y2),
    )
}

fn vector_geometry_bounds(rects: &[PdfRect], lines: &[PdfLine]) -> Option<RegionBounds> {
    let mut bounds: Option<RegionBounds> = None;
    let mut include = |x1: f32, y1: f32, x2: f32, y2: f32| {
        let next = RegionBounds {
            x_min: x1.min(x2),
            y_min: y1.min(y2),
            x_max: x1.max(x2),
            y_max: y1.max(y2),
        };
        bounds = Some(if let Some(prev) = bounds {
            RegionBounds {
                x_min: prev.x_min.min(next.x_min),
                y_min: prev.y_min.min(next.y_min),
                x_max: prev.x_max.max(next.x_max),
                y_max: prev.y_max.max(next.y_max),
            }
        } else {
            next
        });
    };

    for rect in rects {
        let (x1, y1, x2, y2) = normalized_rect_edges(rect);
        include(x1, y1, x2, y2);
    }
    for line in lines {
        include(line.x1, line.y1, line.x2, line.y2);
    }

    bounds
}

fn extracted_cell_to_crop_px(
    bbox: [f32; 4],
    crop_pdf_pt_bbox: [f32; 4],
    render_dpi: f32,
    page_height: f32,
    coord_space: RegionCoordSpace,
) -> Option<[f32; 4]> {
    let [x1, y1, x2, y2] = extracted_bbox_to_page_top_left(bbox, page_height, coord_space);
    if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
        return None;
    }
    if x1 >= x2 || y1 >= y2 {
        return None;
    }

    let ppi = if render_dpi > 0.0 {
        render_dpi / 72.0
    } else {
        1.0
    };
    let [crop_x1, crop_y1, _, _] = crop_pdf_pt_bbox;
    Some([
        (x1 - crop_x1) * ppi,
        (y1 - crop_y1) * ppi,
        (x2 - crop_x1) * ppi,
        (y2 - crop_y1) * ppi,
    ])
}

fn extracted_bbox_to_page_top_left(
    bbox: [f32; 4],
    page_height: f32,
    coord_space: RegionCoordSpace,
) -> [f32; 4] {
    let [x1, y1, x2, y2] = bbox;
    let x_min = x1.min(x2);
    let x_max = x1.max(x2);
    let y_min = y1.min(y2);
    let y_max = y1.max(y2);

    match coord_space {
        RegionCoordSpace::Standard => [x_min, page_height - y_max, x_max, page_height - y_min],
        RegionCoordSpace::Rotated90Ccw => {
            [-y_max, page_height - x_max, -y_min, page_height - x_min]
        }
    }
}

// =========================================================================
// Region-based table extraction with external structure recovery (TSR)
// =========================================================================

/// Input for [`extract_tables_with_structure_mem`]: one cropped table region
/// plus the raw structure-recovery output for it.
///
/// The structure tokens and bboxes are typically produced by an external
/// table-structure recognition model (e.g. SLANet on PaddleOCR) running on
/// a rendered crop of the page. pdf-inspector uses the structure to lay out
/// the cells and pulls the cell text from the native PDF — no OCR involved.
#[derive(Debug, Clone)]
pub struct TsrTableInput {
    /// 0-indexed page number where the crop was taken from.
    pub page: u32,
    /// Crop bbox on the page, `[x1, y1, x2, y2]` in PDF points with
    /// **top-left origin** (matches the layout model's coordinate space).
    pub crop_pdf_pt_bbox: [f32; 4],
    /// DPI the crop image was rendered at (e.g. `200.0`). Used to convert
    /// cell bboxes from image-pixels back to PDF points.
    pub render_dpi: f32,
    /// Raw structure tokens emitted by the TSR model, in document order.
    /// See [`tables::structured::parse_structure`] for the accepted grammar.
    pub structure_tokens: Vec<String>,
    /// One bbox per cell (in document order, parallel to the cell open-tags
    /// in `structure_tokens`). May be 4-element `[x1,y1,x2,y2]` or
    /// 8-element 4-corner polygon, in **crop image-pixel space**.
    pub cell_bboxes: Vec<Vec<f32>>,
}

/// Extract structured cells using externally-supplied structure recovery.
///
/// For each input, this:
/// 1. Pairs each cell open-tag in `structure_tokens` with the next bbox in
///    `cell_bboxes` (document order), tracking row/col with rowspan/colspan
///    awareness.
/// 2. Converts each cell bbox from crop image-pixels into page PDF-points.
/// 3. Pulls the cell's text by overlap-testing PDF text items inside that
///    bbox — same primitives used by [`extract_text_in_regions_mem`].
///
/// Returns one `Vec<StructuredCell>` per input, in input order. Each cell
/// carries its (row, col, rowspan, colspan, is_header) metadata, the
/// extracted text, and its page-PDF-pt bbox so callers can do their own
/// rendering, debug overlays, or per-cell post-processing.
///
/// Inputs whose page is out of range or whose tokens parse to zero cells
/// produce an empty `Vec`.
///
/// See [`extract_tables_with_structure_mem`] if you just want the rendered
/// markdown.
pub fn extract_tables_with_structure_cells_mem(
    buffer: &[u8],
    inputs: &[TsrTableInput],
) -> Result<Vec<Vec<tables::StructuredCell>>, PdfError> {
    use tables::structured::{
        cell_px_to_page_pt, normalize_cell_bands, parse_structure, polygon_to_aabb, StructuredCell,
    };

    validate_pdf_bytes(buffer)?;
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();

    let needed_pages: HashSet<u32> = inputs.iter().map(|t| t.page + 1).collect();
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed_pages));

    let mut items_by_page: HashMap<u32, Vec<TextItem>> = HashMap::new();
    let mut page_heights: HashMap<u32, f32> = HashMap::new();
    let mut page_thresholds: HashMap<u32, f32> = HashMap::new();
    let mut rotated_pages: HashSet<u32> = HashSet::new();

    for (page_num, &page_id) in pages.iter() {
        if !needed_pages.contains(page_num) {
            continue;
        }
        let height = get_page_height(&doc, page_id).unwrap_or(792.0);
        page_heights.insert(*page_num, height);

        let ((mut items, _rects, _lines), _has_gid, coords_rotated) =
            extractor::content_stream::extract_page_text_items(
                &doc,
                page_id,
                *page_num,
                &font_cmaps,
                false,
            )?;
        let threshold = text_utils::fix_letterspaced_items(&mut items);
        if threshold > 0.10 {
            page_thresholds.insert(*page_num, threshold);
        }
        if coords_rotated {
            rotated_pages.insert(*page_num);
        }
        items_by_page.insert(*page_num, items);
    }

    let mut results: Vec<Vec<StructuredCell>> = Vec::with_capacity(inputs.len());

    for input in inputs {
        let page_1idx = input.page + 1;
        let Some(items) = items_by_page.get(&page_1idx) else {
            // Out-of-range page or page with no extractable text — emit empty.
            results.push(Vec::new());
            continue;
        };
        let page_h = page_heights.get(&page_1idx).copied().unwrap_or(792.0);
        let adaptive_threshold = page_thresholds.get(&page_1idx).copied().unwrap_or(0.10);
        let coords = if rotated_pages.contains(&page_1idx) {
            RegionCoordSpace::Rotated90Ccw
        } else {
            RegionCoordSpace::Standard
        };

        let crop_origin = [input.crop_pdf_pt_bbox[0], input.crop_pdf_pt_bbox[1]];

        let slots = parse_structure(&input.structure_tokens);
        if slots.is_empty() {
            results.push(Vec::new());
            continue;
        }

        let mut cells: Vec<StructuredCell> = Vec::with_capacity(slots.len());
        for slot in &slots {
            let page_pt_bbox;

            if let Some(coords_arr) = input.cell_bboxes.get(slot.bbox_idx) {
                if let Some(aabb_px) = polygon_to_aabb(coords_arr) {
                    page_pt_bbox = cell_px_to_page_pt(aabb_px, input.render_dpi, crop_origin);
                } else {
                    page_pt_bbox = [0.0, 0.0, 0.0, 0.0];
                }
            } else {
                page_pt_bbox = [0.0, 0.0, 0.0, 0.0];
            }

            cells.push(StructuredCell {
                row: slot.row,
                col: slot.col,
                rowspan: slot.rowspan,
                colspan: slot.colspan,
                is_header: slot.is_header,
                text: String::new(),
                page_pt_bbox,
            });
        }

        normalize_cell_bands(&mut cells);

        // Stage 1: exclusive per-token assignment. Each PDF text item is
        // first split into whitespace-separated tokens with estimated x
        // positions (see `split_item_into_token_subitems`). For each token
        // we find the cell(s) whose (band-clamped) bbox satisfies the
        // strict membership rule (`tsr_region_contains_item`: center
        // inside OR >=60% overlap on both axes). If multiple cells
        // qualify, the closest-center wins. Tokens that don't land in any
        // cell are eligible for stage-2 orphan recovery.
        //
        // Per-token (rather than per-item) routing is what prevents the
        // dense-grid collapse bug: a row rendered as one wide Tj
        // ("Marshall Islands 0.9 0.9 0.9") produces one TextItem whose
        // CENTER lies in only one cell, so per-item routing parks the
        // whole row in that single cell and leaves the rest of the row
        // empty. Per-token routing distributes the words to whichever
        // cells their (estimated) positions fall into. Single-token items
        // collapse to a one-element token list and behave exactly as
        // before.
        //
        // The token-level exclusivity (one token → one cell) still
        // prevents the cell-overlap bug where SLANet emits cells whose
        // y-extents overlap between rows. Closest-center disambiguation
        // routes each token to the correct row.

        // Pre-compute each cell's bounds + center (in PDF-pt-flipped space)
        // so we don't redo the work per token.
        let cell_meta: Vec<Option<(RegionBounds, f32, f32)>> = cells
            .iter()
            .map(|cell| {
                let [x1, y1, x2, y2] = cell.page_pt_bbox;
                if x1 >= x2 || y1 >= y2 {
                    return None;
                }
                let bounds = region_bounds(x1, y1, x2, y2, page_h, coords);
                let cx = (bounds.x_min + bounds.x_max) * 0.5;
                let cy = (bounds.y_min + bounds.y_max) * 0.5;
                Some((bounds, cx, cy))
            })
            .collect();

        let mut per_cell_items: Vec<Vec<TextItem>> = vec![Vec::new(); cells.len()];
        // Tokens of an item that did NOT land in any cell during stage 1.
        // These are the orphan candidates handed to `tsr_assign_orphan_items`.
        // Using token-grain orphan candidates (rather than the original wide
        // item) lets stage 2 recover individual words that fell just outside
        // their cell's clamped band, without re-attributing already-claimed
        // tokens.
        let mut orphan_token_subitems: Vec<TextItem> = Vec::new();

        for item in items.iter() {
            let token_subitems = split_item_into_token_subitems(item);
            for token_item in token_subitems {
                let token_w = text_utils::effective_width(&token_item);
                let token_cx = token_item.x + token_w * 0.5;
                let token_cy = token_item.y + token_item.height * 0.5;
                let mut best: Option<(usize, f32)> = None;
                for (cell_idx, meta) in cell_meta.iter().enumerate() {
                    let Some((bounds, ccx, ccy)) = meta else {
                        continue;
                    };
                    if !tsr_region_contains_item(&token_item, *bounds) {
                        continue;
                    }
                    let dx = token_cx - ccx;
                    let dy = token_cy - ccy;
                    let dist_sq = dx * dx + dy * dy;
                    if best.is_none_or(|(_, d)| dist_sq < d) {
                        best = Some((cell_idx, dist_sq));
                    }
                }
                if let Some((ci, _)) = best {
                    per_cell_items[ci].push(token_item);
                } else {
                    orphan_token_subitems.push(token_item);
                }
            }
        }

        // Build per-cell text from the assigned tokens. Markdown cells must
        // be one line — collapse line breaks from the line-grouping pass.
        for (cell_idx, matched) in per_cell_items.into_iter().enumerate() {
            cells[cell_idx].text = collect_text_from_matched_items(matched, adaptive_threshold)
                .replace(['\n', '\r'], " ");
        }

        // Stage 2: orphan assignment — tokens that didn't land in any cell
        // during stage 1 get assigned to their nearest *empty* cell,
        // clamped by a plausibility cap derived from cell geometry.
        //
        // This recovers two failure modes left by `normalize_cell_bands`:
        //   (a) header text positioned to the LEFT of a column whose band
        //       was derived from data cells centered farther right, so the
        //       header text falls outside the clamped band; and
        //   (b) local SLANet row drift where a cell's bbox sits slightly
        //       above/below its target text item, so the strict rules miss.
        // Empty-cell-only is the safety net: a cell already filled by stage 1
        // is never overwritten or augmented, so the cell-bleed case PR #62
        // closed cannot regress.
        tsr_assign_orphan_items(
            &orphan_token_subitems,
            &mut cells,
            &std::collections::HashSet::new(),
            page_h,
            coords,
        );

        results.push(cells);
    }

    Ok(results)
}

/// Split a `TextItem` into one virtual sub-item per whitespace-separated
/// token, with each token's `x` / `width` estimated from the original item's
/// effective width and the token's character offset.
///
/// PDFs often render an entire row's content as a single Tj — e.g.
/// "Marshall Islands 0.9 0.9 0.9" — producing one wide TextItem whose
/// center sits in only one of the model-emitted cells. Per-item routing
/// then parks the whole row in that one cell. Splitting on whitespace
/// gives each word its own approximate position so per-cell routing can
/// distribute the words to whichever cells their estimated centers fall
/// into.
///
/// The character-width estimate is `effective_width / char_count`.
/// `effective_width` returns the explicit `item.width` when known and
/// otherwise falls back to `char_count * font_size * 0.5`. Either way the
/// estimate is uniform across the item — fine for routing, since we only
/// need to know which cell each token's center lands in, not its exact
/// position. Single-token items collapse to a one-element vector
/// equivalent to the input item, making this a no-op for the common case.
fn split_item_into_token_subitems(item: &TextItem) -> Vec<TextItem> {
    let total_chars = item.text.chars().count();
    if total_chars == 0 {
        return Vec::new();
    }
    let item_w = text_utils::effective_width(item);
    let char_w = item_w / total_chars as f32;

    let mut tokens: Vec<TextItem> = Vec::new();
    let mut current_token = String::new();
    let mut current_start_idx: Option<usize> = None;

    let push_token =
        |tokens: &mut Vec<TextItem>, text: String, start_idx: usize, end_idx: usize| {
            if text.is_empty() {
                return;
            }
            let mut sub = item.clone();
            sub.text = text;
            sub.x = item.x + start_idx as f32 * char_w;
            sub.width = (end_idx - start_idx) as f32 * char_w;
            tokens.push(sub);
        };

    for (idx, ch) in item.text.chars().enumerate() {
        if ch.is_whitespace() {
            if let Some(start_idx) = current_start_idx.take() {
                let text = std::mem::take(&mut current_token);
                push_token(&mut tokens, text, start_idx, idx);
            }
        } else {
            if current_start_idx.is_none() {
                current_start_idx = Some(idx);
            }
            current_token.push(ch);
        }
    }
    if let Some(start_idx) = current_start_idx {
        let text = std::mem::take(&mut current_token);
        push_token(&mut tokens, text, start_idx, total_chars);
    }

    tokens
}

/// Compute plausibility caps for the orphan-assignment pass. Returns
/// `(cap_x, cap_y)` — the maximum x/y distance from a text item's center
/// to a candidate empty cell's bbox before the candidate is rejected.
///
/// Caps are derived from cell geometry so they scale with the table:
/// dense small-row tables get a tight cap, looser tables get more slack.
/// Floor values guard against degenerate single-cell tables collapsing
/// the cap to zero.
fn tsr_assignment_caps(cells: &[tables::StructuredCell]) -> (f32, f32) {
    let mut widths: Vec<f32> = Vec::with_capacity(cells.len());
    let mut heights: Vec<f32> = Vec::with_capacity(cells.len());
    for cell in cells {
        let [x1, y1, x2, y2] = cell.page_pt_bbox;
        let w = (x2 - x1).abs();
        let h = (y2 - y1).abs();
        if w > 0.0 && h > 0.0 {
            widths.push(w);
            heights.push(h);
        }
    }
    if widths.is_empty() {
        return (0.0, 0.0);
    }
    widths.sort_by(|a, b| a.total_cmp(b));
    heights.sort_by(|a, b| a.total_cmp(b));
    let median_w = widths[widths.len() / 2];
    let median_h = heights[heights.len() / 2];
    // Floor values: even on a dense table, a 5pt floor handles small
    // pixel-level bbox jitter without being so loose that we'd cross
    // into a neighboring row/column. Symmetric in both axes.
    let cap_x = median_w.max(5.0);
    let cap_y = median_h.max(5.0);
    (cap_x, cap_y)
}

/// For each text item that wasn't claimed by any cell during stage 1,
/// find the nearest *empty* cell within `(cap_x, cap_y)` of the item's
/// center and append the item's text to that cell. Cells that already
/// have content are skipped — stage 2 only fills, never augments.
///
/// Distance is point-to-rect: 0 if the item center is inside the cell's
/// bbox, else the axis-aligned gap to the nearest edge. Both x-gap and
/// y-gap must be within their respective caps for a candidate to qualify;
/// among qualifying candidates, the smallest combined euclidean distance
/// wins.
fn tsr_assign_orphan_items(
    items: &[TextItem],
    cells: &mut [tables::StructuredCell],
    claimed: &std::collections::HashSet<usize>,
    page_height: f32,
    coord_space: RegionCoordSpace,
) {
    if cells.is_empty() {
        return;
    }
    let (cap_x, cap_y) = tsr_assignment_caps(cells);
    if cap_x <= 0.0 || cap_y <= 0.0 {
        return;
    }
    // Y-tolerance for "same line as a previous orphan" — multi-token branch
    // names like "Blue Valley Parkway" are 3 separate text items and should
    // all stack into the same cell. But two orphans on different rows of
    // the PDF (different y values) targeting the same empty cell should
    // NOT merge — that produces the "Mitchell Woonsocket" / "Shawnee Blue
    // Valley Parkway" run-on cells. Half a row of slack is conservative.
    let y_tolerance = (cap_y * 0.5).max(3.0);

    // Pre-compute each empty cell's region bounds so we don't re-flip
    // page coordinates per orphan-candidate pair.
    let cell_bounds: Vec<Option<RegionBounds>> = cells
        .iter()
        .map(|cell| {
            if !cell.text.is_empty() {
                return None;
            }
            let [x1, y1, x2, y2] = cell.page_pt_bbox;
            if x1 >= x2 || y1 >= y2 {
                return None;
            }
            Some(region_bounds(x1, y1, x2, y2, page_height, coord_space))
        })
        .collect();

    // Track the y-center of the FIRST orphan that landed in each cell so
    // subsequent orphans only stack if they're on the same line.
    let mut stage2_first_y: std::collections::HashMap<usize, f32> =
        std::collections::HashMap::new();

    for (i, item) in items.iter().enumerate() {
        if claimed.contains(&i) {
            continue;
        }
        let item_w = text_utils::effective_width(item);
        if item.text.trim().is_empty() {
            continue;
        }
        let cx = item.x + item_w * 0.5;
        let cy = item.y + item.height * 0.5;

        let mut best: Option<(usize, f32)> = None;
        for (ci, bounds_opt) in cell_bounds.iter().enumerate() {
            let Some(bounds) = bounds_opt else {
                continue;
            };
            // If a previous orphan already landed in this cell, only let a
            // new orphan join if it's on the same line. Cross-line orphans
            // need to look elsewhere (next-nearest empty cell).
            if let Some(&first_y) = stage2_first_y.get(&ci) {
                if (first_y - cy).abs() > y_tolerance {
                    continue;
                }
            }
            let dx = (bounds.x_min - cx).max(0.0).max(cx - bounds.x_max);
            let dy = (bounds.y_min - cy).max(0.0).max(cy - bounds.y_max);
            if dx > cap_x || dy > cap_y {
                continue;
            }
            let dist_sq = dx * dx + dy * dy;
            if best.is_none_or(|(_, d)| dist_sq < d) {
                best = Some((ci, dist_sq));
            }
        }

        if let Some((ci, _)) = best {
            // Append, preserving stage 1's content. Same-line orphans
            // stack to support multi-token text (e.g. "Blue Valley
            // Parkway"); cross-line orphans are filtered out above.
            let trimmed = item.text.trim();
            if cells[ci].text.is_empty() {
                cells[ci].text = trimmed.to_string();
            } else {
                cells[ci].text.push(' ');
                cells[ci].text.push_str(trimmed);
            }
            stage2_first_y.entry(ci).or_insert(cy);
        }
    }
}

/// Extract markdown tables using externally-supplied structure recovery.
///
/// Convenience wrapper around [`extract_tables_with_structure_cells_mem`]
/// that renders each cell list to markdown via
/// [`tables::cells_to_markdown`]. Returns one markdown string per input,
/// in input order. Inputs whose page is out of range or whose tokens parse
/// to zero cells produce an empty string.
pub fn extract_tables_with_structure_mem(
    buffer: &[u8],
    inputs: &[TsrTableInput],
) -> Result<Vec<String>, PdfError> {
    let cells_lists = extract_tables_with_structure_cells_mem(buffer, inputs)?;
    Ok(cells_lists
        .into_iter()
        .map(|cells| {
            if cells.is_empty() {
                String::new()
            } else {
                tables::cells_to_markdown(&cells)
            }
        })
        .collect())
}

/// Markdown for one extracted table plus a diagnostic flag describing
/// which path produced it.
///
/// `fallback_reason` is `None` when the TSR-hybrid path produced the
/// markdown directly; `Some(<short identifier>)` when stage 1's quality
/// check fired and either in-place expansion or the heuristic fallback
/// produced the output. The reason string is stable enough to use as a
/// metric label (e.g. `multi_row_in_cell_expanded`, `phantom_empty_row`).
#[derive(Debug, Clone)]
pub struct TableExtractionResult {
    pub markdown: String,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
enum TsrQualityIssue {
    PhantomEmptyRow,
    MultiRowInCell {
        expanded_cells: Option<Vec<tables::StructuredCell>>,
    },
}

impl TsrQualityIssue {
    fn reason(&self) -> &'static str {
        match self {
            Self::PhantomEmptyRow => "phantom_empty_row",
            Self::MultiRowInCell { .. } => "multi_row_in_cell",
        }
    }
}

#[derive(Clone)]
struct TsrCellTextLine {
    center_y: f32,
    half_height: f32,
    items: Vec<TextItem>,
}

impl TsrCellTextLine {
    fn new(item: TextItem) -> Self {
        let center_y = item.y + item.height * 0.5;
        let half_height = (item.height * 0.5).max(2.5);
        Self {
            center_y,
            half_height,
            items: vec![item],
        }
    }

    fn add(&mut self, item: TextItem) {
        let center_y = item.y + item.height * 0.5;
        let existing = self.items.len() as f32;
        self.center_y = (self.center_y * existing + center_y) / (existing + 1.0);
        self.half_height = self.half_height.max((item.height * 0.5).max(2.5));
        self.items.push(item);
    }

    fn bottom_y(&self) -> f32 {
        self.items
            .iter()
            .map(|item| item.y)
            .fold(f32::INFINITY, f32::min)
    }
}

#[derive(Clone)]
struct TsrRowExpansion {
    bands: Vec<f32>,
    tolerance: f32,
}

fn collect_items_in_tsr_cell(
    items: &[TextItem],
    cell: &tables::StructuredCell,
    page_height: f32,
    coord_space: RegionCoordSpace,
) -> Vec<TextItem> {
    let [x1, y1, x2, y2] = cell.page_pt_bbox;
    if x1 >= x2 || y1 >= y2 {
        return Vec::new();
    }
    let bounds = region_bounds(x1, y1, x2, y2, page_height, coord_space);
    items
        .iter()
        .filter(|item| !item.text.trim().is_empty() && tsr_region_contains_item(item, bounds))
        .cloned()
        .collect()
}

fn cluster_tsr_cell_text_lines(mut items: Vec<TextItem>) -> Vec<TsrCellTextLine> {
    if items.is_empty() {
        return Vec::new();
    }

    items.sort_by(|a, b| {
        let ay = a.y + a.height * 0.5;
        let by = b.y + b.height * 0.5;
        by.total_cmp(&ay).then(a.x.total_cmp(&b.x))
    });

    let mut lines: Vec<TsrCellTextLine> = Vec::new();
    for item in items {
        let item_top = item.y + item.height;
        let item_half_height = (item.height * 0.5).max(2.5);
        if let Some(last) = lines.last_mut() {
            let gap = last.bottom_y() - item_top;
            if gap <= last.half_height.max(item_half_height) {
                last.add(item);
                continue;
            }
        }
        lines.push(TsrCellTextLine::new(item));
    }

    lines
}

fn build_tsr_row_expansion(
    row_cells: &[usize],
    cells: &[tables::StructuredCell],
    cell_lines: &[Vec<TsrCellTextLine>],
) -> Option<TsrRowExpansion> {
    if row_cells.is_empty() {
        return None;
    }
    let has_spanning_cell = row_cells
        .iter()
        .any(|&idx| cells[idx].rowspan > 1 || cells[idx].colspan > 1);
    if has_spanning_cell {
        return None;
    }

    let multiline_cells = row_cells
        .iter()
        .filter(|&&idx| cell_lines[idx].len() >= 2)
        .count();
    if row_cells.len() >= 2 && multiline_cells < 2 {
        return None;
    }
    if row_cells.len() == 1 && multiline_cells == 0 {
        return None;
    }

    let mut centers: Vec<(f32, f32)> = row_cells
        .iter()
        .flat_map(|&idx| {
            cell_lines[idx]
                .iter()
                .map(|line| (line.center_y, line.half_height))
        })
        .collect();
    if centers.len() < 2 {
        return None;
    }
    centers.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut half_heights: Vec<f32> = centers.iter().map(|(_, h)| *h).collect();
    half_heights.sort_by(|a, b| a.total_cmp(b));
    let tolerance = (half_heights[half_heights.len() / 2] * 0.8).max(3.0);

    let mut bands: Vec<(f32, usize)> = Vec::new();
    for (center, _) in centers {
        if let Some((band_center, count)) = bands
            .iter_mut()
            .find(|(band_center, _)| (*band_center - center).abs() <= tolerance)
        {
            *band_center = (*band_center * *count as f32 + center) / (*count as f32 + 1.0);
            *count += 1;
        } else {
            bands.push((center, 1));
        }
    }

    // V1 targets the common 1-2 lost-row cases; larger compressions stay on
    // the existing heuristic fallback path until we have evidence to broaden it.
    if !(2..=4).contains(&bands.len()) {
        return None;
    }

    let min_support = if row_cells.len() >= 2 { 2 } else { 1 };
    let supported = bands.iter().all(|(band, _)| {
        row_cells
            .iter()
            .filter(|&&idx| {
                cell_lines[idx]
                    .iter()
                    .any(|line| (line.center_y - *band).abs() <= tolerance)
            })
            .count()
            >= min_support
    });
    if !supported {
        return None;
    }

    bands.sort_by(|a, b| b.0.total_cmp(&a.0));
    Some(TsrRowExpansion {
        bands: bands.into_iter().map(|(center, _)| center).collect(),
        tolerance,
    })
}

fn text_for_tsr_band(
    lines: &[TsrCellTextLine],
    band: f32,
    tolerance: f32,
    adaptive_threshold: f32,
) -> String {
    let mut matched: Vec<TextItem> = lines
        .iter()
        .filter(|line| (line.center_y - band).abs() <= tolerance)
        .flat_map(|line| line.items.iter().cloned())
        .collect();
    if matched.is_empty() {
        return String::new();
    }
    matched.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    collect_text_from_matched_items(matched, adaptive_threshold).replace(['\n', '\r'], " ")
}

fn slice_cell_bbox_for_expanded_row(
    cell: &tables::StructuredCell,
    row_idx: usize,
    row_count: usize,
) -> [f32; 4] {
    let mut bbox = cell.page_pt_bbox;
    let top = bbox[1].min(bbox[3]);
    let bottom = bbox[1].max(bbox[3]);
    let height = bottom - top;
    if height <= 0.0 || row_count == 0 {
        return bbox;
    }
    let step = height / row_count as f32;
    bbox[1] = top + step * row_idx as f32;
    bbox[3] = if row_idx + 1 == row_count {
        bottom
    } else {
        top + step * (row_idx + 1) as f32
    };
    bbox
}

fn try_expand_multi_row_cells(
    cells: &[tables::StructuredCell],
    items: &[TextItem],
    page_height: f32,
    coord_space: RegionCoordSpace,
    adaptive_threshold: f32,
) -> Option<Vec<tables::StructuredCell>> {
    if cells.is_empty() {
        return None;
    }

    let cell_lines: Vec<Vec<TsrCellTextLine>> = cells
        .iter()
        .map(|cell| {
            if cell.rowspan > 1 {
                Vec::new()
            } else {
                cluster_tsr_cell_text_lines(collect_items_in_tsr_cell(
                    items,
                    cell,
                    page_height,
                    coord_space,
                ))
            }
        })
        .collect();

    let mut cells_by_row: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (idx, cell) in cells.iter().enumerate() {
        cells_by_row.entry(cell.row).or_default().push(idx);
    }

    let mut expansions: HashMap<usize, TsrRowExpansion> = HashMap::new();
    for (&row, row_cells) in &cells_by_row {
        let covered_by_rowspan = cells
            .iter()
            .any(|cell| cell.rowspan > 1 && cell.row <= row && row < cell.row + cell.rowspan);
        if covered_by_rowspan {
            continue;
        }
        if let Some(expansion) = build_tsr_row_expansion(row_cells, cells, &cell_lines) {
            expansions.insert(row, expansion);
        }
    }

    if expansions.is_empty() {
        return None;
    }

    let mut expanded = Vec::with_capacity(cells.len() + expansions.len());
    let mut row_shift = 0usize;
    for (&row, row_cells) in &cells_by_row {
        if let Some(expansion) = expansions.get(&row) {
            for (band_idx, band) in expansion.bands.iter().enumerate() {
                for &cell_idx in row_cells {
                    let mut cell = cells[cell_idx].clone();
                    cell.row = row + row_shift + band_idx;
                    cell.rowspan = 1;
                    cell.text = text_for_tsr_band(
                        &cell_lines[cell_idx],
                        *band,
                        expansion.tolerance,
                        adaptive_threshold,
                    );
                    cell.page_pt_bbox =
                        slice_cell_bbox_for_expanded_row(&cell, band_idx, expansion.bands.len());
                    expanded.push(cell);
                }
            }
            row_shift += expansion.bands.len() - 1;
        } else {
            for &cell_idx in row_cells {
                let mut cell = cells[cell_idx].clone();
                cell.row += row_shift;
                expanded.push(cell);
            }
        }
    }

    let original_rows = cells
        .iter()
        .map(|cell| cell.row + cell.rowspan.max(1))
        .max()
        .unwrap_or(0);
    let expanded_rows = expanded
        .iter()
        .map(|cell| cell.row + cell.rowspan.max(1))
        .max()
        .unwrap_or(0);
    (expanded_rows > original_rows).then_some(expanded)
}

/// Detect quality issues in the TSR-hybrid output for a single input.
///
/// Returns `Some(issue)` if the cells look like they reflect a known
/// SLANet detection pathology. Reasons (also used as metric labels):
///
/// * `phantom_empty_row` — a row whose every cell is empty, surrounded
///   above and below by rows with content. SLANet sometimes emits an
///   extra row that doesn't correspond to any visible PDF row.
/// * `multi_row_in_cell` — at least one non-label `rowspan==1` cell
///   encloses PDF text items that cluster into two distinct visual lines
///   separated by a whitespace gap larger than the line height. Cells
///   declared as `rowspan>1` are excluded since they are *expected*
///   to span multiple lines. First-row/first-column wraps are ignored
///   unless the in-place row expansion has enough support to repair them,
///   because those are often legitimate wrapped headers or row labels.
///   SLANet's row under-detection on tightly-packed tables produces the
///   rowspan==1-but-multi-line pattern (the FNBO failure mode).
fn detect_tsr_quality_issue(
    buffer: &[u8],
    input: &TsrTableInput,
    cells: &[tables::StructuredCell],
) -> Result<Option<TsrQualityIssue>, PdfError> {
    if cells.is_empty() {
        return Ok(None);
    }

    // Phantom row: cheap, computed from cell metadata alone.
    let max_row = cells.iter().map(|c| c.row).max().unwrap_or(0);
    if max_row >= 2 {
        let mut row_has_content = vec![false; max_row + 1];
        for cell in cells {
            if !cell.text.trim().is_empty() {
                row_has_content[cell.row] = true;
            }
        }
        for r in 1..max_row {
            if !row_has_content[r] && row_has_content[r - 1] && row_has_content[r + 1] {
                return Ok(Some(TsrQualityIssue::PhantomEmptyRow));
            }
        }
    }

    // Multi-row-in-cell: re-extract PDF text items in the page and look
    // for `rowspan==1` cells that contain items grouped into ≥2 visual
    // lines separated by a real whitespace gap. This is the FNBO mode:
    // a tall TSR cell catches text from two adjacent PDF rows that
    // SLANet failed to separate. Cells declared `rowspan>1` are
    // expected to be multi-line and are excluded.
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();
    let page_1idx = input.page + 1;
    let Some(&page_id) = pages.get(&page_1idx) else {
        return Ok(None);
    };
    let page_h = get_page_height(&doc, page_id).unwrap_or(792.0);
    let mut needed: HashSet<u32> = HashSet::new();
    needed.insert(page_1idx);
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed));
    let ((mut items, _rects, _lines), _has_gid, coords_rotated) =
        extractor::content_stream::extract_page_text_items(
            &doc,
            page_id,
            page_1idx,
            &font_cmaps,
            false,
        )?;
    let adaptive_threshold = text_utils::fix_letterspaced_items(&mut items);
    let coords = if coords_rotated {
        RegionCoordSpace::Rotated90Ccw
    } else {
        RegionCoordSpace::Standard
    };
    let expanded_cells =
        try_expand_multi_row_cells(cells, &items, page_h, coords, adaptive_threshold);
    let first_row = cells.iter().map(|cell| cell.row).min().unwrap_or(0);
    let first_col = cells.iter().map(|cell| cell.col).min().unwrap_or(0);

    for cell in cells {
        // rowspan>1 cells are intentionally multi-line — skip them.
        if cell.rowspan > 1 {
            continue;
        }
        if cell.text.trim().is_empty() {
            continue;
        }
        let cell_items = collect_items_in_tsr_cell(&items, cell, page_h, coords);
        if cell_items.len() < 2 {
            continue;
        }
        if cluster_tsr_cell_text_lines(cell_items).len() < 2 {
            continue;
        }
        if expanded_cells.is_some() {
            return Ok(Some(TsrQualityIssue::MultiRowInCell { expanded_cells }));
        }
        if !is_wrapped_tsr_label_cell(cell, first_row, first_col) {
            return Ok(Some(TsrQualityIssue::MultiRowInCell {
                expanded_cells: None,
            }));
        }
    }

    Ok(None)
}

fn is_wrapped_tsr_label_cell(
    cell: &tables::StructuredCell,
    first_row: usize,
    first_col: usize,
) -> bool {
    cell.is_header || cell.row == first_row || cell.col == first_col
}

/// Auto-fallback variant of [`extract_tables_with_structure_mem`]:
/// runs the TSR-hybrid path, checks the resulting cells for known
/// SLANet detection pathologies (phantom rows, multi-row-in-cell text),
/// and falls back to the heuristic [`extract_tables_in_regions_mem`]
/// for any input where the TSR path looks compromised.
///
/// On clean inputs this is identical to the markdown variant. On
/// `multi_row_in_cell`, the wrapper first tries to expand over-stuffed
/// rows in place; if that cannot produce a usable table, the heuristic
/// markdown replaces the TSR markdown and `fallback_reason` is set to
/// the diagnostic label.
///
/// Two failure modes are guarded against per-input:
///
/// * **Empty heuristic**: if the heuristic returns empty/whitespace
///   markdown for a flagged region, the original TSR markdown is
///   preserved and `fallback_reason` is suffixed with
///   `_heuristic_empty` (e.g. `multi_row_in_cell_heuristic_empty`).
///   This avoids replacing a usable wrong-but-non-empty TSR output
///   with literally nothing.
/// * **Expanded multi-row cells**: when in-place recovery succeeds, the
///   result is labeled `multi_row_in_cell_expanded` and the heuristic is
///   not consulted.
/// * **Per-input errors**: any failure in detection or heuristic
///   extraction for a single input is contained — that input
///   returns the raw TSR markdown with `fallback_reason` set to
///   an `_error` label so callers can metric on it. Other inputs
///   in the same batch are unaffected.
///
/// Use this from production callers that want self-healing output.
/// Use [`extract_tables_with_structure_mem`] when you want raw TSR
/// output regardless of quality (e.g. eval harnesses comparing the
/// two paths).
pub fn extract_tables_with_structure_auto_mem(
    buffer: &[u8],
    inputs: &[TsrTableInput],
) -> Result<Vec<TableExtractionResult>, PdfError> {
    let tsr_cells = extract_tables_with_structure_cells_mem(buffer, inputs)?;
    let mut results = Vec::with_capacity(inputs.len());

    for (i, input) in inputs.iter().enumerate() {
        let cells = &tsr_cells[i];
        let tsr_md = if cells.is_empty() {
            String::new()
        } else {
            tables::cells_to_markdown(cells)
        };

        let issue = match detect_tsr_quality_issue(buffer, input, cells) {
            Ok(opt) => opt,
            Err(_) => {
                // Detection failed for this input — fall through with
                // the raw TSR markdown so the rest of the batch is
                // unaffected. Tag the reason for caller metrics.
                results.push(TableExtractionResult {
                    markdown: tsr_md,
                    fallback_reason: Some("detection_error".to_string()),
                });
                continue;
            }
        };

        let result = match issue {
            None => TableExtractionResult {
                markdown: tsr_md,
                fallback_reason: None,
            },
            Some(issue) => {
                let reason = issue.reason().to_string();
                if let TsrQualityIssue::MultiRowInCell {
                    expanded_cells: Some(expanded_cells),
                } = issue
                {
                    let expanded_md = tables::cells_to_markdown(&expanded_cells);
                    if !expanded_md.trim().is_empty() {
                        results.push(TableExtractionResult {
                            markdown: expanded_md,
                            fallback_reason: Some("multi_row_in_cell_expanded".to_string()),
                        });
                        continue;
                    }
                }
                // Fall back to heuristic on the input's table region.
                // The crop's PDF-pt bbox IS the table region.
                let heuristic_md = match extract_tables_in_regions_mem(
                    buffer,
                    &[(input.page, vec![input.crop_pdf_pt_bbox])],
                ) {
                    Ok(pages) => pages
                        .into_iter()
                        .next()
                        .and_then(|p| p.regions.into_iter().next().map(|r| r.text))
                        .unwrap_or_default(),
                    Err(_) => {
                        // Heuristic threw — keep raw TSR markdown.
                        results.push(TableExtractionResult {
                            markdown: tsr_md,
                            fallback_reason: Some(format!("{reason}_heuristic_error")),
                        });
                        continue;
                    }
                };
                if heuristic_md.trim().is_empty() {
                    // Heuristic produced nothing useful — keep TSR
                    // markdown rather than ship empty. The reason
                    // suffix lets callers count this case.
                    TableExtractionResult {
                        markdown: tsr_md,
                        fallback_reason: Some(format!("{reason}_heuristic_empty")),
                    }
                } else {
                    TableExtractionResult {
                        markdown: heuristic_md,
                        fallback_reason: Some(reason),
                    }
                }
            }
        };
        results.push(result);
    }

    Ok(results)
}

/// Get page height in points from MediaBox.
fn get_page_height(doc: &Document, page_id: lopdf::ObjectId) -> Option<f32> {
    let page_dict = doc.get_dictionary(page_id).ok()?;
    // Try MediaBox directly, then follow reference
    let media_box = page_dict.get(b"MediaBox").ok()?;
    let arr = match media_box {
        lopdf::Object::Array(a) => a,
        lopdf::Object::Reference(r) => {
            if let Ok(lopdf::Object::Array(a)) = doc.get_object(*r) {
                a
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if arr.len() >= 4 {
        let y1 = obj_to_f32(&arr[1])?;
        let y2 = obj_to_f32(&arr[3])?;
        Some((y2 - y1).abs())
    } else {
        None
    }
}

fn obj_to_f32(obj: &lopdf::Object) -> Option<f32> {
    match obj {
        lopdf::Object::Integer(i) => Some(*i as f32),
        lopdf::Object::Real(f) => Some(*f),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum RegionCoordSpace {
    Standard,
    Rotated90Ccw,
}

#[derive(Clone, Copy)]
struct RegionBounds {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

/// Collect text items that fall within a region bbox (top-left origin, PDF points)
/// and return them as a single string in reading order.
pub fn collect_text_in_region(
    items: &[TextItem],
    rx1: f32,
    ry1: f32,
    rx2: f32,
    ry2: f32,
    page_height: f32,
) -> String {
    collect_text_in_region_with_options(
        items,
        rx1,
        ry1,
        rx2,
        ry2,
        page_height,
        infer_region_coord_space(items),
        0.10,
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_text_in_region_with_options(
    items: &[TextItem],
    rx1: f32,
    ry1: f32,
    rx2: f32,
    ry2: f32,
    page_height: f32,
    coord_space: RegionCoordSpace,
    adaptive_threshold: f32,
) -> String {
    let bounds = region_bounds(rx1, ry1, rx2, ry2, page_height, coord_space);
    let matched: Vec<TextItem> = items
        .iter()
        .filter(|item| region_overlaps_item(item, bounds))
        .cloned()
        .collect();
    collect_text_from_matched_items(matched, adaptive_threshold)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn collect_text_in_tsr_cell(
    items: &[TextItem],
    rx1: f32,
    ry1: f32,
    rx2: f32,
    ry2: f32,
    page_height: f32,
    coord_space: RegionCoordSpace,
    adaptive_threshold: f32,
) -> String {
    let bounds = region_bounds(rx1, ry1, rx2, ry2, page_height, coord_space);
    let matched: Vec<TextItem> = items
        .iter()
        .filter(|item| tsr_region_contains_item(item, bounds))
        .cloned()
        .collect();
    collect_text_from_matched_items(matched, adaptive_threshold)
}

fn collect_text_from_matched_items(matched: Vec<TextItem>, adaptive_threshold: f32) -> String {
    if matched.is_empty() {
        return String::new();
    }

    // Simple extraction: the caller (fire-pdf) already handles reading order
    // and column splitting via the layout model. We just need to sort items
    // top-to-bottom, left-to-right and group into lines.
    let mut sorted = matched;
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));

    let y_tolerance = 3.0;
    let mut lines: Vec<extractor::TextLine> = Vec::new();

    for item in sorted {
        let should_merge = lines.last().is_some_and(|last_line: &extractor::TextLine| {
            last_line.page == item.page && (last_line.y - item.y).abs() < y_tolerance
        });
        if should_merge {
            lines.last_mut().unwrap().items.push(item);
        } else {
            let y = item.y;
            let page = item.page;
            lines.push(extractor::TextLine {
                items: vec![item],
                y,
                page,
                adaptive_threshold,
            });
        }
    }

    // Sort items within each line by X position
    for line in &mut lines {
        text_utils::sort_line_items(&mut line.items);
    }

    lines
        .into_iter()
        .map(|line| line.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn infer_region_coord_space(items: &[TextItem]) -> RegionCoordSpace {
    // Rotated-page normalization currently maps y = -old_x, so most text items
    // land at negative Y. Use this to keep `collect_text_in_region` behavior
    // compatible for direct callers that do not have extractor metadata.
    let negative_y = items.iter().filter(|item| item.y < 0.0).count();
    if !items.is_empty() && negative_y * 2 >= items.len() {
        RegionCoordSpace::Rotated90Ccw
    } else {
        RegionCoordSpace::Standard
    }
}

fn region_bounds(
    rx1: f32,
    ry1: f32,
    rx2: f32,
    ry2: f32,
    page_height: f32,
    coord_space: RegionCoordSpace,
) -> RegionBounds {
    let tx_min = rx1.min(rx2);
    let tx_max = rx1.max(rx2);
    let ty_min = ry1.min(ry2);
    let ty_max = ry1.max(ry2);
    let by_min = page_height - ty_max;
    let by_max = page_height - ty_min;
    match coord_space {
        RegionCoordSpace::Standard => RegionBounds {
            x_min: tx_min,
            y_min: by_min,
            x_max: tx_max,
            y_max: by_max,
        },
        RegionCoordSpace::Rotated90Ccw => RegionBounds {
            x_min: by_min,
            x_max: by_max,
            y_min: -tx_max,
            y_max: -tx_min,
        },
    }
}

fn region_overlaps_item(item: &TextItem, bounds: RegionBounds) -> bool {
    const REGION_MARGIN: f32 = 1.5;
    let item_x_min = item.x;
    let item_x_max = item.x + text_utils::effective_width(item);
    let item_y_min = item.y;
    let item_y_max = item.y + item.height;

    let x_overlap = (item_x_max.min(bounds.x_max + REGION_MARGIN)
        - item_x_min.max(bounds.x_min - REGION_MARGIN))
    .max(0.0);
    let y_overlap = (item_y_max.min(bounds.y_max + REGION_MARGIN)
        - item_y_min.max(bounds.y_min - REGION_MARGIN))
    .max(0.0);
    x_overlap > 0.0 && y_overlap > 0.0
}

fn region_overlaps_rect(rect: &PdfRect, bounds: RegionBounds) -> bool {
    const REGION_MARGIN: f32 = 1.5;
    let (x_min, y_min, x_max, y_max) = normalized_rect_edges(rect);
    ranges_overlap(
        x_min,
        x_max,
        bounds.x_min - REGION_MARGIN,
        bounds.x_max + REGION_MARGIN,
    ) && ranges_overlap(
        y_min,
        y_max,
        bounds.y_min - REGION_MARGIN,
        bounds.y_max + REGION_MARGIN,
    )
}

fn region_overlaps_line(line: &PdfLine, bounds: RegionBounds) -> bool {
    const REGION_MARGIN: f32 = 1.5;
    let x_min = line.x1.min(line.x2);
    let x_max = line.x1.max(line.x2);
    let y_min = line.y1.min(line.y2);
    let y_max = line.y1.max(line.y2);
    ranges_overlap(
        x_min,
        x_max,
        bounds.x_min - REGION_MARGIN,
        bounds.x_max + REGION_MARGIN,
    ) && ranges_overlap(
        y_min,
        y_max,
        bounds.y_min - REGION_MARGIN,
        bounds.y_max + REGION_MARGIN,
    )
}

fn ranges_overlap(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> bool {
    a_max >= b_min && b_max >= a_min
}

fn tsr_region_contains_item(item: &TextItem, bounds: RegionBounds) -> bool {
    let item_x_min = item.x;
    let item_x_max = item.x + text_utils::effective_width(item);
    let item_y_min = item.y;
    let item_y_max = item.y + item.height;

    let center_x = (item_x_min + item_x_max) * 0.5;
    let center_y = (item_y_min + item_y_max) * 0.5;
    if center_x >= bounds.x_min
        && center_x <= bounds.x_max
        && center_y >= bounds.y_min
        && center_y <= bounds.y_max
    {
        return true;
    }

    let x_overlap = (item_x_max.min(bounds.x_max) - item_x_min.max(bounds.x_min)).max(0.0);
    let y_overlap = (item_y_max.min(bounds.y_max) - item_y_min.max(bounds.y_min)).max(0.0);
    let item_width = (item_x_max - item_x_min).max(0.1);
    let item_height = (item_y_max - item_y_min).max(0.1);

    x_overlap / item_width >= 0.6 && y_overlap / item_height >= 0.6
}

// =========================================================================
// Internal: single-load document pipeline
// =========================================================================

/// Load a PDF from disk, returning the parsed document and page count.
///
/// `Document::load_metadata` for page count + `Document::load` for content
/// are combined here, but lopdf loads the full doc in `load()` so we extract
/// page count from it directly to avoid the metadata-only round-trip.
pub(crate) fn load_document_from_path<P: AsRef<Path>>(
    path: P,
) -> Result<(Document, u32), PdfError> {
    let buffer = std::fs::read(&path)?;
    load_document_from_mem(&buffer)
}

/// Load a PDF from a memory buffer.
pub(crate) fn load_document_from_mem(buffer: &[u8]) -> Result<(Document, u32), PdfError> {
    // Fix malformed struct element names before parsing. Some PDF generators
    // write bare names (/S Code) instead of proper PDF names (/S /Code), which
    // causes lopdf to silently drop the entire object.
    let fixed = structure_tree::fix_bare_struct_names(buffer);
    let buf = fixed.as_ref();

    let doc = match load_document_bytes(buf) {
        Ok(doc) => doc,
        Err(first_err) => {
            for repaired in repair_pdf_container_candidates(buf) {
                match load_document_bytes(&repaired) {
                    Ok(doc) => {
                        log::debug!("loaded PDF after repairing malformed container bytes");
                        let page_count = doc.get_pages().len() as u32;
                        return Ok((doc, page_count));
                    }
                    Err(e) => {
                        if is_encrypted_lopdf_error(&e) {
                            return Err(e.into());
                        }
                    }
                }
            }
            return Err(first_err.into());
        }
    };
    let page_count = doc.get_pages().len() as u32;
    Ok((doc, page_count))
}

fn load_document_bytes(buf: &[u8]) -> Result<Document, lopdf::Error> {
    match Document::load_mem(buf) {
        Ok(doc) => Ok(doc),
        Err(ref e) if is_encrypted_lopdf_error(e) => {
            Document::load_mem_with_options(buf, lopdf::LoadOptions::with_password(""))
        }
        Err(e) => Err(e),
    }
}

fn repair_pdf_container_candidates(buf: &[u8]) -> Vec<Vec<u8>> {
    let mut candidates = Vec::new();

    add_repair_candidate(&mut candidates, append_missing_eof_marker(buf), buf);

    let stripped = strip_leading_pdf_container_bytes(buf);
    if let Some(stripped_buf) = stripped.as_deref() {
        add_repair_candidate(&mut candidates, Some(stripped_buf.to_vec()), buf);
        add_repair_candidate(
            &mut candidates,
            append_missing_eof_marker(stripped_buf),
            buf,
        );
    }

    candidates
}

fn add_repair_candidate(
    candidates: &mut Vec<Vec<u8>>,
    candidate: Option<Vec<u8>>,
    original: &[u8],
) {
    let Some(candidate) = candidate else {
        return;
    };
    if candidate.as_slice() == original {
        return;
    }
    if candidates.iter().any(|existing| existing == &candidate) {
        return;
    }
    candidates.push(candidate);
}

fn append_missing_eof_marker(buf: &[u8]) -> Option<Vec<u8>> {
    if contains_recent_eof_marker(buf) {
        return None;
    }

    let mut end = buf.len();
    while end > 0 && buf[end - 1].is_ascii_whitespace() {
        end -= 1;
    }

    if !buf[..end].ends_with(b"%%EO") {
        return None;
    }

    let mut repaired = Vec::with_capacity(end + 2);
    repaired.extend_from_slice(&buf[..end]);
    repaired.extend_from_slice(b"F\n");
    Some(repaired)
}

fn contains_recent_eof_marker(buf: &[u8]) -> bool {
    let start = buf.len().saturating_sub(1024);
    buf[start..].windows(b"%%EOF".len()).any(|w| w == b"%%EOF")
}

fn strip_leading_pdf_container_bytes(buf: &[u8]) -> Option<Vec<u8>> {
    let mut start = if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };

    while start < buf.len() && buf[start].is_ascii_whitespace() {
        start += 1;
    }

    if start > 0 && buf[start..].starts_with(b"%PDF-") {
        Some(buf[start..].to_vec())
    } else {
        None
    }
}

/// Core processing pipeline operating on a pre-loaded document.
fn process_document(
    doc: Document,
    page_count: u32,
    options: PdfOptions,
    start: std::time::Instant,
) -> Result<PdfProcessResult, PdfError> {
    // Step 1 — Detection (cheap: scans content streams for text operators)
    let detection = detector::detect_from_document(&doc, page_count, &options.detection)?;
    let pdf_type = detection.pdf_type;
    let pages_needing_ocr = detection.pages_needing_ocr;
    let title = detection.title;
    let confidence = detection.confidence;

    // DetectOnly → return immediately
    if options.mode == ProcessMode::DetectOnly {
        return Ok(PdfProcessResult {
            pdf_type,
            markdown: None,
            page_count,
            processing_time_ms: start.elapsed().as_millis() as u64,
            pages_needing_ocr,
            title,
            confidence,
            layout: LayoutComplexity::default(),
            has_encoding_issues: false,
        });
    }

    // Scanned / ImageBased → nothing to extract
    if matches!(pdf_type, PdfType::Scanned | PdfType::ImageBased) {
        return Ok(PdfProcessResult {
            pdf_type,
            markdown: None,
            page_count,
            processing_time_ms: start.elapsed().as_millis() as u64,
            pages_needing_ocr,
            title,
            confidence,
            layout: LayoutComplexity::default(),
            has_encoding_issues: false,
        });
    }

    // Step 2 — Extraction (reuses the already-loaded document)
    let extracted = {
        let font_cmaps = FontCMaps::from_doc(&doc);
        let result = extractor::extract_positioned_text_from_doc(
            &doc,
            &font_cmaps,
            options.page_filter.as_ref(),
        );

        // For Mixed/template PDFs: if normal extraction produces garbage text
        // (mostly non-alphanumeric), retry with invisible (Tr=3) text included.
        // This unlocks OCR text layers behind scanned images.
        if pdf_type == PdfType::Mixed {
            if let Ok((ref items, _, _)) = result.as_ref().map(|(e, _, _)| e) {
                let sample: String = items.iter().take(200).map(|i| i.text.as_str()).collect();
                if is_garbage_text(&sample) || sample.trim().is_empty() {
                    extractor::extract_positioned_text_include_invisible(
                        &doc,
                        &font_cmaps,
                        options.page_filter.as_ref(),
                    )
                } else {
                    result
                }
            } else {
                // Normal extraction failed — try invisible as fallback
                extractor::extract_positioned_text_include_invisible(
                    &doc,
                    &font_cmaps,
                    options.page_filter.as_ref(),
                )
            }
        } else {
            result
        }
    };

    // For Mixed PDFs, extraction failure is non-fatal
    let extracted = if pdf_type == PdfType::Mixed {
        extracted.ok()
    } else {
        Some(extracted?)
    };

    // Parse structure tree for tagged PDFs (reuses the loaded document)
    let (struct_roles, struct_tables) = structure_tree::StructTree::from_doc(&doc)
        .map(|tree| {
            let page_ids = doc.get_pages();
            let roles = tree.mcid_to_roles(&page_ids);
            let tables = tree.extract_tables(&page_ids);
            if !roles.is_empty() {
                log::debug!(
                    "structure tree: {} pages with MCID roles, {} total MCIDs, {} tagged tables",
                    roles.len(),
                    tree.mcid_count(),
                    tables.len()
                );
            }
            let roles = if roles.is_empty() { None } else { Some(roles) };
            (roles, tables)
        })
        .unwrap_or((None, Vec::new()));

    let (markdown, layout, has_encoding_issues, gid_pages) = match extracted {
        Some(((items, rects, lines), page_thresholds, gid_encoded_pages)) => {
            // For TextBased PDFs with pages flagged for OCR (Identity-H or
            // Type3 fonts without ToUnicode), check whether the CID-as-Unicode
            // passthrough actually produced readable text.  If a page's text
            // is garbage, strip its items so we don't emit mojibake.
            // Only applies to TextBased — for Mixed PDFs, OCR flags come from
            // template images rather than font encoding issues.
            let (items, rects, lines) =
                if pages_needing_ocr.is_empty() || pdf_type != PdfType::TextBased {
                    (items, rects, lines)
                } else {
                    let ocr_set: std::collections::HashSet<u32> =
                        pages_needing_ocr.iter().copied().collect();
                    // Collect text per OCR-flagged page and check quality
                    let mut garbage_pages: std::collections::HashSet<u32> =
                        std::collections::HashSet::new();
                    for &pg in &ocr_set {
                        let page_text: String = items
                            .iter()
                            .filter(|i| i.page == pg)
                            .map(|i| i.text.as_str())
                            .collect();
                        if is_cid_garbage(&page_text) {
                            garbage_pages.insert(pg);
                        }
                    }
                    if garbage_pages.is_empty() {
                        (items, rects, lines)
                    } else {
                        log::debug!(
                            "suppressing garbage text from OCR-flagged pages: {:?}",
                            garbage_pages
                        );
                        let items: Vec<_> = items
                            .into_iter()
                            .filter(|i| !garbage_pages.contains(&i.page))
                            .collect();
                        let rects: Vec<_> = rects
                            .into_iter()
                            .filter(|r| !garbage_pages.contains(&r.page))
                            .collect();
                        let lines: Vec<_> = lines
                            .into_iter()
                            .filter(|l| !garbage_pages.contains(&l.page))
                            .collect();
                        (items, rects, lines)
                    }
                };

            let layout = compute_layout_complexity(&items, &rects, &lines);

            let md = if options.mode == ProcessMode::Analyze {
                None
            } else {
                Some(markdown::to_markdown_from_items_with_rects_and_lines(
                    items,
                    options.markdown,
                    &rects,
                    &lines,
                    &page_thresholds,
                    struct_roles.as_ref(),
                    &struct_tables,
                ))
            };

            let enc = md.as_ref().is_some_and(|m| detect_encoding_issues(m));
            (md, layout, enc, gid_encoded_pages)
        }
        None => (
            None,
            LayoutComplexity::default(),
            false,
            std::collections::HashSet::new(),
        ),
    };

    // If the extracted text is predominantly garbage (non-alphanumeric) and
    // the PDF is image-backed (Mixed/template), upgrade to Scanned — the text
    // layer comes from a bad OCR pass, and callers should use proper OCR.
    let (pdf_type, markdown, confidence) =
        if pdf_type == PdfType::Mixed && markdown.as_ref().is_some_and(|m| is_garbage_text(m)) {
            (PdfType::Scanned, None, 0.95)
        } else {
            (pdf_type, markdown, confidence)
        };

    // If a TextBased PDF produces garbage text, the fonts are undecodable
    // (e.g. Identity-H without ToUnicode for non-Latin scripts like Cyrillic).
    // Drop the useless markdown and flag all pages for OCR.
    let (markdown, has_encoding_issues, force_ocr_all) = if pdf_type == PdfType::TextBased
        && markdown.as_ref().is_some_and(|m| is_garbage_text(m))
    {
        log::debug!("TextBased PDF has garbage text — flagging all pages for OCR");
        (None, true, true)
    } else {
        (markdown, has_encoding_issues, false)
    };

    // Add pages with gid-encoded fonts (unresolvable encoding) to OCR list.
    // When ALL pages have gid-encoded fonts, suppress unreliable markdown.
    let all_gid = !gid_pages.is_empty() && gid_pages.len() as u32 >= page_count;
    let mut pages_needing_ocr = pages_needing_ocr;
    if force_ocr_all {
        pages_needing_ocr = (1..=page_count).collect();
    }
    if !gid_pages.is_empty() {
        log::debug!("pages with gid-encoded fonts (need OCR): {:?}", gid_pages);
        for page in gid_pages {
            if !pages_needing_ocr.contains(&page) {
                pages_needing_ocr.push(page);
            }
        }
        pages_needing_ocr.sort_unstable();
    }

    // Detect sparse extraction: when a TEXT-BASED PDF produces very few
    // characters per page, the text is likely embedded in images/forms
    // that need OCR.  Flag all pages for OCR in this case.
    // Only check when markdown was actually generated (not in Analyze mode).
    if pdf_type == PdfType::TextBased
        && page_count > 0
        && pages_needing_ocr.is_empty()
        && markdown.is_some()
    {
        let md_len = markdown.as_ref().map_or(0, |m| m.len());
        let chars_per_page = md_len as f32 / page_count as f32;
        if chars_per_page < 50.0 && md_len < 500 {
            log::debug!(
                "sparse extraction: {:.0} chars/page — recommending OCR for all {} pages",
                chars_per_page,
                page_count
            );
            pages_needing_ocr = (1..=page_count).collect();
        }
    }

    let markdown = if all_gid {
        log::debug!(
            "all {} pages have gid-encoded fonts — suppressing markdown output",
            page_count
        );
        None
    } else {
        markdown
    };

    Ok(PdfProcessResult {
        pdf_type,
        markdown,
        page_count,
        processing_time_ms: start.elapsed().as_millis() as u64,
        pages_needing_ocr,
        title,
        confidence,
        layout,
        has_encoding_issues,
    })
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Detect broken font encodings in extracted markdown text.
///
/// Two heuristics:
/// 1. **U+FFFD**: Any replacement character indicates decode failures.
/// 2. **Dollar-as-space**: Pattern like `Word$Word$Word` where `$` is used as a
///    word separator due to broken ToUnicode CMaps. Triggers when either:
///    - More than 50% of `$` are between letters (clear substitution pattern), OR
///    - More than 20 letter-dollar-letter occurrences (even if some `$` are also
///      used as trailing/leading separators, 20+ is far beyond normal financial text).
fn detect_encoding_issues(markdown: &str) -> bool {
    // Heuristic 1: U+FFFD replacement characters
    if markdown.contains('\u{FFFD}') {
        return true;
    }

    // Heuristic 2: dollar-as-space pattern
    let total_dollars = markdown.matches('$').count();
    if total_dollars > 10 {
        let bytes = markdown.as_bytes();
        let mut letter_dollar_letter = 0usize;
        for i in 1..bytes.len().saturating_sub(1) {
            if bytes[i] == b'$'
                && bytes[i - 1].is_ascii_alphabetic()
                && bytes[i + 1].is_ascii_alphabetic()
            {
                letter_dollar_letter += 1;
            }
        }
        if letter_dollar_letter > 20 || letter_dollar_letter * 2 > total_dollars {
            return true;
        }
    }

    false
}

/// Check if extracted text is predominantly garbage (non-alphanumeric).
///
/// Broken font encodings produce text like "----1-.-.-.___  --.-. .._ I_---."
/// where most characters are punctuation/symbols. Real text in any language
/// has >50% alphanumeric characters.
fn is_garbage_text(markdown: &str) -> bool {
    let mut alphanum = 0usize;
    let mut non_alphanum = 0usize;
    for ch in markdown.chars() {
        if ch.is_whitespace() {
            continue;
        }
        // Skip markdown syntax chars that we add (not from the PDF)
        if matches!(ch, '#' | '*' | '|' | '-' | '\n') {
            continue;
        }
        if ch.is_alphanumeric() {
            alphanum += 1;
        } else {
            non_alphanum += 1;
        }
    }
    let total = alphanum + non_alphanum;
    total >= 50 && alphanum * 2 < total
}

/// Detect garbage from failed CID-to-Unicode mapping on Identity-H fonts.
///
/// When CID values don't correspond to Unicode codepoints, the raw bytes often
/// produce characters in the C1 control range (U+0080–U+009F) or Private Use
/// Area, mixed with random Latin Extended characters.  Valid text in any
/// language almost never contains C1 controls.  We also fall back to the
/// general `is_garbage_text` check for non-alphanumeric-heavy patterns.
fn is_cid_garbage(text: &str) -> bool {
    if is_garbage_text(text) {
        return true;
    }
    let mut total = 0usize;
    let mut c1_control = 0usize;
    let mut high_latin = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        total += 1;
        // C1 control characters (U+0080–U+009F) — almost never in real text
        if ('\u{0080}'..='\u{009F}').contains(&ch) {
            c1_control += 1;
        }
        // High Latin-1 (U+00A0–U+00FF) — legitimate in Western European text
        // but when combined with ASCII in CID passthrough, indicates mojibake
        // from CID values being misinterpreted as Latin-1 characters.
        if ('\u{00A0}'..='\u{00FF}').contains(&ch) {
            high_latin += 1;
        }
    }
    if total < 5 {
        return false;
    }
    // If ≥5% of non-whitespace chars are C1 controls, it's garbage
    if c1_control * 20 >= total {
        return true;
    }
    // If ≥40% of non-whitespace chars are high Latin-1 AND the text has few
    // ASCII letters, it's likely CID-as-Latin-1 mojibake (Japanese/CJK PDFs
    // where CID values 0x80-0xFF become accented Latin characters).
    let ascii_letters = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    high_latin * 5 >= total * 2 && ascii_letters * 3 < total
}

/// Detect markdown tables with suspicious structure that suggest the heuristic
/// missed/mangled rows or columns. Returns true when the caller should treat
/// the result as `needs_ocr` and fall back to GPU OCR.
///
/// Catches three failure modes observed in production:
///
/// 1. **Header row looks like a data row** — first row starts with a numeric
///    value (e.g. `|2|...`), suggesting we missed the actual header above it.
///    Real headers almost never start with a bare number.
///
/// 2. **Header has empty cells in a multi-column table** — e.g.
///    `|Position||Administration|Administration|` (3+ cols, ≥1 empty cell).
///    Indicates poor column boundary detection.
///
/// 3. **Header has duplicate non-empty cells** in a multi-column table —
///    e.g. `Administration|Administration` appearing as adjacent cells means
///    we collapsed multi-line headers wrong.
///
/// Conservative by design: a few false positives (perfectly fine tables flagged)
/// just mean we run GPU OCR which is the existing safe path.
/// When `layout_assisted` is true (the layout model identified this region
/// as a table), we relax boundary-detection heuristics (numeric header,
/// empty header cells, sparse first data row) because the layout model
/// already gave us the table bbox — we're not guessing "is this a table?"
/// anymore, only "can we extract it correctly?". Paragraph and duplicate-
/// header checks stay, since those indicate genuine extraction quality
/// issues regardless of how the region was identified.
fn looks_like_partial_table_ex(markdown: &str, layout_assisted: bool) -> bool {
    let lines: Vec<&str> = markdown.lines().filter(|l| l.starts_with('|')).collect();
    if lines.len() < 2 {
        return false;
    }
    // Header is the first pipe-line; separator is the second
    let header_line = lines[0];
    let separator_line = lines.get(1).copied().unwrap_or("");
    let is_separator = |l: &str| l.chars().all(|c| matches!(c, '|' | '-' | ' '));
    if !is_separator(separator_line) {
        // No separator after the first line — not a well-formed pipe-table.
        // table_to_markdown always emits one when it returns content, so this
        // shouldn't happen in practice. If it does, fall through to OCR.
        return true;
    }

    // Parse header cells: split on '|', drop the leading/trailing empty pieces
    let cells: Vec<&str> = header_line.split('|').map(|s| s.trim()).collect::<Vec<_>>();
    // The first and last items are always empty (string starts and ends with '|')
    if cells.len() < 3 {
        return false;
    }
    let header_cells: Vec<&str> = cells[1..cells.len() - 1].to_vec();
    let n_cols = header_cells.len();
    if n_cols < 2 {
        // Single-column tables are usually lists/keys, not tables. Keep them
        // (caller can decide), but multi-column header checks below don't
        // apply.
        return false;
    }

    // Failure mode 1: header starts with a bare number (likely we missed
    // the real header row above). Skip when layout-assisted — the layout
    // model's bbox includes the real header; a numeric first cell (e.g.,
    // a year "2024") is legitimate.
    if !layout_assisted {
        if let Some(first) = header_cells.first() {
            let trimmed = first.trim();
            if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // Failure mode 2: header has empty cells in a multi-column table.
    // When layout-assisted, allow up to 1 empty header cell (common in
    // tables with merged/spanning header cells that we can't represent).
    let empty_count = header_cells.iter().filter(|c| c.is_empty()).count();
    if layout_assisted {
        // Reject only if >1 empty header cell (2+ means serious boundary issue)
        if n_cols >= 3 && empty_count >= 2 {
            return true;
        }
    } else if n_cols >= 3 && empty_count >= 1 {
        return true;
    }

    // Failure mode 3: header has duplicate non-empty cells
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for cell in &header_cells {
        if cell.is_empty() {
            continue;
        }
        if !seen.insert(cell) {
            return true;
        }
    }

    // Failure mode 4: first data row has many empty cells in a multi-column
    // table. Real tables rarely have a leading row with most cells blank;
    // when this happens it usually means the heuristic split a multi-row
    // header (e.g. "Position\nAdministration (1986-1992) | Administration
    // (1992-1998)") into a single-row header + a sparse data row.
    if let Some(first_data_line) = lines.get(2) {
        let data_cells: Vec<&str> = first_data_line
            .split('|')
            .map(|s| s.trim())
            .collect::<Vec<_>>();
        if data_cells.len() >= 3 {
            let data_inner = &data_cells[1..data_cells.len() - 1];
            let empty_data = data_inner.iter().filter(|c| c.is_empty()).count();
            // ≥3 cols, and significant portion of cells in the first data
            // row are empty → likely we mis-split a multi-row header.
            // When layout-assisted, relax from 33% to 50% — the bbox is
            // more reliable, and real tables with one sparse first row
            // (totals, subtotals) are common.
            let threshold = if layout_assisted { 2 } else { 3 };
            if n_cols >= 3 && empty_data * threshold >= n_cols {
                return true;
            }
        }
    }

    // Failure mode 5: cells flow as continuation paragraph (text wrapping
    // mistaken for column structure). When a paragraph of prose gets mis-
    // detected as a multi-column table, cells in the same column tend to
    // start with lowercase letters or punctuation (continuation), not
    // capital letters / digits (new entries). Real tables almost never
    // have most data cells starting lowercase.
    //
    // Signal: ≥2 cols, ≥4 data rows, and ≥60% of non-empty data cells
    // start with a lowercase letter or continuation punctuation.
    let data_rows: Vec<Vec<&str>> = lines
        .iter()
        .skip(2) // header + separator
        .map(|l| {
            let parts: Vec<&str> = l.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 3 {
                parts[1..parts.len() - 1].to_vec()
            } else {
                Vec::new()
            }
        })
        .filter(|cells| !cells.is_empty())
        .collect();

    if n_cols >= 2 && data_rows.len() >= 4 {
        let mut continuation = 0;
        let mut total = 0;
        for row in &data_rows {
            for cell in row {
                let trimmed = cell.trim();
                if trimmed.is_empty() {
                    continue;
                }
                total += 1;
                let first = trimmed.chars().next().unwrap();
                // Continuation indicators: lowercase letter, common
                // mid-sentence punctuation, closing quote
                if first.is_lowercase()
                    || matches!(first, ',' | '.' | ';' | ')' | '"' | '\'' | '”' | '’')
                {
                    continuation += 1;
                }
            }
        }
        if total > 0 && continuation * 5 >= total * 3 {
            // ≥60% of cells look like sentence continuations → paragraph
            // misread as table.
            return true;
        }
    }

    false
}

/// Original strict validation (no layout assistance). Used by tests and
/// full-page extraction paths that don't have layout model assistance.
#[cfg(test)]
fn looks_like_partial_table(markdown: &str) -> bool {
    looks_like_partial_table_ex(markdown, false)
}

#[cfg(test)]
mod looks_like_partial_table_tests {
    use super::{looks_like_partial_table, looks_like_partial_table_ex};

    #[test]
    fn good_table_passes() {
        let md = "|Name|Year|Country|\n|---|---|---|\n|Alice|2020|US|\n|Bob|2021|UK|";
        assert!(
            !looks_like_partial_table(md),
            "should not flag well-formed table"
        );
    }

    #[test]
    fn header_starting_with_number_is_partial() {
        // Heuristic missed the actual header row above
        let md = "|2|Cambodian Women for Peace|9,835|\n|---|---|---|\n|3|Association|711|";
        assert!(looks_like_partial_table(md));
    }

    #[test]
    fn header_with_empty_cells_in_3col_is_partial() {
        // Empty cell in 3+ column header → bad column detection
        let md =
            "|Position||Administration|Administration|\n|---|---|---|---|\n|Senate|24|8.3|16.7|";
        assert!(looks_like_partial_table(md));
    }

    #[test]
    fn header_with_duplicate_cells_is_partial() {
        // Duplicate "Administration" → collapsed multi-line header wrong
        let md =
            "|Position|Administration|Administration|Notes|\n|---|---|---|---|\n|Senate|24|16|x|";
        assert!(looks_like_partial_table(md));
    }

    #[test]
    fn two_column_with_one_empty_cell_passes() {
        // Many real two-column tables have key-only rows; don't penalise.
        let md = "|Key||\n|---|---|\n|Alice|123|\n|Bob|456|";
        // Header "Key|" has one empty cell but only 2 cols total — keep it.
        assert!(!looks_like_partial_table(md));
    }

    #[test]
    fn single_column_table_is_kept() {
        // Single-column "tables" are common (lists). Caller can decide; we
        // don't second-guess based on column count alone.
        let md = "|Item|\n|---|\n|First|\n|Second|";
        assert!(!looks_like_partial_table(md));
    }

    #[test]
    fn no_table_at_all_returns_true() {
        // table_to_markdown should never produce this, but defensive — if
        // there's no separator, treat as not-a-table.
        let md = "Just some text\nWith multiple lines";
        // No lines start with '|' so we return false (no header to inspect).
        assert!(!looks_like_partial_table(md));
    }

    #[test]
    fn first_data_row_with_many_empty_cells_is_partial() {
        // Multi-row header collapsed to single-row → first "data row" has
        // most cells empty (the actual sub-header values).
        let md = "|Government|No. of Seats|Aquino|Ramos|\n|---|---|---|---|\n|Position|||(1986-1992)|\n|Senate|24|8.3|16.7|";
        assert!(looks_like_partial_table(md));
    }

    #[test]
    fn first_data_row_with_one_empty_cell_in_4col_passes() {
        // Real data rows can have one empty cell (e.g. missing value);
        // only flag when ≥1/3 of cells are empty.
        let md = "|A|B|C|D|\n|---|---|---|---|\n|x|y||z|\n|p|q|r|s|";
        assert!(!looks_like_partial_table(md));
    }

    #[test]
    fn paragraph_misread_as_two_column_table_is_partial() {
        // Real production failure: text-wrapped paragraph mis-detected as
        // 2-col table. Each cell continues the previous one as prose.
        let md = "|Approval is needed from the|Acquisitions of|\n\
                  |---|---|\n\
                  |Treasurer if the acquisition|residential and|\n\
                  |constitutes a \"significant|agricultural|\n\
                  |action,\" including acquiring an|land by foreign|\n\
                  |interest in different types of|persons must be|\n\
                  |land where the monetary|reported to the|";
        assert!(looks_like_partial_table(md));
    }

    #[test]
    fn real_multi_word_table_is_kept() {
        // Real table with multi-word entries — cells start with capital
        // letters / proper nouns, NOT lowercase continuations.
        let md = "|Country|Capital|Notes|\n\
                  |---|---|---|\n\
                  |United States|Washington DC|Federal capital|\n\
                  |United Kingdom|London|City of London is a separate|\n\
                  |France|Paris|Île-de-France region|\n\
                  |Germany|Berlin|Reunified 1990|\n\
                  |Spain|Madrid|Largest city in Spain|";
        assert!(!looks_like_partial_table(md));
    }

    // --- layout_assisted relaxation tests ---

    #[test]
    fn numeric_header_accepted_when_layout_assisted() {
        // Year as first header cell is valid when layout model gave us the bbox.
        let md = "|2024|Revenue|Growth|\n|---|---|---|\n|Q1|1.2M|5%|\n|Q2|1.4M|8%|";
        assert!(
            looks_like_partial_table(md),
            "strict mode rejects numeric header"
        );
        assert!(
            !looks_like_partial_table_ex(md, true),
            "layout-assisted should accept"
        );
    }

    #[test]
    fn one_empty_header_accepted_when_layout_assisted() {
        // Common in merged-header tables: one spanning cell leaves a gap.
        let md = "|Position||Senate|House|\n|---|---|---|---|\n|Chair|1|2|3|\n|Vice|4|5|6|";
        assert!(
            looks_like_partial_table(md),
            "strict rejects 1 empty header"
        );
        assert!(
            !looks_like_partial_table_ex(md, true),
            "layout-assisted allows 1 empty"
        );
    }

    #[test]
    fn two_empty_headers_still_rejected_when_layout_assisted() {
        // 2+ empty headers is still bad even with layout assistance.
        let md = "|A|||D|\n|---|---|---|---|\n|x|y|z|w|";
        assert!(
            looks_like_partial_table_ex(md, true),
            "2 empty headers rejected even layout-assisted"
        );
    }

    #[test]
    fn sparse_first_row_relaxed_when_layout_assisted() {
        // 1/4 empty = 25%, below strict 33% threshold but accepted by layout-assisted 50%.
        let md = "|A|B|C|D|\n|---|---|---|---|\n|x||y|z|\n|p|q|r|s|";
        assert!(!looks_like_partial_table(md), "strict: 25% empty is OK");
        // 2/4 = 50%, strict would flag (2*3>=4), relaxed threshold (2*2>=4) would also flag.
        let md2 = "|A|B|C|D|\n|---|---|---|---|\n|||y|z|\n|p|q|r|s|";
        assert!(looks_like_partial_table(md2), "strict: 50% empty flagged");
        assert!(
            looks_like_partial_table_ex(md2, true),
            "layout-assisted: 50% also flagged"
        );
        // 2/6 = 33%, strict flags (2*3>=6), relaxed does not (2*2<6)
        let md3 = "|A|B|C|D|E|F|\n|---|---|---|---|---|---|\n|x|||y|z|w|\n|a|b|c|d|e|f|";
        assert!(looks_like_partial_table(md3), "strict: 33% flagged");
        assert!(
            !looks_like_partial_table_ex(md3, true),
            "layout-assisted: 33% accepted"
        );
    }

    #[test]
    fn paragraph_still_rejected_when_layout_assisted() {
        // Paragraph detection is not relaxed — it's a genuine extraction issue.
        let md = "|Approval is needed from the|Acquisitions of|\n\
                  |---|---|\n\
                  |Treasurer if the acquisition|residential and|\n\
                  |constitutes a \"significant|agricultural|\n\
                  |action,\" including acquiring an|land by foreign|\n\
                  |interest in different types of|persons must be|\n\
                  |land where the monetary|reported to the|";
        assert!(
            looks_like_partial_table_ex(md, true),
            "paragraph rejection stays strict"
        );
    }

    #[test]
    fn duplicate_headers_still_rejected_when_layout_assisted() {
        let md =
            "|Position|Administration|Administration|Notes|\n|---|---|---|---|\n|Senate|24|16|x|";
        assert!(
            looks_like_partial_table_ex(md, true),
            "duplicate headers rejected even layout-assisted"
        );
    }
}

/// Analyse extracted items and rects for layout complexity.
fn compute_layout_complexity(
    items: &[types::TextItem],
    rects: &[types::PdfRect],
    lines: &[types::PdfLine],
) -> LayoutComplexity {
    use markdown::analysis::calculate_font_stats_from_items;

    // --- Collect unique pages ---
    let mut seen_pages: Vec<u32> = items.iter().map(|i| i.page).collect();
    seen_pages.sort();
    seen_pages.dedup();

    let font_stats = calculate_font_stats_from_items(items);
    let base_size = font_stats.most_common_size;

    // --- Tables: use rect-based → line-based → heuristic detectors per page,
    //     with side-by-side band splitting ---
    let mut pages_with_tables: Vec<u32> = Vec::new();
    for &page in &seen_pages {
        let page_items: Vec<&types::TextItem> = items.iter().filter(|i| i.page == page).collect();

        // Check for side-by-side layout
        let owned_items: Vec<types::TextItem> = page_items.iter().map(|i| (*i).clone()).collect();
        let bands = markdown::split_side_by_side(&owned_items);

        let band_ranges: Vec<(f32, f32)> = if bands.is_empty() {
            // Single region — use sentinel range that includes everything
            vec![(f32::MIN, f32::MAX)]
        } else {
            bands
        };

        let mut found_table = false;
        for &(x_lo, x_hi) in &band_ranges {
            let margin = 2.0;
            let band_items: Vec<types::TextItem> = owned_items
                .iter()
                .filter(|item| {
                    x_lo == f32::MIN || (item.x >= x_lo - margin && item.x < x_hi + margin)
                })
                .cloned()
                .collect();

            let band_rects: Vec<types::PdfRect> = if x_lo == f32::MIN {
                rects.iter().filter(|r| r.page == page).cloned().collect()
            } else {
                markdown::filter_rects_to_band(rects, page, x_lo, x_hi)
            };

            let band_lines: Vec<types::PdfLine> = if x_lo == f32::MIN {
                lines.iter().filter(|l| l.page == page).cloned().collect()
            } else {
                markdown::filter_lines_to_band(lines, page, x_lo, x_hi)
            };

            // TOC pages route through the table detector but render as flat
            // lists. They aren't tables in any user-facing sense, so don't
            // count them toward LayoutComplexity (would also trip the
            // table-page guard in column detection below).
            let has_data_table =
                |tables: &[tables::Table]| tables.iter().any(|t| t.kind == tables::TableKind::Data);

            let (rect_tables, _) = tables::detect_tables_from_rects(&band_items, &band_rects, page);
            if has_data_table(&rect_tables) {
                found_table = true;
                break;
            }
            let line_tables = tables::detect_tables_from_lines(&band_items, &band_lines, page);
            if has_data_table(&line_tables) {
                found_table = true;
                break;
            }
            // Heuristic fallback for borderless tables
            let heuristic_tables = tables::detect_tables(&band_items, base_size, false);
            if has_data_table(&heuristic_tables) {
                found_table = true;
                break;
            }
        }
        if found_table {
            pages_with_tables.push(page);
        }
    }

    let mut pages_with_columns: Vec<u32> = Vec::new();
    for page in seen_pages {
        let cols = extractor::detect_columns(items, page, pages_with_tables.contains(&page));
        if cols.len() >= 2 {
            pages_with_columns.push(page);
        }
    }

    let is_complex = !pages_with_tables.is_empty() || !pages_with_columns.is_empty();

    LayoutComplexity {
        is_complex,
        pages_with_tables,
        pages_with_columns,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("PDF parsing error: {0}")]
    Parse(String),
    #[error("PDF is encrypted")]
    Encrypted,
    #[error("Invalid PDF structure")]
    InvalidStructure,
    #[error("Not a PDF: {0}")]
    NotAPdf(String),
}

impl From<lopdf::Error> for PdfError {
    fn from(e: lopdf::Error) -> Self {
        match e {
            lopdf::Error::IO(io_err) => PdfError::Io(io_err),
            lopdf::Error::Decryption(_)
            | lopdf::Error::InvalidPassword
            | lopdf::Error::AlreadyEncrypted
            | lopdf::Error::UnsupportedSecurityHandler(_) => PdfError::Encrypted,
            lopdf::Error::Unimplemented(msg) if msg.contains("encrypted") => PdfError::Encrypted,
            lopdf::Error::Parse(ref pe) if pe.to_string().contains("invalid file header") => {
                PdfError::NotAPdf("invalid PDF file header".to_string())
            }
            lopdf::Error::MissingXrefEntry
            | lopdf::Error::Xref(_)
            | lopdf::Error::IndirectObject { .. }
            | lopdf::Error::ObjectIdMismatch
            | lopdf::Error::InvalidObjectStream(_)
            | lopdf::Error::InvalidOffset(_) => PdfError::InvalidStructure,
            other => PdfError::Parse(other.to_string()),
        }
    }
}

/// Check whether a `lopdf::Error` represents an encryption-related failure.
pub(crate) fn is_encrypted_lopdf_error(e: &lopdf::Error) -> bool {
    matches!(
        e,
        lopdf::Error::Decryption(_)
            | lopdf::Error::InvalidPassword
            | lopdf::Error::AlreadyEncrypted
            | lopdf::Error::UnsupportedSecurityHandler(_)
    ) || matches!(e, lopdf::Error::Unimplemented(msg) if msg.contains("encrypted"))
}

// ---------------------------------------------------------------------------
// PDF validation helpers
// ---------------------------------------------------------------------------

/// Strip UTF-8 BOM and leading ASCII whitespace from a byte slice.
fn strip_bom_and_whitespace(bytes: &[u8]) -> &[u8] {
    let b = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    let start = b
        .iter()
        .position(|&c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    &b[start..]
}

/// Case-insensitive prefix check on byte slices.
fn starts_with_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    haystack[..needle.len()]
        .iter()
        .zip(needle)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Try to identify what kind of file the bytes represent.
fn detect_file_type_hint(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "file is empty".to_string();
    }

    let trimmed = strip_bom_and_whitespace(bytes);

    // HTML
    if starts_with_ci(trimmed, b"<!doctype html")
        || starts_with_ci(trimmed, b"<html")
        || starts_with_ci(trimmed, b"<head")
        || starts_with_ci(trimmed, b"<body")
    {
        return "file appears to be HTML".to_string();
    }

    // XML (but not HTML)
    if trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<") {
        if starts_with_ci(trimmed, b"<?xml") {
            return "file appears to be XML".to_string();
        }
        if trimmed.starts_with(b"<") && !trimmed.starts_with(b"<%") {
            return "file appears to be XML".to_string();
        }
    }

    // JSON
    if trimmed.starts_with(b"{") || trimmed.starts_with(b"[") {
        return "file appears to be JSON".to_string();
    }

    // PNG
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return "file appears to be a PNG image".to_string();
    }

    // JPEG
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "file appears to be a JPEG image".to_string();
    }

    // ZIP / Office documents
    if bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        return "file appears to be a ZIP archive (possibly an Office document)".to_string();
    }

    // If it looks like mostly printable ASCII/UTF-8, call it plain text
    let sample = &bytes[..bytes.len().min(512)];
    let printable = sample
        .iter()
        .filter(|&&b| b.is_ascii_graphic() || b.is_ascii_whitespace())
        .count();
    if printable > sample.len() * 3 / 4 {
        return "file appears to be plain text".to_string();
    }

    "file is not a PDF".to_string()
}

/// Validate that a byte buffer looks like a PDF (has `%PDF-` magic).
///
/// Scans the first 1024 bytes, allowing for a UTF-8 BOM and leading whitespace.
pub(crate) fn validate_pdf_bytes(buffer: &[u8]) -> Result<(), PdfError> {
    if buffer.is_empty() {
        return Err(PdfError::NotAPdf(detect_file_type_hint(buffer)));
    }

    let header = &buffer[..buffer.len().min(1024)];
    let trimmed = strip_bom_and_whitespace(header);

    if trimmed.starts_with(b"%PDF-") {
        Ok(())
    } else {
        Err(PdfError::NotAPdf(detect_file_type_hint(buffer)))
    }
}

/// Validate that a file on disk looks like a PDF.
///
/// Reads only the first 1024 bytes and delegates to [`validate_pdf_bytes`].
pub(crate) fn validate_pdf_file<P: AsRef<Path>>(path: P) -> Result<(), PdfError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 1024];
    let n = file.read(&mut buf)?;
    validate_pdf_bytes(&buf[..n])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn test_item(text: &str, x: f32, y: f32, width: f32, height: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width,
            height,
            font: "Helvetica".to_string(),
            font_size: height,
            page: 1,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn test_detect_encoding_issues_fffd() {
        assert!(detect_encoding_issues(
            "Some text with \u{FFFD} replacement"
        ));
    }

    #[test]
    fn test_detect_encoding_issues_dollar_as_space() {
        // Simulates broken CMap: "$Workshop$on$Chest$Wall$Deformities$and$..."
        let garbled = "Last$advanced$Book$Programm$3th$Workshop$on$Chest$Wall$Deformities$and$More";
        assert!(detect_encoding_issues(garbled));
    }

    #[test]
    fn test_detect_encoding_issues_financial_text() {
        // Legitimate dollar signs in financial text should NOT trigger
        let financial = "Revenue was $100M in Q1, up from $90M. Costs: $50M, $30M, $20M, $15M, $12M, $8M, $5M, $3M, $2M, $1M, $500K.";
        assert!(!detect_encoding_issues(financial));
    }

    #[test]
    fn test_detect_encoding_issues_clean_text() {
        assert!(!detect_encoding_issues(
            "Normal markdown text with no issues."
        ));
    }

    #[test]
    fn test_detect_encoding_issues_few_dollars() {
        // Under threshold of 10 total dollars — should not trigger
        let text = "a$b c$d e$f";
        assert!(!detect_encoding_issues(text));
    }

    #[test]
    fn test_garbage_text_detection() {
        // Simulates garbage output from Identity-H fonts without ToUnicode.
        // Needs >= 50 non-whitespace chars and < 50% alphanumeric.
        let garbage = ",&<X ~%5&8-!A ~*(!,-!U (/#!U X ~#/=U 9/%*(!U !(  X \
                       (%U-(-/ V %&((8-#&&< *,(6--< %5&8-!( (,(/! #/<5U X \
                       º&( >/5 /5&(#(8-!5 *,(6--( *,%@/-A W";
        assert!(is_garbage_text(garbage));

        // Normal text should not be garbage
        let normal = "This is a normal paragraph with words and sentences that contains enough characters to pass the threshold.";
        assert!(!is_garbage_text(normal));

        // Cyrillic text should not be garbage
        let cyrillic =
            "Роботизированные технологии комплексы для производства металлургических предприятий";
        assert!(!is_garbage_text(cyrillic));
    }

    #[test]
    fn test_cid_garbage_detection() {
        // Simulates CID garbage from Identity-H fonts: Latin Extended chars
        // mixed with C1 control characters (U+0080–U+009F).
        let cid_garbage = "Ë>íÓ\tý\r\u{0088}æ&Ït\u{0094}äí;\ný;wAL¢©èåD\rü£\
                           qq\u{0096}¶Í Æ\réá; Ô 7G\u{008B}ý;èÕç¢ £ ý;C";
        assert!(
            is_cid_garbage(cid_garbage),
            "CID garbage with C1 controls should be detected"
        );

        // Valid Korean text (CID-as-Unicode passthrough) should NOT be garbage
        let korean = "본 가격표는 국내 거주 중인 외국인을 위한 한국어 가격표의 비공식 번역본입니다";
        assert!(
            !is_cid_garbage(korean),
            "Valid Korean text should not be flagged as garbage"
        );

        // Valid Japanese text should NOT be garbage
        let japanese = "羽田空港新飛行経路に係る航空機騒音の測定結果";
        assert!(
            !is_cid_garbage(japanese),
            "Valid Japanese text should not be flagged as garbage"
        );
    }

    #[test]
    fn tsr_text_fill_does_not_pull_neighboring_overlapping_rows() {
        use crate::tables::structured::normalize_cell_bands;
        use crate::tables::StructuredCell;

        let items = vec![
            test_item("Branch Name", 12.0, 88.0, 55.0, 8.0),
            test_item("Deposits", 112.0, 88.0, 36.0, 8.0),
            test_item("Oak Street", 12.0, 72.0, 48.0, 8.0),
            test_item("100", 112.0, 72.0, 18.0, 8.0),
            test_item("Boardwalk", 12.0, 55.2, 46.0, 8.0),
            test_item("200", 112.0, 55.2, 18.0, 8.0),
        ];
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [10.0, 100.0, 100.0, 125.0],
            },
            StructuredCell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [100.0, 100.0, 170.0, 125.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 116.0, 100.0, 141.0],
            },
            StructuredCell {
                row: 1,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [100.0, 116.0, 170.0, 141.0],
            },
            StructuredCell {
                row: 2,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 132.8, 100.0, 157.8],
            },
            StructuredCell {
                row: 2,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [100.0, 132.8, 170.0, 157.8],
            },
        ];

        normalize_cell_bands(&mut cells);
        for cell in &mut cells {
            let [x1, y1, x2, y2] = cell.page_pt_bbox;
            cell.text = collect_text_in_tsr_cell(
                &items,
                x1,
                y1,
                x2,
                y2,
                200.0,
                RegionCoordSpace::Standard,
                0.10,
            );
        }

        assert_eq!(cells[0].text, "Branch Name");
        assert_eq!(cells[2].text, "Oak Street");
        assert_eq!(cells[4].text, "Boardwalk");
        assert!(!cells[0].text.contains("Oak Street"));
        assert!(!cells[2].text.contains("Branch Name"));
        assert!(!cells[2].text.contains("Boardwalk"));
    }

    #[test]
    fn multi_row_expansion_splits_overstuffed_tsr_row() {
        use crate::tables::{cells_to_markdown, StructuredCell};

        let items = vec![
            test_item("Branch Name", 20.0, 166.0, 55.0, 8.0),
            test_item("Deposits", 120.0, 166.0, 36.0, 8.0),
            test_item("Oak Street", 20.0, 136.0, 48.0, 8.0),
            test_item("100", 120.0, 136.0, 18.0, 8.0),
            test_item("Boardwalk", 20.0, 116.0, 46.0, 8.0),
            test_item("200", 120.0, 116.0, 18.0, 8.0),
        ];
        let cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: "Branch Name".into(),
                page_pt_bbox: [10.0, 20.0, 100.0, 40.0],
            },
            StructuredCell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: "Deposits".into(),
                page_pt_bbox: [110.0, 20.0, 190.0, 40.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "Oak Street Boardwalk".into(),
                page_pt_bbox: [10.0, 40.0, 100.0, 100.0],
            },
            StructuredCell {
                row: 1,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "100 200".into(),
                page_pt_bbox: [110.0, 40.0, 190.0, 100.0],
            },
        ];

        let expanded =
            try_expand_multi_row_cells(&cells, &items, 200.0, RegionCoordSpace::Standard, 0.10)
                .expect("overstuffed data row should expand");
        let md = cells_to_markdown(&expanded);

        assert_eq!(expanded.iter().map(|c| c.row).max().unwrap() + 1, 3);
        assert!(
            md.contains("|Oak Street|100|"),
            "missing first data row: {md}"
        );
        assert!(
            md.contains("|Boardwalk|200|"),
            "missing second data row: {md}"
        );
        assert!(
            !md.contains("Oak Street Boardwalk"),
            "compressed cell text should be replaced: {md}"
        );
    }

    #[test]
    fn tsr_assignment_caps_uses_median_geometry() {
        use crate::tables::StructuredCell;
        let cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [0.0, 0.0, 100.0, 20.0], // 100x20
            },
            StructuredCell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [100.0, 0.0, 200.0, 20.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [0.0, 20.0, 100.0, 40.0],
            },
        ];
        let (cap_x, cap_y) = tsr_assignment_caps(&cells);
        assert_eq!(cap_x, 100.0);
        assert_eq!(cap_y, 20.0);
    }

    #[test]
    fn tsr_assignment_caps_floor_protects_degenerate_input() {
        use crate::tables::StructuredCell;
        let cells = vec![StructuredCell {
            row: 0,
            col: 0,
            rowspan: 1,
            colspan: 1,
            is_header: false,
            text: String::new(),
            page_pt_bbox: [0.0, 0.0, 1.0, 1.0],
        }];
        let (cap_x, cap_y) = tsr_assignment_caps(&cells);
        assert_eq!(cap_x, 5.0);
        assert_eq!(cap_y, 5.0);
    }

    #[test]
    fn stage2_recovers_left_aligned_header_text_outside_data_band() {
        // Symptom A reproduction: the column band derived from data-cell
        // centers ends up too far right, so header text positioned at the
        // left of the column falls outside the band and stage 1's strict
        // membership rejects it. Stage 2 should re-attach by proximity.
        //
        // Item coords are bottom-left native; cell page_pt_bbox is top-left.
        // page_height=200 so a top-left bbox y=[88, 100] flips to native y
        // bounds [100, 112]; an item at native y=104 (center 108) lands in.
        use crate::tables::StructuredCell;
        let items = vec![
            // Header text — centered in row 0 (native y=104, center 108) but
            // at the LEFT of the column (x=175, far left of the [410, 700]
            // data-derived band).
            test_item("Address", 175.0, 104.0, 50.0, 8.0),
            // Data row 1 — fits its cell.
            test_item("205 W Oak St", 420.0, 84.0, 100.0, 8.0),
            // Data row 2 — fits its cell.
            test_item("155 E Boardwalk Dr", 420.0, 64.0, 100.0, 8.0),
        ];
        // Cells AFTER normalize_cell_bands would have run — col 0 band
        // shifted right by data-cell centers, header cell now excludes
        // the "Address" text at center x=200.
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [410.0, 88.0, 700.0, 100.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [410.0, 108.0, 700.0, 116.0],
            },
            StructuredCell {
                row: 2,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [410.0, 128.0, 700.0, 136.0],
            },
        ];
        let page_h = 200.0;

        // Stage 1 mimic — fill cells via the strict rule, track claimed.
        let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for cell in &mut cells {
            let [x1, y1, x2, y2] = cell.page_pt_bbox;
            let bounds = region_bounds(x1, y1, x2, y2, page_h, RegionCoordSpace::Standard);
            let mut matched: Vec<TextItem> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                if tsr_region_contains_item(item, bounds) {
                    claimed.insert(i);
                    matched.push(item.clone());
                }
            }
            cell.text = collect_text_from_matched_items(matched, 0.10).replace(['\n', '\r'], " ");
        }
        // Header is empty after stage 1 (Address fell outside col 0 band).
        assert_eq!(cells[0].text, "", "header should be empty after stage 1");
        // Data rows already populated.
        assert!(
            cells[1].text.contains("Oak"),
            "data row 1 should contain Oak: got {:?}",
            cells[1].text
        );
        assert!(
            cells[2].text.contains("Boardwalk"),
            "data row 2 should contain Boardwalk: got {:?}",
            cells[2].text
        );

        // Stage 2 should fill the orphan "Address" into the empty header.
        tsr_assign_orphan_items(
            &items,
            &mut cells,
            &claimed,
            page_h,
            RegionCoordSpace::Standard,
        );
        assert_eq!(cells[0].text, "Address");
        // Data rows must NOT have been augmented (already filled by stage 1).
        assert!(!cells[1].text.contains("Address"));
        assert!(!cells[2].text.contains("Address"));
    }

    #[test]
    fn stage2_recovers_y_shifted_col0_in_consecutive_rows() {
        // Symptom B reproduction: a stretch of rows where col 0 cell bboxes
        // sit just above the actual branch-name text. After stage 1 those
        // cells are empty; stage 2 should pull the orphan items in by
        // y-proximity.
        //
        // page_height=800. Cells are 14pt tall in top-left; flipped native
        // bounds are [240,254], [220,234], [200,214]. Items sit ~1pt below
        // each cell's native y range (still within ~1pt of the edge), so
        // both center-containment and 60% overlap fail in stage 1.
        use crate::tables::StructuredCell;
        let items = vec![
            // Bellevue: native y=235, center 239 — just below row 0's
            // cell native bottom (240). Closer to row 0 than row 1.
            test_item("Bellevue", 30.0, 235.0, 45.0, 8.0),
            // Glenwood: native y=215, center 219 — just below row 1.
            test_item("Glenwood", 30.0, 215.0, 45.0, 8.0),
            // Metro Crossing: native y=195, center 199 — just below row 2.
            test_item("Metro Crossing", 30.0, 195.0, 70.0, 8.0),
        ];
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 546.0, 200.0, 560.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 566.0, 200.0, 580.0],
            },
            StructuredCell {
                row: 2,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 586.0, 200.0, 600.0],
            },
        ];
        let page_h = 800.0;

        let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for cell in &mut cells {
            let [x1, y1, x2, y2] = cell.page_pt_bbox;
            let bounds = region_bounds(x1, y1, x2, y2, page_h, RegionCoordSpace::Standard);
            let mut matched: Vec<TextItem> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                if tsr_region_contains_item(item, bounds) {
                    claimed.insert(i);
                    matched.push(item.clone());
                }
            }
            cell.text = collect_text_from_matched_items(matched, 0.10).replace(['\n', '\r'], " ");
        }
        // All three cells empty after stage 1 (text falls just below each).
        for c in &cells {
            assert!(
                c.text.is_empty(),
                "stage 1 should leave all cells empty: {:?}",
                c
            );
        }

        tsr_assign_orphan_items(
            &items,
            &mut cells,
            &claimed,
            page_h,
            RegionCoordSpace::Standard,
        );
        assert_eq!(cells[0].text, "Bellevue");
        assert_eq!(cells[1].text, "Glenwood");
        assert_eq!(cells[2].text, "Metro Crossing");
    }

    #[test]
    fn stage2_rejects_cross_line_stacking_into_same_cell() {
        // Two orphans on different rows of the PDF, both equidistant from
        // the same empty cell. Without the same-line guard they'd both stack
        // into that cell ("Shawnee Blue Valley Parkway" run-on); the guard
        // keeps the first orphan and routes the second to the next-nearest
        // empty cell on its own line.
        use crate::tables::StructuredCell;
        // page_h=200. Two empty cells:
        //   cell X (row 0): top-left y=[100, 110], native [90, 100]
        //   cell Y (row 1): top-left y=[112, 122], native [78, 88]
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 100.0, 100.0, 110.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 112.0, 100.0, 122.0],
            },
        ];
        // Two orphans, different rows of the PDF (y differs by 14pt = a
        // full row), both 2pt outside their target cell — both within
        // cap_y, both equidistant-ish to cell X. Without the same-line
        // guard they'd both land in X.
        //   "Shawnee" should belong to cell X (row 0) — center y=98 is
        //   2pt below X's native min=100.
        //   "BlueValley" should belong to cell Y (row 1) — center y=84
        //   is 4pt above Y's native max=88.
        let items = vec![
            // Shawnee orphan — closer to X (dy=2) than Y (dy=6 from native min=78).
            test_item("Shawnee", 30.0, 94.0, 50.0, 8.0),
            // BlueValley orphan — closer to Y (dy=4) than X (dy=8 from native max=100).
            test_item("BlueValley", 30.0, 80.0, 60.0, 8.0),
        ];
        let claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        tsr_assign_orphan_items(
            &items,
            &mut cells,
            &claimed,
            200.0,
            RegionCoordSpace::Standard,
        );
        assert_eq!(cells[0].text, "Shawnee");
        assert_eq!(cells[1].text, "BlueValley");
        assert!(!cells[0].text.contains("BlueValley"));
        assert!(!cells[1].text.contains("Shawnee"));
    }

    #[test]
    fn stage2_allows_same_line_orphans_to_stack_into_one_cell() {
        // Multi-token branch names like "Blue Valley Parkway" are 3 PDF
        // text items at the SAME y-coordinate. They should all stack into
        // the cell their row's branch-name belongs to, not get split
        // across rows by the cross-line guard.
        use crate::tables::StructuredCell;
        let mut cells = vec![StructuredCell {
            row: 0,
            col: 0,
            rowspan: 1,
            colspan: 1,
            is_header: false,
            text: String::new(),
            page_pt_bbox: [10.0, 100.0, 200.0, 110.0],
        }];
        // Three same-line items, all 2pt below the cell's native bottom.
        let items = vec![
            test_item("Blue", 30.0, 94.0, 25.0, 8.0),
            test_item("Valley", 60.0, 94.0, 35.0, 8.0),
            test_item("Parkway", 100.0, 94.0, 45.0, 8.0),
        ];
        let claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        tsr_assign_orphan_items(
            &items,
            &mut cells,
            &claimed,
            200.0,
            RegionCoordSpace::Standard,
        );
        // All three same-line orphans stacked into the single empty cell.
        assert_eq!(cells[0].text, "Blue Valley Parkway");
    }

    #[test]
    fn stage2_does_not_overwrite_filled_cells_or_admit_far_orphans() {
        // Stage 2 must only fill EMPTY cells (preserves stage 1's strict
        // behavior on bleed cases) and must reject orphans that fall far
        // outside any cell (prevents pulling a figure title into a table).
        use crate::tables::StructuredCell;
        let items = vec![
            test_item("Real", 50.0, 100.0, 30.0, 8.0),
            // Far orphan — at native y=20 (page bottom edge) on a page where
            // the table sits around native y=92..104 (top-left y=96..108).
            // y-distance to nearest cell is ~70pt, far exceeding the ~12pt
            // cap from median row height.
            test_item("FigureTitle", 50.0, 20.0, 60.0, 8.0),
        ];
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [40.0, 96.0, 100.0, 108.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "Pre-filled".to_string(),
                page_pt_bbox: [40.0, 116.0, 100.0, 128.0],
            },
        ];
        let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // Pretend "Real" got claimed by a different cell (won't be re-assigned).
        // Don't claim "FigureTitle" — it's the far orphan.
        claimed.insert(0);

        tsr_assign_orphan_items(
            &items,
            &mut cells,
            &claimed,
            200.0,
            RegionCoordSpace::Standard,
        );
        // Empty cell stayed empty (orphan was too far).
        assert_eq!(cells[0].text, "");
        // Pre-filled cell was not touched.
        assert_eq!(cells[1].text, "Pre-filled");
    }
}
