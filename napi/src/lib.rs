#![deny(clippy::all)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashSet;
use std::panic;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// PDF document type classification.
#[napi(string_enum)]
pub enum PdfType {
    TextBased,
    Scanned,
    ImageBased,
    Mixed,
}

/// Type of a positioned text item.
#[napi(string_enum)]
pub enum ItemType {
    Text,
    Image,
    Link,
    FormField,
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Full PDF processing result with markdown and metadata.
#[napi(object)]
pub struct PdfResult {
    pub pdf_type: PdfType,
    pub markdown: Option<String>,
    pub page_count: u32,
    pub processing_time_ms: u32,
    /// 1-indexed page numbers that need OCR.
    pub pages_needing_ocr: Vec<u32>,
    pub title: Option<String>,
    pub confidence: f64,
    pub is_complex_layout: bool,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub has_encoding_issues: bool,
}

/// Lightweight PDF classification result.
#[napi(object)]
pub struct PdfClassification {
    pub pdf_type: PdfType,
    pub page_count: u32,
    /// 0-indexed page numbers that need OCR.
    pub pages_needing_ocr: Vec<u32>,
    pub confidence: f64,
}

/// A positioned text item extracted from a PDF.
#[napi(object)]
pub struct TextItem {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font: String,
    pub font_size: f64,
    pub page: u32,
    pub is_bold: bool,
    pub is_italic: bool,
    pub item_type: ItemType,
    /// URL for link items, `None` for other types.
    pub link_url: Option<String>,
}

/// A page's regions for text extraction: (page_index_0based, bboxes).
#[napi(object)]
pub struct PageRegions {
    pub page: u32,
    /// Each bbox is [x1, y1, x2, y2] in PDF points, top-left origin.
    pub regions: Vec<Vec<f64>>,
}

/// Extracted text for a single region.
#[napi(object)]
pub struct RegionText {
    pub text: String,
    /// `true` when the text should not be trusted (empty, GID fonts, garbage, encoding issues).
    pub needs_ocr: bool,
}

/// Extracted text for one page's regions.
#[napi(object)]
pub struct PageRegionTexts {
    pub page: u32,
    pub regions: Vec<RegionText>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn convert_pdf_type(t: pdf_inspector::PdfType) -> PdfType {
    match t {
        pdf_inspector::PdfType::TextBased => PdfType::TextBased,
        pdf_inspector::PdfType::Scanned => PdfType::Scanned,
        pdf_inspector::PdfType::ImageBased => PdfType::ImageBased,
        pdf_inspector::PdfType::Mixed => PdfType::Mixed,
    }
}

fn to_napi_result(r: pdf_inspector::PdfProcessResult) -> PdfResult {
    PdfResult {
        pdf_type: convert_pdf_type(r.pdf_type),
        markdown: r.markdown,
        page_count: r.page_count,
        processing_time_ms: r.processing_time_ms as u32,
        pages_needing_ocr: r.pages_needing_ocr,
        title: r.title,
        confidence: r.confidence as f64,
        is_complex_layout: r.layout.is_complex,
        pages_with_tables: r.layout.pages_with_tables,
        pages_with_columns: r.layout.pages_with_columns,
        has_encoding_issues: r.has_encoding_issues,
    }
}

fn convert_item_type(t: &pdf_inspector::types::ItemType) -> (ItemType, Option<String>) {
    match t {
        pdf_inspector::types::ItemType::Text => (ItemType::Text, None),
        pdf_inspector::types::ItemType::Image => (ItemType::Image, None),
        pdf_inspector::types::ItemType::Link(url) => (ItemType::Link, Some(url.clone())),
        pdf_inspector::types::ItemType::FormField => (ItemType::FormField, None),
    }
}

fn to_napi_err(e: impl std::fmt::Display, ctx: &str) -> Error {
    Error::new(Status::GenericFailure, format!("{ctx}: {e}"))
}

/// Run a closure, catching any Rust panic and converting it to a NAPI error.
/// Prevents process abort from unwind panics in the native module.
fn catch_panic<F, T>(ctx: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + panic::UnwindSafe,
{
    match panic::catch_unwind(f) {
        Ok(result) => result,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(Error::new(
                Status::GenericFailure,
                format!("{ctx}: Rust panic: {msg}"),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Public NAPI API
// ---------------------------------------------------------------------------

/// Process a PDF from a Buffer: detect type, extract text, and convert to Markdown.
#[napi]
pub fn process_pdf(buffer: Buffer, pages: Option<Vec<u32>>) -> Result<PdfResult> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("process_pdf", move || {
        let mut opts = pdf_inspector::PdfOptions::new();
        if let Some(p) = pages {
            opts = opts.pages(p);
        }
        let result = pdf_inspector::process_pdf_mem_with_options(&bytes, opts)
            .map_err(|e| to_napi_err(e, "process_pdf"))?;
        Ok(to_napi_result(result))
    })
}

/// Fast detection only — no text extraction or markdown.
#[napi]
pub fn detect_pdf(buffer: Buffer) -> Result<PdfResult> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("detect_pdf", move || {
        let result =
            pdf_inspector::detect_pdf_mem(&bytes).map_err(|e| to_napi_err(e, "detect_pdf"))?;
        Ok(to_napi_result(result))
    })
}

/// Lightweight PDF classification — returns type, page count, and OCR pages.
/// Faster than detectPdf as it skips building the full PdfResult.
/// Pages in pagesNeedingOcr are 0-indexed.
#[napi]
pub fn classify_pdf(buffer: Buffer) -> Result<PdfClassification> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("classify_pdf", move || {
        let result =
            pdf_inspector::classify_pdf_mem(&bytes).map_err(|e| to_napi_err(e, "classify_pdf"))?;
        Ok(PdfClassification {
            pdf_type: convert_pdf_type(result.pdf_type),
            page_count: result.page_count,
            pages_needing_ocr: result.pages_needing_ocr,
            confidence: result.confidence as f64,
        })
    })
}

/// Extract plain text from a PDF Buffer.
#[napi]
pub fn extract_text(buffer: Buffer) -> Result<String> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("extract_text", move || {
        pdf_inspector::extractor::extract_text_mem(&bytes)
            .map_err(|e| to_napi_err(e, "extract_text"))
    })
}

/// Extract text with position information from a PDF Buffer.
#[napi]
pub fn extract_text_with_positions(
    buffer: Buffer,
    pages: Option<Vec<u32>>,
) -> Result<Vec<TextItem>> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("extract_text_with_positions", move || {
        let items = match pages {
            Some(p) => {
                let page_set: HashSet<u32> = p.into_iter().collect();
                pdf_inspector::extractor::extract_text_with_positions_mem_pages(
                    &bytes,
                    Some(&page_set),
                )
                .map_err(|e| to_napi_err(e, "extract_text_with_positions"))?
            }
            None => pdf_inspector::extractor::extract_text_with_positions_mem(&bytes)
                .map_err(|e| to_napi_err(e, "extract_text_with_positions"))?,
        };

        Ok(items
            .into_iter()
            .map(|item| {
                let (item_type, link_url) = convert_item_type(&item.item_type);
                TextItem {
                    text: item.text,
                    x: item.x as f64,
                    y: item.y as f64,
                    width: item.width as f64,
                    height: item.height as f64,
                    font: item.font,
                    font_size: item.font_size as f64,
                    page: item.page,
                    is_bold: item.is_bold,
                    is_italic: item.is_italic,
                    item_type,
                    link_url,
                }
            })
            .collect())
    })
}

/// Extract text within bounding-box regions from a PDF.
///
/// For hybrid OCR: layout model detects regions in rendered images,
/// this extracts PDF text within those regions — skipping GPU OCR
/// for text-based pages.
///
/// Each region result includes `needsOcr` — set when the extracted text
/// is unreliable (empty, GID-encoded fonts, garbage, encoding issues).
///
/// Coordinates are PDF points with top-left origin.
#[napi]
pub fn extract_text_in_regions(
    buffer: Buffer,
    page_regions: Vec<PageRegions>,
) -> Result<Vec<PageRegionTexts>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let regions = parse_page_regions(&page_regions);

