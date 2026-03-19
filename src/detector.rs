//! Smart PDF type detection without full document load
//!
//! This module detects whether a PDF is text-based, scanned, or image-based
//! by sampling content streams for text operators (Tj/TJ) without loading
//! all objects.

use crate::PdfError;
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// PDF type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfType {
    /// PDF has extractable text (Tj/TJ operators found)
    TextBased,
    /// PDF appears to be scanned (images only, no text operators)
    Scanned,
    /// PDF contains mostly images with minimal/no text
    ImageBased,
    /// PDF has mix of text and image-heavy pages
    Mixed,
}

/// Strategy for which pages to scan during detection
#[derive(Debug, Clone)]
pub enum ScanStrategy {
    /// Scan all pages, stop on first non-text page (current default).
    /// Best for pipelines that route TextBased PDFs to fast extraction.
    EarlyExit,
    /// Scan all pages, no early exit.
    /// Best when you need accurate Mixed vs Scanned classification.
    Full,
    /// Sample up to N evenly distributed pages (first, last, middle).
    /// Best for very large PDFs where speed matters more than precision.
    Sample(u32),
    /// Only scan these specific 1-indexed page numbers.
    /// Best when the caller knows which pages to check.
    Pages(Vec<u32>),
}

/// Result of PDF type detection
#[derive(Debug)]
pub struct PdfTypeResult {
    /// Detected PDF type
    pub pdf_type: PdfType,
    /// Number of pages in the document
    pub page_count: u32,
    /// Number of pages sampled for detection
    pub pages_sampled: u32,
    /// Number of pages with text operators found
    pub pages_with_text: u32,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f32,
    /// Title from metadata (if available)
    pub title: Option<String>,
    /// Whether OCR is recommended for better extraction
    /// True when images provide essential context (e.g., template-based PDFs)
    pub ocr_recommended: bool,
    /// 1-indexed page numbers that need OCR (image-only or insufficient text).
    /// Empty for TextBased. All pages for Scanned/ImageBased. Specific pages for Mixed.
    pub pages_needing_ocr: Vec<u32>,
}

/// Configuration for PDF type detection
#[derive(Debug, Clone)]
pub struct DetectionConfig {
    /// Strategy for which pages to scan
    pub strategy: ScanStrategy,
    /// Minimum text operator count per page to consider as text-based
    pub min_text_ops_per_page: u32,
    /// Threshold ratio of text pages to total pages for classification
    pub text_page_ratio_threshold: f32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            // EarlyExit is too aggressive for PDFs with an image-only cover
            // followed by text-heavy pages (e.g., annual reports).
            strategy: ScanStrategy::Sample(8),
            min_text_ops_per_page: 3,
            text_page_ratio_threshold: 0.6,
        }
    }
}

/// Detect PDF type from file path
pub fn detect_pdf_type<P: AsRef<Path>>(path: P) -> Result<PdfTypeResult, PdfError> {
    detect_pdf_type_with_config(path, DetectionConfig::default())
}

/// Detect PDF type from file path with custom configuration
pub fn detect_pdf_type_with_config<P: AsRef<Path>>(
    path: P,
    config: DetectionConfig,
) -> Result<PdfTypeResult, PdfError> {
    crate::validate_pdf_file(&path)?;

    // First, load metadata only (fast operation)
    let metadata = match Document::load_metadata(&path) {
        Ok(m) => m,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_metadata_with_password(&path, "")?
        }
        Err(e) => return Err(e.into()),
    };

    // Then load the full document for content inspection
    // We use filtered loading to skip heavy objects we don't need
    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_with_password(&path, "")?
        }
        Err(e) => return Err(e.into()),
    };

    detect_from_document(&doc, metadata.page_count, &config)
}

/// Detect PDF type from memory buffer
pub fn detect_pdf_type_mem(buffer: &[u8]) -> Result<PdfTypeResult, PdfError> {
    detect_pdf_type_mem_with_config(buffer, DetectionConfig::default())
}

/// Detect PDF type from memory buffer with custom configuration
pub fn detect_pdf_type_mem_with_config(
    buffer: &[u8],
    config: DetectionConfig,
) -> Result<PdfTypeResult, PdfError> {
    crate::validate_pdf_bytes(buffer)?;

    // Load metadata first (fast)
    let metadata = match Document::load_metadata_mem(buffer) {
        Ok(m) => m,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_metadata_mem_with_password(buffer, "")?
        }
        Err(e) => return Err(e.into()),
    };

    // Load document for inspection
    let doc = match Document::load_mem(buffer) {
        Ok(d) => d,
        Err(ref e) if crate::is_encrypted_lopdf_error(e) => {
            Document::load_mem_with_password(buffer, "")?
        }
        Err(e) => return Err(e.into()),
    };

    detect_from_document(&doc, metadata.page_count, &config)
}

