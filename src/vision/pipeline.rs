//! One-call native extraction and OCR pipeline.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use thiserror::Error;

use crate::{MarkdownOptions, PageOcrReasons, PdfError};

use super::{
    fuse_ocr_pages, route_ocr_pages, run_ocr_pages, FusedPageMarkdown, HttpModelDownloadError,
    HttpModelDownloader, ModelAcquireError, ModelStore, ModelStoreError, OarOcrEngine, OarOcrError,
    OcrFusionError, OcrFusionOptions, OcrMode, OcrOptions, OcrRoutingError, OcrRun, OcrRunError,
    PdfiumRenderer, RenderError, RenderOptions, PP_OCR_V6_SMALL,
};

/// Options for native extraction with optional OCR.
#[derive(Clone)]
pub struct OcrPdfOptions {
    /// Page rasterization settings used when OCR is routed.
    pub render: RenderOptions,
    /// OCR routing, model, and recognition settings.
    pub ocr: OcrOptions,
    /// Markdown formatting shared by native and OCR assembly.
    pub markdown: MarkdownOptions,
    /// Optional 1-indexed page selection. `None` processes the full document.
    pub page_filter: Option<BTreeSet<u32>>,
    /// Password for an encrypted PDF.
    pub password: Option<String>,
    /// Weak OCR threshold for recommending the hosted pipeline.
    pub hosted_recommendation_confidence: f32,
}

impl Default for OcrPdfOptions {
    fn default() -> Self {
        Self {
            render: RenderOptions::default(),
            ocr: OcrOptions::default(),
            markdown: MarkdownOptions::default(),
            page_filter: None,
            password: None,
            hosted_recommendation_confidence: 0.5,
        }
    }
}

impl std::fmt::Debug for OcrPdfOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OcrPdfOptions")
            .field("render", &self.render)
            .field("ocr", &self.ocr)
            .field("markdown", &self.markdown)
            .field("page_filter", &self.page_filter)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field(
                "hosted_recommendation_confidence",
                &self.hosted_recommendation_confidence,
            )
            .finish()
    }
}

impl OcrPdfOptions {
    /// Creates options with OCR disabled, preserving the native-only path.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces page rasterization settings.
    pub fn render(mut self, render: RenderOptions) -> Self {
        self.render = render;
        self
    }

    /// Replaces OCR routing and recognition settings.
    pub fn ocr(mut self, ocr: OcrOptions) -> Self {
        self.ocr = ocr;
        self
    }

    /// Sets OCR routing without changing the remaining OCR settings.
    pub fn mode(mut self, mode: OcrMode) -> Self {
        self.ocr.mode = mode;
        self
    }

    /// Replaces Markdown formatting options.
    pub fn markdown(mut self, markdown: MarkdownOptions) -> Self {
        self.markdown = markdown;
        self
    }

    /// Restricts processing to 1-indexed pages in ascending order.
    pub fn pages(mut self, pages: impl IntoIterator<Item = u32>) -> Self {
        self.page_filter = Some(pages.into_iter().collect());
        self
    }

    /// Sets the password used to decrypt the PDF.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Sets the weak-OCR threshold for recommending hosted document parsing.
    pub fn hosted_recommendation_confidence(mut self, confidence: f32) -> Self {
        self.hosted_recommendation_confidence = confidence;
        self
    }
}

/// Complete native/OCR Markdown output for a PDF request.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPdfResult {
    /// Final document Markdown in selected-page order.
    pub markdown: String,
    /// Final per-page Markdown and provenance, using 1-indexed page numbers.
    pub pages: Vec<FusedPageMarkdown>,
    /// Total pages in the PDF, independent of page selection.
    pub page_count: u32,
    /// 1-indexed selected pages recommended for OCR by native extraction.
    pub pages_recommended_for_ocr: Vec<u32>,
    /// 1-indexed pages actually rendered and recognized.
    pub pages_routed_to_ocr: Vec<u32>,
    /// 1-indexed pages whose OCR result recommends hosted document parsing.
    pub pages_recommending_hosted: Vec<u32>,
    /// Original machine-readable OCR reasons for selected pages.
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    /// Selected pages where deterministic table detection found tables.
    pub pages_with_tables: Vec<u32>,
    /// Selected pages where deterministic layout found multiple columns.
    pub pages_with_columns: Vec<u32>,
    /// Whether deterministic extraction found tables or columns.
    pub is_complex: bool,
    /// End-to-end processing time.
    pub processing_time_ms: u64,
    /// Batch page-rendering time; zero when no OCR work was routed.
    pub render_time_ms: u64,
    /// Batch OCR time; zero when no OCR work was routed.
    pub ocr_time_ms: u64,
}

