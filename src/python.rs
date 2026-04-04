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
    pub font_size: f32,
    #[pyo3(get)]
    pub page: u32,
    #[pyo3(get)]
    pub is_bold: bool,
    #[pyo3(get)]
    pub is_italic: bool,
    #[pyo3(get)]
    pub item_type: String,
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
        title: r.title,
        confidence: r.confidence,
        is_complex_layout: r.layout.is_complex,
        pages_with_tables: r.layout.pages_with_tables,
        pages_with_columns: r.layout.pages_with_columns,
        has_encoding_issues: r.has_encoding_issues,
    }
}

fn to_py_err(e: crate::PdfError) -> PyErr {
    PyValueError::new_err(e.to_string())
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
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            item_type: item_type_str(&item.item_type),
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

/// Python module definition.
#[pymodule]
fn pdf_inspector(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPdfResult>()?;
    m.add_class::<PyPdfClassification>()?;
    m.add_class::<PyTextItem>()?;
    m.add_class::<PyRegionText>()?;
    m.add_class::<PyPageRegionTexts>()?;
    m.add_function(wrap_pyfunction!(process_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(process_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(detect_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(detect_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(classify_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(classify_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_positions, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_positions_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_in_regions, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_in_regions_bytes, m)?)?;
    Ok(())
}
