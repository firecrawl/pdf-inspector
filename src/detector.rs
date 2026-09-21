//! Smart PDF type detection without full document load
//!
//! This module detects whether a PDF is text-based, scanned, or image-based
//! by sampling content streams for text operators (Tj/TJ) without loading
//! all objects.

use crate::extractor::{get_number, visible_page_box, PageBox};
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
    /// Per-page explanation for `pages_needing_ocr`: 1-indexed page → reason
    /// codes (`scanned`, `no_text`, `vector_text`, `invisible_text_layer`,
    /// `suspected_garbled_text`). Only contains pages that need OCR.
    pub ocr_reasons_by_page: std::collections::BTreeMap<u32, Vec<String>>,
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

    let (doc, page_count) = crate::load_document_from_path(&path)?;

    detect_from_document(&doc, page_count, &config)
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

    let (doc, page_count) = crate::load_document_from_mem(buffer)?;

    detect_from_document(&doc, page_count, &config)
}

/// Heuristic page-count fallback for malformed PDFs that cannot be parsed.
///
/// This scans raw bytes for page dictionaries (`/Type /Page`) while excluding
/// the page tree node (`/Type /Pages`). It is intended as a low-confidence hint
/// for diagnostics; parsed page-tree counts remain authoritative.
pub fn estimate_page_count_from_bytes(buffer: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut pos = 0usize;

    while let Some(rel_idx) = find_bytes(&buffer[pos..], b"/Type") {
        let mut value_pos = pos + rel_idx + b"/Type".len();
        value_pos = skip_pdf_whitespace(buffer, value_pos);

        if buffer.get(value_pos) == Some(&b'/') {
            let name_start = value_pos + 1;
            let name_end = name_start + b"Page".len();
            if name_end <= buffer.len()
                && &buffer[name_start..name_end] == b"Page"
                && buffer
                    .get(name_end)
                    .is_none_or(|b| is_pdf_name_delimiter(*b))
            {
                count += 1;
            }
        }

        pos += rel_idx + b"/Type".len();
    }

    count
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn skip_pdf_whitespace(buffer: &[u8], mut pos: usize) -> usize {
    while pos < buffer.len() && is_pdf_whitespace(buffer[pos]) {
        pos += 1;
    }
    pos
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_pdf_name_delimiter(byte: u8) -> bool {
    is_pdf_whitespace(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
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
                "page {}: text_ops={} executed_text_ops={} hidden_text_ops={} images={} image_count={} template={} covering_image={} unique_chars={} alphanum={} path_ops={} vector_text={} image_area={} identity_h_no_tounicode={} type3_only={} font_changes={} decodable_fonts={}",
                page_num, analysis.text_operator_count, analysis.executed_text_operator_count,
                analysis.invisible_text_operator_count,
                analysis.has_images, analysis.image_count, analysis.has_template_image,
                analysis.has_covering_image,
                analysis.unique_text_chars, analysis.unique_alphanum_chars,
                analysis.path_op_count, analysis.has_vector_text,
                analysis.total_image_area, analysis.has_identity_h_no_tounicode,
                analysis.has_only_type3_fonts, analysis.font_change_count,
                analysis.has_decodable_text_fonts
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
                && !analysis.has_only_type3_fonts
                && !analysis.has_invisible_text_layer
            {
                pages_with_text += 1;
            }
            if analysis.has_images {
                pages_with_images += 1;
            }
            // Only count as a template-image page if it looks like a scan
            // (single full-page image) rather than a text page with figures.
            // Scanned-with-OCR PDFs have 1 large image per page + OCR text overlay;
            // text PDFs with figures have multiple smaller images alongside real text.
            //
            // Exception: CID-encoded fonts with ToUnicode produce low
            // unique_alphanum_chars in raw bytes but are fully decodable.
            // When a page has decodable fonts and enough text ops, treat it
            // as having real text regardless of raw byte diversity.
            let alphanum_ok = analysis.unique_alphanum_chars < 10
                && !(analysis.has_decodable_text_fonts && analysis.text_operator_count >= 10);
            // A page whose text is all invisible under an image covering
            // the page is a scan however many operators the layer has:
            // the page shows its raster, and the layer only describes it.
            if (analysis.has_template_image
                && (analysis.image_count <= 1 && analysis.text_operator_count < 50 && alphanum_ok))
                || analysis.has_invisible_text_layer
            {
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
                    || analysis.unique_text_chars < 5
                    || analysis.has_invisible_text_layer)
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

    // Phase 1b: Newspaper-style layout detection.
    // Dense multi-column newspapers (WSJ, NYT) have extractable text but produce
    // poor output due to complex interleaved article layouts. Detect via consistently
    // high text density combined with moderate font switches and a low Tf/Tj ratio.
    //
    // The Tf/Tj ratio distinguishes newspapers from styled legal/business documents:
    // - Newspapers: ratio 0.02-0.06 (dense prose with occasional font switches)
    // - Rich-styled docs (DPA, contracts): ratio 0.25-0.35 (per-character styling)
    //
    // Thresholds calibrated against:
    // - WSJ 50-page newspaper: text_ops 1500-3800, font_changes 50-194, ratio 0.02-0.06
    // - DPA/contracts: text_ops 1300-2260, font_changes 327-630, ratio 0.25-0.32
    // - SEC filings: text_ops 1-1800, font_changes 1-65 (only 1-2 dense pages)
    // - Normal docs: text_ops < 700, font_changes < 55
    let ocr_recommended = if pdf_type == PdfType::TextBased && pages_sampled >= 3 {
        let mut newspaper_pages = 0u32;
        for analysis in analysis_cache.values() {
            let ratio = if analysis.text_operator_count > 0 {
                analysis.font_change_count as f32 / analysis.text_operator_count as f32
            } else {
                1.0
            };
            if analysis.text_operator_count >= 1500
                && analysis.font_change_count >= 50
                && ratio < 0.15
            {
                newspaper_pages += 1;
            }
        }
        let newspaper_ratio = newspaper_pages as f32 / pages_sampled as f32;
        if newspaper_ratio >= 0.5 {
            log::debug!(
                "newspaper layout detected: {}/{} pages with high text_ops + font_changes → OCR recommended",
                newspaper_pages, pages_sampled
            );
            true
        } else {
            ocr_recommended
        }
    } else {
        ocr_recommended
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
                    // Cache the fresh analysis so the reason-classification pass
                    // below sees the real signals (vector_text, etc.) instead of
                    // defaulting to "scanned".
                    let a = analyze_page_content(doc, page_id);
                    analysis_cache.insert(page_num, a.clone());
                    a
                } else {
                    continue;
                };
                // Template images only need OCR when it looks like a scan
                // (single full-page image) rather than figures alongside text.
                // CID-encoded fonts with ToUnicode produce low unique_alphanum_chars
                // in raw bytes but are fully decodable — don't treat as scan.
                let alphanum_low = analysis.unique_alphanum_chars < 10
                    && !(analysis.has_decodable_text_fonts && analysis.text_operator_count >= 10);
                let looks_like_scan =
                    analysis.image_count <= 1 && analysis.text_operator_count < 50 && alphanum_low;
                // A template-image page below the `pages_with_text` floor is
                // a scan with incidental chrome (masthead, stamp, date line)
                // even when that chrome is diverse, decodable text — keep
                // this in sync with `page_ocr_signals`.
                let sparse_text_over_scan = analysis.has_template_image
                    && analysis.text_operator_count < config.min_text_ops_per_page.max(10);
                if (analysis.has_template_image && looks_like_scan)
                    || analysis.has_vector_text
                    || analysis.has_invisible_text_layer
                    || sparse_text_over_scan
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

    // Phase 3: Flag pages with undecodable fonts for OCR.
    // - Identity-H/V without ToUnicode: raw CID values can't map to Unicode
    // - Type3-only without ToUnicode: glyph bitmaps can't map to Unicode
    for (&page_num, analysis) in &analysis_cache {
        if (analysis.has_identity_h_no_tounicode || analysis.has_only_type3_fonts)
            && !pages_needing_ocr.contains(&page_num)
        {
            pages_needing_ocr.push(page_num);
        }
    }
    // Check uncached pages too (when not all pages were sampled).
    // Use analyze_page_content to get usage-based font checks (P1 + P2 fix).
    if pages_needing_ocr.len() < total_pages as usize {
        for page_num in 1..=total_pages {
            if analysis_cache.contains_key(&page_num) || pages_needing_ocr.contains(&page_num) {
                continue;
            }
            if let Some(&page_id) = pages.get(&page_num) {
                let analysis = analyze_page_content(doc, page_id);
                if analysis.has_identity_h_no_tounicode || analysis.has_only_type3_fonts {
                    pages_needing_ocr.push(page_num);
                    // Cache so the reason pass reports suspected_garbled_text
                    // rather than defaulting to "scanned".
                    analysis_cache.insert(page_num, analysis);
                }
            }
        }
    }
    pages_needing_ocr.sort();
    pages_needing_ocr.dedup();

    // Explain each OCR-flagged page. Pages we analyzed get a signal-derived
    // reason. Pages flagged only by whole-document classification — the
    // pages a sample left out of a Scanned/ImageBased document — are still
    // read for the one signal the byte scan alone gives, so a text layer
    // nobody sees is named wherever it is; the font and path signals come
    // from the full analysis and stay with the sampled pages, so such a
    // page otherwise defaults to `scanned`.
    let mut ocr_reasons_by_page: std::collections::BTreeMap<u32, Vec<String>> =
        std::collections::BTreeMap::new();
    for &page_num in &pages_needing_ocr {
        let reasons = match analysis_cache.get(&page_num) {
            Some(analysis) => page_ocr_reasons(analysis),
            None => match pages.get(&page_num) {
                Some(&page_id) if page_shows_only_a_hidden_text_layer(doc, page_id) => {
                    vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER]
                }
                _ => vec![crate::OCR_REASON_SCANNED],
            },
        };
        ocr_reasons_by_page.insert(page_num, reasons.into_iter().map(String::from).collect());
    }

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
        ocr_reasons_by_page,
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
#[derive(Clone, Default)]
struct PageAnalysis {
    text_operator_count: u32,
    /// Text-showing operators the page executes: in its own content and in
    /// the Form XObjects that content invokes, each once — unlike
    /// `text_operator_count`, which also counts forms merely bound.
    executed_text_operator_count: u32,
    /// Those of `executed_text_operator_count` that left nothing to see:
    /// run under text render mode 3 (invisible), or under mode 7 (clip
    /// only) with nothing painted through the clip.
    invisible_text_operator_count: u32,
    has_images: bool,
    /// Whether page has a large background/template image (>50% coverage)
    has_template_image: bool,
    /// Whether the images the page's content draws — by `Do`, in its own
    /// content and in the forms it invokes, each draw clipped to the
    /// visible page box — cover at least half of the page area, whatever
    /// their pixel size. Images bound in resources but never drawn do not
    /// count.
    has_covering_image: bool,
    /// Whether every text-showing operator the page executes is invisible
    /// while images cover the page: a scan carrying a text layer nobody
    /// sees. What the layer says is not what the page shows, so the page
    /// is read from its raster.
    has_invisible_text_layer: bool,
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
    /// Whether the page uses only Type3 fonts (no normal text fonts).
    /// Type3 fonts render each glyph as a custom drawing/bitmap — without a
    /// ToUnicode CMap, the character codes can't be mapped to Unicode.
    has_only_type3_fonts: bool,
    /// Number of Tf (set font) operators — high count indicates many font switches
    font_change_count: u32,
    /// Whether the page has fonts that can produce decodable text (ToUnicode,
    /// standard encoding, Type1/TrueType with known encoding).
    /// CID-encoded text with ToUnicode produces low unique_alphanum_chars in raw
    /// bytes but is fully decodable — this flag prevents misclassifying it as a scan.
    has_decodable_text_fonts: bool,
}

/// Explain *why* a page needs OCR, from its content analysis. Priority: a
/// text layer nobody sees under a covering image (`invisible_text_layer`)
/// comes first — such a page is a scan whatever its fonts are — then
/// undecodable fonts (`suspected_garbled_text`) and vector-outlined text
/// (`vector_text`), which persist even when a text layer is present;
/// otherwise a page with no extractable text is `scanned` when an image
/// backs it or `no_text` when nothing does. `extract_pages_markdown_mem`
/// puts `invisible_text_layer` first as well; the reasons after it keep
/// each surface's own order (there, `scanned` can stand beside the font
/// and path reasons, which here pre-empt it).
fn page_ocr_reasons(a: &PageAnalysis) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if a.has_invisible_text_layer {
        reasons.push(crate::OCR_REASON_INVISIBLE_TEXT_LAYER);
    }
    if a.has_identity_h_no_tounicode || a.has_only_type3_fonts {
        reasons.push(crate::OCR_REASON_SUSPECTED_GARBLED_TEXT);
    }
    if a.has_vector_text {
        reasons.push(crate::OCR_REASON_VECTOR_TEXT);
    }
    if reasons.is_empty() {
        let has_extractable_text = a.text_operator_count > 0 && a.unique_text_chars > 0;
        if !has_extractable_text && !a.has_images && !a.has_template_image {
            reasons.push(crate::OCR_REASON_NO_TEXT);
        } else {
            // Image-backed with no usable text, or too little text to trust.
            reasons.push(crate::OCR_REASON_SCANNED);
        }
    }
    reasons
}

/// Extracted font information from a Resource dictionary entry.
/// Stores the properties needed for decodability/identity-h checks
/// without holding a reference to the document.
#[derive(Clone, Debug)]
struct FontInfo {
    subtype: Option<Vec<u8>>,
    encoding: Option<Vec<u8>>,
    has_tounicode: bool,
    /// The raw font dictionary as an owned lopdf Dictionary.
    /// Needed for fallback checks (DescendantFonts → W array, embedded cmap).
    dict: lopdf::Dictionary,
}

/// Collect font entries from a Resources/Font dictionary into the font map.
/// Each entry maps font ObjectId → FontInfo. Using ObjectId as the key
/// avoids name collisions: different resource dictionaries can legally define
/// `/F1` pointing to different font objects, and ObjectId uniquely identifies
/// the underlying font regardless of the name used to reference it.
///
/// Inline font dictionaries (rare — fonts are almost always indirect refs)
/// are skipped because they have no ObjectId.
fn collect_fonts_from_resource_dict(
    doc: &Document,
    resources: &lopdf::Dictionary,
    font_map: &mut HashMap<ObjectId, FontInfo>,
) {
    let font_obj = match resources.get(b"Font").ok() {
        Some(obj) => obj,
        None => return,
    };
    let font_dict = match font_obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        _ => None,
    };
    let Some(font_dict) = font_dict else {
        return;
    };
    for (_name, value) in font_dict.iter() {
        // Only indirect references have a stable ObjectId.
        // Inline font dicts are extremely rare and have no ObjectId — skip them.
        let font_obj_id = match value {
            Object::Reference(r) => *r,
            _ => continue,
        };
        if font_map.contains_key(&font_obj_id) {
            continue;
        }
        let resolved = doc.get_dictionary(font_obj_id).ok();
        if let Some(fd) = resolved {
            let subtype = fd
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| n.to_vec());
            let encoding = fd
                .get(b"Encoding")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| n.to_vec());
            let has_tounicode = fd.get(b"ToUnicode").is_ok();
            font_map.insert(
                font_obj_id,
                FontInfo {
                    subtype,
                    encoding,
                    has_tounicode,
                    dict: fd.clone(),
                },
            );
        }
    }
}

/// Resolve font names (collected from a content stream) to ObjectIds using the
/// given resource dictionary. This is how we scope font name resolution correctly:
/// each content stream (page-level or Form XObject) resolves `/FontName` against
/// its own Resources/Font dictionary, yielding the correct underlying font object.
fn resolve_font_names_to_ids(
    doc: &Document,
    resources: &lopdf::Dictionary,
    font_names: &HashSet<Vec<u8>>,
    used_font_ids: &mut HashSet<ObjectId>,
) {
    let font_obj = match resources.get(b"Font").ok() {
        Some(obj) => obj,
        None => return,
    };
    let font_dict = match font_obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        _ => None,
    };
    let Some(font_dict) = font_dict else {
        return;
    };
    for name in font_names {
        if let Ok(Object::Reference(r)) = font_dict.get(name) {
            used_font_ids.insert(*r);
        }
    }
}

/// Look up a single font name in a resource dictionary, returning its indirect
/// ObjectId if present.
fn lookup_font_id(
    doc: &Document,
    resources: &lopdf::Dictionary,
    font_name: &[u8],
) -> Option<ObjectId> {
    let font_obj = resources.get(b"Font").ok()?;
    let font_dict = match font_obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        _ => None,
    }?;
    if let Ok(Object::Reference(r)) = font_dict.get(font_name) {
        Some(*r)
    } else {
        None
    }
}

/// Resolve page-level font names with PDF resource inheritance shadowing.
///
/// PDF spec (ISO 32000-1, 7.7.3.4): a page inherits /Resources from its
/// parent /Pages nodes, but a definition in a more-specific scope shadows
/// the same name from an ancestor. lopdf's `get_page_resources` returns
/// ancestors in most-specific-first order (page → parent → grandparent),
/// so the first dictionary that defines a given font name wins.
fn resolve_with_shadowing(
    doc: &Document,
    own_resources: Option<&lopdf::Dictionary>,
    ancestor_resource_ids: &[ObjectId],
    names: &HashSet<Vec<u8>>,
    used_font_ids: &mut HashSet<ObjectId>,
) {
    'name: for name in names {
        // Check page's own inline /Resources first (most specific scope)
        if let Some(rd) = own_resources {
            if let Some(id) = lookup_font_id(doc, rd, name) {
                used_font_ids.insert(id);
                continue 'name;
            }
        }
        // Walk inherited resource dicts (most-specific to root); first hit wins
        for ancestor_id in ancestor_resource_ids {
            if let Ok(rd) = doc.get_dictionary(*ancestor_id) {
                if let Some(id) = lookup_font_id(doc, rd, name) {
                    used_font_ids.insert(id);
                    continue 'name;
                }
            }
        }
    }
}

