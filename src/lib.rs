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
pub mod formula_latex;
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
use std::collections::{HashMap, HashSet};
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

/// Extract formatted markdown for specific pages of a PDF, with layout
/// classification metadata.
///
/// Unlike [`process_pdf_mem`] which returns one concatenated markdown string,
/// this returns per-page markdown so callers can mix direct extraction
/// (for simple text pages) with GPU OCR (for complex/scanned pages).
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
    pages: &[u32],
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

    let mut results = Vec::with_capacity(pages.len());
    let mut pages_needing_ocr = Vec::new();

    for &page_0idx in pages {
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

/// Shared page extraction state for region-based extraction functions.
struct RegionExtractionData {
    items_by_page: HashMap<u32, Vec<TextItem>>,
    page_heights: HashMap<u32, f32>,
    #[allow(dead_code)]
    gid_pages: HashSet<u32>,
    page_thresholds: HashMap<u32, f32>,
    rotated_pages: HashSet<u32>,
}

/// Extract text items, page heights, and metadata for the pages needed by region queries.
///
/// This is the shared boilerplate for `extract_text_in_regions_mem`,
/// `extract_tables_in_regions_mem`, and `extract_formulas_in_regions_mem`.
fn prepare_region_extraction(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
) -> Result<RegionExtractionData, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, _page_count) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();

    let needed_pages: HashSet<u32> = page_regions.iter().map(|(p, _)| p + 1).collect();

    // Fast mode: skip expensive TrueType font fallback parsing.
    // Fonts that can't be decoded from ToUnicode alone will produce empty/garbage
    // text, triggering needs_ocr=true → GPU OCR fallback in the pipeline.
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

    Ok(RegionExtractionData {
        items_by_page,
        page_heights,
        gid_pages,
        page_thresholds,
        rotated_pages,
    })
}

/// Resolve per-page coord space and adaptive threshold for a given page.
fn page_region_context(
    data: &RegionExtractionData,
    page_1idx: u32,
) -> (f32, f32, RegionCoordSpace) {
    let page_h = data.page_heights.get(&page_1idx).copied().unwrap_or(792.0);
    let adaptive_threshold = data
        .page_thresholds
        .get(&page_1idx)
        .copied()
        .unwrap_or(0.10);
    let coords = if data.rotated_pages.contains(&page_1idx) {
        RegionCoordSpace::Rotated90Ccw
    } else {
        RegionCoordSpace::Standard
    };
    (page_h, adaptive_threshold, coords)
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
    let data = prepare_region_extraction(buffer, page_regions)?;

    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = data.items_by_page.get(&page_1idx);
        let (page_h, adaptive_threshold, coords) = page_region_context(&data, page_1idx);

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
    let data = prepare_region_extraction(buffer, page_regions)?;

    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = data.items_by_page.get(&page_1idx);
        let (page_h, _adaptive_threshold, coords) = page_region_context(&data, page_1idx);

        let mut page_results = Vec::with_capacity(regions.len());

        for rect in regions {
            let [rx1, ry1, rx2, ry2] = *rect;

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

/// Extract formula text within bounding-box regions from a PDF in memory.
///
/// Similar to [`extract_text_in_regions_mem`] but uses formula-specific quality
/// checks. Formula text is legitimately symbol-heavy (Greek letters, math
/// operators, subscripts) so the generic `is_garbage_text` check — which rejects
/// text with <50% alphanumeric characters — would false-positive on valid
/// formula regions.
///
/// When the extracted text decodes cleanly, `needs_ocr` is `false` and the
/// caller can skip GPU OCR. When extraction fails (empty, PUA-heavy, encoding
/// issues), `needs_ocr` is `true` for OCR fallback.
pub fn extract_formulas_in_regions_mem(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
) -> Result<Vec<PageRegionResult>, PdfError> {
    let data = prepare_region_extraction(buffer, page_regions)?;

    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = data.items_by_page.get(&page_1idx);
        let (page_h, adaptive_threshold, coords) = page_region_context(&data, page_1idx);

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

            // Formula-specific quality checks:
            // - Skip is_garbage_text (formulas are legitimately symbol-heavy)
            // - Keep CID/encoding checks (broken font decode is still broken)
            // - Add PUA check (extensible delimiters that didn't decode)
            let needs_ocr = text.trim().is_empty()
                || is_cid_garbage(&text)
                || detect_encoding_issues(&text)
                || is_formula_garbage(&text);

            page_results.push(RegionText { text, needs_ocr });
        }

        results.push(PageRegionResult {
            page: *page_0idx,
            regions: page_results,
        });
    }

    Ok(results)
}

