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
            Document::load_mem_with_options(buffer, lopdf::LoadOptions::with_password(""))?
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
                "page {}: text_ops={} images={} image_count={} template={} unique_chars={} alphanum={} path_ops={} vector_text={} image_area={} identity_h_no_tounicode={} type3_only={} font_changes={} decodable_fonts={}",
                page_num, analysis.text_operator_count, analysis.has_images,
                analysis.image_count, analysis.has_template_image,
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
            if analysis.has_template_image
                && (analysis.image_count <= 1 && analysis.text_operator_count < 50 && alphanum_ok)
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
                    analyze_page_content(doc, page_id)
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
                if (analysis.has_template_image && looks_like_scan)
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
    let mut text_ops = 0u32;
    let mut has_images = false;
    let mut image_count = 0u32;
    let mut path_ops = 0u32;
    let mut font_changes = 0u32;
    let mut all_unique_chars: HashSet<u8> = HashSet::new();
    // Collect font ObjectIds (not names) to avoid cross-scope name collisions.
    // Each content stream resolves its Tf font names against its own resource
    // dictionary, producing the correct underlying font ObjectId.
    let mut used_font_ids: HashSet<ObjectId> = HashSet::new();

    // Build font map keyed by ObjectId: collects FontInfo for all fonts from
    // page-level Resources + Form XObject Resources.
    let mut font_map: HashMap<ObjectId, FontInfo> = HashMap::new();

    // Get content streams for this page — these use the page's resource dict
    let content_streams = doc.get_page_contents(page_id);

    // We need the page's resource dict to resolve font names from page content.
    // get_page_resources returns (Option<&Dictionary>, Vec<ObjectId>) for
    // inline and indirect resource dicts respectively.
    let page_resources = doc.get_page_resources(page_id).ok();

    for content_id in content_streams {
        if let Ok(Object::Stream(stream)) = doc.get_object(content_id) {
            let content = match stream.decompressed_content() {
                Ok(data) => data,
                Err(_) => stream.content.clone(),
            };

            // Scan for text operators, collecting raw font names
            let mut page_font_names: HashSet<Vec<u8>> = HashSet::new();
            let (ops, imgs, paths, fonts) = scan_content_for_text_operators(
                &content,
                &mut all_unique_chars,
                &mut page_font_names,
            );
            text_ops += ops;
            image_count += imgs;
            path_ops += paths;
            font_changes += fonts;
            has_images = has_images || imgs > 0;

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
        }
    }

    // Scan XObject Form contents for text operators, collect their fonts,
    // and resolve font names per-XObject scope.
    if let Some((resource_dict, resource_ids)) = page_resources {
        let mut visited = HashSet::new();
        if let Some(resources) = resource_dict {
            collect_fonts_from_resource_dict(doc, resources, &mut font_map);
            let (ops, imgs, paths, fonts) = scan_xobjects_in_resources(
                doc,
                resources,
                &mut visited,
                &mut all_unique_chars,
                &mut used_font_ids,
                &mut font_map,
            );
            text_ops += ops;
            image_count += imgs;
            path_ops += paths;
            font_changes += fonts;
            has_images = has_images || imgs > 0;
        }
        for resource_id in resource_ids {
            if let Ok(resources) = doc.get_dictionary(resource_id) {
                collect_fonts_from_resource_dict(doc, resources, &mut font_map);
                let (ops, imgs, paths, fonts) = scan_xobjects_in_resources(
                    doc,
                    resources,
                    &mut visited,
                    &mut all_unique_chars,
                    &mut used_font_ids,
                    &mut font_map,
                );
                text_ops += ops;
                image_count += imgs;
                path_ops += paths;
                font_changes += fonts;
                has_images = has_images || imgs > 0;
            }
        }
    }

    // Check for XObject images and calculate coverage
    let (found_images, total_image_area, has_template_image) = analyze_page_images(doc, page_id);

    if found_images {
        has_images = true;
    }

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
    let has_vector_text =
        path_ops >= 1000 && path_ops > text_ops.saturating_mul(200) && unique_alphanum_chars < 30;

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
        has_images,
        has_template_image,
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
) -> (u32, u32, u32, u32) {
    let mut text_ops = 0u32;
    let mut image_count = 0u32;
    let mut path_ops = 0u32;
    let mut font_changes = 0u32;

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
                    // Collect raw font names from this XObject's content stream
                    let mut xobj_font_names: HashSet<Vec<u8>> = HashSet::new();
                    let (ops, imgs, paths, fonts) = scan_content_for_text_operators(
                        &content,
                        unique_chars,
                        &mut xobj_font_names,
                    );
                    text_ops += ops;
                    image_count += imgs;
                    path_ops += paths;
                    font_changes += fonts;

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
                        let (ops2, imgs2, paths2, fonts2) = scan_xobjects_in_resources(
                            doc,
                            res,
                            visited,
                            unique_chars,
                            used_font_ids,
                            font_map,
                        );
                        text_ops += ops2;
                        image_count += imgs2;
                        path_ops += paths2;
                        font_changes += fonts2;
                    }
                }
                Some(b"Image") => {
                    image_count += 1;
                }
                _ => {}
            }
        }
    }

    (text_ops, image_count, path_ops, font_changes)
}

