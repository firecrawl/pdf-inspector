//! PyO3 Python bindings for pdf-inspector.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::collections::HashSet;

use crate::detector::PdfType;
use crate::types::ItemType;

// ---------------------------------------------------------------------------
// Result wrapper
// ---------------------------------------------------------------------------

/// Result of processing a PDF file.
#[pyclass(name = "PdfResult")]
#[derive(Clone)]
pub struct PyPdfResult {
    /// The detected PDF type: "text_based", "scanned", "image_based", or "mixed".
    #[pyo3(get)]
    pub pdf_type: String,
    /// Markdown output (None if detect-only or scanned PDF).
    #[pyo3(get)]
    pub markdown: Option<String>,
    /// Total number of pages.
    #[pyo3(get)]
    pub page_count: u32,
    /// Processing time in milliseconds.
    #[pyo3(get)]
    pub processing_time_ms: u64,
    /// 1-indexed page numbers that need OCR.
    #[pyo3(get)]
    pub pages_needing_ocr: Vec<u32>,
    /// Machine-readable OCR reasons by 1-indexed page.
    #[pyo3(get)]
    pub ocr_reasons_by_page: Vec<PyPageOcrReasons>,
    /// Title from PDF metadata.
    #[pyo3(get)]
    pub title: Option<String>,
    /// Detection confidence (0.0-1.0).
    #[pyo3(get)]
    pub confidence: f32,
    /// Whether the layout is complex (tables/columns detected).
    #[pyo3(get)]
    pub is_complex_layout: bool,
    /// Pages with tables detected.
    #[pyo3(get)]
    pub pages_with_tables: Vec<u32>,
    /// Pages with multi-column layout.
    #[pyo3(get)]
    pub pages_with_columns: Vec<u32>,
    /// Whether encoding issues were detected.
    #[pyo3(get)]
    pub has_encoding_issues: bool,
}

#[pymethods]
impl PyPdfResult {
    fn __repr__(&self) -> String {
        format!(
            "PdfResult(pdf_type='{}', pages={}, confidence={:.2})",
            self.pdf_type, self.page_count, self.confidence
        )
    }
}

/// OCR reasons for a single 1-indexed page.
#[pyclass(name = "PageOcrReasons")]
#[derive(Clone)]
pub struct PyPageOcrReasons {
    /// 1-indexed page number.
    #[pyo3(get)]
    pub page: u32,
    /// Machine-readable OCR reason identifiers.
    #[pyo3(get)]
    pub reasons: Vec<String>,
}

#[pymethods]
impl PyPageOcrReasons {
    fn __repr__(&self) -> String {
        format!(
            "PageOcrReasons(page={}, reasons={:?})",
            self.page, self.reasons
        )
    }
}

/// Exact OCR model identity retained in page provenance.
#[pyclass(name = "OcrModelIdentity")]
#[derive(Clone)]
pub struct PyOcrModelIdentity {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub revision: String,
}

/// Per-page OCR processing timings.
#[pyclass(name = "OcrTimings")]
#[derive(Clone)]
pub struct PyOcrTimings {
    #[pyo3(get)]
    pub render_ms: u64,
    #[pyo3(get)]
    pub ocr_ms: u64,
    #[pyo3(get)]
    pub assembly_ms: u64,
}

/// Source, model, confidence, and fallback metadata for one page.
#[pyclass(name = "OcrPageProvenance")]
#[derive(Clone)]
pub struct PyOcrPageProvenance {
    /// 1-indexed page number.
    #[pyo3(get)]
    pub page_number: u32,
    /// "native", "ocr", or "fused".
    #[pyo3(get)]
    pub source: String,
    #[pyo3(get)]
    pub ocr_model: Option<PyOcrModelIdentity>,
    #[pyo3(get)]
    pub render_dpi: Option<f32>,
    #[pyo3(get)]
    pub ocr_confidence: Option<f32>,
    #[pyo3(get)]
    pub timings: PyOcrTimings,
    #[pyo3(get)]
    pub warnings: Vec<String>,
    #[pyo3(get)]
    pub hosted_recommended: bool,
}