/// Extract formula text within bounding-box regions and reconstruct LaTeX.
///
/// For each formula bbox, this function:
/// 1. Gets positioned text items inside the bbox
/// 2. Analyzes font sizes and vertical positions to detect sub/superscripts
/// 3. Detects simple fractions from vertically stacked text
/// 4. Converts Unicode math symbols to LaTeX commands
/// 5. Reconstructs structured LaTeX from the positioned items
///
/// Each result includes a `confidence` score (0.0–1.0) indicating how reliable
/// the LaTeX reconstruction is. Low-confidence results should be sent to GPU OCR.
///
/// The `needs_ocr` flag is set only when extraction fails entirely (empty text,
/// encoding garbage), NOT based on confidence — the caller decides the threshold.
pub fn extract_formulas_in_regions_as_latex(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
) -> Result<Vec<formula_latex::PageFormulaResult>, PdfError> {
    let data = prepare_region_extraction(buffer, page_regions)?;

    let mut results = Vec::with_capacity(page_regions.len());

    for (page_0idx, regions) in page_regions {
        let page_1idx = page_0idx + 1;
        let items = data.items_by_page.get(&page_1idx);
        let (page_h, _adaptive_threshold, coords) = page_region_context(&data, page_1idx);

        let mut page_results = Vec::with_capacity(regions.len());

        for rect in regions {
            let [rx1, ry1, rx2, ry2] = *rect;

            let result = match items {
                Some(items) => {
                    // Convert the bbox from top-left origin to the item coordinate space
                    let bounds = region_bounds(rx1, ry1, rx2, ry2, page_h, coords);

                    // Filter items whose center falls inside the bbox
                    let matched = formula_latex::filter_items_in_bbox(
                        items,
                        bounds.x_min,
                        bounds.y_min,
                        bounds.x_max,
                        bounds.y_max,
                    );

                    if matched.is_empty() {
                        formula_latex::FormulaResult {
                            latex: String::new(),
                            raw_text: String::new(),
                            confidence: 0.0,
                            needs_ocr: true,
                            confidence_breakdown: Vec::new(),
                        }
                    } else {
                        let (latex, raw_text, confidence, confidence_breakdown) =
                            formula_latex::reconstruct_latex(&matched);

                        // Same formula-specific quality checks as extract_formulas_in_regions_mem
                        let needs_ocr = raw_text.trim().is_empty()
                            || is_cid_garbage(&raw_text)
                            || detect_encoding_issues(&raw_text)
                            || is_formula_garbage(&raw_text);

                        formula_latex::FormulaResult {
                            latex,
                            raw_text,
                            confidence,
                            needs_ocr,
                            confidence_breakdown,
                        }
                    }
                }
                None => formula_latex::FormulaResult {
                    latex: String::new(),
                    raw_text: String::new(),
                    confidence: 0.0,
                    needs_ocr: true,
                    confidence_breakdown: Vec::new(),
                },
            };

            page_results.push(result);
        }

        results.push(formula_latex::PageFormulaResult {
            page: *page_0idx,
            regions: page_results,
        });
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

// =========================================================================
// Internal: single-load document pipeline
// =========================================================================

/// Load a PDF from disk, returning the parsed document and page count.
///
/// `Document::load_metadata` for page count + `Document::load` for content
/// are combined here, but lopdf loads the full doc in `load()` so we extract
/// page count from it directly to avoid the metadata-only round-trip.
fn load_document_from_path<P: AsRef<Path>>(path: P) -> Result<(Document, u32), PdfError> {
    let buffer = std::fs::read(&path)?;
    load_document_from_mem(&buffer)
}

/// Load a PDF from a memory buffer.
fn load_document_from_mem(buffer: &[u8]) -> Result<(Document, u32), PdfError> {
    // Fix malformed struct element names before parsing. Some PDF generators
    // write bare names (/S Code) instead of proper PDF names (/S /Code), which
    // causes lopdf to silently drop the entire object.
    let fixed = structure_tree::fix_bare_struct_names(buffer);
    let buf = fixed.as_ref();

    let doc = match Document::load_mem(buf) {
        Ok(d) => d,
        Err(ref e) if is_encrypted_lopdf_error(e) => {
            Document::load_mem_with_options(buf, lopdf::LoadOptions::with_password(""))?
        }
        Err(e) => return Err(e.into()),
    };
    let page_count = doc.get_pages().len() as u32;
    Ok((doc, page_count))
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

/// Detect formula text that is unlikely to be usable despite passing generic checks.
///
/// Formula text (Greek letters, math operators, variables) is legitimately
/// symbol-heavy, so `is_garbage_text` would false-positive. This check instead
/// catches:
///
/// 1. **Private Use Area (PUA) characters** — TeX extensible delimiter glyphs
///    (large brackets from CMEX fonts) often map to PUA U+E000–F8FF when the
///    ToUnicode CMap is missing. >10% PUA means significant undecoded content.
///
/// 2. **Control characters** — C0 controls (U+0000–001F excluding whitespace)
///    indicate broken font encoding, not formula content. >30% is rejected.
fn is_formula_garbage(text: &str) -> bool {
    let mut total = 0usize;
    let mut pua = 0usize;
    let mut control = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        total += 1;
        if ('\u{E000}'..='\u{F8FF}').contains(&ch) {
            pua += 1;
        }
        let cp = ch as u32;
        if cp < 0x20 {
            control += 1;
        }
    }
    if total < 3 {
        return false;
    }
    // >10% PUA — significant undecoded extensible delimiters
    if pua * 10 > total {
        return true;
    }
    // >30% control chars — broken encoding
    if control * 10 > total * 3 {
        return true;
    }
    false
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

            let (rect_tables, _) = tables::detect_tables_from_rects(&band_items, &band_rects, page);
            if !rect_tables.is_empty() {
                found_table = true;
                break;
            }
            let line_tables = tables::detect_tables_from_lines(&band_items, &band_lines, page);
            if !line_tables.is_empty() {
                found_table = true;
                break;
            }
            // Heuristic fallback for borderless tables
            let heuristic_tables = tables::detect_tables(&band_items, base_size, false);
            if !heuristic_tables.is_empty() {
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
    fn test_is_formula_garbage_accepts_math_text() {
        // Greek letters, math operators, variables — typical formula text
        let formula = "Φ(ν) = ∫ ∞ dze −it r1 − iνr2 sinh z";
        assert!(
            !is_formula_garbage(formula),
            "Valid formula text should not be flagged as garbage"
        );

        // Dense operator text
        let operators = "α + β − γ × δ ÷ ε ≤ ζ ≥ η ≈ θ ≠ ι ± κ";
        assert!(
            !is_formula_garbage(operators),
            "Math operator text should not be flagged as garbage"
        );

        // Short formula (e.g. single equation variable)
        let short = "αβ";
        assert!(
            !is_formula_garbage(short),
            "Short formula text should not be flagged"
        );
    }

    #[test]
    fn test_is_formula_garbage_rejects_pua_heavy() {
        // Simulates extensible delimiters from CMEX fonts mapping to PUA
        let pua_heavy = "x \u{F8EB} \u{F8EC} \u{F8ED} \u{F8F6} \u{F8F7} \u{F8F8} y";
        assert!(
            is_formula_garbage(pua_heavy),
            "PUA-heavy text should be flagged as formula garbage"
        );
    }

    #[test]
    fn test_is_formula_garbage_rejects_control_chars() {
        // Control characters indicate broken encoding
        let control_heavy = "a\x01b\x02c\x03d\x04e\x05f\x06g\x07h\x08i";
        assert!(
            is_formula_garbage(control_heavy),
            "Control-char-heavy text should be flagged as formula garbage"
        );
    }

    #[test]
    fn test_is_formula_garbage_accepts_few_pua() {
        // A few PUA chars among many valid chars is fine (<10% threshold)
        let mostly_good = "Φ(ν) = ∫ dze r1 − iνr2 sinh z α β γ δ ε ζ η θ \u{F8EB}";
        assert!(
            !is_formula_garbage(mostly_good),
            "Mostly-good text with rare PUA should pass"
        );
    }
}
