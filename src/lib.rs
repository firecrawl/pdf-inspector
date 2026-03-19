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
use std::collections::HashSet;
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
        Err(ref e) if is_encrypted_lopdf_error(e) => Document::load_mem_with_password(buf, "")?,
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
    let struct_roles = structure_tree::StructTree::from_doc(&doc).and_then(|tree| {
        let page_ids = doc.get_pages();
        let roles = tree.mcid_to_roles(&page_ids);
        if roles.is_empty() {
            None
        } else {
            log::debug!(
                "structure tree: {} pages with MCID roles, {} total MCIDs",
                roles.len(),
                tree.mcid_count()
            );
            Some(roles)
        }
    });

    let (markdown, layout, has_encoding_issues, gid_pages) = match extracted {
        Some(((items, rects, lines), page_thresholds, gid_encoded_pages)) => {
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

    // Add pages with gid-encoded fonts (unresolvable encoding) to OCR list
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
        let cols = extractor::detect_columns(items, page);
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
}