/// Final Markdown and provenance for one page.
#[pyclass(name = "OcrPageResult")]
#[derive(Clone)]
pub struct PyOcrPageResult {
    /// 1-indexed page number.
    #[pyo3(get)]
    pub page_number: u32,
    #[pyo3(get)]
    pub markdown: String,
    #[pyo3(get)]
    pub provenance: PyOcrPageProvenance,
}

/// Complete native/OCR Markdown output.
#[pyclass(name = "OcrPdfResult")]
#[derive(Clone)]
pub struct PyOcrPdfResult {
    #[pyo3(get)]
    pub markdown: String,
    #[pyo3(get)]
    pub pages: Vec<PyOcrPageResult>,
    #[pyo3(get)]
    pub page_count: u32,
    #[pyo3(get)]
    pub pages_recommended_for_ocr: Vec<u32>,
    #[pyo3(get)]
    pub pages_routed_to_ocr: Vec<u32>,
    #[pyo3(get)]
    pub pages_recommending_hosted: Vec<u32>,
    #[pyo3(get)]
    pub ocr_reasons_by_page: Vec<PyPageOcrReasons>,
    #[pyo3(get)]
    pub pages_with_tables: Vec<u32>,
    #[pyo3(get)]
    pub pages_with_columns: Vec<u32>,
    #[pyo3(get)]
    pub is_complex: bool,
    #[pyo3(get)]
    pub processing_time_ms: u64,
    #[pyo3(get)]
    pub render_time_ms: u64,
    #[pyo3(get)]
    pub ocr_time_ms: u64,
}

#[pymethods]
impl PyOcrPdfResult {
    fn __repr__(&self) -> String {
        format!(
            "OcrPdfResult(pages={}, routed_to_ocr={:?}, recommending_hosted={:?})",
            self.page_count, self.pages_routed_to_ocr, self.pages_recommending_hosted
        )
    }
}

// ---------------------------------------------------------------------------
// Classification wrapper (lightweight)
// ---------------------------------------------------------------------------

/// Lightweight PDF classification result.
#[pyclass(name = "PdfClassification")]
#[derive(Clone)]
pub struct PyPdfClassification {
    /// The detected PDF type: "text_based", "scanned", "image_based", or "mixed".
    #[pyo3(get)]
    pub pdf_type: String,
    /// Total number of pages.
    #[pyo3(get)]
    pub page_count: u32,
    /// 0-indexed page numbers that need OCR.
    #[pyo3(get)]
    pub pages_needing_ocr: Vec<u32>,
    /// Detection confidence (0.0-1.0).
    #[pyo3(get)]
    pub confidence: f32,
}

#[pymethods]
impl PyPdfClassification {
    fn __repr__(&self) -> String {
        format!(
            "PdfClassification(pdf_type='{}', pages={}, confidence={:.2})",
            self.pdf_type, self.page_count, self.confidence
        )
    }
}

// ---------------------------------------------------------------------------
// Region extraction wrappers
// ---------------------------------------------------------------------------

/// Extracted text for a single region.
#[pyclass(name = "RegionText")]
#[derive(Clone)]
pub struct PyRegionText {
    /// Extracted text content.
    #[pyo3(get)]
    pub text: String,
    /// True when the text should not be trusted (empty, GID fonts, garbage, encoding issues).
    #[pyo3(get)]
    pub needs_ocr: bool,
    /// Machine-readable OCR reason when the cause is known.
    #[pyo3(get)]
    pub ocr_reason: Option<String>,
}

#[pymethods]
impl PyRegionText {
    fn __repr__(&self) -> String {
        format!(
            "RegionText(text='{}', needs_ocr={})",
            self.text.chars().take(40).collect::<String>(),
            self.needs_ocr
        )
    }
}

/// Extracted text for one page's regions.
#[pyclass(name = "PageRegionTexts")]
#[derive(Clone)]
pub struct PyPageRegionTexts {
    /// 0-indexed page number.
    #[pyo3(get)]
    pub page: u32,
    /// Per-region results, parallel to the input regions.
    #[pyo3(get)]
    pub regions: Vec<PyRegionText>,
}

#[pymethods]
impl PyPageRegionTexts {
    fn __repr__(&self) -> String {
        format!(
            "PageRegionTexts(page={}, regions={})",
            self.page,
            self.regions.len()
        )
    }
}

// ---------------------------------------------------------------------------
// Text item wrapper
// ---------------------------------------------------------------------------