/// Detection logic on a pre-loaded document.
///
/// `page_count` should come from `Document::load_metadata()`.
pub(crate) fn detect_from_document(
    doc: &Document,
    page_count: u32,
    config: &DetectionConfig,
) -> Result<PdfTypeResult, PdfError> {
    let pages = doc.get_pages();
    let total_pages = pages.len() as u32;

    // Select pages to scan based on strategy
    let (sample_indices, allow_early_exit) = match &config.strategy {
        ScanStrategy::EarlyExit => ((1..=total_pages).collect::<Vec<_>>(), true),
        ScanStrategy::Full => ((1..=total_pages).collect::<Vec<_>>(), false),
        ScanStrategy::Sample(max_pages) => {
            let n = (*max_pages).min(total_pages);
            (distribute_pages(n, total_pages), false)
        }
        ScanStrategy::Pages(pages) => {
            let mut valid: Vec<u32> = pages
                .iter()
                .copied()
                .filter(|&p| p >= 1 && p <= total_pages)
                .collect();
            valid.sort();
            valid.dedup();
            (valid, false)
        }
    };

    let mut pages_with_text = 0u32;
    let mut pages_with_images = 0u32;
    let mut pages_with_template_images = 0u32;
    let mut pages_with_vector_text = 0u32;
    let mut total_text_ops = 0u32;
    // Cache Phase 1 results to avoid re-analyzing sampled pages in Phase 2
    let mut analysis_cache: HashMap<u32, PageAnalysis> = HashMap::new();
    let mut pages_actually_sampled = 0u32;

    for page_num in &sample_indices {
        if let Some(&page_id) = pages.get(page_num) {
            let analysis = analyze_page_content(doc, page_id);
            pages_actually_sampled += 1;
            log::debug!(
                "page {}: text_ops={} images={} image_count={} template={} unique_chars={} alphanum={} path_ops={} vector_text={} image_area={} identity_h_no_tounicode={}",
                page_num, analysis.text_operator_count, analysis.has_images,
                analysis.image_count, analysis.has_template_image,
                analysis.unique_text_chars, analysis.unique_alphanum_chars,
                analysis.path_op_count, analysis.has_vector_text,
                analysis.total_image_area, analysis.has_identity_h_no_tounicode
            );
            let is_image_dominated = analysis.image_count > 10
                && analysis.image_count > analysis.text_operator_count * 3;
            let effective_min_ops = if analysis.has_images || analysis.image_count > 0 {
                config.min_text_ops_per_page.max(10)
            } else {
                config.min_text_ops_per_page
            };
            if analysis.text_operator_count >= effective_min_ops
                && !is_image_dominated
                && analysis.unique_text_chars >= 5
                && !analysis.has_vector_text
            {
                pages_with_text += 1;
            }
            if analysis.has_images {
                pages_with_images += 1;
            }
            if analysis.has_template_image {
                pages_with_template_images += 1;
            }
            if analysis.has_vector_text {
                pages_with_vector_text += 1;
            }
            total_text_ops += analysis.text_operator_count;
            analysis_cache.insert(*page_num, analysis.clone());

            // Early exit: if this page is non-text (insufficient meaningful text
            // but has images), this PDF won't be purely TextBased.
            if allow_early_exit
                && (analysis.text_operator_count < config.min_text_ops_per_page
                    || is_image_dominated
                    || analysis.unique_text_chars < 5)
                && (analysis.has_images || analysis.has_template_image)
            {
                break;
            }
        }
    }

    let pages_sampled = pages_actually_sampled;
    let text_ratio = if pages_sampled > 0 {
        pages_with_text as f32 / pages_sampled as f32
    } else {
        0.0
    };

    // Check if this is a template-based PDF (images provide essential context)
    // Template PDFs have text AND large background images on most pages
    let has_template_images = pages_with_template_images > 0;
    let template_ratio = if pages_sampled > 0 {
        pages_with_template_images as f32 / pages_sampled as f32
    } else {
        0.0
    };

    // OCR is recommended when:
    // 1. Template images are present (text alone is insufficient), OR
    // 2. PDF is scanned/image-based
    let ocr_recommended: bool;

    // Classification logic
    let (pdf_type, confidence) = if has_template_images && pages_with_text > 0 {
        ocr_recommended = true;
        // Template-based PDF: has text but images provide essential context
        (PdfType::Mixed, 0.5 + (0.3 * (1.0 - template_ratio)))
    } else if text_ratio >= config.text_page_ratio_threshold {
        ocr_recommended = false;
        (PdfType::TextBased, text_ratio)
    } else if pages_with_text == 0 && (pages_with_images > 0 || pages_with_vector_text > 0) {
        // No extractable text but has images or vector-outlined text
        ocr_recommended = true;
        if total_text_ops == 0 && pages_with_vector_text == 0 {
            (PdfType::Scanned, 0.95)
        } else {
            (PdfType::ImageBased, 0.8)
        }
    } else if pages_with_text > 0 && (pages_with_images > 0 || pages_with_vector_text > 0) {
        ocr_recommended = true;
        (PdfType::Mixed, 0.7)
    } else if total_text_ops == 0 {
        ocr_recommended = true;
        (PdfType::Scanned, 0.9)
    } else {
        ocr_recommended = false;
        (PdfType::TextBased, text_ratio.max(0.5))
    };

    // Phase 2: Build per-page OCR list
    let mut pages_needing_ocr = match pdf_type {
        PdfType::TextBased => Vec::new(),
        PdfType::Scanned | PdfType::ImageBased => (1..=total_pages).collect(),
        PdfType::Mixed => {
            let mut ocr_pages = Vec::new();
            for page_num in 1..=total_pages {
                let analysis = if let Some(cached) = analysis_cache.get(&page_num) {
                    cached.clone()
                } else if let Some(&page_id) = pages.get(&page_num) {
                    analyze_page_content(doc, page_id)
                } else {
                    continue;
                };
                if analysis.has_template_image
                    || analysis.has_vector_text
                    || (analysis.text_operator_count < config.min_text_ops_per_page
                        && analysis.has_images)
                {
                    ocr_pages.push(page_num);
                }
            }
            ocr_pages.sort();
            ocr_pages.dedup();
            ocr_pages
        }
    };

    // Phase 3: Flag pages with Identity-H/V fonts lacking ToUnicode for OCR.
    // These fonts produce garbage text (raw CID values) for non-Latin scripts.
    for (&page_num, analysis) in &analysis_cache {
        if analysis.has_identity_h_no_tounicode && !pages_needing_ocr.contains(&page_num) {
            pages_needing_ocr.push(page_num);
        }
    }
    // Check uncached pages too (when not all pages were sampled)
    if pages_needing_ocr.len() < total_pages as usize {
        for page_num in 1..=total_pages {
            if analysis_cache.contains_key(&page_num) || pages_needing_ocr.contains(&page_num) {
                continue;
            }
            if let Some(&page_id) = pages.get(&page_num) {
                if page_has_identity_h_no_tounicode(doc, page_id) {
                    pages_needing_ocr.push(page_num);
                }
            }
        }
    }
    pages_needing_ocr.sort();
    pages_needing_ocr.dedup();

    // Try to get title from metadata
    let title = get_document_title(doc);

    Ok(PdfTypeResult {
        pdf_type,
        page_count,
        pages_sampled,
        pages_with_text,
        confidence,
        title,
        ocr_recommended,
        pages_needing_ocr,
    })
}