/// Analyze a page's content stream for text operators and images
fn analyze_page_content(doc: &Document, page_id: ObjectId) -> PageAnalysis {
    let mut counts = ContentCounts::default();
    let mut all_unique_chars: HashSet<u8> = HashSet::new();
    // Collect font ObjectIds (not names) to avoid cross-scope name collisions.
    // Each content stream resolves its Tf font names against its own resource
    // dictionary, producing the correct underlying font ObjectId.
    let mut used_font_ids: HashSet<ObjectId> = HashSet::new();

    // Build font map keyed by ObjectId: collects FontInfo for all fonts from
    // page-level Resources + Form XObject Resources.
    let mut font_map: HashMap<ObjectId, FontInfo> = HashMap::new();

    // We need the page's resource dict to resolve font names from page content.
    // get_page_resources returns (Option<&Dictionary>, Vec<ObjectId>) for
    // inline and indirect resource dicts respectively.
    let page_resources = doc.get_page_resources(page_id).ok();

    // The page's content streams, read as one and followed through `Do`
    // (see `scan_page_content`), with the raw font names they use.
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    let resources = page_resources
        .as_ref()
        .map(|(own, ancestors)| resource_chain(doc, *own, ancestors))
        .unwrap_or_default();
    let mut page_font_names: HashSet<Vec<u8>> = HashSet::new();
    let (scan_state, page_counts) = scan_page_content(
        doc,
        page_id,
        page_box,
        &resources,
        &mut all_unique_chars,
        &mut page_font_names,
    );
    counts.add(page_counts);

    // Resolve font names against the page's resource dictionaries,
    // respecting PDF resource inheritance shadowing: the most-specific
    // scope (page's own /Resources) wins over inherited ancestors.
    if let Some((ref resource_dict, ref resource_ids)) = page_resources {
        resolve_with_shadowing(
            doc,
            *resource_dict,
            resource_ids,
            &page_font_names,
            &mut used_font_ids,
        );
    }

    // Scan XObject Form contents for text operators, collect their fonts,
    // and resolve font names per-XObject scope.
    if let Some((resource_dict, resource_ids)) = page_resources {
        let mut visited = HashSet::new();
        if let Some(resources) = resource_dict {
            collect_fonts_from_resource_dict(doc, resources, &mut font_map);
            counts.add(scan_xobjects_in_resources(
                doc,
                resources,
                &mut visited,
                &mut all_unique_chars,
                &mut used_font_ids,
                &mut font_map,
            ));
        }
        for resource_id in resource_ids {
            if let Ok(resources) = doc.get_dictionary(resource_id) {
                collect_fonts_from_resource_dict(doc, resources, &mut font_map);
                counts.add(scan_xobjects_in_resources(
                    doc,
                    resources,
                    &mut visited,
                    &mut all_unique_chars,
                    &mut used_font_ids,
                    &mut font_map,
                ));
            }
        }
    }
    let text_ops = counts.text_ops;
    let image_count = counts.image_count;
    let path_ops = counts.path_ops;
    let font_changes = counts.font_changes;

    // Check for XObject images and calculate coverage
    let (found_images, total_image_area, has_template_image) = analyze_page_images(doc, page_id);
    let has_images = image_count > 0 || found_images;

    // The images the page's content drew — in its own streams and in the
    // forms they invoke, each draw clipped to the page — cover the page
    // when their boxes on it add up to at least half of it, whatever their
    // pixel size. Images the resources merely bind, and forms never
    // invoked, are not content: `has_template_image` judges the pixels of
    // whatever is bound and has no say here.
    let has_covering_image = scan_state.covers_page();

    // A page whose every executed text-showing operator left nothing to
    // see while images cover it shows the raster alone; the text layer
    // describes the raster rather than being the page's content.
    let executed_text_ops = scan_state.executed_text_ops;
    let hidden_text_ops = scan_state.executed_hidden_text_ops;
    let has_invisible_text_layer = scan_state.shows_only_a_hidden_text_layer();

    let unique_alphanum_chars = all_unique_chars
        .iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .count() as u32;

    // Vector-outlined text: massive path ops with minimal text ops.
    // Each outlined glyph needs ~10-30 path commands, so a page of
    // outlined text produces thousands of path ops.
    //
    // Also require few unique alphanum chars: real outlined-text pages have
    // very few because each glyph is a path, not a Tj/TJ text op. Pages with
    // real selectable text plus decorative paths (column borders, dividers)
    // have many unique alphanum chars — these are NOT vector-outlined text.
    //
    // The byte count says nothing about text shown through a CID-keyed
    // font: its two-byte codes are glyph indices whose bytes are rarely
    // ASCII letters or digits, however much text they carry. A page the
    // byte count would flag is therefore judged on its decoded text when
    // any of it came through such a font's ToUnicode CMap (see
    // `DecodedTextCounts::is_page_text`); decoding walks the content
    // streams again, so it is not done for the other pages.
    let mut has_vector_text = path_ops >= 1000
        && path_ops > text_ops.saturating_mul(200)
        && (unique_alphanum_chars as usize) < VECTOR_TEXT_MIN_ALPHANUMERICS;
    if has_vector_text {
        let decoded = decoded_text_counts(doc, page_id);
        has_vector_text = !(decoded.through_cid_cmap && decoded.is_page_text(path_ops));
    }

    // Check for Identity-H/V fonts without ToUnicode — these produce garbage text.
    // Only consider fonts actually USED by Tf operators in content streams (P1 fix),
    // and include fonts from Form XObject Resources (P2 fix).
    let has_identity_h_no_tounicode =
        text_ops > 0 && used_fonts_have_identity_h_no_tounicode(&used_font_ids, &font_map, doc);

    // Check for Type3-only fonts — glyph bitmaps without Unicode mapping.
    // Uses the usage-based font set for accuracy.
    let has_only_type3_fonts = text_ops > 0 && used_fonts_are_only_type3(&used_font_ids, &font_map);

    // Check if the page has fonts that can decode text to Unicode.
    // CID-encoded fonts with ToUnicode produce low unique_alphanum_chars in raw
    // bytes but are fully decodable — we need this to avoid false scan detection.
    // Only considers fonts actually USED via Tf operators (P1 + P2 fix).
    let has_decodable_text_fonts =
        text_ops > 0 && used_fonts_have_decodable_text(&used_font_ids, &font_map, doc);

    PageAnalysis {
        text_operator_count: text_ops,
        executed_text_operator_count: executed_text_ops,
        invisible_text_operator_count: hidden_text_ops,
        has_images,
        has_template_image,
        has_covering_image,
        has_invisible_text_layer,
        total_image_area,
        image_count,
        unique_text_chars: all_unique_chars.len() as u32,
        unique_alphanum_chars,
        path_op_count: path_ops,
        has_vector_text,
        has_identity_h_no_tounicode,
        has_only_type3_fonts,
        font_change_count: font_changes,
        has_decodable_text_fonts,
    }
}

/// The share of the page area the images a page's content draws must
/// cover, their boxes clipped to the page, for the page to count as
/// covered.
const COVERING_IMAGE_MIN_PAGE_FRACTION: f64 = 0.5;

/// The dictionaries a page's names resolve in, most specific first: its
/// own `/Resources`, then those it inherits from its ancestors (see
/// `resolve_with_shadowing`).
fn resource_chain<'a>(
    doc: &'a Document,
    own: Option<&'a lopdf::Dictionary>,
    ancestors: &[ObjectId],
) -> Vec<&'a lopdf::Dictionary> {
    own.into_iter()
        .chain(
            ancestors
                .iter()
                .filter_map(|id| doc.get_dictionary(*id).ok()),
        )
        .collect()
}

/// The page's content streams read as one — the text render mode and the
/// matrix carry from each to the next — and followed through `Do`: an
/// image is measured on the visible page box, a form is run in place.
/// Returns the state at the end and the streams' own counts; the text
/// characters and font names met go to the sets given.
fn scan_page_content<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    page_box: PageBox,
    resources: &[&'a lopdf::Dictionary],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> (ContentScanState<'a>, ContentCounts) {
    let mut state = ContentScanState::new(doc, page_box, true);
    let mut counts = ContentCounts::default();
    for content_id in doc.get_page_contents(page_id) {
        if let Ok(Object::Stream(stream)) = doc.get_object(content_id) {
            let content = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            counts.add(scan_content_stream(
                &content,
                unique_chars,
                used_font_names,
                &mut state,
                resources,
            ));
        }
    }
    (state, counts)
}

/// Whether the page shows only a text layer nobody sees over images that
/// cover it — from the executed-content scan alone, without the font,
/// pixel and path analysis of `analyze_page_content`. For the pages a
/// sample left out, whose OCR reason is reported all the same.
fn page_shows_only_a_hidden_text_layer(doc: &Document, page_id: ObjectId) -> bool {
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    let page_resources = doc.get_page_resources(page_id).ok();
    let resources = page_resources
        .as_ref()
        .map(|(own, ancestors)| resource_chain(doc, *own, ancestors))
        .unwrap_or_default();
    let (state, _) = scan_page_content(
        doc,
        page_id,
        page_box,
        &resources,
        &mut HashSet::new(),
        &mut HashSet::new(),
    );
    state.shows_only_a_hidden_text_layer()
}

/// Distinct alphanumeric characters a page must show for its text to count
/// as real text next to a mass of path operators — below it, the paths are
/// taken for outlined glyphs and the page for vector text. Applied to the
/// bytes of string operands, and to the decoded text of pages that show
/// text through a CID-keyed font with a ToUnicode CMap.
const VECTOR_TEXT_MIN_ALPHANUMERICS: usize = 30;

/// Path operators per decoded alphanumeric character beyond which a page's
/// text is a header or footer next to a drawing rather than the page's
/// content: outlined body text with a live title and address line runs into
/// the hundreds, a title over an illustration or a paragraph beside a chart
/// stays well under.
const VECTOR_TEXT_MAX_PATH_OPS_PER_CHARACTER: u32 = 100;

/// Form XObjects read at most when counting a page's decoded text; a page
/// invoking more is counted as far as that. The page and its forms
/// together are also held to the extractor's page budgets of decompressed
/// bytes and of operations.
const DECODED_TEXT_MAX_FORMS: usize = 1_000;

/// What a page's text amounts to once every string is decoded.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DecodedTextCounts {
    /// Distinct alphanumeric characters.
    distinct_alphanumerics: usize,
    /// Alphanumeric characters, repeats included.
    alphanumerics: usize,
    /// Whether any string was decoded through the ToUnicode CMap of a
    /// CID-keyed (Type0) font — the strings whose bytes the byte count
    /// cannot read.
    through_cid_cmap: bool,
}

impl DecodedTextCounts {
    /// Whether text this diverse and this plentiful is the content of a
    /// page carrying `path_ops` path operators, rather than the caption,
    /// title or footer of a drawing: as many distinct letters and digits as
    /// the byte count asks of a simple font, and at most
    /// [`VECTOR_TEXT_MAX_PATH_OPS_PER_CHARACTER`] path operators per
    /// character.
    fn is_page_text(self, path_ops: u32) -> bool {
        self.distinct_alphanumerics >= VECTOR_TEXT_MIN_ALPHANUMERICS
            && (self.alphanumerics as u64) * u64::from(VECTOR_TEXT_MAX_PATH_OPS_PER_CHARACTER)
                >= u64::from(path_ops)
    }
}

/// How a font's string operands are read for the decoded text counts.
enum FontDecoder {
    /// Codes mapped through the font's ToUnicode CMap; `cid` when the font
    /// is CID-keyed.
    CMap {
        cmap: crate::tounicode::ToUnicodeCMap,
        cid: bool,
    },
    /// A simple font without a usable CMap: one character per byte.
    Bytes,
}

/// The decoder for the font dictionary `font`: its ToUnicode CMap when it
/// has one that parses, direct or referenced; the bytes themselves for a
/// simple font without one; `None` for a font whose text cannot be decoded
/// here (a CID-keyed font without a CMap, a Type3 font), whose strings
/// count for nothing.
fn font_decoder(doc: &Document, font: &lopdf::Dictionary) -> Option<FontDecoder> {
    let subtype = font.get(b"Subtype").ok().and_then(|s| s.as_name().ok());
    let to_unicode = match font.get(b"ToUnicode") {
        Ok(Object::Stream(stream)) => Some(stream),
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Stream(stream)) => Some(stream),
            _ => None,
        },
        _ => None,
    };
    if let Some(stream) = to_unicode {
        // Decompressed within the loader's per-stream bound, which the
        // largest real CMaps come nowhere near: a stream inflating past it
        // is not read, and the font goes undecoded rather than costing
        // more memory than the document's own streams may.
        let data = match stream
            .decompressed_content_with_limit(crate::MAX_STREAM_DECOMPRESSED_BYTES)
        {
            Ok(data) => data,
            Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
                ..
            })) => return None,
            Err(_) if stream.content.len() > crate::MAX_STREAM_DECOMPRESSED_BYTES => return None,
            Err(_) => stream.content.clone(),
        };
        if let Some(cmap) = crate::tounicode::ToUnicodeCMap::parse(&data) {
            return Some(FontDecoder::CMap {
                cmap,
                cid: subtype == Some(b"Type0"),
            });
        }
    }
    match subtype {
        Some(b"Type1") | Some(b"TrueType") | Some(b"MMType1") => Some(FontDecoder::Bytes),
        _ => None,
    }
}

/// The font dictionary `name` resolves to in `resources`, with its object
/// id when it is an indirect object; an inline dictionary has none.
fn lookup_font<'a>(
    doc: &'a Document,
    resources: &'a lopdf::Dictionary,
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a lopdf::Dictionary)> {
    let fonts = match resources.get(b"Font").ok()? {
        Object::Dictionary(dict) => dict,
        Object::Reference(id) => doc.get_dictionary(*id).ok()?,
        _ => return None,
    };
    match fonts.get(name).ok()? {
        Object::Reference(id) => Some((Some(*id), doc.get_dictionary(*id).ok()?)),
        Object::Dictionary(dict) => Some((None, dict)),
        _ => None,
    }
}

/// The Form XObject `name` resolves to in `resources`, when it is one.
fn lookup_form(doc: &Document, resources: &lopdf::Dictionary, name: &[u8]) -> Option<ObjectId> {
    let xobjects = match resources.get(b"XObject").ok()? {
        Object::Dictionary(dict) => dict,
        Object::Reference(id) => doc.get_dictionary(*id).ok()?,
        _ => return None,
    };
    let id = xobjects.get(name).ok()?.as_reference().ok()?;
    let Ok(Object::Stream(stream)) = doc.get_object(id) else {
        return None;
    };
    (stream
        .dict
        .get(b"Subtype")
        .ok()
        .and_then(|s| s.as_name().ok())
        == Some(b"Form"))
    .then_some(id)
}

/// The alphanumeric characters in the text the page shows — in its own
/// content and in the Form XObjects its content invokes with `Do`, each
/// once — once every string is decoded through the font in force: its
/// ToUnicode CMap where it has one, the bytes themselves for a simple font
/// without one, nothing for a font that cannot be decoded here. Font names
/// resolve against the stream's own resources, then those of the stream
/// that invoked it, up to the page's and its ancestors'; the font in force
/// follows `q`/`Q` like the rest of the graphics state, and a form starts
/// with the font its invoker had selected, as it does the rest of the text
/// state. A form is decompressed when its turn comes, so one form's
/// content is held at a time. Replacement characters, which a CMap that
/// does not cover its codes produces, are not alphanumeric and do not
/// count. The page and its forms together are read within the extractor's
/// page budgets of decompressed bytes and of operations, in the order
/// invoked — the page, then the forms it invokes, then theirs — so the
/// budgets go to the text drawn first; what lies beyond them is not
/// counted, as the extractor would not read it either. A stream that does
/// not parse is skipped, as the extractor skips it.
fn decoded_text_counts(doc: &Document, page_id: ObjectId) -> DecodedTextCounts {
    decoded_text_counts_within(
        doc,
        page_id,
        crate::extractor::content_decode::MAX_PAGE_CONTENT_BYTES,
        crate::extractor::content_decode::MAX_PAGE_OPERATIONS,
    )
}

/// [`decoded_text_counts`] within the budgets given: at most `max_bytes`
/// of decompressed content and `max_operations` operators, over the page
/// and the forms it invokes together.
fn decoded_text_counts_within(
    doc: &Document,
    page_id: ObjectId,
    max_bytes: usize,
    max_operations: usize,
) -> DecodedTextCounts {
    use std::rc::Rc;

    let mut page_scope: Vec<&lopdf::Dictionary> = Vec::new();
    if let Ok((own, ancestors)) = doc.get_page_resources(page_id) {
        page_scope.extend(own);
        page_scope.extend(
            ancestors
                .iter()
                .filter_map(|id| doc.get_dictionary(*id).ok()),
        );
    }
    // A page whose content alone is over the budget is not read here, as
    // the extractor does not read it.
    let Ok(page_content) = doc.get_page_content_with_limit(page_id, max_bytes) else {
        return DecodedTextCounts::default();
    };
    let mut bytes_left = max_bytes.saturating_sub(page_content.len());
    let mut operations_left = max_operations;

    enum Pending {
        Page(Vec<u8>),
        Form(ObjectId),
    }
    /// A stream waiting to be read: the page, or a form with the resource
    /// scope and the font in force where it was invoked.
    struct Invocation<'a> {
        source: Pending,
        scope: Vec<&'a lopdf::Dictionary>,
        font: Option<Rc<FontDecoder>>,
    }
    let mut pending = std::collections::VecDeque::from([Invocation {
        source: Pending::Page(page_content),
        scope: page_scope,
        font: None,
    }]);
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut counts = DecodedTextCounts::default();
    let mut seen: HashSet<char> = HashSet::new();
    let mut decoders: HashMap<ObjectId, Option<Rc<FontDecoder>>> = HashMap::new();
    while let Some(Invocation {
        source,
        scope: invoking_scope,
        font: inherited,
    }) = pending.pop_front()
    {
        let (content, scope) = match source {
            Pending::Page(content) => (content, invoking_scope),
            Pending::Form(id) => {
                let Ok(Object::Stream(stream)) = doc.get_object(id) else {
                    continue;
                };
                let mut scope: Vec<&lopdf::Dictionary> = match stream.dict.get(b"Resources") {
                    Ok(Object::Dictionary(dict)) => vec![dict],
                    Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok().into_iter().collect(),
                    _ => Vec::new(),
                };
                scope.extend(invoking_scope);
                // Decompressed within what is left of the page's byte
                // budget; a stream that fails to decode for another reason
                // is read raw, as the page content is, if that fits too.
                let content = match stream.decompressed_content_with_limit(bytes_left) {
                    Ok(content) => content,
                    Err(lopdf::Error::Decompress(
                        lopdf::DecompressError::MemoryLimitExceeded { .. },
                    )) => break,
                    Err(_) if stream.content.len() > bytes_left => break,
                    Err(_) => stream.content.clone(),
                };
                bytes_left = bytes_left.saturating_sub(content.len());
                (content, scope)
            }
        };
        let ops = match crate::extractor::content_decode::decode_content_bounded(
            &content,
            operations_left,
        ) {
            Ok(Some(ops)) => ops,
            // The operation budget is spent: nothing further is read.
            Ok(None) => break,
            // A stream that does not parse is skipped, as the extractor
            // skips it; the streams after it are still read.
            Err(_) => continue,
        };
        operations_left = operations_left.saturating_sub(ops.operations.len());
        let mut current: Option<Rc<FontDecoder>> = inherited;
        let mut saved: Vec<Option<Rc<FontDecoder>>> = Vec::new();
        for op in &ops.operations {
            match op.operator.as_str() {
                "q" => saved.push(current.clone()),
                "Q" => {
                    if let Some(restored) = saved.pop() {
                        current = restored;
                    }
                }
                "Do" => {
                    if visited.len() >= DECODED_TEXT_MAX_FORMS {
                        continue;
                    }
                    let form = op
                        .operands
                        .first()
                        .and_then(|name| name.as_name().ok())
                        .and_then(|name| {
                            scope
                                .iter()
                                .find_map(|resources| lookup_form(doc, resources, name))
                        });
                    if let Some(id) = form {
                        if visited.insert(id) {
                            pending.push_back(Invocation {
                                source: Pending::Form(id),
                                scope: scope.clone(),
                                font: current.clone(),
                            });
                        }
                    }
                }
                "Tf" => {
                    current = op
                        .operands
                        .first()
                        .and_then(|name| name.as_name().ok())
                        .and_then(|name| {
                            scope
                                .iter()
                                .find_map(|resources| lookup_font(doc, resources, name))
                        })
                        .and_then(|(id, font)| match id {
                            Some(id) => decoders
                                .entry(id)
                                .or_insert_with(|| font_decoder(doc, font).map(Rc::new))
                                .clone(),
                            None => font_decoder(doc, font).map(Rc::new),
                        });
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    let Some(decoder) = current.as_deref() else {
                        continue;
                    };
                    let mut shown: Vec<&[u8]> = Vec::new();
                    if op.operator == "TJ" {
                        if let Some(Ok(array)) = op.operands.first().map(|array| array.as_array()) {
                            shown.extend(array.iter().filter_map(|element| match element {
                                Object::String(bytes, _) => Some(bytes.as_slice()),
                                _ => None,
                            }));
                        }
                    } else if let Some(Object::String(bytes, _)) = op.operands.last() {
                        shown.push(bytes);
                    }
                    for bytes in shown {
                        let decoded: Vec<char> = match decoder {
                            FontDecoder::CMap { cmap, cid } => {
                                counts.through_cid_cmap |= *cid && !bytes.is_empty();
                                cmap.decode_cids(bytes)
                                    .chars()
                                    .filter(|c| c.is_alphanumeric())
                                    .collect()
                            }
                            FontDecoder::Bytes => bytes
                                .iter()
                                .map(|&b| b as char)
                                .filter(|c| c.is_alphanumeric())
                                .collect(),
                        };
                        counts.alphanumerics += decoded.len();
                        seen.extend(decoded);
                    }
                }
                _ => {}
            }
        }
    }
    counts.distinct_alphanumerics = seen.len();
    counts
}

/// Check if a page has Type0 fonts with Identity-H/V encoding and no ToUnicode CMap.
/// These fonts encode text as raw CID values that can't be mapped to Unicode without
/// a ToUnicode CMap, producing garbage output for non-Latin scripts (e.g. Cyrillic).
///
/// Returns false when the page also has other decodable text fonts (Type1, TrueType,
/// or Type0 with ToUnicode/fallback). In that case the undecodable Identity-H font
/// is supplementary and the page has enough good text for extraction.
///
/// NOTE: This is a resource-based check (examines ALL fonts in Resources/Font, not just
/// those used by Tf operators). Superseded by `used_fonts_have_identity_h_no_tounicode`
/// in production code. Kept for unit tests that validate font-level classification.
#[cfg(test)]
fn page_has_identity_h_no_tounicode(doc: &Document, page_id: ObjectId) -> bool {
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let mut has_undecodable_identity_h = false;
    let mut has_other_decodable_font = false;

    for font_dict in fonts.values() {
        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok());

        match subtype {
            Some(b"Type0") => {
                let encoding = font_dict
                    .get(b"Encoding")
                    .ok()
                    .and_then(|o| o.as_name().ok());
                let is_identity = matches!(encoding, Some(b"Identity-H") | Some(b"Identity-V"));

                if !is_identity {
                    // Type0 with non-Identity encoding (e.g. a named CMap) — decodable
                    has_other_decodable_font = true;
                    continue;
                }
                if font_dict.get(b"ToUnicode").is_ok() {
                    // Has ToUnicode — decodable
                    has_other_decodable_font = true;
                    continue;
                }
                if identity_h_font_has_fallback(font_dict, doc) {
                    // Fallback decoding path works — decodable
                    has_other_decodable_font = true;
                    continue;
                }

                // Identity-H/V without ToUnicode and no fallback — undecodable
                log::debug!(
                    "page has Identity-H/V font without ToUnicode: {:?}",
                    font_dict
                        .get(b"BaseFont")
                        .ok()
                        .and_then(|o| o.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).to_string())
                );
                has_undecodable_identity_h = true;
            }
            Some(b"Type3") => {
                // Type3 fonts are handled separately by page_has_only_type3_fonts;
                // don't count them as decodable here.
            }
            _ => {
                // Type1, TrueType, MMType1, CIDFontType0/2 — these are generally
                // decodable via standard encoding, ToUnicode, or glyph name lookup.
                has_other_decodable_font = true;
            }
        }
    }

    // Only flag when there are undecodable Identity-H fonts AND no other
    // decodable fonts on the page. If the page has other text fonts, the
    // Identity-H font is supplementary and the page still extracts well.
    has_undecodable_identity_h && !has_other_decodable_font
}