/// Fast scan of content stream bytes for text operators
///
/// This is a fast heuristic scan that looks for:
/// - "Tj" - show text string
/// - "TJ" - show text with individual glyph positioning
/// - "'" - move to next line and show text
/// - "\"" - set word/char spacing, move to next line, show text
///
/// Returns (text_op_count, image_count, path_op_count, font_change_count).
/// Unique non-whitespace text characters are collected into `unique_chars`.
fn scan_content_for_text_operators(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> (u32, u32, u32, u32) {
    let mut text_ops = 0u32;
    let image_count = 0u32;
    let mut path_ops = 0u32;
    let mut font_changes = 0u32;

    // Helper: check if position is a word boundary (start of content or preceded by whitespace)
    let is_word_start = |pos: usize| -> bool { pos == 0 || content[pos - 1].is_ascii_whitespace() };
    // Helper: check if position is at end or followed by whitespace
    let is_word_end =
        |pos: usize| -> bool { pos + 1 >= content.len() || content[pos + 1].is_ascii_whitespace() };

    // Simple state machine to find operators
    let mut i = 0;
    while i < content.len() {
        let b = content[i];

        // Look for 'T' followed by 'j', 'J', or 'f'
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
            } else if next == b'f' {
                // Tf = set font operator
                // Some PDFs concatenate Tf with the next operator without
                // whitespace (e.g. "25 Tf[<01>..." or "25 Tf(<text>..."),
                // so also accept '[', '(', '<', '/' as valid followers.
                if i + 2 >= content.len()
                    || content[i + 2].is_ascii_whitespace()
                    || content[i + 2] == b'\n'
                    || content[i + 2] == b'\r'
                    || content[i + 2] == b'['
                    || content[i + 2] == b'('
                    || content[i + 2] == b'<'
                    || content[i + 2] == b'/'
                {
                    font_changes += 1;
                    // Extract the font name operand preceding the size + Tf.
                    // Pattern: /FontName <size> Tf
                    // Scan backward past the size number and whitespace to find /Name.
                    if let Some(name) = extract_font_name_before_tf(content, i) {
                        used_font_names.insert(name);
                    }
                }
            }
        }

        // Note: We do NOT count 'Do' operators here because Do invokes any
        // XObject — including Form XObjects that contain text.  Actual image
        // detection is handled by scan_xobjects_in_resources (checks Subtype)
        // and analyze_page_images (measures pixel area).

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

    (text_ops, image_count, path_ops, font_changes)
}

/// Extract the font name operand from content stream bytes preceding a Tf operator.
///
/// The Tf operator syntax is: `/FontName size Tf`
/// We scan backward from the position of 'T' in 'Tf' past the size number and
/// whitespace to find the `/Name` token.
///
/// Returns the font name bytes (without the leading `/`), e.g. `b"F1"` for `/F1`.
fn extract_font_name_before_tf(content: &[u8], tf_pos: usize) -> Option<Vec<u8>> {
    // Scan backward past whitespace before "Tf"
    let mut j = tf_pos;
    while j > 0 && content[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    // Scan backward past the size number (digits, '.', '-')
    while j > 0
        && (content[j - 1].is_ascii_digit() || content[j - 1] == b'.' || content[j - 1] == b'-')
    {
        j -= 1;
    }
    // Scan backward past whitespace between font name and size
    while j > 0 && content[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    // Now j should point just after the font name. Scan backward to find '/'.
    let name_end = j;
    while j > 0 && content[j - 1] != b'/' {
        // Font names consist of regular characters (not whitespace, not delimiters)
        if content[j - 1].is_ascii_whitespace() || content[j - 1] == b'(' || content[j - 1] == b')'
        {
            return None;
        }
        j -= 1;
    }
    if j == 0 || content[j - 1] != b'/' {
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
        let name = extract_font_name_before_tf(content, 6); // 'T' is at index 6
        assert_eq!(name, Some(b"F1".to_vec()));
    }

    #[test]
    fn test_extract_font_name_long_name() {
        let content = b"/ArialMT-Bold 9.5 Tf";
        let name = extract_font_name_before_tf(content, 18);
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
}