    catch_panic("extract_text_in_regions", move || {
        let results = pdf_inspector::extract_text_in_regions_mem(&bytes, &regions)
            .map_err(|e| to_napi_err(e, "extract_text_in_regions"))?;
        Ok(to_page_region_texts(results))
    })
}

/// Extract markdown tables within bounding-box regions from a PDF.
///
/// Like `extractTextInRegions` but runs table detection on items within each
/// region and returns markdown pipe-tables instead of flat text.
///
/// When table structure is detected, `text` contains a markdown pipe-table and
/// `needsOcr` is `false`. When no table is found, `text` is empty and
/// `needsOcr` is `true` so the caller can fall back to GPU OCR.
///
/// Coordinates are PDF points with top-left origin.
#[napi]
pub fn extract_tables_in_regions(
    buffer: Buffer,
    page_regions: Vec<PageRegions>,
) -> Result<Vec<PageRegionTexts>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let regions = parse_page_regions(&page_regions);

    catch_panic("extract_tables_in_regions", move || {
        let results = pdf_inspector::extract_tables_in_regions_mem(&bytes, &regions)
            .map_err(|e| to_napi_err(e, "extract_tables_in_regions"))?;
        Ok(to_page_region_texts(results))
    })
}

/// Per-page markdown extraction result.
#[napi(object)]
pub struct PageMarkdownResult {
    /// 0-indexed page number.
    pub page: u32,
    /// Formatted markdown for this page.
    pub markdown: String,
    /// `true` when text on this page is unreliable.
    pub needs_ocr: bool,
}

/// Extract formatted markdown for specific pages of a PDF.
///
/// Returns per-page markdown so callers can mix direct extraction
/// (for simple text pages) with GPU OCR (for complex/scanned pages).
///
/// Font statistics are computed from the full document so header
/// detection is consistent across pages.
#[napi]
pub fn extract_pages_markdown(buffer: Buffer, pages: Vec<u32>) -> Result<Vec<PageMarkdownResult>> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("extract_pages_markdown", move || {
        let results = pdf_inspector::extract_pages_markdown_mem(&bytes, &pages)
            .map_err(|e| to_napi_err(e, "extract_pages_markdown"))?;
        Ok(results
            .into_iter()
            .map(|r| PageMarkdownResult {
                page: r.page,
                markdown: r.markdown,
                needs_ocr: r.needs_ocr,
            })
            .collect())
    })
}

fn parse_page_regions(page_regions: &[PageRegions]) -> Vec<(u32, Vec<[f32; 4]>)> {
    page_regions
        .iter()
        .map(|pr| {
            let bboxes: Vec<[f32; 4]> = pr
                .regions
                .iter()
                .map(|r| {
                    if r.len() != 4 {
                        [0.0, 0.0, 0.0, 0.0]
                    } else {
                        [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]
                    }
                })
                .collect();
            (pr.page, bboxes)
        })
        .collect()
}

fn to_page_region_texts(results: Vec<pdf_inspector::PageRegionResult>) -> Vec<PageRegionTexts> {
    results
        .into_iter()
        .map(|page_result| PageRegionTexts {
            page: page_result.page,
            regions: page_result
                .regions
                .into_iter()
                .map(|r| RegionText {
                    text: r.text,
                    needs_ocr: r.needs_ocr,
                })
                .collect(),
        })
        .collect()
}