/// Check whether an Identity-H font without ToUnicode can still be decoded
/// via one of the extraction pipeline's fallback paths.
fn identity_h_font_has_fallback(font_dict: &lopdf::Dictionary, doc: &Document) -> bool {
    let desc_fonts_obj = match font_dict.get(b"DescendantFonts").ok() {
        Some(obj) => obj,
        None => return false,
    };
    let desc_fonts = match desc_fonts_obj {
        Object::Array(arr) => arr,
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Array(arr)) => arr,
            _ => return false,
        },
        _ => return false,
    };
    if desc_fonts.is_empty() {
        return false;
    }
    let cid_font_dict = match &desc_fonts[0] {
        Object::Reference(r) => match doc.get_dictionary(*r) {
            Ok(d) => d,
            _ => return false,
        },
        Object::Dictionary(d) => d,
        _ => return false,
    };

    // Fallback 1: W array CIDs look like Unicode codepoints → passthrough works.
    // Many PDF generators (Chromium, wkhtmltopdf) use Identity-H where CID = Unicode.
    if crate::tounicode::cid_values_look_like_unicode(cid_font_dict) {
        return true;
    }

    // Fallback 2: Embedded TrueType/OpenType font has a usable cmap table.
    if let Some(font_descriptor) = cid_font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| match o {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        })
    {
        let font_file_ref = font_descriptor
            .get(b"FontFile2")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .or_else(|| {
                font_descriptor
                    .get(b"FontFile3")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
            });
        if let Some(ff_ref) = font_file_ref {
            if embedded_font_has_cmap(doc, ff_ref) {
                return true;
            }
            // Fallback 3: no cmap, but the glyph order is the standard
            // Macintosh one and the metrics corroborate it (see
            // `mac_glyph_order`); extraction decodes it.
            if crate::mac_glyph_order::cid_to_gid_is_identity(cid_font_dict, doc)
                && doc
                    .get_object(ff_ref)
                    .and_then(Object::as_stream)
                    .ok()
                    .and_then(|stream| stream.decompressed_content().ok())
                    .is_some_and(|data| crate::mac_glyph_order::font_file_follows_mac_order(&data))
            {
                return true;
            }
        }
    }

    false
}

