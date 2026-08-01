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
    /// Machine-readable OCR reasons by 1-indexed page.
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    pub title: Option<String>,
    pub confidence: f64,
    pub is_complex_layout: bool,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub has_encoding_issues: bool,
}

/// OCR reasons for a single 1-indexed page.
#[napi(object)]
pub struct PageOcrReasons {
    pub page: u32,
    pub reasons: Vec<String>,
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
    /// Underline detected geometrically (drawn rule/thin rect under the
    /// baseline) — PDFs carry no underline font flag.
    pub is_underline: bool,
    /// Strikeout detected geometrically (rule crossing the glyphs at mid
    /// x-height).
    pub is_strikeout: bool,
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
    /// Machine-readable OCR reason when the cause is known.
    pub ocr_reason: Option<String>,
}

/// Extracted text for one page's regions.
#[napi(object)]
pub struct PageRegionTexts {
    pub page: u32,
    pub regions: Vec<RegionText>,
}

/// Vector-grid detection result compatible with `extractTablesWithStructure*`.
#[napi(object)]
pub struct VectorGridDetectionJs {
    pub structure_tokens: Vec<String>,
    pub cell_bboxes: Vec<Vec<f64>>,
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
        ocr_reasons_by_page: to_napi_page_ocr_reasons(r.ocr_reasons_by_page),
        title: r.title,
        confidence: r.confidence as f64,
        is_complex_layout: r.layout.is_complex,
        pages_with_tables: r.layout.pages_with_tables,
        pages_with_columns: r.layout.pages_with_columns,
        has_encoding_issues: r.has_encoding_issues,
    }
}

fn to_napi_page_ocr_reasons(
    reasons: Vec<pdf_inspector::PageOcrReasons>,
) -> Vec<PageOcrReasons> {
    reasons
        .into_iter()
        .map(|reason| PageOcrReasons {
            page: reason.page,
            reasons: reason.reasons,
        })
        .collect()
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
                    is_underline: item.is_underline,
                    is_strikeout: item.is_strikeout,
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

/// Detect a vector ruled-line / rectangle grid inside one page region.
///
/// Returns TSR-compatible structure tokens plus crop-pixel cell bboxes, or
/// `null` when the region does not contain a valid vector grid.
///
/// `pageIdx` is 0-indexed. `regionPdfPtBbox` is `[x1,y1,x2,y2]` in PDF
/// points with top-left origin. `renderDpi` is the DPI of the crop image that
/// will consume the returned cell bboxes.
#[napi]
pub fn detect_vector_grid_in_region(
    buffer: Buffer,
    page_idx: u32,
    region_pdf_pt_bbox: Vec<f64>,
    render_dpi: f64,
) -> Result<Option<VectorGridDetectionJs>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let region = if region_pdf_pt_bbox.len() == 4 {
        [
            region_pdf_pt_bbox[0] as f32,
            region_pdf_pt_bbox[1] as f32,
            region_pdf_pt_bbox[2] as f32,
            region_pdf_pt_bbox[3] as f32,
        ]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    };

    catch_panic("detect_vector_grid_in_region", move || {
        let result = pdf_inspector::detect_vector_grid_in_region_mem(
            &bytes,
            page_idx,
            region,
            render_dpi as f32,
        )
        .map_err(|e| to_napi_err(e, "detect_vector_grid_in_region"))?;

        Ok(result.map(|r| VectorGridDetectionJs {
            structure_tokens: r.structure_tokens,
            cell_bboxes: r
                .cell_bboxes
                .into_iter()
                .map(|bbox| bbox.into_iter().map(|v| v as f64).collect())
                .collect(),
        }))
    })
}

/// One cropped table region plus its raw structure-recovery output, for
/// `extractTablesWithStructure`.
///
/// `structureTokens` and `cellBboxes` are typically produced by an external
/// table-structure recognition model (e.g. SLANet on PaddleOCR) running on
/// a rendered crop of the page. pdf-inspector uses the structure to lay out
/// the cells and pulls the cell text from the native PDF — no OCR involved.
#[napi(object)]
pub struct TsrTableInputJs {
    /// 0-indexed page number where the crop was taken from.
    pub page: u32,
    /// Crop bbox on the page, `[x1, y1, x2, y2]` in PDF points with
    /// top-left origin.
    pub crop_pdf_pt_bbox: Vec<f64>,
    /// DPI the crop image was rendered at (e.g. `200.0`).
    pub render_dpi: f64,
    /// Raw structure tokens emitted by the TSR model, in document order.
    pub structure_tokens: Vec<String>,
    /// One bbox per cell (in document order). May be 4-element
    /// `[x1,y1,x2,y2]` or 8-element 4-corner polygon, in crop image-pixel
    /// space.
    pub cell_bboxes: Vec<Vec<f64>>,
}