/// Per-page markdown extraction result.
#[pyclass(name = "PageMarkdown")]
#[derive(Clone)]
pub struct PyPageMarkdown {
    /// 0-indexed page number.
    #[pyo3(get)]
    pub page: u32,
    /// Formatted markdown for this page.
    #[pyo3(get)]
    pub markdown: String,
    /// True when text on this page is unreliable (GID-encoded fonts,
    /// encoding issues, garbage text, or empty extraction).
    #[pyo3(get)]
    pub needs_ocr: bool,
    /// Machine-readable OCR reason when the cause is known.
    #[pyo3(get)]
    pub ocr_reason: Option<String>,
}

#[pymethods]
impl PyPageMarkdown {
    fn __repr__(&self) -> String {
        format!(
            "PageMarkdown(page={}, markdown='{}', needs_ocr={})",
            self.page,
            self.markdown.chars().take(40).collect::<String>(),
            self.needs_ocr
        )
    }
}

/// Combined per-page markdown extraction and layout classification result.
#[pyclass(name = "PagesExtractionResult")]
#[derive(Clone)]
pub struct PyPagesExtractionResult {
    /// Per-page markdown results, in the order requested.
    #[pyo3(get)]
    pub pages: Vec<PyPageMarkdown>,
    /// 1-indexed pages where tables were detected.
    #[pyo3(get)]
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where multi-column layout was detected.
    #[pyo3(get)]
    pub pages_with_columns: Vec<u32>,
    /// 1-indexed pages that need OCR (scanned/image-based or unreliable text).
    #[pyo3(get)]
    pub pages_needing_ocr: Vec<u32>,
    /// Machine-readable OCR reasons by 1-indexed page.
    #[pyo3(get)]
    pub ocr_reasons_by_page: Vec<PyPageOcrReasons>,
    /// True if any page has tables or columns.
    #[pyo3(get)]
    pub is_complex: bool,
}

#[pymethods]
impl PyPagesExtractionResult {
    fn __repr__(&self) -> String {
        format!(
            "PagesExtractionResult(pages={}, pages_with_tables={:?}, is_complex={})",
            self.pages.len(),
            self.pages_with_tables,
            self.is_complex
        )
    }
}

/// A positioned text item extracted from a PDF.
#[pyclass(name = "TextItem")]
#[derive(Clone)]
pub struct PyTextItem {
    #[pyo3(get)]
    pub text: String,
    #[pyo3(get)]
    pub x: f32,
    #[pyo3(get)]
    pub y: f32,
    #[pyo3(get)]
    pub width: f32,
    #[pyo3(get)]
    pub height: f32,
    #[pyo3(get)]
    pub font: String,
    #[pyo3(get)]
    pub font_tag: String,
    #[pyo3(get)]
    pub font_size: f32,
    #[pyo3(get)]
    pub page: u32,
    #[pyo3(get)]
    pub is_bold: bool,
    #[pyo3(get)]
    pub is_italic: bool,
    #[pyo3(get)]
    pub is_underline: bool,
    #[pyo3(get)]
    pub is_strikeout: bool,
    #[pyo3(get)]
    pub item_type: String,
    /// Marked Content ID from the content stream's BDC/BMC operator, None
    /// when the text is not part of marked content. Join with the
    /// (page, mcid) pairs from extract_structure_elements to attach
    /// structure-tree roles (headings, paragraphs, ...) in tagged PDFs.
    #[pyo3(get)]
    pub mcid: Option<i64>,
}

#[pymethods]
impl PyTextItem {
    fn __repr__(&self) -> String {
        format!(
            "TextItem(text='{}', page={}, x={:.1}, y={:.1})",
            self.text.chars().take(40).collect::<String>(),
            self.page,
            self.x,
            self.y,
        )
    }
}

/// One structure-tree element reference from a tagged PDF.
#[pyclass(name = "StructureElement")]
#[derive(Clone)]
pub struct PyStructureElement {
    /// 1-indexed page number (matches TextItem.page).
    #[pyo3(get)]
    pub page: u32,
    /// Marked Content ID from the page's content stream (matches
    /// TextItem.mcid).
    #[pyo3(get)]
    pub mcid: i64,
    /// Standard structure type name ("H1".."H6", "P", "Table", "TD", ...).
    #[pyo3(get)]
    pub role: String,
}