/// Processes a PDF file through native extraction and selective OCR.
pub fn process_pdf_with_ocr(
    path: impl AsRef<Path>,
    options: OcrPdfOptions,
) -> Result<OcrPdfResult, OcrPipelineError> {
    let bytes = std::fs::read(path).map_err(PdfError::from)?;
    process_pdf_with_ocr_mem(&bytes, options)
}

/// Processes PDF bytes through native extraction and selective OCR.
///
/// Native extraction always runs first. `Auto` initializes PDFium, downloads
/// models, and starts OAR only if the detector selected at least one page.
/// `Off` therefore has no renderer, model-cache, network, or inference side
/// effects even though the complete feature is compiled into the application.
pub fn process_pdf_with_ocr_mem(
    buffer: &[u8],
    options: OcrPdfOptions,
) -> Result<OcrPdfResult, OcrPipelineError> {
    OcrFusionOptions::new()
        .render_dpi(options.render.dpi)
        .hosted_recommendation_confidence(options.hosted_recommendation_confidence)
        .validate()?;
    let minimum_confidence = options.ocr.minimum_confidence;
    if !minimum_confidence.is_finite() || !(0.0..=1.0).contains(&minimum_confidence) {
        return Err(OcrPipelineError::InvalidMinimumConfidence {
            value: minimum_confidence,
        });
    }
    if options
        .page_filter
        .as_ref()
        .is_some_and(|pages| pages.contains(&0))
    {
        return Err(OcrPipelineError::InvalidSelectedPage { page: 0 });
    }

    let started = Instant::now();
    let selected_pages: Option<Vec<u32>> = options
        .page_filter
        .as_ref()
        .map(|pages| pages.iter().copied().collect());
    let selected_pages_zero_indexed: Option<Vec<u32>> = selected_pages
        .as_ref()
        .map(|pages| pages.iter().map(|page| page - 1).collect());

    let mut page_markdown_options = options.markdown.clone();
    page_markdown_options.include_page_numbers = false;
    let (native, page_count) = crate::extract_pages_markdown_mem_for_ocr(
        buffer,
        selected_pages_zero_indexed.as_deref(),
        options.password.as_deref(),
        &page_markdown_options,
    )?;
    if let Some(invalid) = selected_pages
        .as_ref()
        .and_then(|pages| pages.iter().copied().find(|page| *page > page_count))
    {
        return Err(OcrPipelineError::InvalidSelectedPage { page: invalid });
    }

    let routed = route_ocr_pages(
        options.ocr.mode,
        page_count,
        &native.pages_needing_ocr,
        selected_pages.as_deref(),
    )?;

    let ocr_run = if routed.is_empty() {
        OcrRun {
            pages: Vec::new(),
            render_time_ms: 0,
            ocr_time_ms: 0,
        }
    } else {
        // Resolve the native renderer before any network request so a missing
        // PDFium installation cannot trigger a model download it cannot use.
        let renderer = PdfiumRenderer::load()?;
        let store = ModelStore::from_options(&options.ocr)?;
        let models = store.resolve_or_download(
            &PP_OCR_V6_SMALL,
            options.ocr.model_downloads,
            &HttpModelDownloader::default(),
        )?;
        let engine = OarOcrEngine::from_models(&models)?;
        run_ocr_pages(
            &renderer,
            &engine,
            buffer,
            &routed,
            options.password.as_deref(),
            &options.render,
            &options.ocr,
        )?
    };

    let fusion_options = OcrFusionOptions::new()
        .markdown(page_markdown_options)
        .render_dpi(options.render.dpi)
        .hosted_recommendation_confidence(options.hosted_recommendation_confidence);
    let fused = fuse_ocr_pages(&native.pages, &ocr_run, page_count, &fusion_options)?;
    let pages_recommending_hosted = fused
        .pages
        .iter()
        .filter(|page| page.provenance.hosted_recommended)
        .map(|page| page.provenance.page)
        .collect();
    let markdown = assemble_document_markdown(&fused.pages, options.markdown.include_page_numbers);

    Ok(OcrPdfResult {
        markdown,
        pages: fused.pages,
        page_count,
        pages_recommended_for_ocr: native.pages_needing_ocr,
        pages_routed_to_ocr: routed,
        pages_recommending_hosted,
        ocr_reasons_by_page: native.ocr_reasons_by_page,
        pages_with_tables: native.pages_with_tables,
        pages_with_columns: native.pages_with_columns,
        is_complex: native.is_complex,
        processing_time_ms: elapsed_ms(started),
        render_time_ms: fused.render_time_ms,
        ocr_time_ms: fused.ocr_time_ms,
    })
}