/// Distribute `n` page indices evenly across `total` pages (1-indexed).
///
/// Always includes the first and last page, with remaining pages
/// spaced evenly in between.
fn distribute_pages(n: u32, total: u32) -> Vec<u32> {
    if n == 0 {
        return Vec::new();
    }
    if n >= total {
        return (1..=total).collect();
    }

    let mut indices = Vec::with_capacity(n as usize);
    indices.push(1);

    if n > 1 {
        indices.push(total);
    }

    let remaining = n.saturating_sub(2);
    if remaining > 0 && total > 2 {
        let step = (total - 2) / (remaining + 1);
        for i in 1..=remaining {
            let idx = 1 + (step * i);
            if idx > 1 && idx < total && !indices.contains(&idx) {
                indices.push(idx);
            }
        }
    }

    indices.sort();
    indices.dedup();
    indices
}

/// Page content analysis result
#[derive(Clone)]
struct PageAnalysis {
    text_operator_count: u32,
    has_images: bool,
    /// Whether page has a large background/template image (>50% coverage)
    has_template_image: bool,
    /// Total image area in pixels (reserved for future use)
    #[allow(dead_code)]
    total_image_area: u64,
    /// Number of Do (XObject invocation) operators in content streams
    image_count: u32,
    /// Number of unique non-whitespace text characters found in string operands
    unique_text_chars: u32,
    /// Number of unique ASCII alphanumeric bytes (letters + digits) in string operands
    unique_alphanum_chars: u32,
    /// Number of path construction/painting ops (m, l, c, h, f, re, etc.)
    #[allow(dead_code)]
    path_op_count: u32,
    /// Whether the page has vector-outlined text (massive path ops, minimal text ops)
    has_vector_text: bool,
    /// Whether the page has Type0 fonts with Identity-H/V encoding but no ToUnicode CMap.
    /// These fonts produce garbage text because CID values can't be mapped to Unicode.
    has_identity_h_no_tounicode: bool,
}