/// Extract markdown tables using externally-supplied structure recovery.
///
/// For each input, pairs structure tokens with cell bboxes (rowspan/colspan
/// aware), converts each cell bbox from crop image-pixels into page PDF
/// points, pulls the cell's text from the native PDF, and emits a markdown
/// pipe-table.
///
/// Returns one markdown string per input, in input order.
#[napi]
pub fn extract_tables_with_structure(
    buffer: Buffer,
    inputs: Vec<TsrTableInputJs>,
) -> Result<Vec<String>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let parsed = parse_tsr_inputs(&inputs);

    catch_panic("extract_tables_with_structure", move || {
        pdf_inspector::extract_tables_with_structure_mem(&bytes, &parsed)
            .map_err(|e| to_napi_err(e, "extract_tables_with_structure"))
    })
}

/// One resolved cell from `extractTablesWithStructureCells`.
#[napi(object)]
pub struct StructuredCellJs {
    /// 0-indexed grid row.
    pub row: u32,
    /// 0-indexed grid column.
    pub col: u32,
    /// 1 for a normal cell.
    pub rowspan: u32,
    /// 1 for a normal cell.
    pub colspan: u32,
    /// `true` when the cell is a `<th>` or sits inside `<thead>`.
    pub is_header: bool,
    /// Text extracted from the native PDF for this cell (may be empty).
    pub text: String,
    /// Axis-aligned bbox `[x1, y1, x2, y2]` in page PDF-points, top-left
    /// origin. Useful for debug overlays or per-cell post-processing.
    pub page_pt_bbox: Vec<f64>,
}

/// Extract structured cells using externally-supplied structure recovery.
///
/// Lower-level sibling of [`extractTablesWithStructure`]: instead of
/// rendering markdown, returns the resolved cells (row, col, rowspan,
/// colspan, isHeader, text, pagePtBbox) so callers can drive their own
/// rendering, debug overlays, or per-cell post-processing.
///
/// Returns one `Array<StructuredCellJs>` per input, in input order.
#[napi]
pub fn extract_tables_with_structure_cells(
    buffer: Buffer,
    inputs: Vec<TsrTableInputJs>,
) -> Result<Vec<Vec<StructuredCellJs>>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let parsed = parse_tsr_inputs(&inputs);

    catch_panic("extract_tables_with_structure_cells", move || {
        let result = pdf_inspector::extract_tables_with_structure_cells_mem(&bytes, &parsed)
            .map_err(|e| to_napi_err(e, "extract_tables_with_structure_cells"))?;
        Ok(result
            .into_iter()
            .map(|cells| {
                cells
                    .into_iter()
                    .map(|c| StructuredCellJs {
                        row: c.row as u32,
                        col: c.col as u32,
                        rowspan: c.rowspan as u32,
                        colspan: c.colspan as u32,
                        is_header: c.is_header,
                        text: c.text,
                        page_pt_bbox: c.page_pt_bbox.iter().map(|v| *v as f64).collect(),
                    })
                    .collect()
            })
            .collect())
    })
}

/// One result from `extractTablesWithStructureAuto` — markdown plus a
/// diagnostic flag identifying which path produced it.
///
/// `fallbackReason` is `null` when the TSR-hybrid path produced the
/// markdown directly. When stage 1's quality check fires (the cells
/// look like a SLANet detection pathology — phantom rows or multi-row
/// content in a single cell), the auto path may expand the TSR cells
/// in-place or run the heuristic table extractor on the same region.
/// `fallbackReason` carries the diagnostic label (for example
/// `"multi_row_in_cell_expanded"` or `"phantom_empty_row"`).
#[napi(object)]
pub struct TableExtractionResultJs {
    pub markdown: String,
    pub fallback_reason: Option<String>,
}