fn assemble_document_markdown(pages: &[FusedPageMarkdown], include_page_numbers: bool) -> String {
    let mut document = String::new();
    for (index, page) in pages.iter().enumerate() {
        if index > 0 {
            document.push_str("\n\n");
        }
        if include_page_numbers {
            document.push_str(&format!("<!-- Page {} -->\n\n", page.page));
        }
        document.push_str(page.markdown.trim());
    }
    if !document.is_empty() {
        document.push('\n');
    }
    document
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Failures from the complete OCR pipeline.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OcrPipelineError {
    /// PDF loading or native extraction failed.
    #[error(transparent)]
    Pdf(#[from] PdfError),
    /// Page routing rejected an invalid request.
    #[error(transparent)]
    Routing(#[from] OcrRoutingError),
    /// The model cache could not be located or initialized.
    #[error(transparent)]
    ModelStore(#[from] ModelStoreError),
    /// A pinned model set could not be resolved or acquired.
    #[error(transparent)]
    ModelAcquire(#[from] ModelAcquireError<HttpModelDownloadError>),
    /// PDFium could not load or rasterize the request.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// The OAR engine could not initialize.
    #[error(transparent)]
    Oar(#[from] OarOcrError),
    /// Selective rendering or OCR execution failed.
    #[error(transparent)]
    Run(#[from] OcrRunError),
    /// OCR/native Markdown fusion failed.
    #[error(transparent)]
    Fusion(#[from] OcrFusionError),
    /// Page zero is invalid because public page selections are 1-indexed.
    #[error("selected page {page} is invalid; page numbers are 1-indexed")]
    InvalidSelectedPage {
        /// Invalid page number.
        page: u32,
    },
    /// OCR span confidence is outside the inclusive 0–1 range.
    #[error("minimum OCR confidence must be between 0 and 1, got {value}")]
    InvalidMinimumConfidence {
        /// Invalid value.
        value: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_extracts_native_text_without_runtime_side_effects() {
        let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();
        let result = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new()).unwrap();

        assert_eq!(result.page_count, 3);
        assert_eq!(result.pages.len(), 3);
        assert!(result.markdown.contains("Thermodynamic Properties"));
        assert!(result.pages_recommended_for_ocr.is_empty());
        assert!(result.pages_routed_to_ocr.is_empty());
        assert!(result.pages_recommending_hosted.is_empty());
        assert_eq!(result.render_time_ms, 0);
        assert_eq!(result.ocr_time_ms, 0);
    }

    #[test]
    fn auto_mode_does_not_load_pdfium_or_models_for_clean_pdf() {
        let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();
        let result =
            process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().mode(OcrMode::Auto)).unwrap();

        assert!(result.pages_routed_to_ocr.is_empty());
        assert!(result.markdown.contains("Freon 12"));
    }

    #[test]
    fn selection_and_page_markers_use_public_one_indexed_pages() {
        let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();
        let mut markdown = MarkdownOptions::default();
        markdown.include_page_numbers = true;
        let result =
            process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().pages([2]).markdown(markdown))
                .unwrap();

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].page, 2);
        assert_eq!(result.pages[0].page, result.pages[0].provenance.page);
        assert!(result.markdown.starts_with("<!-- Page 2 -->"));
    }

    #[test]
    fn off_mode_marks_unprocessed_scan_for_hosted_fallback() {
        let bytes = std::fs::read("tests/fixtures/scan_with_native_header_text.pdf").unwrap();
        let result = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new()).unwrap();

        assert!(result.pages_routed_to_ocr.is_empty());
        assert_eq!(result.pages_recommending_hosted, vec![1]);
    }

    #[test]
    fn password_is_redacted_and_used_for_native_extraction() {
        let options = OcrPdfOptions::new().password("secret123");
        assert!(!format!("{options:?}").contains("secret123"));

        let bytes = std::fs::read("tests/fixtures/encrypted-secret123.pdf").unwrap();
        let result = process_pdf_with_ocr_mem(&bytes, options).unwrap();
        assert!(result.markdown.contains("Procurement"));
    }

    #[test]
    fn rejects_out_of_range_selection_even_with_ocr_off() {
        let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();
        let error = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().pages([4])).unwrap_err();
        assert!(matches!(
            error,
            OcrPipelineError::InvalidSelectedPage { page: 4 }
        ));
    }

    #[test]
    fn invalid_expensive_options_fail_before_pdf_or_runtime_access() {
        let mut invalid_dpi = OcrPdfOptions::new();
        invalid_dpi.render.dpi = f32::NAN;
        assert!(matches!(
            process_pdf_with_ocr_mem(b"not a PDF", invalid_dpi),
            Err(OcrPipelineError::Fusion(
                OcrFusionError::InvalidRenderDpi { .. }
            ))
        ));

        let invalid_hosted = OcrPdfOptions::new().hosted_recommendation_confidence(1.1);
        assert!(matches!(
            process_pdf_with_ocr_mem(b"not a PDF", invalid_hosted),
            Err(OcrPipelineError::Fusion(
                OcrFusionError::InvalidHostedConfidence { .. }
            ))
        ));
    }
}