/// Analyze a page's content stream for text operators and images
fn analyze_page_content(doc: &Document, page_id: ObjectId) -> PageAnalysis {
    let mut text_ops = 0u32;
    let mut has_images = false;
    let mut image_count = 0u32;
    let mut path_ops = 0u32;
    let mut all_unique_chars: HashSet<u8> = HashSet::new();

    // Get content streams for this page
    let content_streams = doc.get_page_contents(page_id);

    for content_id in content_streams {
        if let Ok(Object::Stream(stream)) = doc.get_object(content_id) {
            // Try to decompress and scan content
            let content = match stream.decompressed_content() {
                Ok(data) => data,
                Err(_) => stream.content.clone(),
            };

            // Scan for text operators (Tj, TJ), image operators (Do), and path ops
            let (ops, imgs, paths) =
                scan_content_for_text_operators(&content, &mut all_unique_chars);
            text_ops += ops;
            image_count += imgs;
            path_ops += paths;
            has_images = has_images || imgs > 0;
        }
    }

    // Scan XObject Form contents for text operators
    if let Ok((resource_dict, resource_ids)) = doc.get_page_resources(page_id) {
        let mut visited = HashSet::new();
        if let Some(resources) = resource_dict {
            let (ops, imgs, paths) =
                scan_xobjects_in_resources(doc, resources, &mut visited, &mut all_unique_chars);
            text_ops += ops;
            image_count += imgs;
            path_ops += paths;
            has_images = has_images || imgs > 0;
        }
        for resource_id in resource_ids {
            if let Ok(resources) = doc.get_dictionary(resource_id) {
                let (ops, imgs, paths) =
                    scan_xobjects_in_resources(doc, resources, &mut visited, &mut all_unique_chars);
                text_ops += ops;
                image_count += imgs;
                path_ops += paths;
                has_images = has_images || imgs > 0;
            }
        }
    }

    // Check for XObject images and calculate coverage
    let (found_images, total_image_area, has_template_image) = analyze_page_images(doc, page_id);

    if found_images {
        has_images = true;
    }

    // Vector-outlined text: massive path ops with minimal text ops.
    // Each outlined glyph needs ~10-30 path commands, so a page of
    // outlined text produces thousands of path ops.
    let has_vector_text = path_ops >= 1000 && path_ops > text_ops.saturating_mul(200);

    let unique_alphanum_chars = all_unique_chars
        .iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .count() as u32;

    // Check for Identity-H/V fonts without ToUnicode — these produce garbage text
    let has_identity_h_no_tounicode =
        text_ops > 0 && page_has_identity_h_no_tounicode(doc, page_id);

    PageAnalysis {
        text_operator_count: text_ops,
        has_images,
        has_template_image,
        total_image_area,
        image_count,
        unique_text_chars: all_unique_chars.len() as u32,
        unique_alphanum_chars,
        path_op_count: path_ops,
        has_vector_text,
        has_identity_h_no_tounicode,
    }
}

/// Check if a page has Type0 fonts with Identity-H/V encoding and no ToUnicode CMap.
/// These fonts encode text as raw CID values that can't be mapped to Unicode without
/// a ToUnicode CMap, producing garbage output for non-Latin scripts (e.g. Cyrillic).
fn page_has_identity_h_no_tounicode(doc: &Document, page_id: ObjectId) -> bool {
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(_) => return false,
    };
    for font_dict in fonts.values() {
        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok());
        if subtype != Some(b"Type0") {
            continue;
        }
        let encoding = font_dict
            .get(b"Encoding")
            .ok()
            .and_then(|o| o.as_name().ok());
        let is_identity = matches!(encoding, Some(b"Identity-H") | Some(b"Identity-V"));
        if !is_identity {
            continue;
        }
        // Has ToUnicode? Then the font is decodable.
        if font_dict.get(b"ToUnicode").is_ok() {
            continue;
        }
        // Identity-H/V without ToUnicode — flag it
        log::debug!(
            "page has Identity-H/V font without ToUnicode: {:?}",
            font_dict
                .get(b"BaseFont")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).to_string())
        );
        return true;
    }
    false
}

fn scan_xobjects_in_resources(
    doc: &Document,
    resources: &lopdf::Dictionary,
    visited: &mut HashSet<ObjectId>,
    unique_chars: &mut HashSet<u8>,
) -> (u32, u32, u32) {
    let mut text_ops = 0u32;
    let mut image_count = 0u32;
    let mut path_ops = 0u32;

    let xobjects = match resources.get(b"XObject").ok() {
        Some(Object::Dictionary(d)) => Some(d.clone()),
        Some(Object::Reference(r)) => doc.get_dictionary(*r).ok().cloned(),
        _ => None,
    };

    if let Some(xobj_dict) = xobjects {
        for (_, obj) in xobj_dict.iter() {
            let Some(obj_id) = obj.as_reference().ok() else {
                continue;
            };
            if !visited.insert(obj_id) {
                continue;
            }
            let Ok(Object::Stream(stream)) = doc.get_object(obj_id) else {
                continue;
            };
            let subtype = stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok());
            match subtype {
                Some(b"Form") => {
                    let content = stream
                        .decompressed_content()
                        .unwrap_or_else(|_| stream.content.clone());
                    let (ops, imgs, paths) =
                        scan_content_for_text_operators(&content, unique_chars);
                    text_ops += ops;
                    image_count += imgs;
                    path_ops += paths;
                    if let Some(res) = stream
                        .dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|o| o.as_dict().ok())
                    {
                        let (ops2, imgs2, paths2) =
                            scan_xobjects_in_resources(doc, res, visited, unique_chars);
                        text_ops += ops2;
                        image_count += imgs2;
                        path_ops += paths2;
                    }
                }
                Some(b"Image") => {
                    image_count += 1;
                }
                _ => {}
            }
        }
    }

    (text_ops, image_count, path_ops)
}