#[pymethods]
impl PyStructureElement {
    fn __repr__(&self) -> String {
        format!(
            "StructureElement(page={}, mcid={}, role='{}')",
            self.page, self.mcid, self.role
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn pdf_type_str(t: PdfType) -> String {
    match t {
        PdfType::TextBased => "text_based".into(),
        PdfType::Scanned => "scanned".into(),
        PdfType::ImageBased => "image_based".into(),
        PdfType::Mixed => "mixed".into(),
    }
}

fn to_py_result(r: crate::PdfProcessResult) -> PyPdfResult {
    PyPdfResult {
        pdf_type: pdf_type_str(r.pdf_type),
        markdown: r.markdown,
        page_count: r.page_count,
        processing_time_ms: r.processing_time_ms,
        pages_needing_ocr: r.pages_needing_ocr,
        ocr_reasons_by_page: to_py_page_ocr_reasons(r.ocr_reasons_by_page),
        title: r.title,
        confidence: r.confidence,
        is_complex_layout: r.layout.is_complex,
        pages_with_tables: r.layout.pages_with_tables,
        pages_with_columns: r.layout.pages_with_columns,
        has_encoding_issues: r.has_encoding_issues,
    }
}

fn to_py_page_ocr_reasons(reasons: Vec<crate::PageOcrReasons>) -> Vec<PyPageOcrReasons> {
    reasons
        .into_iter()
        .map(|reason| PyPageOcrReasons {
            page: reason.page,
            reasons: reason.reasons,
        })
        .collect()
}

fn to_py_err(e: crate::PdfError) -> PyErr {
    PyValueError::new_err(e.to_string())
}

struct PythonOcrOptions {
    mode: String,
    page_numbers: Option<Vec<u32>>,
    password: Option<String>,
    dpi: f32,
    minimum_confidence: f32,
    hosted_recommendation_confidence: f32,
    model_directory: Option<String>,
    offline: bool,
}

fn build_ocr_options(binding: PythonOcrOptions) -> PyResult<crate::vision::OcrPdfOptions> {
    let mode = match binding.mode.trim().to_ascii_lowercase().as_str() {
        "off" => crate::vision::OcrMode::Off,
        "auto" => crate::vision::OcrMode::Auto,
        "force" => crate::vision::OcrMode::Force,
        _ => {
            return Err(PyValueError::new_err(
                "mode must be 'off', 'auto', or 'force'",
            ));
        }
    };

    let mut options = crate::vision::OcrPdfOptions::new().mode(mode);
    options.render.dpi = binding.dpi;
    options.ocr.minimum_confidence = binding.minimum_confidence;
    options.hosted_recommendation_confidence = binding.hosted_recommendation_confidence;
    if let Some(pages) = binding.page_numbers {
        options = options.page_numbers(pages);
    }
    if let Some(password) = binding.password {
        options = options.password(password);
    }
    if let Some(directory) = binding.model_directory {
        options.ocr.model_directory = Some(directory.into());
    }
    if binding.offline {
        options.ocr.model_downloads = crate::vision::ModelDownloadPolicy::Offline;
    }
    Ok(options)
}

fn page_content_source_str(source: crate::vision::PageContentSource) -> String {
    match source {
        crate::vision::PageContentSource::Native => "native".into(),
        crate::vision::PageContentSource::Ocr => "ocr".into(),
        crate::vision::PageContentSource::Fused => "fused".into(),
    }
}

fn to_py_ocr_result(result: crate::vision::OcrPdfResult) -> PyOcrPdfResult {
    PyOcrPdfResult {
        markdown: result.markdown,
        pages: result
            .pages
            .into_iter()
            .map(|page| {
                let provenance = page.provenance;
                PyOcrPageResult {
                    page_number: page.page_number,
                    markdown: page.markdown,
                    provenance: PyOcrPageProvenance {
                        page_number: provenance.page_number,
                        source: page_content_source_str(provenance.source),
                        ocr_model: provenance.ocr_model.map(|model| PyOcrModelIdentity {
                            name: model.name,
                            revision: model.revision,
                        }),
                        render_dpi: provenance.render_dpi,
                        ocr_confidence: provenance.ocr_confidence,
                        timings: PyOcrTimings {
                            render_ms: provenance.timings.render_ms,
                            ocr_ms: provenance.timings.ocr_ms,
                            assembly_ms: provenance.timings.assembly_ms,
                        },
                        warnings: provenance.warnings,
                        hosted_recommended: provenance.hosted_recommended,
                    },
                }
            })
            .collect(),
        page_count: result.page_count,
        pages_recommended_for_ocr: result.pages_recommended_for_ocr,
        pages_routed_to_ocr: result.pages_routed_to_ocr,
        pages_recommending_hosted: result.pages_recommending_hosted,
        ocr_reasons_by_page: to_py_page_ocr_reasons(result.ocr_reasons_by_page),
        pages_with_tables: result.pages_with_tables,
        pages_with_columns: result.pages_with_columns,
        is_complex: result.is_complex,
        processing_time_ms: result.processing_time_ms,
        render_time_ms: result.render_time_ms,
        ocr_time_ms: result.ocr_time_ms,
    }
}

fn item_type_str(t: &ItemType) -> String {
    match t {
        ItemType::Text => "text".into(),
        ItemType::Image => "image".into(),
        ItemType::Link(url) => format!("link:{url}"),
        ItemType::FormField => "form_field".into(),
    }
}

fn convert_text_items(items: Vec<crate::TextItem>) -> Vec<PyTextItem> {
    items
        .into_iter()
        .map(|item| PyTextItem {
            text: item.text,
            x: item.x,
            y: item.y,
            width: item.width,
            height: item.height,
            font: item.font,
            font_tag: item.font_tag,
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            is_underline: item.is_underline,
            is_strikeout: item.is_strikeout,
            item_type: item_type_str(&item.item_type),
            mcid: item.mcid,
        })
        .collect()
}

fn convert_structure_elements(elements: Vec<crate::StructureElement>) -> Vec<PyStructureElement> {
    elements
        .into_iter()
        .map(|e| PyStructureElement {
            page: e.page,
            mcid: e.mcid,
            role: e.role,
        })
        .collect()
}

fn parse_page_regions(
    page_regions: Vec<(u32, Vec<Vec<f64>>)>,
) -> PyResult<Vec<(u32, Vec<[f32; 4]>)>> {
    page_regions
        .into_iter()
        .map(|(page, regions)| {
            let mut bboxes: Vec<[f32; 4]> = Vec::with_capacity(regions.len());
            for (idx, region) in regions.into_iter().enumerate() {
                if region.len() != 4 {
                    return Err(PyValueError::new_err(format!(
                        "Invalid region at page {page}, index {idx}: expected [x1, y1, x2, y2], got {} values",
                        region.len()
                    )));
                }
                let [x1, y1, x2, y2] = [region[0], region[1], region[2], region[3]];
                if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
                    return Err(PyValueError::new_err(format!(
                        "Invalid region at page {page}, index {idx}: coordinates must be finite numbers"
                    )));
                }
                if x2 < x1 || y2 < y1 {
                    return Err(PyValueError::new_err(format!(
                        "Invalid region at page {page}, index {idx}: expected x2>=x1 and y2>=y1, got [{x1}, {y1}, {x2}, {y2}]"
                    )));
                }
                bboxes.push([x1 as f32, y1 as f32, x2 as f32, y2 as f32]);
            }
            Ok((page, bboxes))
        })
        .collect()
}

fn to_py_pages_result(r: crate::PagesExtractionResult) -> PyPagesExtractionResult {
    PyPagesExtractionResult {
        pages: r
            .pages
            .into_iter()
            .map(|p| PyPageMarkdown {
                page: p.page,
                markdown: p.markdown,
                needs_ocr: p.needs_ocr,
                ocr_reason: p.ocr_reason,
            })
            .collect(),
        pages_with_tables: r.pages_with_tables,
        pages_with_columns: r.pages_with_columns,
        pages_needing_ocr: r.pages_needing_ocr,
        ocr_reasons_by_page: to_py_page_ocr_reasons(r.ocr_reasons_by_page),
        is_complex: r.is_complex,
    }
}

fn convert_region_results(results: Vec<crate::PageRegionResult>) -> Vec<PyPageRegionTexts> {
    results
        .into_iter()
        .map(|page_result| PyPageRegionTexts {
            page: page_result.page,
            regions: page_result
                .regions
                .into_iter()
                .map(|r| PyRegionText {
                    text: r.text,
                    needs_ocr: r.needs_ocr,
                    ocr_reason: r.ocr_reason,
                })
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public Python API
// ---------------------------------------------------------------------------

/// Process a PDF file: detect type, extract text, and convert to Markdown.
#[pyfunction]
#[pyo3(signature = (path, pages=None))]
fn process_pdf(path: &str, pages: Option<Vec<u32>>) -> PyResult<PyPdfResult> {
    let mut opts = crate::PdfOptions::new();
    if let Some(p) = pages {
        opts = opts.pages(p);
    }
    let result = crate::process_pdf_with_options(path, opts).map_err(to_py_err)?;
    Ok(to_py_result(result))
}

/// Process a PDF from bytes in memory.
#[pyfunction]
#[pyo3(signature = (data, pages=None))]
fn process_pdf_bytes(data: &[u8], pages: Option<Vec<u32>>) -> PyResult<PyPdfResult> {
    let mut opts = crate::PdfOptions::new();
    if let Some(p) = pages {
        opts = opts.pages(p);
    }
    let result = crate::process_pdf_mem_with_options(data, opts).map_err(to_py_err)?;
    Ok(to_py_result(result))
}

/// Process a PDF file through native extraction and selective OCR.
///
/// OCR defaults to ``auto`` and only initializes its external runtime and
/// model when native quality signals route at least one page. Page numbers
/// are 1-indexed. The GIL is released for the complete processing call.
#[pyfunction]
#[pyo3(signature = (
    path,
    *,
    mode="auto",
    page_numbers=None,
    password=None,
    dpi=150.0,
    minimum_confidence=0.0,
    hosted_recommendation_confidence=0.5,
    model_directory=None,
    offline=false
))]
#[allow(clippy::too_many_arguments)]
fn process_pdf_with_ocr(
    py: Python<'_>,
    path: String,
    mode: &str,
    page_numbers: Option<Vec<u32>>,
    password: Option<String>,
    dpi: f32,
    minimum_confidence: f32,
    hosted_recommendation_confidence: f32,
    model_directory: Option<String>,
    offline: bool,
) -> PyResult<PyOcrPdfResult> {
    let options = build_ocr_options(PythonOcrOptions {
        mode: mode.to_string(),
        page_numbers,
        password,
        dpi,
        minimum_confidence,
        hosted_recommendation_confidence,
        model_directory,
        offline,
    })?;
    let result = py
        .allow_threads(move || crate::vision::process_pdf_with_ocr(path, options))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(to_py_ocr_result(result))
}

/// Process PDF bytes through native extraction and selective OCR.
///
/// See [`process_pdf_with_ocr`] for options and result semantics.
#[pyfunction]
#[pyo3(signature = (
    data,
    *,
    mode="auto",
    page_numbers=None,
    password=None,
    dpi=150.0,
    minimum_confidence=0.0,
    hosted_recommendation_confidence=0.5,
    model_directory=None,
    offline=false
))]
#[allow(clippy::too_many_arguments)]
fn process_pdf_with_ocr_bytes(
    py: Python<'_>,
    data: &[u8],
    mode: &str,
    page_numbers: Option<Vec<u32>>,
    password: Option<String>,
    dpi: f32,
    minimum_confidence: f32,
    hosted_recommendation_confidence: f32,
    model_directory: Option<String>,
    offline: bool,
) -> PyResult<PyOcrPdfResult> {
    let options = build_ocr_options(PythonOcrOptions {
        mode: mode.to_string(),
        page_numbers,
        password,
        dpi,
        minimum_confidence,
        hosted_recommendation_confidence,
        model_directory,
        offline,
    })?;
    let data = data.to_vec();
    let result = py
        .allow_threads(move || crate::vision::process_pdf_with_ocr_mem(&data, options))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(to_py_ocr_result(result))
}