/// Quick check whether an embedded TrueType/OpenType font has a cmap table
/// that can map GIDs to Unicode codepoints.
fn embedded_font_has_cmap(doc: &Document, font_ref: lopdf::ObjectId) -> bool {
    let stream = match doc.get_object(font_ref).and_then(Object::as_stream) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let data = match stream.decompressed_content() {
        Ok(d) => d,
        Err(_) => return false,
    };
    let face = match ttf_parser::Face::parse(&data, 0) {
        Ok(f) => f,
        Err(_) => return false,
    };
    // Check that the font has a cmap table with at least some Unicode mappings
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables {
            if subtable.is_unicode()
                || (subtable.platform_id == ttf_parser::PlatformId::Windows
                    && subtable.encoding_id == 0)
            {
                let mut count = 0u32;
                subtable.codepoints(|_| count += 1);
                if count > 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// Returns true if every font on the page is Type3 (no normal text fonts).
/// Type3 fonts render glyphs as custom drawings/bitmaps. Without a ToUnicode
/// CMap, character codes can't be mapped to Unicode — the page needs OCR.
///
/// NOTE: Resource-based check. Superseded by `used_fonts_are_only_type3`.
/// Kept for existing unit tests.
#[cfg(test)]
fn page_has_only_type3_fonts(doc: &Document, page_id: ObjectId) -> bool {
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if fonts.is_empty() {
        return false;
    }
    let mut has_type3 = false;
    for font_dict in fonts.values() {
        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok());
        if subtype == Some(b"Type3") {
            // Type3 with a ToUnicode CMap can still produce usable text
            if font_dict.get(b"ToUnicode").is_ok() {
                return false;
            }
            has_type3 = true;
        } else {
            // Has a non-Type3 font — page has real text fonts
            return false;
        }
    }
    if has_type3 {
        log::debug!("page has only Type3 fonts without ToUnicode — text is undecodable");
    }
    has_type3
}

/// Check if the page has at least one font that can produce decodable Unicode text.
///
/// Returns true when any font on the page has:
/// - A /ToUnicode CMap (works for all font types including CID fonts), OR
/// - A standard /Encoding (WinAnsiEncoding, MacRomanEncoding, etc.) for Type1/TrueType, OR
/// - Is a Type1 or TrueType font (these use glyph names → Adobe Glyph List fallback)
///
/// This distinguishes pages with CID-encoded text that IS decodable (via ToUnicode)
/// from scanned pages that happen to have a few decorative text ops. CID text produces
/// low unique_alphanum_chars in raw bytes but can map to full Unicode through ToUnicode.
///
/// NOTE: Resource-based check. Superseded by `used_fonts_have_decodable_text`.
/// Kept for existing unit tests.
#[cfg(test)]
fn page_has_decodable_text_fonts(doc: &Document, page_id: ObjectId) -> bool {
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(_) => return false,
    };
    for font_dict in fonts.values() {
        // Any font with ToUnicode is decodable
        if font_dict.get(b"ToUnicode").is_ok() {
            return true;
        }
        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok());
        match subtype {
            Some(b"Type1") | Some(b"TrueType") | Some(b"MMType1") => {
                // Type1/TrueType with a named encoding or glyph names are decodable
                // via the Adobe Glyph List or encoding vectors.
                return true;
            }
            Some(b"Type0") => {
                // Type0 (CID) without ToUnicode — check if it has a fallback path
                if identity_h_font_has_fallback(font_dict, doc) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Usage-based check: do the USED fonts include an undecodable Identity-H/V font
/// without any other decodable font to compensate?
///
/// Unlike `page_has_identity_h_no_tounicode`, this only considers fonts actually
/// referenced by Tf operators in content streams (P1 fix) and includes fonts from
/// Form XObject Resources (P2 fix).
fn used_fonts_have_identity_h_no_tounicode(
    used_font_ids: &HashSet<ObjectId>,
    font_map: &HashMap<ObjectId, FontInfo>,
    doc: &Document,
) -> bool {
    let mut has_undecodable_identity_h = false;
    let mut has_other_decodable_font = false;

    for id in used_font_ids {
        let Some(info) = font_map.get(id) else {
            continue;
        };
        match info.subtype.as_deref() {
            Some(b"Type0") => {
                let is_identity = matches!(
                    info.encoding.as_deref(),
                    Some(b"Identity-H") | Some(b"Identity-V")
                );
                if !is_identity {
                    has_other_decodable_font = true;
                    continue;
                }
                if info.has_tounicode {
                    has_other_decodable_font = true;
                    continue;
                }
                if identity_h_font_has_fallback(&info.dict, doc) {
                    has_other_decodable_font = true;
                    continue;
                }
                has_undecodable_identity_h = true;
            }
            Some(b"Type3") => {
                // Handled separately by used_fonts_are_only_type3
            }
            _ => {
                // Type1, TrueType, MMType1, etc. — generally decodable
                has_other_decodable_font = true;
            }
        }
    }

    has_undecodable_identity_h && !has_other_decodable_font
}

/// Usage-based check: are ALL used fonts Type3 without ToUnicode?
///
/// Unlike `page_has_only_type3_fonts`, this only considers fonts actually referenced
/// by Tf operators (P1 fix) and includes Form XObject fonts (P2 fix).
fn used_fonts_are_only_type3(
    used_font_ids: &HashSet<ObjectId>,
    font_map: &HashMap<ObjectId, FontInfo>,
) -> bool {
    if used_font_ids.is_empty() {
        return false;
    }
    let mut has_type3 = false;
    for id in used_font_ids {
        let Some(info) = font_map.get(id) else {
            continue;
        };
        if info.subtype.as_deref() == Some(b"Type3") {
            if info.has_tounicode {
                return false;
            }
            has_type3 = true;
        } else {
            return false;
        }
    }
    has_type3
}

/// Usage-based check: do the USED fonts include at least one that can produce
/// decodable Unicode text?
///
/// Unlike `page_has_decodable_text_fonts`, this only considers fonts actually
/// referenced by Tf operators (P1 fix) and includes Form XObject fonts (P2 fix).
fn used_fonts_have_decodable_text(
    used_font_ids: &HashSet<ObjectId>,
    font_map: &HashMap<ObjectId, FontInfo>,
    doc: &Document,
) -> bool {
    for id in used_font_ids {
        let Some(info) = font_map.get(id) else {
            continue;
        };
        if info.has_tounicode {
            return true;
        }
        match info.subtype.as_deref() {
            Some(b"Type1") | Some(b"TrueType") | Some(b"MMType1") => {
                return true;
            }
            Some(b"Type0") => {
                if identity_h_font_has_fallback(&info.dict, doc) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn scan_xobjects_in_resources(
    doc: &Document,
    resources: &lopdf::Dictionary,
    visited: &mut HashSet<ObjectId>,
    unique_chars: &mut HashSet<u8>,
    used_font_ids: &mut HashSet<ObjectId>,
    font_map: &mut HashMap<ObjectId, FontInfo>,
) -> ContentCounts {
    let mut counts = ContentCounts::default();

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
                    // Collect raw font names from this XObject's content stream.
                    // Every form bound is counted here, invoked or not, as
                    // the text and font tallies always have been; what the
                    // page executes is followed from its own content, so
                    // this scan follows no `Do`.
                    let mut xobj_font_names: HashSet<Vec<u8>> = HashSet::new();
                    let mut form_state = ContentScanState::new(doc, PageBox::LETTER, false);
                    counts.add(scan_content_stream(
                        &content,
                        unique_chars,
                        &mut xobj_font_names,
                        &mut form_state,
                        &[],
                    ));

                    // Resolve the Form XObject's /Resources — handle both inline
                    // dicts and indirect references (P2 fix: indirect refs were
                    // previously skipped by as_dict()).
                    let xobj_res_owned;
                    let xobj_res = match stream.dict.get(b"Resources").ok() {
                        Some(Object::Dictionary(d)) => Some(d),
                        Some(Object::Reference(r)) => {
                            xobj_res_owned = doc.get_dictionary(*r).ok();
                            xobj_res_owned
                        }
                        _ => None,
                    };

                    if let Some(res) = xobj_res {
                        // Resolve font names against the XObject's own resource dict
                        // (P1 fix: scoped resolution, not global name-based lookup)
                        resolve_font_names_to_ids(doc, res, &xobj_font_names, used_font_ids);
                        // Collect font definitions from this scope
                        collect_fonts_from_resource_dict(doc, res, font_map);
                        // Recurse into nested XObjects
                        counts.add(scan_xobjects_in_resources(
                            doc,
                            res,
                            visited,
                            unique_chars,
                            used_font_ids,
                            font_map,
                        ));
                    }
                }
                Some(b"Image") => {
                    counts.image_count += 1;
                }
                _ => {}
            }
        }
    }

    counts
}

/// What a scan of content streams counted.
#[derive(Clone, Copy, Default)]
struct ContentCounts {
    /// Text-showing operators (`Tj`, `TJ`, `'`, `"`), whatever their render
    /// mode.
    text_ops: u32,
    /// Image XObjects among the resources scanned.
    image_count: u32,
    /// Path construction and painting operators.
    path_ops: u32,
    /// `Tf` operators whose font name could be read.
    font_changes: u32,
}

impl ContentCounts {
    fn add(&mut self, other: ContentCounts) {
        self.text_ops += other.text_ops;
        self.image_count += other.image_count;
        self.path_ops += other.path_ops;
        self.font_changes += other.font_changes;
    }
}

/// How many `q` levels the scan keeps a saved state for. Deeper nesting
/// keeps the innermost state, and its `Q`s restore nothing.
const SCAN_STATE_MAX_DEPTH: usize = 256;

/// How many Form XObjects one page's scan follows through `Do`, each once.
const FOLLOWED_FORMS_MAX: usize = 1_000;

/// How many image draws one page's scan keeps for its coverage.
const IMAGE_DRAWS_MAX: usize = 1_024;

/// A box in user space, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct UserBox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl UserBox {
    fn area(&self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }

    /// `None` when the boxes do not overlap.
    fn intersect(&self, other: &UserBox) -> Option<UserBox> {
        let clipped = UserBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (clipped.x1 > clipped.x0 && clipped.y1 > clipped.y0).then_some(clipped)
    }
}

/// One image a page's content drew: its bounding box clipped to the page,
/// and its own area under the matrix in force, which the box exceeds when
/// the image is turned.
#[derive(Clone, Copy)]
struct ImageDraw {
    on_page: UserBox,
    own_area: f64,
}

/// The area the boxes cover together, overlaps counted once.
fn union_area(draws: &[ImageDraw]) -> f64 {
    let mut xs: Vec<f64> = draws
        .iter()
        .flat_map(|draw| [draw.on_page.x0, draw.on_page.x1])
        .collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut total = 0.0;
    for slab in xs.windows(2) {
        let (x0, x1) = (slab[0], slab[1]);
        let mut spans: Vec<(f64, f64)> = draws
            .iter()
            .filter(|draw| draw.on_page.x0 <= x0 && draw.on_page.x1 >= x1)
            .map(|draw| (draw.on_page.y0, draw.on_page.y1))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut covered = 0.0;
        let mut open: Option<(f64, f64)> = None;
        for (y0, y1) in spans {
            match open {
                Some((o0, o1)) if y0 <= o1 => open = Some((o0, o1.max(y1))),
                Some((o0, o1)) => {
                    covered += o1 - o0;
                    open = Some((y0, y1));
                }
                None => open = Some((y0, y1)),
            }
        }
        if let Some((o0, o1)) = open {
            covered += o1 - o0;
        }
        total += covered * (x1 - x0);
    }
    total
}

/// What `q` saves of the state the scan follows.
#[derive(Clone, Copy)]
struct SavedScanState {
    render_mode: u8,
    ctm: [f64; 6],
    clip_text_ops: u32,
}

/// The part of the graphics state a content scan follows, saved by `q`
/// and restored by `Q`: the text render mode `Tr` sets; the current
/// transformation matrix `cm` concatenates; and the clip-only (mode 7)
/// text whose clip is in force, which a painting operator shows through
/// and the `Q` closing its level discards unseen.
///
/// One state runs through a page's content streams, which the PDF reads
/// as one, and — when it follows `Do` — through the Form XObjects they
/// invoke, each once, with the state in force at the invocation, the
/// form's `/Matrix` applied and the form's own changes undone afterwards,
/// as a renderer runs them. It tallies what the page executes: its
/// text-showing operators, those of them that leave nothing to see, and
/// the images it draws. A scan that follows no `Do` only counts.
struct ContentScanState<'a> {
    doc: &'a Document,
    /// The visible page box, which image draws are clipped to.
    page: UserBox,
    /// Whether `Do` is followed: images drawn are measured and forms are
    /// scanned in place.
    follow_do: bool,
    render_mode: u8,
    ctm: [f64; 6],
    /// Mode-7 text-showing operators whose clip was set at the current
    /// level and has not been painted through; those set at outer levels
    /// sit in `saved`.
    clip_text_ops: u32,
    /// `clip_text_ops` over the current and the saved levels together.
    pending_clip_text_ops: u32,
    /// Mode-7 text-showing operators of the open text object: their clip
    /// takes effect at its `ET`, so what is painted before that — the rest
    /// of the text object — does not show through them.
    clip_text_ops_open: u32,
    saved: Vec<SavedScanState>,
    /// `q` operators past `SCAN_STATE_MAX_DEPTH`, whose `Q`s restore nothing.
    unsaved_depth: u32,
    /// The depth `Q` does not restore below: a form's content cannot close
    /// its invoker's levels.
    stack_floor: usize,
    /// Text-showing operators executed, all of them.
    executed_text_ops: u32,
    /// Those of `executed_text_ops` that left nothing to see: mode 3, or
    /// mode 7 with nothing painted through its clip.
    executed_hidden_text_ops: u32,
    /// The images drawn, up to `IMAGE_DRAWS_MAX`.
    image_draws: Vec<ImageDraw>,
    /// The forms followed so far, each scanned once.
    followed_forms: HashSet<ObjectId>,
}

impl<'a> ContentScanState<'a> {
    fn new(doc: &'a Document, page_box: PageBox, follow_do: bool) -> Self {
        Self {
            doc,
            page: UserBox {
                x0: f64::from(page_box.x0),
                y0: f64::from(page_box.y0),
                x1: f64::from(page_box.x1),
                y1: f64::from(page_box.y1),
            },
            follow_do,
            render_mode: 0,
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            clip_text_ops: 0,
            pending_clip_text_ops: 0,
            clip_text_ops_open: 0,
            saved: Vec::new(),
            unsaved_depth: 0,
            stack_floor: 0,
            executed_text_ops: 0,
            executed_hidden_text_ops: 0,
            image_draws: Vec::new(),
            followed_forms: HashSet::new(),
        }
    }

    fn save(&mut self) {
        if self.saved.len() < SCAN_STATE_MAX_DEPTH {
            self.saved.push(SavedScanState {
                render_mode: self.render_mode,
                ctm: self.ctm,
                clip_text_ops: self.clip_text_ops,
            });
            self.clip_text_ops = 0;
        } else {
            self.unsaved_depth += 1;
        }
    }

    /// A `Q` with nothing of its own saved is ignored, as renderers ignore
    /// it. The clips set at the level being left go with it: what they
    /// hid, with nothing painted through them, stays hidden.
    fn restore(&mut self) {
        if self.unsaved_depth > 0 {
            self.unsaved_depth -= 1;
        } else if self.saved.len() > self.stack_floor {
            if let Some(saved) = self.saved.pop() {
                self.pending_clip_text_ops -= self.clip_text_ops;
                self.render_mode = saved.render_mode;
                self.ctm = saved.ctm;
                self.clip_text_ops = saved.clip_text_ops;
            }
        }
    }

    /// `cm`: the matrix given goes before the one in force.
    fn concat(&mut self, [a1, b1, c1, d1, e1, f1]: [f64; 6]) {
        let [a2, b2, c2, d2, e2, f2] = self.ctm;
        self.ctm = [
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ];
    }

    /// A text-showing operator ran. In mode 3 it left nothing to see; in
    /// mode 7 nothing yet — from the text object's `ET` on, its glyphs clip
    /// whatever is painted, until the `Q` closing its level; in any other
    /// mode it painted, through any clip in force.
    fn text_shown(&mut self) {
        self.executed_text_ops += 1;
        match self.render_mode {
            3 => self.executed_hidden_text_ops += 1,
            7 => {
                self.executed_hidden_text_ops += 1;
                self.clip_text_ops_open += 1;
            }
            _ => self.painted(),
        }
    }

    /// `ET`: the clip the text object's mode-7 text built takes effect.
    fn text_object_ended(&mut self) {
        self.clip_text_ops += self.clip_text_ops_open;
        self.pending_clip_text_ops += self.clip_text_ops_open;
        self.clip_text_ops_open = 0;
    }

    /// Something was painted, so the clip-only text in force shows it
    /// through its glyphs: that text is visible after all.
    fn painted(&mut self) {
        let shown = self.pending_clip_text_ops;
        if shown > 0 {
            self.executed_hidden_text_ops -= shown;
            self.pending_clip_text_ops = 0;
            self.clip_text_ops = 0;
            for saved in &mut self.saved {
                saved.clip_text_ops = 0;
            }
        }
    }

    /// `Do` of an image: it paints the unit square under the matrix in
    /// force, of which the part on the page counts.
    fn image_drawn(&mut self) {
        self.painted();
        let [a, b, c, d, e, f] = self.ctm;
        let xs = [e, a + e, c + e, a + c + e];
        let ys = [f, b + f, d + f, b + d + f];
        if !xs.iter().chain(&ys).all(|v| v.is_finite()) {
            return;
        }
        let drawn = UserBox {
            x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
            y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
            x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        };
        if let Some(on_page) = drawn.intersect(&self.page) {
            if self.image_draws.len() < IMAGE_DRAWS_MAX {
                self.image_draws.push(ImageDraw {
                    on_page,
                    own_area: (a * d - b * c).abs(),
                });
            }
        }
    }

    /// `Do` of a form: run in place, once, as a renderer runs it — under
    /// the state in force, with its `/Matrix` applied and its own changes
    /// undone afterwards. Its names resolve in its own resources first,
    /// then in its invoker's.
    fn form_drawn(
        &mut self,
        id: ObjectId,
        form: &'a lopdf::Stream,
        invoker_resources: &[&'a lopdf::Dictionary],
    ) {
        if self.followed_forms.len() >= FOLLOWED_FORMS_MAX
            || self.saved.len() >= SCAN_STATE_MAX_DEPTH
            || !self.followed_forms.insert(id)
        {
            return;
        }
        let content = form
            .decompressed_content()
            .unwrap_or_else(|_| form.content.clone());
        let mut resources = Vec::with_capacity(invoker_resources.len() + 1);
        resources.extend(stream_resources(self.doc, form));
        resources.extend_from_slice(invoker_resources);

        let outer_floor = self.stack_floor;
        self.save();
        let base = self.saved.len();
        self.stack_floor = base;
        if let Some(matrix) = form_matrix(self.doc, form) {
            self.concat(matrix);
        }
        scan_content_stream(
            &content,
            &mut HashSet::new(),
            &mut HashSet::new(),
            self,
            &resources,
        );
        // Levels the form left open close with it.
        while self.unsaved_depth > 0 || self.saved.len() > base {
            self.restore();
        }
        self.stack_floor = outer_floor;
        self.restore();
    }

    /// The page area the images drawn cover: the union of their boxes on
    /// the page, and no more than their own areas add up to, since a
    /// turned image's box exceeds it.
    fn covered_image_area(&self) -> f64 {
        let own: f64 = self
            .image_draws
            .iter()
            .map(|draw| draw.own_area.min(draw.on_page.area()))
            .sum();
        union_area(&self.image_draws).min(own)
    }

    /// Whether the images drawn cover the page: at least
    /// `COVERING_IMAGE_MIN_PAGE_FRACTION` of its area.
    fn covers_page(&self) -> bool {
        self.covered_image_area() >= COVERING_IMAGE_MIN_PAGE_FRACTION * self.page.area()
    }

    /// Whether every text-showing operator executed left nothing to see
    /// while the images drawn cover the page: the raster is all the page
    /// shows, and the text layer describes it rather than being its
    /// content.
    fn shows_only_a_hidden_text_layer(&self) -> bool {
        self.executed_text_ops > 0
            && self.executed_hidden_text_ops == self.executed_text_ops
            && self.covers_page()
    }
}

/// What a `Do` operand names, in the first of the resources binding it.
enum XObjectDrawn<'a> {
    Image,
    Form(ObjectId, &'a lopdf::Stream),
}

fn resolve_xobject<'a>(
    doc: &'a Document,
    resources: &[&'a lopdf::Dictionary],
    name: &[u8],
) -> Option<XObjectDrawn<'a>> {
    for scope in resources {
        let xobjects = match scope.get(b"XObject").ok() {
            Some(Object::Dictionary(dict)) => dict,
            Some(Object::Reference(id)) => match doc.get_dictionary(*id) {
                Ok(dict) => dict,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Ok(entry) = xobjects.get(name) else {
            continue;
        };
        let id = entry.as_reference().ok()?;
        let Ok(Object::Stream(stream)) = doc.get_object(id) else {
            return None;
        };
        return match stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|subtype| subtype.as_name().ok())
        {
            Some(b"Image") => Some(XObjectDrawn::Image),
            Some(b"Form") => Some(XObjectDrawn::Form(id, stream)),
            _ => None,
        };
    }
    None
}

/// A stream's `/Resources`, inline or by reference.
fn stream_resources<'a>(
    doc: &'a Document,
    stream: &'a lopdf::Stream,
) -> Option<&'a lopdf::Dictionary> {
    match stream.dict.get(b"Resources").ok()? {
        Object::Dictionary(dict) => Some(dict),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

/// A form's `/Matrix`, when it holds six numbers.
fn form_matrix(doc: &Document, form: &lopdf::Stream) -> Option<[f64; 6]> {
    let array = match form.dict.get(b"Matrix").ok()? {
        Object::Array(array) => array,
        Object::Reference(id) => doc.get_object(*id).ok()?.as_array().ok()?,
        _ => return None,
    };
    if array.len() != 6 {
        return None;
    }
    let mut matrix = [0.0f64; 6];
    for (slot, value) in matrix.iter_mut().zip(array) {
        let number = match value {
            Object::Reference(id) => doc.get_object(*id).ok().and_then(get_number),
            other => get_number(other),
        }?;
        *slot = f64::from(number);
    }
    Some(matrix)
}

/// `content` with everything that is not an operator or its operands
/// blanked to spaces, at the same offsets: the insides of literal strings
/// (nesting and escapes honoured), of hex strings and of comments, and
/// inline image data, from the `ID` of an inline image `BI` opened through
/// its `EI`. The delimiters stay, so a string still closes an operand; the
/// strings' bytes are read from the original when a text operator is
/// found.
fn mask_strings_comments_and_inline_images(content: &[u8]) -> Vec<u8> {
    /// Whether an operator token may begin at `i`: at the start, or after
    /// whitespace or a closing delimiter.
    fn after_token_break(content: &[u8], i: usize) -> bool {
        i == 0 || is_pdf_whitespace(content[i - 1]) || matches!(content[i - 1], b')' | b']' | b'>')
    }
    let mut masked = content.to_vec();
    // `ID` begins image data only inside an inline image, which `BI` opens;
    // anywhere else — a bare token, or the name `/ID` — it is left alone.
    let mut inline_image_open = false;
    let mut i = 0;
    while i < content.len() {
        match content[i] {
            b'(' => {
                let mut depth = 1u32;
                i += 1;
                while i < content.len() {
                    match content[i] {
                        b'\\' => {
                            masked[i] = b' ';
                            if i + 1 < content.len() {
                                masked[i + 1] = b' ';
                            }
                            i += 2;
                            continue;
                        }
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    masked[i] = b' ';
                    i += 1;
                }
            }
            b'<' if content.get(i + 1) == Some(&b'<') => i += 1,
            b'<' => {
                i += 1;
                while i < content.len() && content[i] != b'>' {
                    masked[i] = b' ';
                    i += 1;
                }
            }
            b'%' => {
                while i < content.len() && !matches!(content[i], b'\n' | b'\r') {
                    masked[i] = b' ';
                    i += 1;
                }
                continue;
            }
            b'B' if content.get(i + 1) == Some(&b'I')
                && after_token_break(content, i)
                && content.get(i + 2).is_none_or(|&b| is_pdf_whitespace(b)) =>
            {
                inline_image_open = true;
                i += 1;
            }
            b'I' if inline_image_open
                && content.get(i + 1) == Some(&b'D')
                && after_token_break(content, i)
                && content.get(i + 2).is_none_or(|&b| is_pdf_whitespace(b)) =>
            {
                inline_image_open = false;
                let end = inline_image_end(content, i + 2).unwrap_or(content.len());
                masked[i..end].fill(b' ');
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    masked
}

/// Just past the `EI` that ends inline image data starting at `from`: the
/// first `EI` set off by whitespace on both sides. `None` when the data
/// runs to the end of the stream.
fn inline_image_end(content: &[u8], from: usize) -> Option<usize> {
    let mut i = from + 1;
    while i + 1 < content.len() {
        if content[i] == b'E'
            && content[i + 1] == b'I'
            && is_pdf_whitespace(content[i - 1])
            && content.get(i + 2).is_none_or(|&b| is_pdf_whitespace(b))
        {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// [`scan_content_stream`] of a stream on its own — the initial graphics
/// state, nothing followed through `Do` — as the counts alone:
/// `(text_ops, image_count, path_ops, font_changes)`.
#[cfg(test)]
fn scan_content_for_text_operators(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> (u32, u32, u32, u32) {
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    let counts = scan_content_stream(content, unique_chars, used_font_names, &mut state, &[]);
    (
        counts.text_ops,
        counts.image_count,
        counts.path_ops,
        counts.font_changes,
    )
}

/// Fast scan of content stream bytes for text operators
///
/// This is a fast heuristic scan that looks for:
/// - "Tj" - show text string
/// - "TJ" - show text with individual glyph positioning
/// - "'" and "\"" - move to the next line and show text
/// - "Tf" - set font, whose name goes to `used_font_names`
/// - path construction and painting operators
///
/// Operators are found in a copy of the stream with its strings, comments
/// and inline image data blanked, so none of those can pass for one; the
/// text itself is read from the original. The scan follows the graphics
/// state in `state`: `Tr` sets the text render mode, which decides whether
/// a text-showing operator left anything to see; `cm` concatenates the
/// matrix an image is drawn under; `q` and `Q` save and restore both. A
/// `Do` — when the state follows them — draws the image, or runs the
/// form, that the first of `resources` binding its name holds. Unique
/// non-whitespace text characters are collected into `unique_chars`.
fn scan_content_stream<'a>(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    state: &mut ContentScanState<'a>,
    resources: &[&'a lopdf::Dictionary],
) -> ContentCounts {
    let mut counts = ContentCounts::default();
    let masked = mask_strings_comments_and_inline_images(content);
    let ops: &[u8] = &masked;

    // Helper: check if position is a word boundary (start of content or preceded by whitespace)
    let is_word_start = |pos: usize| -> bool { pos == 0 || ops[pos - 1].is_ascii_whitespace() };
    // Helper: check if position is at end or followed by whitespace
    let is_word_end =
        |pos: usize| -> bool { pos + 1 >= ops.len() || ops[pos + 1].is_ascii_whitespace() };
    // Helpers for the graphics-state operators, which may sit against a
    // delimiter (`Q/Im0 Do`, `3 Tr(x)`): a token starts after whitespace
    // or a closing delimiter, and ends before whitespace or an opening one.
    let is_token_start = |pos: usize| -> bool {
        pos == 0 || ops[pos - 1].is_ascii_whitespace() || matches!(ops[pos - 1], b')' | b']' | b'>')
    };
    let is_token_end = |pos: usize| -> bool {
        pos + 1 >= ops.len()
            || ops[pos + 1].is_ascii_whitespace()
            || matches!(ops[pos + 1], b'/' | b'[' | b'(' | b'<' | b'%')
    };

    // Simple state machine to find operators.
    // Each Tj/TJ/Tf lookback stops at the previous text/font operator so a
    // malformed `] TJ` (no `[`) cannot rescan the entire prefix — that was
    // quadratic in the number of operators.
    // `Tj`/`TJ` are only counted when the preceding token closes a string or
    // array (')', '>', ']').
    let mut operand_floor = 0usize;
    let mut i = 0;
    while i < ops.len() {
        let b = ops[i];

        // Look for 'T' followed by 'j', 'J', 'f' or 'r'
        if b == b'T' && i + 1 < ops.len() {
            let next = ops[i + 1];
            if next == b'j' || next == b'J' {
                // Verify it's an operator (followed by whitespace or newline)
                if (i + 2 >= ops.len()
                    || ops[i + 2].is_ascii_whitespace()
                    || ops[i + 2] == b'\n'
                    || ops[i + 2] == b'\r')
                    && preceding_operand_closer(ops, i, operand_floor)
                {
                    counts.text_ops += 1;
                    state.text_shown();
                    collect_text_chars_before(content, i, unique_chars, operand_floor);
                    operand_floor = i;
                }
            } else if next == b'f' {
                // Tf = set font operator
                // Some PDFs concatenate Tf with the next operator without
                // whitespace (e.g. "25 Tf[<01>..." or "25 Tf(<text>..."),
                // so also accept '[', '(', '<', '/' as valid followers.
                if i + 2 >= ops.len()
                    || ops[i + 2].is_ascii_whitespace()
                    || ops[i + 2] == b'\n'
                    || ops[i + 2] == b'\r'
                    || ops[i + 2] == b'['
                    || ops[i + 2] == b'('
                    || ops[i + 2] == b'<'
                    || ops[i + 2] == b'/'
                {
                    if let Some(name) = extract_font_name_before_tf(ops, i, operand_floor) {
                        used_font_names.insert(name);
                        counts.font_changes += 1;
                        operand_floor = i;
                    }
                }
            } else if next == b'r' && is_token_start(i) && is_token_end(i + 1) {
                // Tr = set text render mode. A mode outside 0..=7 is
                // ignored, as renderers ignore it.
                if let Some([mode]) = numeric_operands_before::<1>(ops, i, operand_floor) {
                    if mode.fract() == 0.0 && (0.0..=7.0).contains(&mode) {
                        state.render_mode = mode as u8;
                    }
                    operand_floor = i;
                }
            }
        } else if (b == b'\'' || b == b'"')
            && is_token_start(i)
            && is_token_end(i)
            && preceding_operand_closer(ops, i, operand_floor)
        {
            // ' and " = move to the next line and show text (" sets the
            // word and character spacing first). An apostrophe inside a
            // string was blanked, so it cannot get here.
            counts.text_ops += 1;
            state.text_shown();
            collect_text_chars_before(content, i, unique_chars, operand_floor);
            operand_floor = i;
        } else if b == b'c'
            && ops.get(i + 1) == Some(&b'm')
            && is_token_start(i)
            && is_token_end(i + 1)
        {
            // cm = concatenate matrix.
            if let Some(matrix) = numeric_operands_before::<6>(ops, i, operand_floor) {
                state.concat(matrix);
                operand_floor = i;
            }
        } else if b == b'D'
            && ops.get(i + 1) == Some(&b'o')
            && is_token_start(i)
            && is_token_end(i + 1)
        {
            // Do = paint an XObject: an image is measured, a form run in
            // place. Whether a page has images at all is read from its
            // resources (scan_xobjects_in_resources, analyze_page_images).
            if let Some(name) = name_operand_before(ops, i, operand_floor) {
                operand_floor = i;
                if state.follow_do {
                    match resolve_xobject(state.doc, resources, &name) {
                        Some(XObjectDrawn::Image) => state.image_drawn(),
                        Some(XObjectDrawn::Form(id, form)) => state.form_drawn(id, form, resources),
                        None => {}
                    }
                }
            }
        } else if (b == b's' && ops.get(i + 1) == Some(&b'h')
            || b == b'B' && ops.get(i + 1) == Some(&b'I'))
            && is_token_start(i)
            && is_token_end(i + 1)
        {
            // sh = paint a shading; BI = begin an inline image.
            state.painted();
        } else if b == b'E'
            && ops.get(i + 1) == Some(&b'T')
            && is_token_start(i)
            && is_token_end(i + 1)
        {
            // ET = end a text object: its clip-only text's clip takes effect.
            state.text_object_ended();
        } else if b == b'q' && is_token_start(i) && is_token_end(i) {
            state.save();
        } else if b == b'Q' && is_token_start(i) && is_token_end(i) {
            state.restore();
        }

        // Count path construction/painting operators.
        // Single-byte: m (moveto), l (lineto), c (curveto), h (closepath),
        //              f (fill), S (stroke), s (close+stroke), B (fill+stroke),
        //              F (fill, variant)
        // These are the high-volume operators in vector-outlined text.
        // A painting operator also shows any clip-only text through its
        // glyphs; b, B* and b* paint too but are not counted as path
        // operators.
        let mut painted = false;
        match b {
            b'm' | b'l' | b'c' | b'h' if is_word_start(i) && is_word_end(i) => {
                counts.path_ops += 1;
            }
            b'f' | b'S' | b's' | b'B' | b'F' if is_word_start(i) && is_word_end(i) => {
                counts.path_ops += 1;
                painted = true;
            }
            b'b' if is_word_start(i) && is_word_end(i) => {
                painted = true;
            }
            // Two-byte: re (rect), f* (fill even-odd)
            b'r' if i + 1 < ops.len()
                && ops[i + 1] == b'e'
                && is_word_start(i)
                && (i + 2 >= ops.len() || ops[i + 2].is_ascii_whitespace()) =>
            {
                counts.path_ops += 1;
            }
            b'f' if i + 1 < ops.len()
                && ops[i + 1] == b'*'
                && is_word_start(i)
                && (i + 2 >= ops.len() || ops[i + 2].is_ascii_whitespace()) =>
            {
                counts.path_ops += 1;
                painted = true;
            }
            b'B' | b'b'
                if i + 1 < ops.len()
                    && ops[i + 1] == b'*'
                    && is_word_start(i)
                    && (i + 2 >= ops.len() || ops[i + 2].is_ascii_whitespace()) =>
            {
                painted = true;
            }
            _ => {}
        }
        if painted {
            state.painted();
        }

        i += 1;
    }

    counts
}

/// The `N` numeric operands before the operator at `op_pos`, in stream
/// order; `None` when a token there is not a number or the lookback would
/// cross `floor`.
fn numeric_operands_before<const N: usize>(
    content: &[u8],
    op_pos: usize,
    floor: usize,
) -> Option<[f64; N]> {
    let mut values = [0.0f64; N];
    let mut end = op_pos;
    for value in values.iter_mut().rev() {
        while end > floor && content[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > floor && matches!(content[start - 1], b'0'..=b'9' | b'.' | b'-' | b'+') {
            start -= 1;
        }
        if start == end {
            return None;
        }
        *value = std::str::from_utf8(&content[start..end])
            .ok()?
            .parse()
            .ok()?;
        end = start;
    }
    Some(values)
}

/// The name operand (`/Name`, given without its slash) before the operator
/// at `op_pos`; `None` when the token there is not a name or the lookback
/// would cross `floor`.
fn name_operand_before(content: &[u8], op_pos: usize, floor: usize) -> Option<Vec<u8>> {
    let mut end = op_pos;
    while end > floor && content[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > floor && !is_pdf_name_delimiter(content[start - 1]) {
        start -= 1;
    }
    (start > floor && start < end && content[start - 1] == b'/')
        .then(|| content[start..end].to_vec())
}

/// True when the token before `op_pos` (skipping whitespace, not crossing
/// `floor`) is a string/array closer. Used so `Tj` inside `(Hello Tj World)`
/// is not treated as an operator.
fn preceding_operand_closer(content: &[u8], op_pos: usize, floor: usize) -> bool {
    let mut j = op_pos;
    while j > floor {
        j -= 1;
        if !content[j].is_ascii_whitespace() {
            return matches!(content[j], b')' | b'>' | b']');
        }
    }
    false
}

/// Extract the font name operand from content stream bytes preceding a Tf operator.
///
/// The Tf operator syntax is: `/FontName size Tf`
/// We scan backward from the position of 'T' in 'Tf' past the size number and
/// whitespace to find the `/Name` token.
///
/// Returns the font name bytes (without the leading `/`), e.g. `b"F1"` for `/F1`.
/// `floor` is the start of the previous text/font operator (or 0); lookback
/// must not cross it.
fn extract_font_name_before_tf(content: &[u8], tf_pos: usize, floor: usize) -> Option<Vec<u8>> {
    // Scan backward past whitespace before "Tf"
    let mut j = tf_pos;
    while j > floor && content[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    // Scan backward past the size number (digits, '.', '-')
    while j > floor
        && (content[j - 1].is_ascii_digit() || content[j - 1] == b'.' || content[j - 1] == b'-')
    {
        j -= 1;
    }
    // Scan backward past whitespace between font name and size
    while j > floor && content[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    // Now j should point just after the font name. Scan backward to find '/'.
    let name_end = j;
    while j > floor && content[j - 1] != b'/' {
        // Font names consist of regular characters (not whitespace, not delimiters)
        if content[j - 1].is_ascii_whitespace() || content[j - 1] == b'(' || content[j - 1] == b')'
        {
            return None;
        }
        j -= 1;
    }
    if j <= floor || content[j - 1] != b'/' {
        return None;
    }
    // j-1 is the '/', font name is content[j..name_end]
    if j < name_end {
        Some(content[j..name_end].to_vec())
    } else {
        None
    }
}

/// Scan backward from a Tj/TJ operator to find the preceding string operand
/// and collect unique non-whitespace bytes from it.
///
/// Handles both literal strings `(...)` and hex strings `<...>`.
/// `floor` is the start of the previous text/font operator (or 0); lookback
/// must not cross it, or a missing `[` before `TJ` rescans the whole prefix.
fn collect_text_chars_before(
    content: &[u8],
    op_pos: usize,
    unique_chars: &mut HashSet<u8>,
    floor: usize,
) {
    // Walk backward past whitespace to find the closing delimiter
    let mut j = op_pos;
    while j > floor {
        j -= 1;
        if !content[j].is_ascii_whitespace() {
            break;
        }
    }
    // All whitespace, or we landed on the previous operator token.
    if j == floor {
        return;
    }

    let closing = content[j];

    if closing == b')' {
        // Literal string: scan backward for matching '('
        let mut depth = 1i32;
        let mut k = j;
        while k > floor && depth > 0 {
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
        while k > floor {
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
        while k > floor {
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
/// Returns `(has_images, total_image_area, has_template_image)` for a page.
/// `has_template_image` means a single large (>50% page coverage)
/// background image — the signal `classify_pdf`/`detect_pdf_type` uses to
/// route a page to OCR regardless of any incidental native text drawn over
/// it. Exposed at crate visibility so extraction-side per-page `needs_ocr`
/// computation (`extract_pages_markdown_mem`) can consult the same signal
/// instead of maintaining its own, independent notion of "needs OCR" that
/// can silently disagree with detection — see #227.
pub(crate) fn analyze_page_images(doc: &Document, page_id: ObjectId) -> (bool, u64, bool) {
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

/// Computes `template_image_needs_ocr`, `has_vector_text` and
/// `has_invisible_text_layer` (see [`PageOcrSignals`]) for a page from a
/// single shared `analyze_page_content` pass — that call
/// decompresses and scans every content stream (page + XObjects) plus
/// image coverage, so `extract_pages_markdown_mem` must not invoke it
/// twice per page (once per signal) the way `detect_from_document` avoids
/// by caching its per-page `PageAnalysis`.
///
/// `needs_ocr_for_template_image` is true when a page's template image
/// should be treated as a scan needing OCR — a single full-page background
/// image with little/no real text — rather than a text page that happens
/// to carry a watermark, letterhead, or figure. Mirrors the two distinct
/// signals classification uses to route a template-image page to OCR:
///
/// 1. `looks_like_scan`: image_count <= 1, few text operators (<50), and
///    low alphanumeric diversity in raw string operands (unless decodable
///    CID/ToUnicode fonts explain that away) — the gate used for
///    `pages_with_template_images` and Mixed-type per-page routing.
/// 2. Insufficient real text volume, using the same `effective_min_ops`
///    floor (`min_text_ops_per_page.max(10)`) that `pages_with_text`
///    applies to image-bearing pages. That floor is a per-page judgment,
///    not part of the cross-page aggregate: classification counts a
///    template-image page with fewer ops as textless and routes it to OCR,
///    so this function must agree. The lower bare threshold (3) let a
///    full-page scan carrying a small native masthead — a newspaper
///    header, stamp, or date line of ~4 diverse, decodable text ops —
///    extract as "a text page" here while whole-document classification
///    called the same page scanned, silently dropping the page body from
///    OCR routing. `alphanum_low` can't catch that case: masthead chrome
///    is real text, so its byte diversity is high.
///
/// This function always evaluates against `DetectionConfig::default()` —
/// it has no config parameter, and the per-page extraction path that calls
/// it never carries one. A caller passing a custom `min_text_ops_per_page`
/// to `detect_from_document` affects whole-document detection only; the
/// two paths agree under the default configuration.
///
/// `has_vector_text` is true when a page has vector-outlined text (glyphs
/// drawn as paths rather than shown via text-showing operators) —
/// `detect_from_document`'s Mixed-type per-page routing always sends
/// these pages to OCR, independent of any template-image check, since
/// outlined glyphs can't be extracted as text at all.
///
/// Exposed at crate visibility so `extract_pages_markdown_mem` can apply
/// the same gates classification needs elsewhere instead of treating the
/// raw signals alone as sufficient — see #227/#231.
pub(crate) fn page_ocr_signals(doc: &Document, page_id: ObjectId) -> PageOcrSignals {
    let analysis = analyze_page_content(doc, page_id);

    let needs_ocr_for_template_image = if !analysis.has_template_image {
        false
    } else {
        let alphanum_low = analysis.unique_alphanum_chars < 10
            && !(analysis.has_decodable_text_fonts && analysis.text_operator_count >= 10);
        let looks_like_scan =
            analysis.image_count <= 1 && analysis.text_operator_count < 50 && alphanum_low;
        let insufficient_text =
            analysis.text_operator_count < DetectionConfig::default().min_text_ops_per_page.max(10);
        looks_like_scan || insufficient_text
    };

    PageOcrSignals {
        template_image_needs_ocr: needs_ocr_for_template_image,
        has_vector_text: analysis.has_vector_text,
        has_invisible_text_layer: analysis.has_invisible_text_layer,
    }
}

/// The per-page signals [`page_ocr_signals`] shares between classification
/// and per-page extraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PageOcrSignals {
    /// The page's template image is a scan needing OCR rather than a
    /// watermark, letterhead or figure under a text page.
    pub(crate) template_image_needs_ocr: bool,
    /// The page's text is drawn as vector outlines.
    pub(crate) has_vector_text: bool,
    /// Every text-showing operator on the page is invisible while an image
    /// covers the page.
    pub(crate) has_invisible_text_layer: bool,
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
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_be_bytes(*chunk))
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
    fn page_ocr_reasons_classify() {
        // Scanned: no text, full-page image.
        let scanned = PageAnalysis {
            has_template_image: true,
            ..Default::default()
        };
        assert_eq!(page_ocr_reasons(&scanned), vec![crate::OCR_REASON_SCANNED]);

        // Image-only page (no template flag, but has an image).
        let image_only = PageAnalysis {
            has_images: true,
            ..Default::default()
        };
        assert_eq!(
            page_ocr_reasons(&image_only),
            vec![crate::OCR_REASON_SCANNED]
        );

        // No text, no image → no_text.
        let blank = PageAnalysis::default();
        assert_eq!(page_ocr_reasons(&blank), vec![crate::OCR_REASON_NO_TEXT]);

        // Vector-outlined text.
        let vector = PageAnalysis {
            has_vector_text: true,
            ..Default::default()
        };
        assert_eq!(
            page_ocr_reasons(&vector),
            vec![crate::OCR_REASON_VECTOR_TEXT]
        );

        // Undecodable fonts → garbled, and it wins over the fall-through.
        let garbled = PageAnalysis {
            has_identity_h_no_tounicode: true,
            has_images: true,
            ..Default::default()
        };
        assert_eq!(
            page_ocr_reasons(&garbled),
            vec![crate::OCR_REASON_SUSPECTED_GARBLED_TEXT]
        );

        // A page with real extractable text and an image is not flagged here
        // as scanned/no_text (only reached for pages already needing OCR).
        let text_with_image = PageAnalysis {
            text_operator_count: 40,
            unique_text_chars: 120,
            has_images: true,
            ..Default::default()
        };
        assert_eq!(
            page_ocr_reasons(&text_with_image),
            vec![crate::OCR_REASON_SCANNED]
        );

        // A text layer nobody sees under a covering image: the specific
        // reason, not the `scanned` fall-through, and ahead of garbled
        // fonts and vector text — the page is a scan whatever its fonts.
        let invisible_layer = PageAnalysis {
            text_operator_count: 300,
            executed_text_operator_count: 300,
            invisible_text_operator_count: 300,
            unique_text_chars: 40,
            has_images: true,
            has_covering_image: true,
            has_invisible_text_layer: true,
            ..Default::default()
        };
        assert_eq!(
            page_ocr_reasons(&invisible_layer),
            vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER]
        );
        let garbled_vector_invisible_layer = PageAnalysis {
            has_identity_h_no_tounicode: true,
            has_vector_text: true,
            ..invisible_layer
        };
        assert_eq!(
            page_ocr_reasons(&garbled_vector_invisible_layer),
            vec![
                crate::OCR_REASON_INVISIBLE_TEXT_LAYER,
                crate::OCR_REASON_SUSPECTED_GARBLED_TEXT,
                crate::OCR_REASON_VECTOR_TEXT
            ]
        );
    }

    #[test]
    fn test_scan_content_operators() {
        let mut uchars = HashSet::new();

        // Sample PDF content stream with text operators
        let content = b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET";
        let (ops, imgs, _, _) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 1);
        assert_eq!(imgs, 0);
        // "Hello World" without space: H, e, l, o, W, r, d = 7 unique
        assert!(uchars.len() >= 7);

        // Content with TJ array
        uchars.clear();
        let content2 = b"BT /F1 12 Tf 100 700 Td [(H) 10 (ello)] TJ ET";
        let (ops2, _, _, _) =
            scan_content_for_text_operators(content2, &mut uchars, &mut HashSet::new());
        assert_eq!(ops2, 1);
        // H, e, l, o = 4 unique
        assert!(uchars.len() >= 4);

        // Content with Do (XObject invocation — not counted as image here;
        // actual image detection is handled by scan_xobjects_in_resources)
        uchars.clear();
        let content3 = b"q 100 0 0 100 50 700 cm /Img1 Do Q";
        let (ops3, imgs3, _, _) =
            scan_content_for_text_operators(content3, &mut uchars, &mut HashSet::new());
        assert_eq!(ops3, 0);
        assert_eq!(imgs3, 0);
    }

    #[test]
    fn test_scan_content_successive_tj_collects_each_operand() {
        // Lookback is floored at the previous Tj/TJ/Tf so later operators must
        // still see their own operands.
        let content = b"[(Hello)] TJ [(World)] TJ (More) Tj";
        let mut uchars = HashSet::new();
        let (ops, _, _, _) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 3);
        for &ch in b"HeloWrdM" {
            assert!(uchars.contains(&ch), "missing char {}", ch as char);
        }
    }

    #[test]
    fn test_scan_content_tj_inside_literal_is_not_an_operator() {
        // `Tj` followed by space inside a literal must not count as an operator
        // or pin the lookback floor; the real `Tj` still collects the string.
        let content = b"BT (Hello Tj World) Tj ET";
        let mut uchars = HashSet::new();
        let (ops, _, _, _) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 1);
        for &ch in b"HeloTjWrd" {
            assert!(uchars.contains(&ch), "missing char {}", ch as char);
        }
    }

    #[test]
    fn test_scan_content_malformed_tj_lookback_stays_linear() {
        // `] TJ` with no `[` used to walk the entire prefix for every operator
        // (quadratic). 30k repeats is enough that a prefix rescan would dominate
        // the test runtime; with the floor it is a single linear pass.
        let n = 30_000usize;
        let mut content = Vec::with_capacity(n * 5);
        for _ in 0..n {
            content.extend_from_slice(b"] TJ\n");
        }
        let mut uchars = HashSet::new();
        let (ops, _, _, _) =
            scan_content_for_text_operators(&content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, n as u32);
        assert!(uchars.is_empty());
    }

    #[test]
    fn test_image_dominated_detection() {
        // Do operators are no longer counted as images by scan_content_for_text_operators.
        // Image-dominated detection now relies on scan_xobjects_in_resources which
        // checks XObject Subtype. Here we verify that Do operators don't inflate image_count.
        let mut content = Vec::new();
        for i in 0..50 {
            content.extend_from_slice(format!("/Im{i} Do\n").as_bytes());
        }
        content.extend_from_slice(b"BT (x) Tj ET\n");
        content.extend_from_slice(b"BT (x) Tj ET\n");
        content.extend_from_slice(b"BT (x) Tj ET\n");

        let mut uchars = HashSet::new();
        let (ops, imgs, _, _) =
            scan_content_for_text_operators(&content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 3);
        assert_eq!(imgs, 0); // Do operators are not counted here
        assert_eq!(uchars.len(), 1);
    }

    #[test]
    fn test_normal_text_not_image_dominated() {
        let content = b"BT /F1 12 Tf (The quick brown fox jumps over the lazy dog) Tj ET\n\
                         /Img1 Do\n/Img2 Do\n";
        let mut uchars = HashSet::new();
        let (ops, imgs, _, _) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 1);
        assert_eq!(imgs, 0); // Do operators not counted here
                             // Many unique chars from the sentence
        assert!(uchars.len() >= 5);
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
        let (text, imgs, paths, _) =
            scan_content_for_text_operators(&content, &mut uchars, &mut HashSet::new());
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
        let (text, _, paths, _) =
            scan_content_for_text_operators(&content, &mut uchars, &mut HashSet::new());
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

    #[test]
    fn test_identity_h_with_unicode_cids_not_flagged() {
        // Type0 Identity-H font without ToUnicode but with W array CIDs
        // that look like Unicode codepoints (e.g. from Chromium/wkhtmltopdf).
        // The CID-as-Unicode passthrough can decode these — don't flag.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        // CIDFont with W array containing Unicode-range CIDs (>= 0x41)
        let cid_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"CIDFontType2".to_vec()),
            "W" => Object::Array(vec![
                Object::Integer(0x41),  // CID 65 = 'A'
                Object::Array(vec![
                    Object::Integer(600), Object::Integer(600), Object::Integer(600),
                ]),
                Object::Integer(0x61),  // CID 97 = 'a'
                Object::Array(vec![
                    Object::Integer(500), Object::Integer(500), Object::Integer(500),
                ]),
            ]),
        });
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+ArialMT".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
            "DescendantFonts" => Object::Array(vec![Object::Reference(cid_font_id)]),
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
        assert!(
            !page_has_identity_h_no_tounicode(&doc, page_id),
            "Should NOT flag: W array CIDs look like Unicode, passthrough works"
        );
    }

    #[test]
    fn test_identity_h_with_low_gid_cids_still_flagged() {
        // Type0 Identity-H font without ToUnicode and W array CIDs
        // that are low GID values (subset font, no cmap). These can't
        // be decoded — should still be flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        // CIDFont with W array containing low GID values (< 0x41)
        let cid_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"CIDFontType2".to_vec()),
            "W" => Object::Array(vec![
                Object::Integer(3),  // Low GID
                Object::Array(vec![
                    Object::Integer(600), Object::Integer(600), Object::Integer(600),
                    Object::Integer(600), Object::Integer(600),
                ]),
                Object::Integer(10),  // Still low
                Object::Array(vec![
                    Object::Integer(500), Object::Integer(500), Object::Integer(500),
                ]),
            ]),
        });
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"GPBCHP+TimesNewRoman".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
            "DescendantFonts" => Object::Array(vec![Object::Reference(cid_font_id)]),
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
        assert!(
            page_has_identity_h_no_tounicode(&doc, page_id),
            "Should flag: low GID CIDs, no cmap, no passthrough"
        );
    }

    #[test]
    fn test_scan_content_counts_tf_operators() {
        let mut uchars = HashSet::new();
        let content = b"BT /F1 12 Tf (Hello) Tj /F2 10 Tf (World) Tj ET";
        let (ops, _, _, fonts) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(ops, 2);
        assert_eq!(fonts, 2);
    }

    #[test]
    fn test_tf_without_trailing_whitespace() {
        // Some PDFs concatenate Tf directly with the next operator's operand,
        // e.g. "25 Tf[<01>..." or "25 Tf(<text>..."
        let mut uchars = HashSet::new();

        // Tf followed by '[' (TJ array start)
        let content = b"BT /F1 25 Tf[<01>1<02>-1] TJ ET";
        let (ops, _, _, fonts) =
            scan_content_for_text_operators(content, &mut uchars, &mut HashSet::new());
        assert_eq!(fonts, 1, "Tf followed by '[' should be counted");
        assert_eq!(ops, 1);

        // Tf followed by '(' (literal string)
        uchars.clear();
        let content2 = b"BT /F1 12 Tf(Hello) Tj ET";
        let (ops2, _, _, fonts2) =
            scan_content_for_text_operators(content2, &mut uchars, &mut HashSet::new());
        assert_eq!(fonts2, 1, "Tf followed by '(' should be counted");
        assert_eq!(ops2, 1);

        // Tf followed by '<' (hex string)
        uchars.clear();
        let content3 = b"BT /F1 12 Tf<0102> Tj ET";
        let (ops3, _, _, fonts3) =
            scan_content_for_text_operators(content3, &mut uchars, &mut HashSet::new());
        assert_eq!(fonts3, 1, "Tf followed by '<' should be counted");
        assert_eq!(ops3, 1);

        // Tf followed by '/' (next font name)
        uchars.clear();
        let content4 = b"BT /F1 12 Tf/F2 10 Tf (x) Tj ET";
        let (_, _, _, fonts4) =
            scan_content_for_text_operators(content4, &mut uchars, &mut HashSet::new());
        assert_eq!(fonts4, 2, "Tf followed by '/' should be counted");
    }

    #[test]
    fn test_newspaper_heuristic_thresholds() {
        // Newspaper page: high text ops, moderate font changes, low ratio
        let text_ops = 3500u32;
        let font_changes = 150u32;
        let ratio = font_changes as f32 / text_ops as f32;
        assert!(text_ops >= 1500);
        assert!(font_changes >= 50);
        assert!(ratio < 0.15); // 0.043

        // Dense styled doc (DPA/contract): high text ops, very high font changes, high ratio
        let text_ops = 1800u32;
        let font_changes = 540u32;
        let ratio = font_changes as f32 / text_ops as f32;
        assert!(text_ops >= 1500);
        assert!(font_changes >= 50);
        assert!(ratio >= 0.15); // 0.30 — should NOT trigger newspaper heuristic

        // Normal doc: low text ops — doesn't qualify at all
        let text_ops = 300u32;
        let font_changes = 50u32;
        assert!(text_ops < 1500);
    }

    #[test]
    fn test_looks_like_scan_requires_all_conditions() {
        // The looks_like_scan heuristic requires ALL three conditions (AND):
        // 1. image_count <= 1
        // 2. text_operator_count < 50
        // 3. unique_alphanum_chars < 10

        // A text page with one figure: has text ops and alphanum chars
        // Should NOT look like a scan
        let image_count = 1u32;
        let text_operator_count = 135u32;
        let unique_alphanum_chars = 58u32;
        let looks_like_scan =
            image_count <= 1 && text_operator_count < 50 && unique_alphanum_chars < 10;
        assert!(
            !looks_like_scan,
            "text page with one figure should not be flagged as scan"
        );

        // A genuine scan: single image, no real text
        let image_count = 1u32;
        let text_operator_count = 3u32;
        let unique_alphanum_chars = 2u32;
        let looks_like_scan =
            image_count <= 1 && text_operator_count < 50 && unique_alphanum_chars < 10;
        assert!(
            looks_like_scan,
            "single image with no real text should be flagged as scan"
        );

        // OCR overlay page: single image but has OCR text operators and chars
        // Should NOT look like a scan (OCR text is sufficient)
        let image_count = 1u32;
        let text_operator_count = 200u32;
        let unique_alphanum_chars = 40u32;
        let looks_like_scan =
            image_count <= 1 && text_operator_count < 50 && unique_alphanum_chars < 10;
        assert!(
            !looks_like_scan,
            "OCR overlay page should not be flagged as scan"
        );

        // Multiple images but low text: still not a scan (multiple figures page)
        let image_count = 4u32;
        let text_operator_count = 25u32;
        let unique_alphanum_chars = 1u32;
        let looks_like_scan =
            image_count <= 1 && text_operator_count < 50 && unique_alphanum_chars < 10;
        assert!(
            !looks_like_scan,
            "multiple images page should not match single-image scan pattern"
        );
    }

    // ---------- Tests for has_vector_text alphanum guard ----------

    #[test]
    fn test_has_vector_text_real_text_with_decorations_not_flagged() {
        // Newspaper-style page: high path_ops (column borders/dividers/decorations)
        // BUT also lots of selectable real text → high unique_alphanum_chars.
        // Should NOT trigger has_vector_text — the paths are decorations, not glyphs.
        let path_ops = 8354u32;
        let text_ops = 41u32;
        let unique_alphanum_chars = 53u32;
        let has_vector_text = path_ops >= 1000
            && path_ops > text_ops.saturating_mul(200)
            && unique_alphanum_chars < 30;
        assert!(
            !has_vector_text,
            "page with real selectable text alongside decorative paths should not be vector_text"
        );
    }

    #[test]
    fn test_has_vector_text_outlined_glyphs_still_flagged() {
        // True outlined-text page: massive path_ops, very few unique alphanum chars
        // (each char is a path, not a Tj op). MUST still flag as vector_text.
        let path_ops = 8000u32;
        let text_ops = 5u32;
        let unique_alphanum_chars = 4u32;
        let has_vector_text = path_ops >= 1000
            && path_ops > text_ops.saturating_mul(200)
            && unique_alphanum_chars < 30;
        assert!(
            has_vector_text,
            "true outlined-text page should still be flagged as vector_text"
        );
    }

    // ---------- Tests for page_has_identity_h_no_tounicode supplementary-font handling ----------

    #[test]
    fn test_identity_h_with_supplementary_decodable_font_not_flagged() {
        // Page has TWO fonts: an undecodable Identity-H Type0 (supplementary,
        // e.g. a decorative font for headers) AND a Type1 font with ToUnicode
        // (carries the body text). Should NOT flag — body text is decodable.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Undecodable Identity-H: no ToUnicode, no W array → no fallback.
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        // Decodable Type1 with ToUnicode: typical body-text font.
        let cmap_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            b"fake cmap".to_vec(),
        )));
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
            "ToUnicode" => Object::Reference(cmap_id),
        });

        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
                "F2" => Object::Reference(good_font_id),
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

        assert!(
            !page_has_identity_h_no_tounicode(&doc, page_id),
            "page with supplementary undecodable Identity-H but decodable Type1 should not flag"
        );
    }

    #[test]
    fn test_identity_h_with_no_other_fonts_still_flagged() {
        // Regression check: page with ONLY the undecodable Identity-H font
        // (no other decodable text font) MUST still flag for OCR.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
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
        assert!(
            page_has_identity_h_no_tounicode(&doc, page_id),
            "page with only undecodable Identity-H must still be flagged"
        );
    }

    // ---------- Tests for page_has_decodable_text_fonts ----------

    #[test]
    fn test_page_has_decodable_text_fonts_type1() {
        // Type1 font (no ToUnicode required — uses Adobe Glyph List)
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Times-Roman".to_vec()),
        });
        let resources = dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
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
        assert!(page_has_decodable_text_fonts(&doc, page_id));
    }

    #[test]
    fn test_page_has_decodable_text_fonts_type0_with_tounicode() {
        // Type0/Identity-H font with ToUnicode: CID-encoded text but decodable.
        // This is the bank-annual-report pattern.
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
            "BaseFont" => Object::Name(b"BentonSans-Bold".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
            "ToUnicode" => Object::Reference(cmap_id),
        });
        let resources = dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
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
        assert!(page_has_decodable_text_fonts(&doc, page_id));
    }

    #[test]
    fn test_page_has_decodable_text_fonts_undecodable_only_returns_false() {
        // ONLY undecodable Identity-H (no ToUnicode, no fallback).
        // Should return false — no path to recover this text.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+UnknownFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        let resources = dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
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
        assert!(!page_has_decodable_text_fonts(&doc, page_id));
    }

    // ---------- Test for the CID-aware looks_like_scan override ----------

    #[test]
    fn test_looks_like_scan_overridden_by_decodable_cid_text() {
        // CID-encoded text page (Type0 with ToUnicode) has:
        //   image_count = 1 (template image)
        //   text_operator_count = 36 (real Tj/TJ ops emitting CID values)
        //   unique_alphanum_chars = 8 (raw bytes are CIDs, not ASCII)
        //   has_decodable_text_fonts = true
        // Old check: looks_like_scan = (image<=1 && text<50 && alphanum<10) → TRUE (incorrect).
        // New check: alphanum < 10 is overridden when decodable fonts present + text_ops >= 10
        //           → looks_like_scan = false (correct — text IS decodable).
        let image_count = 1u32;
        let text_operator_count = 36u32;
        let unique_alphanum_chars = 8u32;
        let has_decodable_text_fonts = true;

        let alphanum_low =
            unique_alphanum_chars < 10 && !(has_decodable_text_fonts && text_operator_count >= 10);
        let looks_like_scan = image_count <= 1 && text_operator_count < 50 && alphanum_low;
        assert!(
            !looks_like_scan,
            "CID-encoded decodable text page should not be flagged as scan"
        );
    }

    #[test]
    fn test_looks_like_scan_keeps_flag_when_no_decodable_fonts() {
        // Same metrics as above but no decodable fonts → genuinely could be a scan.
        // Override should NOT kick in — looks_like_scan remains true.
        let image_count = 1u32;
        let text_operator_count = 36u32;
        let unique_alphanum_chars = 8u32;
        let has_decodable_text_fonts = false;

        let alphanum_low =
            unique_alphanum_chars < 10 && !(has_decodable_text_fonts && text_operator_count >= 10);
        let looks_like_scan = image_count <= 1 && text_operator_count < 50 && alphanum_low;
        assert!(
            looks_like_scan,
            "page with no decodable fonts should remain flagged as scan"
        );
    }

    #[test]
    fn test_looks_like_scan_keeps_flag_with_few_text_ops_even_if_decodable() {
        // Truly scanned page with a small overlay (1-5 text_ops, e.g. page number).
        // Has a decodable font (the page number font) but text_ops too low to
        // override. MUST still flag as scan.
        let image_count = 1u32;
        let text_operator_count = 4u32;
        let unique_alphanum_chars = 2u32;
        let has_decodable_text_fonts = true;

        let alphanum_low =
            unique_alphanum_chars < 10 && !(has_decodable_text_fonts && text_operator_count >= 10);
        let looks_like_scan = image_count <= 1 && text_operator_count < 50 && alphanum_low;
        assert!(
            looks_like_scan,
            "scanned page with small text overlay (page number) should still flag"
        );
    }

    // ---------- Tests for extract_font_name_before_tf ----------

    #[test]
    fn test_extract_font_name_basic() {
        // Standard pattern: /F1 12 Tf
        let content = b"/F1 12 Tf";
        let name = extract_font_name_before_tf(content, 6, 0); // 'T' is at index 6
        assert_eq!(name, Some(b"F1".to_vec()));
    }

    #[test]
    fn test_extract_font_name_long_name() {
        let content = b"/ArialMT-Bold 9.5 Tf";
        let name = extract_font_name_before_tf(content, 18, 0);
        assert_eq!(name, Some(b"ArialMT-Bold".to_vec()));
    }

    #[test]
    fn test_scan_content_collects_used_font_names() {
        let mut uchars = HashSet::new();
        let mut fonts = HashSet::new();
        let content = b"BT /F1 12 Tf (Hello) Tj /F2 10 Tf (World) Tj ET";
        scan_content_for_text_operators(content, &mut uchars, &mut fonts);
        assert!(fonts.contains(&b"F1".to_vec()), "should collect F1");
        assert!(fonts.contains(&b"F2".to_vec()), "should collect F2");
        assert_eq!(fonts.len(), 2);
    }

    // ---------- P1 tests: usage-based font filtering ----------

    #[test]
    fn test_p1_unused_decodable_font_does_not_save_undecodable_page() {
        // P1 bug scenario: page Resources has TWO fonts:
        // - /F1: undecodable Identity-H (used in content stream)
        // - /F2: decodable Type1 (NOT used in content stream — leftover/inherited)
        //
        // Old resource-based check: sees F2 decodable → wrongly unflagged.
        // New usage-based check: only F1 is used → correctly flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // F1: undecodable Identity-H (no ToUnicode, no fallback)
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        // F2: decodable Type1 (unused — leftover in Resources)
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        // Content stream only uses /F1
        let content_data = b"BT /F1 12 Tf <0102030405> Tj ET";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
                "F2" => Object::Reference(good_font_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_identity_h_no_tounicode,
            "P1: page using only undecodable Identity-H should be flagged, even though \
             Resources also contains unused decodable Type1"
        );
        // Verify the old resource-based check would have been WRONG (the bug we're fixing)
        assert!(
            !page_has_identity_h_no_tounicode(&doc, page_id),
            "sanity: old resource-based check incorrectly sees unused decodable font"
        );
    }

    #[test]
    fn test_p1_used_decodable_font_still_unflagged() {
        // Counterpart: both fonts ARE used in content → decodable font saves the page.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        // Content stream uses BOTH /F1 and /F2
        let content_data = b"BT /F1 12 Tf <0102> Tj /F2 10 Tf (Hello world) Tj ET";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
                "F2" => Object::Reference(good_font_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "page using both undecodable and decodable fonts should NOT be flagged"
        );
    }

    // ---------- masthead-over-scan tests: template image + sparse chrome ----------

    /// Builds a page whose only image is a full-page scan inside a Form
    /// XObject, plus `masthead_ops` native text-show ops of diverse,
    /// decodable chrome (newspaper masthead / date line style).
    fn masthead_scan_page(masthead_lines: &[&str]) -> (Document, ObjectId) {
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Image".to_vec()),
                "Width" => Object::Integer(1500),
                "Height" => Object::Integer(2383),
            },
            Vec::new(),
        )));
        let form_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "XObject" => dictionary! {
                        "Im0" => Object::Reference(image_id),
                    },
                },
            },
            b"1500 0 0 2383 0 0 cm /Im0 Do".to_vec(),
        )));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        let mut content = b"q /Fm0 Do Q BT /F1 12 Tf ".to_vec();
        for line in masthead_lines {
            content.extend_from_slice(format!("({line}) Tj ").as_bytes());
        }
        content.extend_from_slice(b"ET");
        let content_id =
            doc.add_object(Object::Stream(lopdf::Stream::new(dictionary! {}, content)));

        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), 1500.into(), 2383.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                    "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) },
                },
                "Contents" => Object::Reference(content_id),
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
        (doc, page_id)
    }

    #[test]
    fn test_masthead_over_form_wrapped_scan_needs_ocr() {
        // A full-page scan wrapped in a Form XObject with ~4 ops of real,
        // diverse masthead text. `alphanum_low` can't flag it (the chrome is
        // genuine text), so the sparse-text floor must: without OCR the page
        // body is silently lost while classification calls the page scanned.
        let (doc, page_id) = masthead_scan_page(&[
            "18",
            "FINANCIAL EXPRESS",
            "WWW.FINANCIALEXPRESS.COM",
            "FRIDAY, DECEMBER 13, 2024",
        ]);
        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_template_image,
            "sanity: full-page image inside the form must be found"
        );
        assert!(
            analysis.unique_alphanum_chars >= 10,
            "sanity: masthead text is diverse, alphanum_low cannot fire"
        );
        let needs_ocr = page_ocr_signals(&doc, page_id).template_image_needs_ocr;
        assert!(
            needs_ocr,
            "template image + text below the pages_with_text floor is a scan"
        );
    }

    #[test]
    fn test_text_page_over_background_image_stays_native() {
        // Counterpart: a real text page over a full-page background image
        // (letterhead/watermark) has enough text ops to clear the
        // `pages_with_text` floor and must NOT be routed to OCR.
        let lines: Vec<String> = (0..12)
            .map(|i| format!("Paragraph line {i} with ordinary body text"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (doc, page_id) = masthead_scan_page(&refs);
        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_template_image,
            "sanity: background image found"
        );
        assert!(
            analysis.text_operator_count >= 10,
            "sanity: body text clears the floor"
        );
        let needs_ocr = page_ocr_signals(&doc, page_id).template_image_needs_ocr;
        assert!(
            !needs_ocr,
            "a text page with a background image must stay native"
        );
    }

    // ---------- Text render mode and covering images ----------

    /// A scan of `content` on its own — no page, nothing followed through
    /// `Do` — as its counts and executed tallies: (counts, text ops,
    /// hidden text ops).
    fn scan_alone(content: &[u8]) -> (ContentCounts, u32, u32) {
        let doc = Document::new();
        let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
        let counts = scan_content_stream(
            content,
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut state,
            &[],
        );
        (
            counts,
            state.executed_text_ops,
            state.executed_hidden_text_ops,
        )
    }

    #[test]
    fn render_mode_splits_hidden_text_ops() {
        let content = b"BT /F1 12 Tf 3 Tr (a) Tj 0 Tr (b) Tj 7 Tr [(c)] TJ 1 Tr (d) Tj ET";
        let (counts, executed, hidden) = scan_alone(content);
        assert_eq!(counts.text_ops, 4);
        assert_eq!(executed, 4);
        assert_eq!(hidden, 2, "modes 3 and 7 paint nothing");
    }

    #[test]
    fn render_mode_follows_q_and_capital_q() {
        let doc = Document::new();
        let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
        // `Q` restores the mode `q` saved; a `Q` with nothing saved changes
        // nothing; a nested save and restore keeps the outer mode.
        let content = b"Q q 3 Tr (a) Tj Q (b) Tj q q 3 Tr Q (c) Tj Q (d) Tj";
        scan_content_stream(
            content,
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut state,
            &[],
        );
        assert_eq!(state.executed_text_ops, 4);
        assert_eq!(state.executed_hidden_text_ops, 1);
        assert_eq!(state.render_mode, 0);
    }

    #[test]
    fn render_mode_carries_across_a_page_s_content_streams() {
        let doc = Document::new();
        let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
        let mut scan = |content: &[u8]| {
            scan_content_stream(
                content,
                &mut HashSet::new(),
                &mut HashSet::new(),
                &mut state,
                &[],
            )
        };
        assert_eq!(scan(b"q 3 Tr").text_ops, 0);
        assert_eq!(scan(b"BT (a) Tj ET Q BT (b) Tj ET").text_ops, 2);
        assert_eq!(state.executed_hidden_text_ops, 1);
    }

    #[test]
    fn strings_comments_and_inline_image_data_hold_no_operators() {
        // Text saying `3 Tr`, a comment saying it, operators spelled inside
        // strings and inline image data: none of them is an operator, and
        // the text is still read.
        let content = b"BT /F1 12 Tf (3 Tr) Tj % 3 Tr\n(a m b c d f) Tj (/F2 9 Tf) Tj \
                        [(x] TJ)] TJ <3320547220> Tj ET % q\n\
                        BI /W 1 /H 1 /BPC 8 /CS /G ID q 3 Tr Q EI BT (e) Tj ET";
        let mut unique_chars = HashSet::new();
        let mut fonts = HashSet::new();
        let doc = Document::new();
        let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
        let counts = scan_content_stream(content, &mut unique_chars, &mut fonts, &mut state, &[]);
        assert_eq!(counts.text_ops, 6);
        assert_eq!(state.executed_text_ops, 6);
        assert_eq!(state.executed_hidden_text_ops, 0);
        assert_eq!(counts.path_ops, 0);
        assert_eq!(counts.font_changes, 1);
        assert_eq!(fonts.len(), 1);
        for &ch in b"3Tramdxe" {
            assert!(
                unique_chars.contains(&ch),
                "the text is still read: {}",
                ch as char
            );
        }
    }

    #[test]
    fn quote_show_text_operators_are_counted_and_follow_the_render_mode() {
        // `'` and `"` show text as `Tj` does; an apostrophe inside a
        // string is not an operator.
        let hidden = b"BT /F1 10 Tf 3 Tr 12 TL 72 720 Td (a) ' (b) ' 1 0 (c) \" (don't) Tj ET";
        let (counts, executed_ops, hidden_ops) = scan_alone(hidden);
        assert_eq!((counts.text_ops, executed_ops, hidden_ops), (4, 4, 4));

        let visible = b"BT /F1 10 Tf 12 TL 72 720 Td (a) ' (b) ' 1 0 (c) \" ET";
        let (counts, executed_ops, hidden_ops) = scan_alone(visible);
        assert_eq!((counts.text_ops, executed_ops, hidden_ops), (3, 3, 0));

        // Their strings are read like `Tj`'s.
        let mut unique_chars = HashSet::new();
        let doc = Document::new();
        let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
        scan_content_stream(
            visible,
            &mut unique_chars,
            &mut HashSet::new(),
            &mut state,
            &[],
        );
        for &ch in b"abc" {
            assert!(unique_chars.contains(&ch), "{}", ch as char);
        }
    }

    #[test]
    fn id_begins_image_data_only_inside_an_inline_image() {
        // `/ID` as a name, and a bare `ID` with no inline image open, are
        // left alone; the `ID` of a `BI` still hides its data through `EI`.
        let content = b"/Span <</ID 7 /MCID 0>> BDC BT /F1 12 Tf (a) Tj ET EMC \
                        ID BT (b) Tj ET \
                        BI /W 1 /H 1 /BPC 8 /CS /G ID q 3 Tr Q EI BT (c) Tj ET";
        let (counts, executed_ops, hidden) = scan_alone(content);
        assert_eq!((counts.text_ops, executed_ops, hidden), (3, 3, 0));
    }

    #[test]
    fn render_mode_out_of_range_is_not_set() {
        let (counts, _, hidden) = scan_alone(b"9 Tr (a) Tj 3.5 Tr (b) Tj");
        assert_eq!(counts.text_ops, 2);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn saved_states_past_the_depth_cap_restore_nothing() {
        let mut content = Vec::new();
        for _ in 0..=SCAN_STATE_MAX_DEPTH {
            content.extend_from_slice(b"q ");
        }
        content.extend_from_slice(b"3 Tr ");
        for _ in 0..=SCAN_STATE_MAX_DEPTH {
            content.extend_from_slice(b"Q ");
        }
        content.extend_from_slice(b"(a) Tj");
        let (counts, _, hidden) = scan_alone(&content);
        assert_eq!(counts.text_ops, 1);
        assert_eq!(hidden, 0, "the outermost `Q` restores mode 0");
    }

    /// A one-page 612×792 document whose content stream is set with
    /// [`set_page_content`]: a 2×2 gray image `Im0` when `image`; a
    /// 1500×2383 image `ImBig`, bound whether or not the content draws it,
    /// when `large_image`; and the given forms by name — each with the
    /// page's font as `F1`, a page-sized `/BBox` and the `/Matrix` given —
    /// bound whether or not the content invokes them.
    fn synthetic_page(
        image: bool,
        large_image: bool,
        forms: &[(&str, &str, Option<[i64; 6]>)],
    ) -> (Document, ObjectId, ObjectId) {
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        let mut xobjects = dictionary! {};
        let mut add_image =
            |doc: &mut Document, name: &str, width: i64, height: i64, data: Vec<u8>| {
                let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
                    dictionary! {
                        "Type" => "XObject",
                        "Subtype" => Object::Name(b"Image".to_vec()),
                        "Width" => Object::Integer(width),
                        "Height" => Object::Integer(height),
                        "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                        "BitsPerComponent" => Object::Integer(8),
                    },
                    data,
                )));
                xobjects.set(name, Object::Reference(image_id));
            };
        if image {
            add_image(&mut doc, "Im0", 2, 2, vec![200, 60, 60, 200]);
        }
        if large_image {
            add_image(&mut doc, "ImBig", 1500, 2383, Vec::new());
        }
        for &(name, content, matrix) in forms {
            let mut dict = dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                },
            };
            if let Some(matrix) = matrix {
                dict.set(
                    "Matrix",
                    matrix
                        .iter()
                        .map(|&value| Object::Integer(value))
                        .collect::<Vec<_>>(),
                );
            }
            let form_id = doc.add_object(Object::Stream(lopdf::Stream::new(
                dict,
                content.as_bytes().to_vec(),
            )));
            xobjects.set(name, Object::Reference(form_id));
        }
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            Vec::new(),
        )));

        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                    "XObject" => xobjects,
                },
                "Contents" => Object::Reference(content_id),
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
        (doc, page_id, content_id)
    }

    fn set_page_content(doc: &mut Document, content_id: ObjectId, content: &str) {
        doc.objects.insert(
            content_id,
            Object::Stream(lopdf::Stream::new(
                lopdf::dictionary! {},
                content.as_bytes().to_vec(),
            )),
        );
    }

    /// The executed tallies of `streams` run, in order, as the page's
    /// content: (text ops, hidden text ops, covered image area).
    fn executed(doc: &Document, page_id: ObjectId, streams: &[&str]) -> (u32, u32, f64) {
        let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
        let (own, ancestors) = doc.get_page_resources(page_id).unwrap();
        let resources: Vec<&lopdf::Dictionary> = own
            .into_iter()
            .chain(
                ancestors
                    .iter()
                    .filter_map(|id| doc.get_dictionary(*id).ok()),
            )
            .collect();
        let mut state = ContentScanState::new(doc, page_box, true);
        for stream in streams {
            scan_content_stream(
                stream.as_bytes(),
                &mut HashSet::new(),
                &mut HashSet::new(),
                &mut state,
                &resources,
            );
        }
        (
            state.executed_text_ops,
            state.executed_hidden_text_ops,
            state.covered_image_area(),
        )
    }

    const PAGE_AREA: f64 = 612.0 * 792.0;
    const FULL_PAGE_IMAGE: &str = "q 612 0 0 792 0 0 cm /Im0 Do Q\n";

    /// 120 one-glyph `Tj` blocks under `mode`, as a producer writes a text
    /// layer.
    fn glyph_layer(mode: u8) -> String {
        let mut layer = format!("{mode} Tr\n");
        let glyphs = "thepagecarriesalayernobodysees".chars().cycle().take(120);
        for (n, glyph) in glyphs.enumerate() {
            let x = 72 + (n % 40) * 12;
            let y = 720 - (n / 40) * 14;
            layer.push_str(&format!(
                "BT 1 0 0 1 {x} {y} Tm /F1 10 Tf ({glyph}) Tj ET\n"
            ));
        }
        layer
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn clip_only_text_painted_through_is_visible() {
        let (doc, page_id, _) = synthetic_page(true, false, &[]);
        // A title filled with an image: the glyphs clip the image painted
        // through them, so they are visible. The image under the page,
        // drawn before the text, shows nothing of it.
        let (executed_ops, hidden, covered) = executed(
            &doc,
            page_id,
            &["q 612 0 0 792 0 0 cm /Im0 Do Q \
               q BT 7 Tr (L) Tj (O) Tj ET q 612 0 0 792 0 0 cm /Im0 Do Q Q"],
        );
        assert_eq!((executed_ops, hidden), (2, 0));
        assert!(close(covered, PAGE_AREA));

        // Shadings, inline images, path painting and visible text show it
        // too; mode 3 is never shown, whatever is painted after it.
        for painting in [
            "sh",
            "BI /W 1 /H 1 ID x EI",
            "0 0 1 1 re f",
            "0 0 m 1 1 l S",
            "b",
            "BT 0 Tr (v) Tj ET",
        ] {
            let content = format!("q BT 7 Tr (a) Tj ET {painting} Q BT 3 Tr (b) Tj ET {painting}");
            let (executed_ops, hidden, _) = executed(&doc, page_id, &[&content]);
            let visible_text = if painting.contains("Tj") { 2 } else { 0 };
            assert_eq!(executed_ops, 2 + visible_text, "{painting}");
            assert_eq!(hidden, 1, "{painting}");
        }

        // A clip whose level closes unpainted hides its text for good: the
        // image painted afterwards, in a later stream, is outside it.
        let (executed_ops, hidden, _) = executed(
            &doc,
            page_id,
            &[
                "q BT 7 Tr (a) Tj ET Q BT 7 Tr (b) Tj ET",
                "q 612 0 0 792 0 0 cm /Im0 Do Q",
            ],
        );
        assert_eq!(
            (executed_ops, hidden),
            (2, 1),
            "(b) was painted through, (a) was not"
        );
    }

    #[test]
    fn covered_image_area_follows_the_matrix_and_the_page() {
        let (doc, page_id, _) = synthetic_page(true, false, &[]);
        let covered = |content: &str| executed(&doc, page_id, &[content]).2;
        assert!(close(covered("q 612 0 0 792 0 0 cm /Im0 Do Q"), PAGE_AREA));
        // Turned, its box runs past the right edge: only what lies on the
        // page counts.
        assert!(close(
            covered("q 0 612 -792 0 792 0 cm /Im0 Do Q"),
            612.0 * 612.0
        ));
        // Scaled under nested q/Q.
        assert!(close(
            covered("q 2 0 0 2 0 0 cm q 100 0 0 50 0 0 cm /Im0 Do Q Q"),
            20_000.0
        ));
        // Shifted mostly off the page, or a little.
        assert!(close(
            covered("q 612 0 0 792 500 0 cm /Im0 Do Q"),
            112.0 * 792.0
        ));
        assert!(close(
            covered("q 612 0 0 792 -100 0 cm /Im0 Do Q"),
            512.0 * 792.0
        ));
        // Drawn twice, or as two strips: the page, once.
        assert!(close(
            covered("q 612 0 0 792 0 0 cm /Im0 Do Q q 612 0 0 792 0 0 cm /Im0 Do Q"),
            PAGE_AREA
        ));
        assert!(close(
            covered("q 612 0 0 396 0 0 cm /Im0 Do Q q 612 0 0 396 0 396 cm /Im0 Do Q"),
            PAGE_AREA
        ));
        // A name bound to nothing draws nothing.
        assert!(close(covered("q 612 0 0 792 0 0 cm /Im9 Do Q"), 0.0));
    }

    #[test]
    fn forms_are_run_in_place_under_the_state_in_force() {
        let (doc, page_id, _) = synthetic_page(
            true,
            false,
            &[
                ("FmEmpty", "", None),
                ("FmHidden", "3 Tr BT /F1 10 Tf 72 700 Td (b) Tj ET", None),
                ("FmImage", "q 612 0 0 792 0 0 cm /Im0 Do Q", None),
                ("FmText", "BT /F1 10 Tf 0 Tr 72 700 Td (c) Tj ET", None),
                ("FmPlain", "BT /F1 10 Tf 72 700 Td (c) Tj ET", None),
                (
                    "FmScaled",
                    "306 0 0 396 0 0 cm /Im0 Do",
                    Some([2, 0, 0, 2, 0, 0]),
                ),
                ("FmUnbalanced", "Q Q 3 Tr BT /F1 10 Tf (a) Tj ET q q", None),
            ],
        );
        let run = |content: &str| executed(&doc, page_id, &[content]);

        // A form invoked inside a clip-only text's level shows it through
        // only when it paints: an empty form or an invisible-only form
        // does not; an image or visible text inside one does.
        let clipped = |form: &str| run(&format!("q BT /F1 10 Tf 7 Tr (a) Tj ET /{form} Do Q"));
        let (executed_ops, hidden, _) = clipped("FmEmpty");
        assert_eq!((executed_ops, hidden), (1, 1));
        let (executed_ops, hidden, _) = clipped("FmHidden");
        assert_eq!((executed_ops, hidden), (2, 2));
        let (executed_ops, hidden, covered) = clipped("FmImage");
        assert_eq!((executed_ops, hidden), (1, 0));
        assert!(
            close(covered, PAGE_AREA),
            "the image resolves in the invoker's resources"
        );
        let (executed_ops, hidden, _) = clipped("FmText");
        assert_eq!((executed_ops, hidden), (2, 0));

        // A form inherits the render mode in force, unless it sets its own.
        let (executed_ops, hidden, _) = run("3 Tr /FmPlain Do");
        assert_eq!((executed_ops, hidden), (1, 1));
        let (executed_ops, hidden, _) = run("3 Tr /FmText Do");
        assert_eq!((executed_ops, hidden), (1, 0));

        // A form's `/Matrix` scales what it draws.
        assert!(close(run("/FmScaled Do").2, PAGE_AREA));

        // A form's `Q`s cannot close its invoker's levels, and the levels
        // it leaves open close with it.
        let (executed_ops, hidden, _) = run("q 3 Tr /FmUnbalanced Do Q BT /F1 10 Tf (d) Tj ET");
        assert_eq!((executed_ops, hidden), (2, 1));

        // A form is run once.
        let (executed_ops, hidden, _) = run("/FmText Do /FmText Do");
        assert_eq!((executed_ops, hidden), (1, 0));
    }

    #[test]
    fn resources_merely_bound_are_not_content() {
        let hidden_layer = "3 Tr BT /F1 10 Tf 72 700 Td (a) Tj (b) Tj ET";
        let (mut doc, page_id, content_id) =
            synthetic_page(false, true, &[("FmHidden", hidden_layer, None)]);

        // Visible text on a page that binds, without using them, a large
        // image and a form holding a hidden layer.
        set_page_content(
            &mut doc,
            content_id,
            "BT /F1 12 Tf 72 720 Td (Plain visible text) Tj 0 -14 Td (on an ordinary page) Tj ET",
        );
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(
            analysis.text_operator_count, 4,
            "the tally over every form bound is as it was"
        );
        assert!(
            analysis.has_template_image,
            "sanity: the bound image has the pixels of a template image"
        );
        assert_eq!(analysis.executed_text_operator_count, 2);
        assert_eq!(analysis.invisible_text_operator_count, 0);
        assert!(!analysis.has_covering_image);
        assert!(!analysis.has_invisible_text_layer);
        assert!(!page_ocr_signals(&doc, page_id).has_invisible_text_layer);

        // The same image drawn over the page and the same form invoked,
        // and nothing else: a layer nobody sees.
        set_page_content(
            &mut doc,
            content_id,
            "q 612 0 0 792 0 0 cm /ImBig Do Q /FmHidden Do",
        );
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.executed_text_operator_count, 2);
        assert_eq!(analysis.invisible_text_operator_count, 2);
        assert!(analysis.has_covering_image);
        assert!(analysis.has_invisible_text_layer);
        assert!(page_ocr_signals(&doc, page_id).has_invisible_text_layer);
    }

    #[test]
    fn an_image_drawn_off_the_page_does_not_cover_it() {
        let (mut doc, page_id, content_id) = synthetic_page(true, false, &[]);
        let layer = glyph_layer(3);
        set_page_content(
            &mut doc,
            content_id,
            &format!("q 612 0 0 792 500 0 cm /Im0 Do Q\n{layer}"),
        );
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(
            !analysis.has_covering_image,
            "less than a fifth of the image lies on the page"
        );
        assert!(!analysis.has_invisible_text_layer);

        set_page_content(
            &mut doc,
            content_id,
            &format!("q 612 0 0 792 -100 0 cm /Im0 Do Q\n{layer}"),
        );
        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_covering_image,
            "five sixths of the image lies on the page"
        );
        assert!(analysis.has_invisible_text_layer);
    }

    /// A one-page document: a 2×2 gray image drawn over the whole page when
    /// `covering_image`; a layer of 120 one-glyph `Tj` blocks under
    /// `layer_mode` when given, in the page's content or, when
    /// `layer_in_form`, in a Form XObject the page invokes; and a visible
    /// caption line when given.
    fn layered_scan_page(
        covering_image: bool,
        layer_mode: Option<u8>,
        layer_in_form: bool,
        caption: Option<&str>,
    ) -> (Document, ObjectId) {
        let layer = layer_mode.map(glyph_layer).unwrap_or_default();
        let forms: Vec<(&str, &str, Option<[i64; 6]>)> = if layer_in_form {
            vec![("Fm0", layer.as_str(), None)]
        } else {
            Vec::new()
        };
        let (mut doc, page_id, content_id) = synthetic_page(covering_image, false, &forms);
        let mut content = String::new();
        if covering_image {
            content.push_str(FULL_PAGE_IMAGE);
        }
        if layer_in_form {
            content.push_str("/Fm0 Do\n");
        } else {
            content.push_str(&layer);
        }
        if let Some(caption) = caption {
            content.push_str(&format!("BT 0 Tr /F1 12 Tf 72 40 Td ({caption}) Tj ET\n"));
        }
        set_page_content(&mut doc, content_id, &content);
        (doc, page_id)
    }

    #[test]
    fn invisible_layer_under_a_covering_image_is_flagged() {
        let (doc, page_id) = layered_scan_page(true, Some(3), false, None);
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.text_operator_count, 120);
        assert_eq!(analysis.executed_text_operator_count, 120);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(
            analysis.has_covering_image,
            "a 2×2 image scaled to the page covers it"
        );
        assert!(
            !analysis.has_template_image,
            "sanity: four pixels are no template image"
        );
        assert!(analysis.has_invisible_text_layer);
        assert_eq!(
            page_ocr_reasons(&analysis),
            vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER]
        );
        let signals = page_ocr_signals(&doc, page_id);
        assert!(signals.has_invisible_text_layer);
        assert!(!signals.template_image_needs_ocr);
        assert!(!signals.has_vector_text);

        // Mode 7 (clip only) with nothing painted through it paints
        // nothing either.
        let (doc, page_id) = layered_scan_page(true, Some(7), false, None);
        assert!(analyze_page_content(&doc, page_id).has_invisible_text_layer);
    }

    #[test]
    fn invisible_layer_inside_a_form_xobject_is_followed() {
        let (doc, page_id) = layered_scan_page(true, Some(3), true, None);
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.text_operator_count, 120);
        assert_eq!(analysis.executed_text_operator_count, 120);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(analysis.has_invisible_text_layer);

        // The form's mode does not reach a caption the page paints itself.
        let (doc, page_id) = layered_scan_page(true, Some(3), true, Some("Figure 1"));
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.executed_text_operator_count, 121);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(!analysis.has_invisible_text_layer);
    }

    #[test]
    fn painted_layer_caption_no_image_or_image_alone_is_not_an_invisible_layer() {
        // The same layer painted (mode 0) is a text page over an image.
        let (doc, page_id) = layered_scan_page(true, Some(0), false, None);
        let analysis = analyze_page_content(&doc, page_id);
        assert!(analysis.has_covering_image);
        assert_eq!(analysis.invisible_text_operator_count, 0);
        assert!(!analysis.has_invisible_text_layer);
        assert!(!page_ocr_signals(&doc, page_id).has_invisible_text_layer);

        // One visible caption over the image: the page shows text of its own.
        let (doc, page_id) = layered_scan_page(true, Some(3), false, Some("Figure 1"));
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.executed_text_operator_count, 121);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(!analysis.has_invisible_text_layer);

        // Invisible text with no image under it is not a scan.
        let (doc, page_id) = layered_scan_page(false, Some(3), false, None);
        let analysis = analyze_page_content(&doc, page_id);
        assert_eq!(analysis.invisible_text_operator_count, 120);
        assert!(!analysis.has_covering_image);
        assert!(!analysis.has_invisible_text_layer);

        // An image alone is a scan with no text layer at all.
        let (doc, page_id) = layered_scan_page(true, None, false, None);
        let analysis = analyze_page_content(&doc, page_id);
        assert!(analysis.has_covering_image);
        assert_eq!(analysis.executed_text_operator_count, 0);
        assert!(!analysis.has_invisible_text_layer);
        assert_eq!(page_ocr_reasons(&analysis), vec![crate::OCR_REASON_SCANNED]);
    }

    // ---------- P2 tests: Form XObject font traversal ----------

    #[test]
    fn test_p2_decodable_font_in_xobject_unflagged() {
        // P2 scenario: page-level Resources has only undecodable Identity-H (/F1),
        // but a Form XObject's Resources has a decodable Type1 font (/F2).
        // Content stream uses /F1 at page level, and the XObject uses /F2.
        // The page should NOT be flagged because text IS decodable (in XObject).
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // F1: undecodable Identity-H at page level
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        // F2: decodable Type1 in XObject
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        // Form XObject: uses /F2 for decodable text
        let xobj_content = b"BT /F2 10 Tf (Hello from XObject) Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F2" => Object::Reference(good_font_id),
                    },
                },
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        // Page content: uses /F1 and invokes the XObject
        let content_data = b"BT /F1 12 Tf <0102> Tj ET /XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
            },
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P2: page with decodable font in Form XObject should NOT be flagged — \
             the XObject has decodable text"
        );
        assert!(
            analysis.has_decodable_text_fonts,
            "P2: should detect decodable fonts from Form XObject Resources"
        );
    }

    #[test]
    fn test_p2_undecodable_font_only_in_xobject_flagged() {
        // P2 negative test: page-level Resources has decodable Type1 (/F1),
        // but only the Form XObject uses text (with undecodable Identity-H /F2).
        // Content stream uses ONLY /F2 (via XObject). Should flag.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // F1: decodable Type1 at page level (NOT used by content)
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        // F2: undecodable Identity-H in XObject
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+Cosmos-Medium".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        // Form XObject: uses /F2 (undecodable)
        let xobj_content = b"BT /F2 10 Tf <0102030405> Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F2" => Object::Reference(bad_font_id),
                    },
                },
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        // Page content: only invokes XObject (no direct Tf at page level)
        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(good_font_id),
            },
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_identity_h_no_tounicode,
            "P2 negative: only used font is undecodable (in XObject) — must flag, \
             even though page-level Resources has an unused decodable Type1"
        );
    }

    #[test]
    fn test_p2_decodable_fonts_detected_from_xobject() {
        // P2: has_decodable_text_fonts should be true when the only decodable font
        // is inside a Form XObject's Resources (not at page level).
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // F1: decodable Type1, only in XObject
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        let xobj_content = b"BT /F1 10 Tf (Hello) Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(good_font_id),
                    },
                },
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_decodable_text_fonts,
            "P2: decodable font from Form XObject should be detected"
        );
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P2: no Identity-H font used, should not flag"
        );
    }

    // ---------- P1 regression: font name collisions across resource scopes ----------

    #[test]
    fn test_p1_name_collision_xobject_decodable_page_undecodable() {
        // P1 bug scenario: Page Resources has /F1 -> undecodable Identity-H.
        // Form XObject Resources has /F1 -> decodable Type1. DIFFERENT font, same name.
        // Only the XObject's content uses /F1.
        //
        // Without fix: global name-keyed font_map has page's undecodable /F1;
        //   XObject's /F1 is skipped (contains_key). Lookup resolves to the WRONG font
        //   -> page wrongly flagged.
        // With fix (ObjectId-based): XObject's Tf resolves /F1 against XObject's own
        //   Resources, yielding the decodable Type1's ObjectId -> correctly NOT flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Page-level /F1: undecodable Identity-H (no ToUnicode, no fallback)
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+BadFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        // XObject-level /F1: decodable Type1 — DIFFERENT underlying font, same /F1 name
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        // Form XObject: its own Resources define /F1 -> good_font_id
        let xobj_content = b"BT /F1 10 Tf (Hello from XObject) Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(good_font_id),
                    },
                },
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        // Page content: only invokes the XObject, no direct text
        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
            },
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P1 name collision: XObject uses /F1 which resolves to decodable Type1 \
             in XObject scope — should NOT flag even though page's /F1 is undecodable"
        );
        assert!(
            analysis.has_decodable_text_fonts,
            "P1 name collision: XObject's /F1 is decodable Type1"
        );
    }

    #[test]
    fn test_p1_name_collision_xobject_undecodable_page_decodable() {
        // Inverse P1 scenario: Page Resources has /F1 -> decodable Type1.
        // Form XObject Resources has /F1 -> undecodable Identity-H.
        // Only the XObject's content uses /F1.
        //
        // Without fix: global font_map has page's decodable /F1; XObject's /F1
        //   skipped. Lookup resolves to page's decodable font -> wrongly unflagged.
        // With fix: XObject's Tf resolves /F1 against XObject Resources, gets the
        //   undecodable Identity-H ObjectId -> correctly flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Page-level /F1: decodable Type1
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        // XObject-level /F1: undecodable Identity-H — DIFFERENT font, same name
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"XYZDEF+BadCIDFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        // Form XObject: its own Resources define /F1 -> bad_font_id
        let xobj_content = b"BT /F1 10 Tf <0102030405> Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(bad_font_id),
                    },
                },
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        // Page content: only invokes the XObject
        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(good_font_id),
            },
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_identity_h_no_tounicode,
            "P1 inverse: XObject uses /F1 which resolves to undecodable Identity-H \
             in XObject scope — MUST flag even though page's /F1 is decodable Type1"
        );
    }

    // ---------- P2 regression: indirect Form XObject Resources ----------

    #[test]
    fn test_p2_indirect_xobject_resources() {
        // P2 bug: Form XObject's /Resources stored as an indirect reference (X 0 R)
        // instead of an inline dictionary. The old code used as_dict() which returns
        // None for indirect refs, causing the entire Resources branch to be skipped.
        //
        // With fix: we also handle Object::Reference by resolving it.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Font inside the XObject — decodable Type1
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        // Store the XObject's Resources as a separate indirect object
        let xobj_resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(font_id),
            },
        }));

        // Form XObject: /Resources is an indirect reference (the bug trigger)
        let xobj_content = b"BT /F1 10 Tf (Hello) Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => Object::Reference(xobj_resources_id),
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_decodable_text_fonts,
            "P2 indirect: decodable font behind indirect /Resources must be discovered"
        );
        assert_eq!(
            analysis.text_operator_count, 1,
            "P2 indirect: text ops from XObject content should be counted"
        );
    }

    // ---------- P1 + P2 combined: indirect Resources with name collisions ----------

    #[test]
    fn test_p1_p2_combined_indirect_resources_with_name_collision() {
        // Combined scenario: Page has /F1 -> undecodable Identity-H.
        // Form XObject has /F1 -> decodable Type1 stored via INDIRECT /Resources.
        // XObject content uses /F1 which should resolve to the decodable one.
        //
        // This tests both bugs simultaneously:
        // P1: name collision (/F1 means different fonts in different scopes)
        // P2: XObject Resources is an indirect reference
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Page-level /F1: undecodable
        let bad_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+BadFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        // XObject-level /F1: decodable Type1 — different underlying font
        let good_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"TimesNewRoman".to_vec()),
        });

        // XObject Resources as an indirect reference (P2)
        let xobj_resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(good_font_id),
            },
        }));

        let xobj_content = b"BT /F1 12 Tf (Decodable text in XObject) Tj ET";
        let xobj_stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => Object::Reference(xobj_resources_id),
            },
            xobj_content.to_vec(),
        );
        let xobj_id = doc.add_object(Object::Stream(xobj_stream));

        let content_data = b"/XF1 Do";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(bad_font_id),
            },
            "XObject" => dictionary! {
                "XF1" => Object::Reference(xobj_id),
            },
        };
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
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

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P1+P2 combined: XObject /F1 resolves to decodable Type1 via indirect \
             Resources — should NOT flag despite page /F1 being undecodable"
        );
        assert!(
            analysis.has_decodable_text_fonts,
            "P1+P2 combined: should detect decodable font from indirect XObject Resources"
        );
    }

    // ---------- P3 tests: resource inheritance shadowing ----------

    #[test]
    fn test_p3_page_overrides_parent_font_undecodable_shadows_decodable() {
        // Page tree: /Pages root has /Resources with /F1 → decodable Type1.
        // Page itself has /Resources with /F1 → undecodable Identity-H.
        // Content uses /F1. The page's /F1 shadows the parent's /F1.
        // Expectation: MUST be flagged (only the undecodable font is "used").
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Parent's /F1: decodable Type1 (SHADOWED — should NOT be in used set)
        let parent_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        let parent_resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(parent_font_id),
            },
        }));

        // Page's /F1: undecodable Identity-H (no ToUnicode, no fallback)
        let page_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+BadFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });

        let content_data = b"BT /F1 12 Tf <0102030405> Tj ET";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));

        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(page_font_id),
                    },
                },
                "Contents" => Object::Reference(content_id),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
                "Resources" => Object::Reference(parent_resources_id),
            }),
        );

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.has_identity_h_no_tounicode,
            "P3: page /F1 (undecodable) shadows parent /F1 (decodable) — \
             must be flagged for OCR"
        );
    }

    #[test]
    fn test_p3_page_overrides_parent_font_decodable_shadows_undecodable() {
        // Inverse: page /F1 → decodable Type1, parent /F1 → undecodable Identity-H.
        // Content uses /F1. The page's decodable font shadows the parent's bad one.
        // Expectation: MUST NOT be flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Parent's /F1: undecodable Identity-H (SHADOWED — should NOT be in used set)
        let parent_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type0".to_vec()),
            "BaseFont" => Object::Name(b"ABCDEF+BadFont".to_vec()),
            "Encoding" => Object::Name(b"Identity-H".to_vec()),
        });
        let parent_resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(parent_font_id),
            },
        }));

        // Page's /F1: decodable Type1
        let page_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });

        let content_data = b"BT /F1 12 Tf (Hello world) Tj ET";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));

        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(page_font_id),
                    },
                },
                "Contents" => Object::Reference(content_id),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
                "Resources" => Object::Reference(parent_resources_id),
            }),
        );

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P3: page /F1 (decodable) shadows parent /F1 (undecodable) — \
             must NOT be flagged for OCR"
        );
        assert!(
            analysis.has_decodable_text_fonts,
            "P3: page's decodable font should be detected as used"
        );
    }

    #[test]
    fn test_p3_inheritance_without_override_uses_parent_font() {
        // Page has NO /F1 in its own /Resources. Parent has /F1 → decodable.
        // Content uses /F1. Should inherit the parent's font.
        // Expectation: MUST NOT be flagged.
        use lopdf::dictionary;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        // Parent's /F1: decodable Type1
        let parent_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => Object::Name(b"Type1".to_vec()),
            "BaseFont" => Object::Name(b"Helvetica".to_vec()),
        });
        let parent_resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(parent_font_id),
            },
        }));

        let content_data = b"BT /F1 12 Tf (Hello world) Tj ET";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            content_data.to_vec(),
        )));

        // Page has NO own /Resources — inherits everything from parent
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Contents" => Object::Reference(content_id),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
                "Resources" => Object::Reference(parent_resources_id),
            }),
        );

        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            !analysis.has_identity_h_no_tounicode,
            "P3: page inherits parent's decodable /F1 — must NOT be flagged"
        );
        assert!(
            analysis.has_decodable_text_fonts,
            "P3: inherited decodable font should be detected as used"
        );
    }

    /// Three lines that together show forty distinct letters, digits and
    /// spaces.
    const CID_TEXT_LINES: [&str; 3] = [
        "The quick brown fox jumps over the lazy dog",
        "Pack my box with five dozen liquor jugs 0123456789",
        "Sphinx of black quartz judge my vow",
    ];

    /// Where a page's CID text is shown and how its font is written.
    #[derive(Clone, Copy, PartialEq)]
    enum CidTextLayout {
        /// In the page content, the font an indirect object with a
        /// referenced ToUnicode stream.
        Page,
        /// In the page content, the font dictionary and its ToUnicode
        /// stream written inline in the resources.
        InlineFont,
        /// In a Form XObject with its own font resources.
        Form,
        /// In a Form XObject without resources, using the page's font.
        FormInheritingFonts,
        /// In the page content after a `q`/`Q` that selected another font
        /// inside the saved state.
        AfterRestoredFont,
        /// In a Form XObject the page's resources name but its content
        /// never invokes.
        UninvokedForm,
        /// In a Form XObject without resources or a `Tf` of its own, shown
        /// in the font the page selected before invoking it.
        FormInheritingFontState,
        /// In a Form XObject with its own font resources, invoked before a
        /// second, larger form of paths.
        FormThenLargerForm,
        /// In a Form XObject with its own font resources, invoked after a
        /// form whose content breaks off inside a string.
        MalformedFormThenForm,
    }

    /// A page of `paths` filled triangles and `lines` of text shown as
    /// two-byte codes through a Type0 font: an embedded TrueType subset
    /// under Identity-H whose glyph `i` is the `i`th distinct character of
    /// the lines, with a ToUnicode CMap saying so — or, when `broken_cmap`,
    /// mapping every code to the same letter. `layout` says where the text
    /// is shown and how the font is written.
    fn cid_text_over_vector_art(
        lines: &[&str],
        paths: usize,
        broken_cmap: bool,
        layout: CidTextLayout,
    ) -> (Document, ObjectId) {
        use lopdf::{dictionary, Stream};

        let mut alphabet: Vec<char> = lines.iter().flat_map(|line| line.chars()).collect();
        alphabet.sort_unstable();
        alphabet.dedup();
        let code_of = |c: char| alphabet.iter().position(|&a| a == c).unwrap() as u16 + 1;

        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        cmap.push_str(&format!("{} beginbfchar\n", alphabet.len()));
        for &c in &alphabet {
            let unicode = if broken_cmap { 'x' as u32 } else { c as u32 };
            cmap.push_str(&format!("<{:04X}> <{:04X}>\n", code_of(c), unicode));
        }
        cmap.push_str(
            "endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n",
        );

        let glyphs: Vec<(bool, u16)> = alphabet.iter().map(|_| (true, 600)).collect();
        let codes: Vec<u8> = alphabet.iter().map(|&c| c as u8).collect();
        let font_file =
            crate::extractor::fonts::blank_glyph_tests::synthetic_truetype(&glyphs, &codes);

        let mut doc = Document::with_version("1.5");
        let font_file_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! { "Length1" => font_file.len() as i64 },
            font_file,
        )));
        let descriptor_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "AAAAAA+Subset",
            "Flags" => 4,
            "FontBBox" => vec![0.into(), 0.into(), 600.into(), 700.into()],
            "ItalicAngle" => 0,
            "Ascent" => 700,
            "Descent" => 0,
            "CapHeight" => 700,
            "StemV" => 80,
            "FontFile2" => Object::Reference(font_file_id),
        });
        let cid_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "BaseFont" => "AAAAAA+Subset",
            "CIDSystemInfo" => dictionary! {
                "Registry" => Object::string_literal("Adobe"),
                "Ordering" => Object::string_literal("Identity"),
                "Supplement" => 0,
            },
            "FontDescriptor" => Object::Reference(descriptor_id),
            "DW" => 600,
            "CIDToGIDMap" => "Identity",
        });
        let cmap_stream = Stream::new(dictionary! {}, cmap.into_bytes());
        let font = |to_unicode: Object| {
            dictionary! {
                "Type" => "Font",
                "Subtype" => "Type0",
                "BaseFont" => "AAAAAA+Subset",
                "Encoding" => "Identity-H",
                "DescendantFonts" => vec![Object::Reference(cid_font_id)],
                "ToUnicode" => to_unicode,
            }
        };
        let font_entry = if layout == CidTextLayout::InlineFont {
            Object::Dictionary(font(Object::Stream(cmap_stream)))
        } else {
            let cmap_id = doc.add_object(Object::Stream(cmap_stream));
            Object::Reference(doc.add_object(font(Object::Reference(cmap_id))))
        };
        let helvetica_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });

        let mut art = String::new();
        for i in 0..paths {
            let (x, y) = (50 + (i % 40) * 12, 100 + (i / 40) * 20);
            art.push_str(&format!(
                "{x} {y} m {} {} l {} {y} l h f\n",
                x + 5,
                y + 8,
                x + 10
            ));
        }
        let mut text = String::new();
        for (index, line) in lines.iter().enumerate() {
            let hex: String = line
                .chars()
                .map(|c| format!("{:04X}", code_of(c)))
                .collect();
            text.push_str(&format!(
                "BT /F1 12 Tf 72 {} Td <{hex}> Tj ET\n",
                700 - 20 * index
            ));
        }
        let fonts = dictionary! {
            "F1" => font_entry,
            "F2" => Object::Reference(helvetica_id),
        };
        let (page_content, page_resources) = match layout {
            CidTextLayout::Page | CidTextLayout::InlineFont => {
                (format!("{art}{text}"), dictionary! { "Font" => fonts })
            }
            CidTextLayout::AfterRestoredFont => (
                format!(
                    "{art}BT /F1 12 Tf ET q BT /F2 12 Tf 72 40 Td (x) Tj ET Q\n{}",
                    text.replace("/F1 12 Tf ", "")
                ),
                dictionary! { "Font" => fonts },
            ),
            CidTextLayout::Form
            | CidTextLayout::FormInheritingFonts
            | CidTextLayout::UninvokedForm
            | CidTextLayout::FormInheritingFontState
            | CidTextLayout::FormThenLargerForm
            | CidTextLayout::MalformedFormThenForm => {
                let form_dict = || {
                    dictionary! {
                        "Type" => "XObject",
                        "Subtype" => "Form",
                        "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                    }
                };
                let mut text_form_dict = form_dict();
                if matches!(
                    layout,
                    CidTextLayout::Form
                        | CidTextLayout::FormThenLargerForm
                        | CidTextLayout::MalformedFormThenForm
                ) {
                    text_form_dict.set("Resources", dictionary! { "Font" => fonts.clone() });
                }
                let form_text = if layout == CidTextLayout::FormInheritingFontState {
                    text.replace("/F1 12 Tf ", "")
                } else {
                    text
                };
                let form_id = doc.add_object(Object::Stream(Stream::new(
                    text_form_dict,
                    form_text.into_bytes(),
                )));
                let mut xobjects = dictionary! { "Fm1" => Object::Reference(form_id) };
                let invoke = match layout {
                    CidTextLayout::UninvokedForm => String::new(),
                    CidTextLayout::FormInheritingFontState => {
                        "BT /F1 12 Tf ET q /Fm1 Do Q\n".to_string()
                    }
                    CidTextLayout::FormThenLargerForm => {
                        // A second form of paths, invoked after the text.
                        let larger = doc.add_object(Object::Stream(Stream::new(
                            form_dict(),
                            art.clone().into_bytes(),
                        )));
                        xobjects.set("Fm2", Object::Reference(larger));
                        "q /Fm1 Do Q q /Fm2 Do Q\n".to_string()
                    }
                    CidTextLayout::MalformedFormThenForm => {
                        // A form whose content breaks off inside a string,
                        // invoked before the text.
                        let malformed = doc.add_object(Object::Stream(Stream::new(
                            form_dict(),
                            b"BT /F2 12 Tf 72 40 Td (broken".to_vec(),
                        )));
                        xobjects.set("Fm0", Object::Reference(malformed));
                        "q /Fm0 Do Q q /Fm1 Do Q\n".to_string()
                    }
                    _ => "q /Fm1 Do Q\n".to_string(),
                };
                (
                    format!("{art}{invoke}"),
                    dictionary! {
                        "Font" => fonts,
                        "XObject" => xobjects,
                    },
                )
            }
        };
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            page_content.into_bytes(),
        )));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => page_resources,
            "Contents" => Object::Reference(content_id),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        (doc, page_id)
    }

    /// Text shown through a CID-keyed font with a ToUnicode CMap counts by
    /// its decoded characters: three lines of prose next to two thousand
    /// path operators (a drawing of about seventeen operators per
    /// character) are text, although their two-byte codes hold no ASCII
    /// letter or digit.
    #[test]
    fn cid_text_with_a_working_tounicode_is_not_vector_text() {
        for layout in [
            CidTextLayout::Page,
            CidTextLayout::InlineFont,
            CidTextLayout::Form,
            CidTextLayout::FormInheritingFonts,
            CidTextLayout::AfterRestoredFont,
            CidTextLayout::FormInheritingFontState,
            CidTextLayout::FormThenLargerForm,
            CidTextLayout::MalformedFormThenForm,
        ] {
            let (doc, page_id) = cid_text_over_vector_art(&CID_TEXT_LINES, 400, false, layout);
            let analysis = analyze_page_content(&doc, page_id);
            assert!(analysis.path_op_count >= 1000, "{}", analysis.path_op_count);
            assert!(
                (analysis.unique_alphanum_chars as usize) < VECTOR_TEXT_MIN_ALPHANUMERICS,
                "the byte count is blind to the codes: {}",
                analysis.unique_alphanum_chars
            );
            assert!(!analysis.has_vector_text, "layout {}", layout as u8);
            assert_eq!(page_ocr_signals(&doc, page_id), PageOcrSignals::default());
        }
    }

    /// A caption's worth of decoded text does not make a page of paths a
    /// text page; neither does a CMap that maps every code alike, nor a
    /// title and address line, diverse as they are, over forty thousand
    /// path operators of outlined body text, nor text in a form the page
    /// never invokes.
    #[test]
    fn little_or_undecodable_cid_text_over_vector_art_stays_vector_text() {
        let (doc, page_id) = cid_text_over_vector_art(&["Fig 3"], 400, false, CidTextLayout::Page);
        assert!(analyze_page_content(&doc, page_id).has_vector_text);
        let (doc, page_id) =
            cid_text_over_vector_art(&CID_TEXT_LINES, 400, true, CidTextLayout::Page);
        assert!(analyze_page_content(&doc, page_id).has_vector_text);
        let (doc, page_id) =
            cid_text_over_vector_art(&CID_TEXT_LINES, 8_000, false, CidTextLayout::Page);
        let analysis = analyze_page_content(&doc, page_id);
        assert!(
            analysis.path_op_count >= 40_000,
            "{}",
            analysis.path_op_count
        );
        assert!(analysis.has_vector_text);
        // Text in a form the page never invokes is not shown.
        let (doc, page_id) =
            cid_text_over_vector_art(&CID_TEXT_LINES, 400, false, CidTextLayout::UninvokedForm);
        assert!(analyze_page_content(&doc, page_id).has_vector_text);
        assert!(!decoded_text_counts(&doc, page_id).through_cid_cmap);
    }

    /// The decoded counts cover the whole text: the three lines show
    /// thirty-nine distinct letters and digits, one hundred and six in all,
    /// through the CID font's CMap. They are the page's text next to a
    /// drawing of up to a hundred path operators per character, and a
    /// header next to a larger one.
    #[test]
    fn decoded_text_counts_cover_the_whole_text() {
        for layout in [
            CidTextLayout::Page,
            CidTextLayout::InlineFont,
            CidTextLayout::FormInheritingFonts,
            CidTextLayout::AfterRestoredFont,
            CidTextLayout::FormInheritingFontState,
            CidTextLayout::FormThenLargerForm,
            CidTextLayout::MalformedFormThenForm,
        ] {
            let (doc, page_id) = cid_text_over_vector_art(&CID_TEXT_LINES, 400, false, layout);
            let counts = decoded_text_counts(&doc, page_id);
            assert_eq!(
                (counts.distinct_alphanumerics, counts.through_cid_cmap),
                (39, true),
                "layout {}",
                layout as u8
            );
            // The restored-font layout paints one more glyph in the
            // simple font before the CID text.
            let extra = usize::from(layout == CidTextLayout::AfterRestoredFont);
            assert_eq!(counts.alphanumerics, 106 + extra);
            let at_the_bar = counts.alphanumerics as u32 * VECTOR_TEXT_MAX_PATH_OPS_PER_CHARACTER;
            assert!(counts.is_page_text(at_the_bar));
            assert!(!counts.is_page_text(at_the_bar + 1));
        }
        let sparse = DecodedTextCounts {
            distinct_alphanumerics: 29,
            alphanumerics: 1_000,
            through_cid_cmap: true,
        };
        assert!(!sparse.is_page_text(1_000));
    }

    /// The page and the forms it invokes are read together within the
    /// page's budgets: with the operations, or the bytes, spent on the
    /// page's own content, the form's text goes uncounted; with room for
    /// the form, it is counted in full; a page over the byte budget by
    /// itself counts nothing.
    #[test]
    fn decoded_text_counts_stay_within_the_page_budgets() {
        let (doc, page_id) =
            cid_text_over_vector_art(&CID_TEXT_LINES, 400, false, CidTextLayout::Form);
        let page_content = doc.get_page_content(page_id);
        let page_operations = lopdf::content::Content::decode(&page_content)
            .unwrap()
            .operations
            .len();
        let form = doc
            .objects
            .values()
            .find_map(|object| match object {
                Object::Stream(stream)
                    if stream.dict.get(b"Subtype").ok()
                        == Some(&Object::Name(b"Form".to_vec())) =>
                {
                    Some(stream)
                }
                _ => None,
            })
            .unwrap();
        let form_operations = lopdf::content::Content::decode(&form.content)
            .unwrap()
            .operations
            .len();
        let full = DecodedTextCounts {
            distinct_alphanumerics: 39,
            alphanumerics: 106,
            through_cid_cmap: true,
        };
        let bytes = crate::extractor::content_decode::MAX_PAGE_CONTENT_BYTES;
        let operations = crate::extractor::content_decode::MAX_PAGE_OPERATIONS;

        assert_eq!(
            decoded_text_counts_within(&doc, page_id, bytes, page_operations),
            DecodedTextCounts::default()
        );
        assert_eq!(
            decoded_text_counts_within(&doc, page_id, bytes, page_operations + form_operations),
            full
        );
        assert_eq!(
            decoded_text_counts_within(&doc, page_id, page_content.len(), operations),
            DecodedTextCounts::default()
        );
        assert_eq!(
            decoded_text_counts_within(
                &doc,
                page_id,
                page_content.len() + form.content.len(),
                operations
            ),
            full
        );
        assert_eq!(
            decoded_text_counts_within(&doc, page_id, page_content.len() / 2, operations),
            DecodedTextCounts::default()
        );
        assert_eq!(decoded_text_counts(&doc, page_id), full);
    }

    /// The Form XObject streams of `doc`, in no particular order.
    fn form_streams(doc: &Document) -> Vec<&lopdf::Stream> {
        doc.objects
            .values()
            .filter_map(|object| match object {
                Object::Stream(stream)
                    if stream.dict.get(b"Subtype").ok()
                        == Some(&Object::Name(b"Form".to_vec())) =>
                {
                    Some(stream)
                }
                _ => None,
            })
            .collect()
    }

    /// The forms a page invokes are read in the order invoked, so the
    /// budgets go to the text drawn first: with room for the page and the
    /// text form only, a larger form invoked after it goes unread and the
    /// text is counted in full. A form whose content breaks off is read up
    /// to the fault, and the forms invoked after it are still read.
    #[test]
    fn forms_are_read_in_invocation_order_within_the_budgets() {
        let full = DecodedTextCounts {
            distinct_alphanumerics: 39,
            alphanumerics: 106,
            through_cid_cmap: true,
        };
        let operations = |content: &[u8]| {
            lopdf::content::Content::decode(content)
                .unwrap()
                .operations
                .len()
        };
        let bytes = crate::extractor::content_decode::MAX_PAGE_CONTENT_BYTES;
        let ops = crate::extractor::content_decode::MAX_PAGE_OPERATIONS;

        let (doc, page_id) = cid_text_over_vector_art(
            &CID_TEXT_LINES,
            400,
            false,
            CidTextLayout::FormThenLargerForm,
        );
        let page_content = doc.get_page_content(page_id);
        let text_form = form_streams(&doc)
            .into_iter()
            .find(|stream| stream.content.windows(3).any(|w| w == &b" Tj"[..]))
            .unwrap();
        let just_enough_operations = operations(&page_content) + operations(&text_form.content);
        let just_enough_bytes = page_content.len() + text_form.content.len();
        assert_eq!(
            decoded_text_counts_within(&doc, page_id, bytes, just_enough_operations),
            full
        );
        assert_eq!(
            decoded_text_counts_within(&doc, page_id, just_enough_bytes, ops),
            full
        );
        assert_eq!(decoded_text_counts(&doc, page_id), full);

        let (doc, page_id) = cid_text_over_vector_art(
            &CID_TEXT_LINES,
            400,
            false,
            CidTextLayout::MalformedFormThenForm,
        );
        assert_eq!(decoded_text_counts(&doc, page_id), full);
        assert!(!analyze_page_content(&doc, page_id).has_vector_text);
    }

    /// A ToUnicode stream is read within the loader's per-stream bound: at
    /// the bound the CMap is read and the text through it counted; one byte
    /// over it the stream is not read, the font goes undecoded and the page
    /// stays vector text.
    #[test]
    fn a_tounicode_cmap_over_the_stream_bound_is_not_read() {
        fn pad_to(doc: &mut Document, id: ObjectId, total: usize) {
            let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) else {
                unreachable!()
            };
            stream.content.resize(total, b' ');
        }
        let (mut doc, page_id) =
            cid_text_over_vector_art(&CID_TEXT_LINES, 400, false, CidTextLayout::Page);
        let cmap_id = doc
            .objects
            .iter()
            .find_map(|(id, object)| match object {
                Object::Stream(stream)
                    if stream.content.windows(9).any(|w| w == &b"begincmap"[..]) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .unwrap();
        let bound = crate::MAX_STREAM_DECOMPRESSED_BYTES;
        pad_to(&mut doc, cmap_id, bound);
        assert!(!analyze_page_content(&doc, page_id).has_vector_text);
        pad_to(&mut doc, cmap_id, bound + 1);
        assert!(analyze_page_content(&doc, page_id).has_vector_text);
        assert_eq!(
            decoded_text_counts(&doc, page_id),
            DecodedTextCounts::default()
        );
    }
}