/// Fast scan of content stream bytes for text operators
///
/// This is a fast heuristic scan that looks for:
/// - "Tj" - show text string
/// - "TJ" - show text with individual glyph positioning
/// - "'" - move to next line and show text
/// - "\"" - set word/char spacing, move to next line, show text
///
/// Returns (text_op_count, image_count, path_op_count).
/// Unique non-whitespace text characters are collected into `unique_chars`.
fn scan_content_for_text_operators(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
) -> (u32, u32, u32) {
    let mut text_ops = 0u32;
    let mut image_count = 0u32;
    let mut path_ops = 0u32;

    // Helper: check if position is a word boundary (start of content or preceded by whitespace)
    let is_word_start = |pos: usize| -> bool { pos == 0 || content[pos - 1].is_ascii_whitespace() };
    // Helper: check if position is at end or followed by whitespace
    let is_word_end =
        |pos: usize| -> bool { pos + 1 >= content.len() || content[pos + 1].is_ascii_whitespace() };

    // Simple state machine to find operators
    let mut i = 0;
    while i < content.len() {
        let b = content[i];

        // Look for 'T' followed by 'j' or 'J'
        if b == b'T' && i + 1 < content.len() {
            let next = content[i + 1];
            if next == b'j' || next == b'J' {
                // Verify it's an operator (followed by whitespace or newline)
                if i + 2 >= content.len()
                    || content[i + 2].is_ascii_whitespace()
                    || content[i + 2] == b'\n'
                    || content[i + 2] == b'\r'
                {
                    text_ops += 1;
                    // Scan backward for text string operand to collect unique chars
                    collect_text_chars_before(content, i, unique_chars);
                }
            }
        }

        // Look for 'Do' operator (XObject/image placement)
        if b == b'D'
            && i + 1 < content.len()
            && content[i + 1] == b'o'
            && (i + 2 >= content.len() || content[i + 2].is_ascii_whitespace())
        {
            image_count += 1;
        }

        // Count path construction/painting operators.
        // Single-byte: m (moveto), l (lineto), c (curveto), h (closepath),
        //              f (fill), S (stroke), s (close+stroke), B (fill+stroke),
        //              F (fill, variant)
        // These are the high-volume operators in vector-outlined text.
        match b {
            b'm' | b'l' | b'c' | b'h' | b'f' | b'S' | b's' | b'B' | b'F'
                if is_word_start(i) && is_word_end(i) =>
            {
                path_ops += 1;
            }
            // Two-byte: re (rect), f* (fill even-odd)
            b'r' if i + 1 < content.len()
                && content[i + 1] == b'e'
                && is_word_start(i)
                && (i + 2 >= content.len() || content[i + 2].is_ascii_whitespace()) =>
            {
                path_ops += 1;
            }
            b'f' if i + 1 < content.len()
                && content[i + 1] == b'*'
                && is_word_start(i)
                && (i + 2 >= content.len() || content[i + 2].is_ascii_whitespace()) =>
            {
                path_ops += 1;
            }
            _ => {}
        }

        i += 1;
    }

    (text_ops, image_count, path_ops)
}