/// Fast detection only — no text extraction or markdown.
#[pyfunction]
fn detect_pdf(path: &str) -> PyResult<PyPdfResult> {
    let result = crate::detect_pdf(path).map_err(to_py_err)?;
    Ok(to_py_result(result))
}

/// Fast detection from bytes — no text extraction or markdown.
#[pyfunction]
fn detect_pdf_bytes(data: &[u8]) -> PyResult<PyPdfResult> {
    let result = crate::detect_pdf_mem(data).map_err(to_py_err)?;
    Ok(to_py_result(result))
}

/// Lightweight PDF classification — returns type, page count, and OCR pages.
/// Faster than detect_pdf as it skips building the full PdfProcessResult.
/// Pages in pages_needing_ocr are 0-indexed.
#[pyfunction]
fn classify_pdf(path: &str) -> PyResult<PyPdfClassification> {
    let data = std::fs::read(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    classify_pdf_bytes(&data)
}

/// Lightweight PDF classification from bytes.
/// Pages in pages_needing_ocr are 0-indexed.
#[pyfunction]
fn classify_pdf_bytes(data: &[u8]) -> PyResult<PyPdfClassification> {
    let result = crate::classify_pdf_mem(data).map_err(to_py_err)?;
    Ok(PyPdfClassification {
        pdf_type: pdf_type_str(result.pdf_type),
        page_count: result.page_count,
        pages_needing_ocr: result.pages_needing_ocr,
        confidence: result.confidence,
    })
}

/// Extract plain text from a PDF file.
#[pyfunction]
fn extract_text(path: &str) -> PyResult<String> {
    crate::extract_text(path).map_err(to_py_err)
}

/// Extract plain text from PDF bytes.
#[pyfunction]
fn extract_text_bytes(data: &[u8]) -> PyResult<String> {
    crate::extractor::extract_text_mem(data).map_err(to_py_err)
}

/// Extract text with position information from a file.
#[pyfunction]
#[pyo3(signature = (path, pages=None))]
fn extract_text_with_positions(path: &str, pages: Option<Vec<u32>>) -> PyResult<Vec<PyTextItem>> {
    let items = match pages {
        Some(p) => {
            let page_set: HashSet<u32> = p.into_iter().collect();
            crate::extract_text_with_positions_pages(path, Some(&page_set)).map_err(to_py_err)?
        }
        None => crate::extract_text_with_positions(path).map_err(to_py_err)?,
    };
    Ok(convert_text_items(items))
}

/// Extract text with position information from bytes.
#[pyfunction]
#[pyo3(signature = (data, pages=None))]
fn extract_text_with_positions_bytes(
    data: &[u8],
    pages: Option<Vec<u32>>,
) -> PyResult<Vec<PyTextItem>> {
    let items = match pages {
        Some(p) => {
            let page_set: HashSet<u32> = p.into_iter().collect();
            crate::extractor::extract_text_with_positions_mem_pages(data, Some(&page_set))
                .map_err(to_py_err)?
        }
        None => crate::extractor::extract_text_with_positions_mem(data).map_err(to_py_err)?,
    };
    Ok(convert_text_items(items))
}

/// Extract text within bounding-box regions from a PDF file.
///
/// Args:
///     path: Path to the PDF file.
///     page_regions: List of (page_0indexed, [[x1, y1, x2, y2], ...]) tuples.
///         Coordinates are PDF points with top-left origin.
///
/// Returns:
///     List of PageRegionTexts with per-region text and needs_ocr flag.
#[pyfunction]
fn extract_text_in_regions(
    path: &str,
    page_regions: Vec<(u32, Vec<Vec<f64>>)>,
) -> PyResult<Vec<PyPageRegionTexts>> {
    let data = std::fs::read(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    extract_text_in_regions_bytes(&data, page_regions)
}

/// Extract text within bounding-box regions from PDF bytes.
///
/// Args:
///     data: PDF file contents as bytes.
///     page_regions: List of (page_0indexed, [[x1, y1, x2, y2], ...]) tuples.
///         Coordinates are PDF points with top-left origin.
///
/// Returns:
///     List of PageRegionTexts with per-region text and needs_ocr flag.
#[pyfunction]
fn extract_text_in_regions_bytes(
    data: &[u8],
    page_regions: Vec<(u32, Vec<Vec<f64>>)>,
) -> PyResult<Vec<PyPageRegionTexts>> {
    let regions = parse_page_regions(page_regions)?;
    let results = crate::extract_text_in_regions_mem(data, &regions).map_err(to_py_err)?;
    Ok(convert_region_results(results))
}

/// Extract formatted markdown for pages of a PDF file, with layout
/// classification metadata.
///
/// Returns per-page markdown and classification data (tables, columns,
/// OCR needs) from a single parse. Font statistics are computed from the
/// full document so header detection is consistent across pages.
///
/// Args:
///     path: Path to the PDF file.
///     pages: Optional list of 0-indexed pages. When None (default), every
///         page is returned in document order. When provided, output
///         matches the caller-supplied order.
///
/// Returns:
///     PagesExtractionResult with per-page markdown and classification data.
#[pyfunction]
#[pyo3(signature = (path, pages=None))]
fn extract_pages_markdown(
    path: &str,
    pages: Option<Vec<u32>>,
) -> PyResult<PyPagesExtractionResult> {
    let result = crate::extract_pages_markdown(path, pages.as_deref()).map_err(to_py_err)?;
    Ok(to_py_pages_result(result))
}

/// Extract formatted markdown for pages of a PDF from bytes.
///
/// See [`extract_pages_markdown`] for details.
#[pyfunction]
#[pyo3(signature = (data, pages=None))]
fn extract_pages_markdown_bytes(
    data: &[u8],
    pages: Option<Vec<u32>>,
) -> PyResult<PyPagesExtractionResult> {
    let result = crate::extract_pages_markdown_mem(data, pages.as_deref()).map_err(to_py_err)?;
    Ok(to_py_pages_result(result))
}

/// Extract structure-tree element references from a tagged PDF file.
///
/// Parses the document's structure tree (when present) and returns one
/// entry per marked-content reference, resolved to its 1-indexed page,
/// MCID, and structure type name ("H1".."H6", "P", "Table", ...). Returns
/// an empty list when the PDF is not tagged.
///
/// Join (page, mcid) against the page/mcid attributes from
/// [`extract_text_with_positions`] to attach heading levels and other
/// semantic roles to extracted text.
///
/// Args:
///     path: Path to the PDF file.
///     pages: Optional list of 1-indexed pages (matching TextItem.page).
///         When None (default), the whole document is returned.
///
/// Returns:
///     List of StructureElement sorted by (page, mcid).
#[pyfunction]
#[pyo3(signature = (path, pages=None))]
fn extract_structure_elements(
    path: &str,
    pages: Option<Vec<u32>>,
) -> PyResult<Vec<PyStructureElement>> {
    let elements = crate::extract_structure_elements(path, pages.as_deref()).map_err(to_py_err)?;
    Ok(convert_structure_elements(elements))
}

/// Extract structure-tree element references from tagged PDF bytes.
///
/// See [`extract_structure_elements`] for details.
#[pyfunction]
#[pyo3(signature = (data, pages=None))]
fn extract_structure_elements_bytes(
    data: &[u8],
    pages: Option<Vec<u32>>,
) -> PyResult<Vec<PyStructureElement>> {
    let elements =
        crate::extract_structure_elements_mem(data, pages.as_deref()).map_err(to_py_err)?;
    Ok(convert_structure_elements(elements))
}

/// Python module definition.
#[pymodule]
fn pdf_inspector(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPdfResult>()?;
    m.add_class::<PyPageOcrReasons>()?;
    m.add_class::<PyOcrModelIdentity>()?;
    m.add_class::<PyOcrTimings>()?;
    m.add_class::<PyOcrPageProvenance>()?;
    m.add_class::<PyOcrPageResult>()?;
    m.add_class::<PyOcrPdfResult>()?;
    m.add_class::<PyPdfClassification>()?;
    m.add_class::<PyTextItem>()?;
    m.add_class::<PyStructureElement>()?;
    m.add_class::<PyRegionText>()?;
    m.add_class::<PyPageRegionTexts>()?;
    m.add_class::<PyPageMarkdown>()?;
    m.add_class::<PyPagesExtractionResult>()?;
    m.add_function(wrap_pyfunction!(process_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(process_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(process_pdf_with_ocr, m)?)?;
    m.add_function(wrap_pyfunction!(process_pdf_with_ocr_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(detect_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(detect_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(classify_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(classify_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_positions, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_positions_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_structure_elements, m)?)?;
    m.add_function(wrap_pyfunction!(extract_structure_elements_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_in_regions, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_in_regions_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_pages_markdown, m)?)?;
    m.add_function(wrap_pyfunction!(extract_pages_markdown_bytes, m)?)?;
    Ok(())
}