/// Auto-fallback variant of [`extractTablesWithStructure`].
///
/// Runs the TSR-hybrid path, checks the resulting cells for known
/// SLANet detection pathologies, expands multi-row cells in-place when
/// possible, and otherwise falls back to the heuristic
/// `extractTablesInRegions` for inputs where the TSR path looks
/// compromised.
///
/// On clean inputs this returns identical markdown to
/// `extractTablesWithStructure`; on flagged inputs `fallbackReason` is
/// set to the recovery path that produced the result.
#[napi]
pub fn extract_tables_with_structure_auto(
    buffer: Buffer,
    inputs: Vec<TsrTableInputJs>,
) -> Result<Vec<TableExtractionResultJs>> {
    let bytes: Vec<u8> = buffer.to_vec();
    let parsed = parse_tsr_inputs(&inputs);

    catch_panic("extract_tables_with_structure_auto", move || {
        let result = pdf_inspector::extract_tables_with_structure_auto_mem(&bytes, &parsed)
            .map_err(|e| to_napi_err(e, "extract_tables_with_structure_auto"))?;
        Ok(result
            .into_iter()
            .map(|r| TableExtractionResultJs {
                markdown: r.markdown,
                fallback_reason: r.fallback_reason,
            })
            .collect())
    })
}

fn parse_tsr_inputs(inputs: &[TsrTableInputJs]) -> Vec<pdf_inspector::TsrTableInput> {
    inputs
        .iter()
        .map(|i| {
            let crop = if i.crop_pdf_pt_bbox.len() == 4 {
                [
                    i.crop_pdf_pt_bbox[0] as f32,
                    i.crop_pdf_pt_bbox[1] as f32,
                    i.crop_pdf_pt_bbox[2] as f32,
                    i.crop_pdf_pt_bbox[3] as f32,
                ]
            } else {
                [0.0, 0.0, 0.0, 0.0]
            };
            let cell_bboxes: Vec<Vec<f32>> = i
                .cell_bboxes
                .iter()
                .map(|bb| bb.iter().map(|v| *v as f32).collect())
                .collect();
            pdf_inspector::TsrTableInput {
                page: i.page,
                crop_pdf_pt_bbox: crop,
                render_dpi: i.render_dpi as f32,
                structure_tokens: i.structure_tokens.clone(),
                cell_bboxes,
            }
        })
        .collect()
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
    /// Machine-readable OCR reason when the cause is known.
    pub ocr_reason: Option<String>,
}

/// Combined per-page markdown extraction and layout classification result.
#[napi(object)]
pub struct PagesExtractionResult {
    /// Per-page markdown results.
    pub pages: Vec<PageMarkdownResult>,
    /// 1-indexed pages where tables were detected.
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where multi-column layout was detected.
    pub pages_with_columns: Vec<u32>,
    /// 1-indexed pages that need OCR (scanned/image-based).
    pub pages_needing_ocr: Vec<u32>,
    /// Machine-readable OCR reasons by 1-indexed page.
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    /// True if any page has tables or columns.
    pub is_complex: bool,
}

/// Extract formatted markdown for pages of a PDF, with layout classification
/// metadata.
///
/// Returns per-page markdown and classification data (tables, columns,
/// OCR needs) from a single parse. Font statistics are computed from the
/// full document so header detection is consistent across pages.
///
/// Omit `pages` (or pass `undefined`) to return every page in document
/// order. Pass an array of 0-indexed page numbers to restrict output to
/// those pages, in caller-supplied order.
#[napi]
pub fn extract_pages_markdown(
    buffer: Buffer,
    pages: Option<Vec<u32>>,
) -> Result<PagesExtractionResult> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("extract_pages_markdown", move || {
        let result = pdf_inspector::extract_pages_markdown_mem(&bytes, pages.as_deref())
            .map_err(|e| to_napi_err(e, "extract_pages_markdown"))?;
        Ok(PagesExtractionResult {
            pages: result
                .pages
                .into_iter()
                .map(|r| PageMarkdownResult {
                    page: r.page,
                    markdown: r.markdown,
                    needs_ocr: r.needs_ocr,
                    ocr_reason: r.ocr_reason,
                })
                .collect(),
            pages_with_tables: result.pages_with_tables,
            pages_with_columns: result.pages_with_columns,
            pages_needing_ocr: result.pages_needing_ocr,
            ocr_reasons_by_page: to_napi_page_ocr_reasons(result.ocr_reasons_by_page),
            is_complex: result.is_complex,
        })
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
                    ocr_reason: r.ocr_reason,
                })
                .collect(),
        })
        .collect()
}