/// Scan backward from a Tj/TJ operator to find the preceding string operand
/// and collect unique non-whitespace bytes from it.
///
/// Handles both literal strings `(...)` and hex strings `<...>`.
fn collect_text_chars_before(content: &[u8], op_pos: usize, unique_chars: &mut HashSet<u8>) {
    // Walk backward past whitespace to find the closing delimiter
    let mut j = op_pos;
    while j > 0 {
        j -= 1;
        if !content[j].is_ascii_whitespace() {
            break;
        }
    }
    if j == 0 {
        return;
    }

    let closing = content[j];

    if closing == b')' {
        // Literal string: scan backward for matching '('
        let mut depth = 1i32;
        let mut k = j;
        while k > 0 && depth > 0 {
            k -= 1;
            match content[k] {
                b')' if k == 0 || content[k - 1] != b'\\' => depth += 1,
                b'(' if k == 0 || content[k - 1] != b'\\' => depth -= 1,
                _ => {}
            }
        }
        // k now points at '('; collect bytes between (k+1..j)
        if depth == 0 && k + 1 < j {
            for &ch in &content[k + 1..j] {
                if !ch.is_ascii_whitespace() {
                    unique_chars.insert(ch);
                }
            }
        }
    } else if closing == b'>' {
        // Hex string: scan backward for '<'
        let mut k = j;
        while k > 0 {
            k -= 1;
            if content[k] == b'<' {
                break;
            }
        }
        if content[k] == b'<' && k + 1 < j {
            // Decode hex pairs and collect unique non-whitespace bytes
            let hex_slice = &content[k + 1..j];
            let hex_clean: Vec<u8> = hex_slice
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            for pair in hex_clean.chunks(2) {
                if pair.len() == 2 {
                    let high = hex_val(pair[0]);
                    let low = hex_val(pair[1]);
                    if let (Some(h), Some(l)) = (high, low) {
                        let byte = (h << 4) | l;
                        if byte != 0 && byte != b' ' && byte != b'\t' && byte != b'\n' {
                            unique_chars.insert(byte);
                        }
                    }
                }
            }
        }
    } else if closing == b']' {
        // TJ array: scan backward for '[' and collect from all strings inside
        let mut k = j;
        while k > 0 {
            k -= 1;
            if content[k] == b'[' {
                break;
            }
        }
        if content[k] == b'[' {
            // Scan forward through the array collecting string contents
            let mut m = k + 1;
            while m < j {
                if content[m] == b'(' {
                    let start = m + 1;
                    let mut depth = 1i32;
                    m += 1;
                    while m < j && depth > 0 {
                        match content[m] {
                            b')' if content[m - 1] != b'\\' => depth -= 1,
                            b'(' if content[m - 1] != b'\\' => depth += 1,
                            _ => {}
                        }
                        if depth > 0 {
                            m += 1;
                        }
                    }
                    // collect bytes from start..m
                    for &ch in &content[start..m] {
                        if !ch.is_ascii_whitespace() {
                            unique_chars.insert(ch);
                        }
                    }
                } else if content[m] == b'<' {
                    let hex_start = m + 1;
                    m += 1;
                    while m < j && content[m] != b'>' {
                        m += 1;
                    }
                    let hex_slice = &content[hex_start..m];
                    let hex_clean: Vec<u8> = hex_slice
                        .iter()
                        .copied()
                        .filter(|b| !b.is_ascii_whitespace())
                        .collect();
                    for pair in hex_clean.chunks(2) {
                        if pair.len() == 2 {
                            let high = hex_val(pair[0]);
                            let low = hex_val(pair[1]);
                            if let (Some(h), Some(l)) = (high, low) {
                                let byte = (h << 4) | l;
                                if byte != 0 && byte != b' ' && byte != b'\t' && byte != b'\n' {
                                    unique_chars.insert(byte);
                                }
                            }
                        }
                    }
                }
                m += 1;
            }
        }
    }
}

