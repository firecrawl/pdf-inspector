//! UniFFI bindings for pdf-inspector.

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPdfType {
    TextBased,
    Scanned,
    ImageBased,
    Mixed,
}

impl From<crate::PdfType> for FfiPdfType {
    fn from(t: crate::PdfType) -> Self {
        match t {
            crate::PdfType::TextBased => FfiPdfType::TextBased,
            crate::PdfType::Scanned => FfiPdfType::Scanned,
            crate::PdfType::ImageBased => FfiPdfType::ImageBased,
            crate::PdfType::Mixed => FfiPdfType::Mixed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPageOcrReasons {
    /// 1-indexed page number.
    pub page: u32,
    /// Machine-readable OCR reason identifiers.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, uniffi::Record)]
pub struct FfiLayoutComplexity {
    /// True if any page has tables or multi-column text.
    pub is_complex: bool,
    /// 1-indexed pages where table borders were detected.
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where 2+ text columns were detected.
    pub pages_with_columns: Vec<u32>,
}

impl From<crate::LayoutComplexity> for FfiLayoutComplexity {
    fn from(l: crate::LayoutComplexity) -> Self {
        FfiLayoutComplexity {
            is_complex: l.is_complex,
            pages_with_tables: l.pages_with_tables,
            pages_with_columns: l.pages_with_columns,
        }
    }
}

#[derive(uniffi::Record, Clone)]
pub struct FfiPdfResult {
    /// The detected PDF type.
    pub pdf_type: FfiPdfType,
    /// Markdown output (None if detect-only or scanned PDF).
    pub markdown: Option<String>,
    /// Total number of pages.
    pub page_count: u32,
    /// Processing time in milliseconds.
    pub processing_time_ms: u64,
    /// 1-indexed page numbers that need OCR.
    pub pages_needing_ocr: Vec<u32>,
    /// Machine-readable OCR reasons by 1-indexed page.
    pub ocr_reasons_by_page: Vec<FfiPageOcrReasons>,
    /// Title from PDF metadata.
    pub title: Option<String>,
    /// Detection confidence (0.0-1.0).
    pub confidence: f32,
    /// Layout complexity analysis (tables, multi-column detection).
    pub layout: FfiLayoutComplexity,
    /// Whether encoding issues were detected.
    pub has_encoding_issues: bool,
}

impl From<crate::PdfProcessResult> for FfiPdfResult {
    fn from(r: crate::PdfProcessResult) -> Self {
        FfiPdfResult {
            pdf_type: r.pdf_type.into(),
            markdown: r.markdown,
            page_count: r.page_count,
            processing_time_ms: r.processing_time_ms,
            pages_needing_ocr: r.pages_needing_ocr,
            ocr_reasons_by_page: r
                .ocr_reasons_by_page
                .into_iter()
                .map(|reason| FfiPageOcrReasons {
                    page: reason.page,
                    reasons: reason.reasons,
                })
                .collect(),
            title: r.title,
            confidence: r.confidence,
            layout: r.layout.into(),
            has_encoding_issues: r.has_encoding_issues,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPdfClassification {
    /// The detected PDF type.
    pub pdf_type: FfiPdfType,
    /// Total number of pages.
    pub page_count: u32,
    /// 0-indexed page numbers that need OCR.
    pub pages_needing_ocr: Vec<u32>,
    /// Detection confidence (0.0-1.0).
    pub confidence: f32,
}

impl From<crate::PdfClassification> for FfiPdfClassification {
    fn from(r: crate::PdfClassification) -> Self {
        FfiPdfClassification {
            pdf_type: r.pdf_type.into(),
            page_count: r.page_count,
            pages_needing_ocr: r.pages_needing_ocr,
            confidence: r.confidence,
        }
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    #[error("{0}")]
    Msg(String),
}

impl From<crate::PdfError> for FfiError {
    fn from(e: crate::PdfError) -> Self {
        FfiError::Msg(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Exported functions
// ---------------------------------------------------------------------------

/// Process a PDF file: detect type, extract text, and convert to Markdown.
#[uniffi::export]
pub fn process_pdf(path: String) -> Result<FfiPdfResult, FfiError> {
    let result = crate::process_pdf_with_options(path, crate::PdfOptions::new())?;
    Ok(result.into())
}

/// Process a PDF from bytes in memory.
#[uniffi::export]
pub fn process_pdf_bytes(data: Vec<u8>) -> Result<FfiPdfResult, FfiError> {
    let result = crate::process_pdf_mem_with_options(&data, crate::PdfOptions::new())?;
    Ok(result.into())
}

/// Fast detection only — no text extraction or markdown.
#[uniffi::export]
pub fn detect_pdf(path: String) -> Result<FfiPdfResult, FfiError> {
    let result = crate::detect_pdf(path)?;
    Ok(result.into())
}

/// Fast detection from bytes — no text extraction or markdown.
#[uniffi::export]
pub fn detect_pdf_bytes(data: Vec<u8>) -> Result<FfiPdfResult, FfiError> {
    let result = crate::detect_pdf_mem(&data)?;
    Ok(result.into())
}

/// Lightweight PDF classification for routing decisions.
#[uniffi::export]
pub fn classify_pdf(path: String) -> Result<FfiPdfClassification, FfiError> {
    let data = std::fs::read(&path).map_err(crate::PdfError::from)?;
    classify_pdf_bytes(data)
}

/// Lightweight PDF classification from bytes.
#[uniffi::export]
pub fn classify_pdf_bytes(data: Vec<u8>) -> Result<FfiPdfClassification, FfiError> {
    let result = crate::classify_pdf_mem(&data)?;
    Ok(result.into())
}

/// Extract plain text from a PDF file.
#[uniffi::export]
pub fn extract_text(path: String) -> Result<String, FfiError> {
    Ok(crate::extract_text(path)?)
}

/// Extract plain text from PDF bytes.
#[uniffi::export]
pub fn extract_text_bytes(data: Vec<u8>) -> Result<String, FfiError> {
    Ok(crate::extractor::extract_text_mem(&data)?)
}