/// Convert a hex ASCII character to its numeric value (0-15)
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Analyze page images: returns (has_images, total_area, has_template_image)
///
/// A template image is one that covers >50% of a standard page area.
/// Standard page: 612x792 points (US Letter) = ~485,000 sq points
/// At 2x resolution that's ~1.9M pixels, so we use 250K pixels as threshold
/// (accounting for varying DPI and page sizes)
fn analyze_page_images(doc: &Document, page_id: ObjectId) -> (bool, u64, bool) {
    // Threshold: image covering roughly half a page at 150+ DPI
    // 612 * 792 / 2 * (150/72)^2 ≈ 1M pixels, but we'll be conservative
    const TEMPLATE_IMAGE_THRESHOLD: u64 = 500_000; // 500K pixels

    let mut has_images = false;
    let mut total_area: u64 = 0;
    let mut has_template_image = false;
    let mut visited: HashSet<ObjectId> = HashSet::new();

    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        let resources = match page_dict.get(b"Resources") {
            Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok(),
            Ok(Object::Dictionary(dict)) => Some(dict),
            _ => None,
        };

        if let Some(resources) = resources {
            collect_images_from_resources(
                doc,
                resources,
                &mut has_images,
                &mut total_area,
                &mut has_template_image,
                TEMPLATE_IMAGE_THRESHOLD,
                &mut visited,
            );

            // Also check Pattern resources: tiling patterns can contain
            // XObject images (e.g., screenshots pasted into PDFs via
            // Chrome "Save as PDF").
            if let Ok(pattern_obj) = resources.get(b"Pattern") {
                let pattern_dict = match pattern_obj {
                    Object::Reference(id) => doc.get_dictionary(*id).ok(),
                    Object::Dictionary(dict) => Some(dict),
                    _ => None,
                };
                if let Some(pattern_dict) = pattern_dict {
                    for (_, value) in pattern_dict.iter() {
                        let pat_ref = match value.as_reference() {
                            Ok(r) => r,
                            _ => continue,
                        };
                        if !visited.insert(pat_ref) {
                            continue;
                        }
                        if let Ok(Object::Stream(stream)) = doc.get_object(pat_ref) {
                            if let Ok(pat_resources) = stream.dict.get(b"Resources") {
                                let pat_res_dict = match pat_resources {
                                    Object::Reference(id) => doc.get_dictionary(*id).ok(),
                                    Object::Dictionary(dict) => Some(dict),
                                    _ => None,
                                };
                                if let Some(pat_res) = pat_res_dict {
                                    collect_images_from_resources(
                                        doc,
                                        pat_res,
                                        &mut has_images,
                                        &mut total_area,
                                        &mut has_template_image,
                                        TEMPLATE_IMAGE_THRESHOLD,
                                        &mut visited,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Tiled scans: many small image tiles (e.g., JBIG2 strips) that together
    // cover the full page. No individual tile triggers the template threshold,
    // but the aggregate area clearly indicates a scanned/image-backed page.
    if !has_template_image && total_area >= TEMPLATE_IMAGE_THRESHOLD * 4 {
        has_template_image = true;
    }

    (has_images, total_area, has_template_image)
}

/// Recursively collect image dimensions from XObject resources,
/// including images nested inside Form XObjects.
fn collect_images_from_resources(
    doc: &Document,
    resources: &lopdf::Dictionary,
    has_images: &mut bool,
    total_area: &mut u64,
    has_template_image: &mut bool,
    threshold: u64,
    visited: &mut HashSet<ObjectId>,
) {
    let xobject = match resources.get(b"XObject") {
        Ok(obj) => obj,
        _ => return,
    };
    let xobject_dict = match xobject {
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    };
    let Some(xobject_dict) = xobject_dict else {
        return;
    };

    for (_, value) in xobject_dict.iter() {
        let xobj_ref = match value.as_reference() {
            Ok(r) => r,
            _ => continue,
        };
        if !visited.insert(xobj_ref) {
            continue;
        }
        let xobj = match doc.get_object(xobj_ref) {
            Ok(o) => o,
            _ => continue,
        };
        let stream = match xobj.as_stream() {
            Ok(s) => s,
            _ => continue,
        };
        let subtype = match stream.dict.get(b"Subtype") {
            Ok(s) => s,
            _ => continue,
        };
        let name = match subtype.as_name() {
            Ok(n) => n,
            _ => continue,
        };

        if name == b"Image" {
            *has_images = true;
            let width = stream
                .dict
                .get(b"Width")
                .ok()
                .and_then(|w| w.as_i64().ok())
                .unwrap_or(0) as u64;
            let height = stream
                .dict
                .get(b"Height")
                .ok()
                .and_then(|h| h.as_i64().ok())
                .unwrap_or(0) as u64;
            let area = width * height;
            *total_area += area;
            if area >= threshold {
                *has_template_image = true;
            }
        } else if name == b"Form" {
            // Recurse into Form XObject's own Resources
            if let Ok(form_resources) = stream.dict.get(b"Resources") {
                let form_res_dict = match form_resources {
                    Object::Reference(id) => doc.get_dictionary(*id).ok(),
                    Object::Dictionary(dict) => Some(dict),
                    _ => None,
                };
                if let Some(form_res) = form_res_dict {
                    collect_images_from_resources(
                        doc,
                        form_res,
                        has_images,
                        total_area,
                        has_template_image,
                        threshold,
                        visited,
                    );
                }
            }
        }
    }
}

/// Get document title from Info dictionary
fn get_document_title(doc: &Document) -> Option<String> {
    let info_ref = doc.trailer.get(b"Info").ok()?.as_reference().ok()?;
    let info = doc.get_dictionary(info_ref).ok()?;
    let title_obj = info.get(b"Title").ok()?;

    match title_obj {
        Object::String(bytes, _) => {
            // Handle UTF-16BE encoding (BOM: 0xFE 0xFF)
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let utf16: Vec<u16> = bytes[2..]
                    .chunks_exact(2)
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect();
                Some(String::from_utf16_lossy(&utf16))
            } else {
                Some(String::from_utf8_lossy(bytes).to_string())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_content_operators() {
        let mut uchars = HashSet::new();

        // Sample PDF content stream with text operators
        let content = b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET";
        let (ops, imgs, _) = scan_content_for_text_operators(content, &mut uchars);
        assert_eq!(ops, 1);
        assert_eq!(imgs, 0);
        // "Hello World" without space: H, e, l, o, W, r, d = 7 unique
        assert!(uchars.len() >= 7);

        // Content with TJ array
        uchars.clear();
        let content2 = b"BT /F1 12 Tf 100 700 Td [(H) 10 (ello)] TJ ET";
        let (ops2, _, _) = scan_content_for_text_operators(content2, &mut uchars);
        assert_eq!(ops2, 1);
        // H, e, l, o = 4 unique
        assert!(uchars.len() >= 4);

        // Content with Do (image)
        uchars.clear();
        let content3 = b"q 100 0 0 100 50 700 cm /Img1 Do Q";
        let (ops3, imgs3, _) = scan_content_for_text_operators(content3, &mut uchars);
        assert_eq!(ops3, 0);
        assert_eq!(imgs3, 1);
    }

    #[test]
    fn test_image_dominated_detection() {
        // Simulate a page with many Do operators and minimal text
        let mut content = Vec::new();
        // Add 50 Do operators (image-heavy)
        for i in 0..50 {
            content.extend_from_slice(format!("/Im{i} Do\n").as_bytes());
        }
        // Add a few text operators with only a bullet char
        content.extend_from_slice(b"BT (x) Tj ET\n");
        content.extend_from_slice(b"BT (x) Tj ET\n");
        content.extend_from_slice(b"BT (x) Tj ET\n");

        let mut uchars = HashSet::new();
        let (ops, imgs, _) = scan_content_for_text_operators(&content, &mut uchars);
        assert_eq!(ops, 3);
        assert_eq!(imgs, 50);
        // Only 'x' unique char
        assert_eq!(uchars.len(), 1);

        // This should be image-dominated: 50 > 10 && 50 > 3*3=9
        let is_image_dominated = imgs > 10 && imgs > ops * 3;
        assert!(is_image_dominated);
        // And fails unique char threshold
        assert!(uchars.len() < 5);
    }

    #[test]
    fn test_normal_text_not_image_dominated() {
        let content = b"BT /F1 12 Tf (The quick brown fox jumps over the lazy dog) Tj ET\n\
                         /Img1 Do\n/Img2 Do\n";
        let mut uchars = HashSet::new();
        let (ops, imgs, _) = scan_content_for_text_operators(content, &mut uchars);
        assert_eq!(ops, 1);
        assert_eq!(imgs, 2);
        // Many unique chars from the sentence
        assert!(uchars.len() >= 5);
        // Not image-dominated: 2 > 10 fails
        let is_image_dominated = imgs > 10 && imgs > ops * 3;
        assert!(!is_image_dominated);
    }

    #[test]
    fn test_path_heavy_detection() {
        // Simulate vector-outlined text: many path ops, few text ops
        let mut content = Vec::new();
        // Add a couple text ops
        content.extend_from_slice(b"BT (Header) Tj ET\n");
        // Add 2000 path ops (simulating outlined glyphs)
        for _ in 0..500 {
            content.extend_from_slice(b"100 200 m 150 250 l 200 200 c h\n");
        }
        content.extend_from_slice(b"f\n");

        let mut uchars = HashSet::new();
        let (text, imgs, paths) = scan_content_for_text_operators(&content, &mut uchars);
        assert_eq!(text, 1);
        assert_eq!(imgs, 0);
        // 500 * (m + l + c + h) + 1 f = 2001
        assert!(paths >= 2000, "expected >= 2000 path ops, got {paths}");

        // Should trigger vector text detection: paths >= 1000 && paths > text * 200
        let has_vector_text = paths >= 1000 && paths > text.saturating_mul(200);
        assert!(has_vector_text);
    }

    #[test]
    fn test_normal_paths_not_vector_text() {
        // Normal page: text with some decorative paths (charts, borders)
        let mut content = Vec::new();
        // 20 text ops
        for _ in 0..20 {
            content.extend_from_slice(b"BT (Some text content here) Tj ET\n");
        }
        // 50 path ops (a chart or border)
        for _ in 0..10 {
            content.extend_from_slice(b"100 200 m 150 250 l 200 200 c h f\n");
        }

        let mut uchars = HashSet::new();
        let (text, _, paths) = scan_content_for_text_operators(&content, &mut uchars);
        assert_eq!(text, 20);
        assert!(paths >= 40, "expected >= 40 path ops, got {paths}");

        // Should NOT trigger: paths < 1000
        let has_vector_text = paths >= 1000 && paths > text.saturating_mul(200);
        assert!(!has_vector_text);
    }

    #[test]
    fn test_epever_vector_text_detection() {
        // Integration test: EPEVER PDF should be Mixed with page 2 needing OCR
        let path = std::path::Path::new("./tests/fixtures/EPEVER-DataSheet-XTRA-N-G3-Series-3.pdf");
        let path = if path.exists() {
            path.to_path_buf()
        } else {
            let alt = std::path::PathBuf::from(
                "../pdf-evals/pdfs/EPEVER-DataSheet-XTRA-N-G3-Series-3.pdf",
            );
            if !alt.exists() {
                // PDF not available, skip test
                return;
            }
            alt
        };

        let config = DetectionConfig {
            strategy: ScanStrategy::Full,
            ..DetectionConfig::default()
        };
        let result = detect_pdf_type_with_config(&path, config).unwrap();
        assert_eq!(
            result.pdf_type,
            PdfType::Mixed,
            "EPEVER should be Mixed (page 2 has vector-outlined text)"
        );
        assert!(
            result.pages_needing_ocr.contains(&2),
            "Page 2 should need OCR, got: {:?}",
            result.pages_needing_ocr
        );
        assert!(result.ocr_recommended);
    }

    #[test]
    fn test_page_has_identity_h_no_tounicode_positive() {
        // Build a minimal PDF with a Type0 Identity-H font and no ToUnicode.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+ArialMT".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(font_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        assert!(page_has_identity_h_no_tounicode(&doc, page_id));
    }

    #[test]
    fn test_page_has_identity_h_with_tounicode_negative() {
        // Type0 Identity-H font WITH ToUnicode — should NOT flag.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let cmap_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            b"fake cmap".to_vec(),
        )));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+ArialMT".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
            "ToUnicode" => Object::Reference(cmap_id),
        });
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(font_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        assert!(!page_has_identity_h_no_tounicode(&doc, page_id));
    }
}
