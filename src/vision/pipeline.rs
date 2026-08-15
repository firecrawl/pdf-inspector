//! One-call native extraction and OCR pipeline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use thiserror::Error;

use crate::text_quality::{
    analyze_text_quality, detect_encoding_issues, is_cid_garbage, is_garbage_text,
};
use crate::{
    MarkdownOptions, PageOcrReasons, PdfError, OCR_REASON_SUSPECTED_GARBLED_TEXT,
    OCR_REASON_VECTOR_TEXT,
};

use super::oar::onnx_runtime_library_path;
use super::pdfium::PdfiumTextPage;
use super::{
    fuse_ocr_pages, route_ocr_pages, run_ocr_pages, FusedPageMarkdown, HttpModelDownloadError,
    HttpModelDownloader, ModelAcquireError, ModelStore, ModelStoreError, OarOcrEngine, OarOcrError,
    OcrFusionError, OcrFusionOptions, OcrMode, OcrOptions, OcrRoutingError, OcrRun, OcrRunError,
    PdfiumRenderer, RenderError, RenderOptions, PP_OCR_V6_SMALL,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct OcrEngineCacheKey {
    model_root: PathBuf,
    runtime_library: PathBuf,
    manifest_schema: u32,
    manifest_id: &'static str,
    manifest_revision: &'static str,
    artifact_digests: Vec<&'static str>,
}

#[derive(Debug)]
struct CachedOcrEngine {
    key: OcrEngineCacheKey,
    engine: Arc<OarOcrEngine>,
}

static OCR_ENGINE_CACHE: OnceLock<Mutex<Option<CachedOcrEngine>>> = OnceLock::new();

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
    let (mut native, page_count) = crate::extract_pages_markdown_mem_for_ocr(
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

    let mut routed = route_ocr_pages(
        options.ocr.mode,
        page_count,
        &native.pages_needing_ocr,
        selected_pages.as_deref(),
    )?;

    let mut renderer = None;
    let mut recovered_natively = BTreeSet::new();
    if options.ocr.mode == OcrMode::Auto {
        let recovery_candidates = native_recovery_candidates(&routed, &native.ocr_reasons_by_page);
        if !recovery_candidates.is_empty() {
            let native_renderer = PdfiumRenderer::load()?;
            let recovered = native_renderer.extract_text_pages(
                buffer,
                &recovery_candidates,
                options.password.as_deref(),
            )?;
            renderer = Some(native_renderer);

            for page in recovered {
                let Some(markdown) =
                    credible_native_recovery(&page.items, page_count, &page_markdown_options)
                else {
                    continue;
                };
                if !is_complete_native_recovery(&markdown) || !native_recovery_covers_page(&page) {
                    continue;
                }
                let Some(native_page) = native
                    .pages
                    .iter_mut()
                    .find(|entry| entry.page + 1 == page.page)
                else {
                    continue;
                };
                native_page.markdown = markdown;
                native_page.needs_ocr = false;
                recovered_natively.insert(page.page);
            }
            routed.retain(|page| !recovered_natively.contains(page));
        }
    }

    let ocr_run = if routed.is_empty() {
        OcrRun {
            pages: Vec::new(),
            render_time_ms: 0,
            ocr_time_ms: 0,
        }
    } else {
        // Resolve the native renderer before any network request so a missing
        // PDFium installation cannot trigger a model download it cannot use.
        let renderer = match renderer {
            Some(renderer) => renderer,
            None => PdfiumRenderer::load()?,
        };
        let engine = cached_ocr_engine(&options.ocr)?;
        run_ocr_pages(
            &renderer,
            engine.as_ref(),
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
    let mut fused = fuse_ocr_pages(&native.pages, &ocr_run, page_count, &fusion_options)?;
    for page in &mut fused.pages {
        if recovered_natively.contains(&page.page) {
            page.provenance
                .warnings
                .push("recovered a credible positioned native text layer before OCR".to_string());
        }
    }
    let pages_recommending_hosted = fused
        .pages
        .iter()
        .filter(|page| page.provenance.hosted_recommended)
        .map(|page| page.provenance.page)
        .collect();
    let markdown = assemble_document_markdown(&fused.pages, options.markdown.include_page_numbers);

    let mut pages_with_tables = native.pages_with_tables;
    for page in &fused.pages {
        if markdown_has_table(&page.markdown) && !pages_with_tables.contains(&page.page) {
            pages_with_tables.push(page.page);
        }
    }
    pages_with_tables.sort_unstable();

    Ok(OcrPdfResult {
        markdown,
        pages: fused.pages,
        page_count,
        pages_recommended_for_ocr: native.pages_needing_ocr,
        pages_routed_to_ocr: routed,
        pages_recommending_hosted,
        ocr_reasons_by_page: native.ocr_reasons_by_page,
        pages_with_tables: pages_with_tables.clone(),
        pages_with_columns: native.pages_with_columns,
        is_complex: native.is_complex || !pages_with_tables.is_empty(),
        processing_time_ms: elapsed_ms(started),
        render_time_ms: fused.render_time_ms,
        ocr_time_ms: fused.ocr_time_ms,
    })
}

fn cached_ocr_engine(options: &OcrOptions) -> Result<Arc<OarOcrEngine>, OcrPipelineError> {
    let store = ModelStore::from_options(options)?;
    let key = OcrEngineCacheKey {
        model_root: normalized_cache_path(store.model_root(&PP_OCR_V6_SMALL)),
        runtime_library: normalized_cache_path(onnx_runtime_library_path()),
        manifest_schema: PP_OCR_V6_SMALL.schema_version,
        manifest_id: PP_OCR_V6_SMALL.id,
        manifest_revision: PP_OCR_V6_SMALL.revision,
        artifact_digests: PP_OCR_V6_SMALL
            .artifacts
            .iter()
            .map(|artifact| artifact.sha256)
            .collect(),
    };
    let cache = OCR_ENGINE_CACHE.get_or_init(|| Mutex::new(None));
    {
        let cached = cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(cached) = cached.as_ref().filter(|cached| cached.key == key) {
            return Ok(Arc::clone(&cached.engine));
        }
    }

    // The loaded sessions own the verified model data, so a cache hit never
    // needs to trust or reopen mutable files on disk. Do the expensive download
    // and session construction outside the process-wide cache lock so unrelated
    // OCR requests can continue using a warm engine. Concurrent cold misses may
    // build redundantly; the second cache check keeps only one shared session.
    let models = store.resolve_or_download(
        &PP_OCR_V6_SMALL,
        options.model_downloads,
        &HttpModelDownloader::default(),
    )?;
    let engine = Arc::new(OarOcrEngine::from_models(&models)?);

    let mut cached = cache.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(cached) = cached.as_ref().filter(|cached| cached.key == key) {
        return Ok(Arc::clone(&cached.engine));
    }
    *cached = Some(CachedOcrEngine {
        key,
        engine: Arc::clone(&engine),
    });
    Ok(engine)
}

fn normalized_cache_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(&path))
            .unwrap_or(path)
    };
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }

    // A managed cache often does not exist on the first request. Resolve the
    // nearest existing ancestor so a relative path has the same key before
    // and after model acquisition creates its final directories.
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while let Some(name) = ancestor.file_name() {
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
        if let Ok(mut canonical) = std::fs::canonicalize(ancestor) {
            for component in suffix.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
    }
    absolute
}

fn native_recovery_candidates(routed: &[u32], reasons: &[PageOcrReasons]) -> Vec<u32> {
    let reasons_by_page: BTreeMap<u32, &PageOcrReasons> =
        reasons.iter().map(|entry| (entry.page, entry)).collect();
    routed
        .iter()
        .copied()
        .filter(|page| {
            reasons_by_page.get(page).is_some_and(|entry| {
                !entry.reasons.is_empty()
                    && entry.reasons.iter().all(|reason| {
                        reason == OCR_REASON_SUSPECTED_GARBLED_TEXT
                            || reason == OCR_REASON_VECTOR_TEXT
                    })
            })
        })
        .collect()
}

fn credible_native_recovery(
    items: &[crate::TextItem],
    document_page_count: u32,
    options: &MarkdownOptions,
) -> Option<String> {
    let text = items
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if is_garbage_text(&text) || is_cid_garbage(&text) {
        return None;
    }

    let markdown = crate::to_markdown_from_items_with_rects_and_page_count(
        items.to_vec(),
        options.clone(),
        &[],
        document_page_count,
    );
    let markdown = remove_duplicate_table_lines(&markdown);
    let structured_uniform_ascii = is_uniform_case_structured_ascii(&text, &markdown);
    let quality = analyze_text_quality(items);
    if !structured_uniform_ascii && (quality.has_encoding_issues || detect_encoding_issues(&text)) {
        return None;
    }
    (!markdown.trim().is_empty()).then_some(markdown)
}

fn is_complete_native_recovery(markdown: &str) -> bool {
    let alphanumeric_chars = markdown
        .chars()
        .filter(|character| character.is_alphanumeric())
        .count();
    if alphanumeric_chars < 40 {
        return false;
    }
    let visible_chars = markdown
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(1);
    let density = alphanumeric_chars as f32 / visible_chars as f32;
    let length_score = (alphanumeric_chars as f32 / 160.0).min(1.0);
    let nonempty_lines = markdown
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        .max(1);
    let line_score = (alphanumeric_chars as f32 / nonempty_lines as f32 / 12.0).min(1.0);
    let score = (0.45 + length_score * 0.25 + density * 0.20 + line_score * 0.10).min(1.0);
    score >= 0.68
}

fn native_recovery_covers_page(page: &PdfiumTextPage) -> bool {
    const VERTICAL_BANDS: f32 = 6.0;

    let width = page.page_width;
    let height = page.page_height;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return false;
    }

    let mut min_left = width;
    let mut max_right = 0.0_f32;
    let mut min_bottom = height;
    let mut max_top = 0.0_f32;
    let mut positioned_items = 0usize;
    let mut occupied_bands = BTreeSet::new();
    for item in &page.items {
        if !item.text.chars().any(char::is_alphanumeric)
            || !item.x.is_finite()
            || !item.y.is_finite()
            || !item.width.is_finite()
            || !item.height.is_finite()
            || item.width <= 0.0
            || item.height <= 0.0
        {
            continue;
        }
        let left = item.x.clamp(0.0, width);
        let right = (item.x + item.width).clamp(0.0, width);
        let bottom = item.y.clamp(0.0, height);
        let top = (item.y + item.height).clamp(0.0, height);
        if right <= left || top <= bottom {
            continue;
        }

        positioned_items += 1;
        min_left = min_left.min(left);
        max_right = max_right.max(right);
        min_bottom = min_bottom.min(bottom);
        max_top = max_top.max(top);
        let center = (bottom + top) * 0.5;
        let band = ((center / height) * VERTICAL_BANDS)
            .floor()
            .clamp(0.0, VERTICAL_BANDS - 1.0) as u8;
        occupied_bands.insert(band);
    }

    positioned_items >= 6
        && (max_right - min_left) / width >= 0.15
        && (max_top - min_bottom) / height >= 0.35
        && occupied_bands.len() >= 3
}

fn is_uniform_case_structured_ascii(text: &str, markdown: &str) -> bool {
    if !text.is_ascii() || text.contains('$') || text.chars().any(char::is_control) {
        return false;
    }

    let letters: Vec<_> = text
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .collect();
    if letters.len() < 200 {
        return false;
    }
    let uniform_case = letters
        .iter()
        .all(|character| character.is_ascii_uppercase())
        || letters
            .iter()
            .all(|character| character.is_ascii_lowercase());
    if !uniform_case {
        return false;
    }
    if markdown_has_table(markdown) {
        return true;
    }

    let nonempty_lines = markdown
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let visible_chars = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(1);
    let structural_chars = text
        .chars()
        .filter(|character| {
            character.is_ascii_digit()
                || matches!(
                    character,
                    '{' | '}'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '_'
                        | '='
                        | '+'
                        | '*'
                        | '/'
                        | '\\'
                        | '|'
                        | '&'
                        | '^'
                        | '%'
                        | '#'
                        | '@'
                        | '~'
                )
        })
        .count();
    nonempty_lines >= 4 && structural_chars * 20 >= visible_chars
}

fn markdown_has_table(markdown: &str) -> bool {
    markdown.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with('|')
            && trimmed.ends_with('|')
            && trimmed
                .split('|')
                .filter(|cell| !cell.is_empty())
                .all(|cell| !cell.is_empty() && cell.chars().all(|ch| ch == '-'))
    })
}

fn remove_duplicate_table_lines(markdown: &str) -> String {
    let mut output = String::new();
    let mut adjacent_table_row = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        let is_table_line = trimmed.starts_with('|') && trimmed.ends_with('|');
        if is_table_line {
            if !trimmed.contains("|---") && trimmed.matches('|').count() >= 4 {
                let canonical = canonical_table_text(trimmed);
                if !canonical.is_empty() {
                    adjacent_table_row = Some(canonical);
                }
            }
        } else if trimmed.is_empty() {
            // Keep adjacency across the blank line emitted after a table.
        } else {
            let duplicate = adjacent_table_row
                .as_ref()
                .is_some_and(|table_row| *table_row == canonical_table_text(trimmed));
            adjacent_table_row = None;
            if duplicate {
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    while output.ends_with("\n\n\n") {
        output.pop();
    }
    output
}

fn canonical_table_text(text: &str) -> String {
    text.replace('|', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

    fn recovery_item(text: &str, x: f32, y: f32, width: f32, height: f32) -> crate::TextItem {
        crate::TextItem {
            text: text.to_string(),
            x,
            y,
            width,
            height,
            font: "PDFium native text".to_string(),
            font_size: height,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn cache_path_is_stable_when_a_relative_directory_is_created() {
        let current = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let temporary = tempfile::tempdir_in(&current).unwrap();
        let relative_root = temporary.path().strip_prefix(&current).unwrap();
        let relative = relative_root.join("models").join("revision");

        let before = normalized_cache_path(relative.clone());
        std::fs::create_dir_all(&relative).unwrap();
        let after = normalized_cache_path(relative);

        assert!(before.is_absolute());
        assert_eq!(before, after);
    }

    #[test]
    fn native_recovery_is_limited_to_recoverable_routing_reasons() {
        let routed = [1, 2, 3, 4];
        let reasons = [
            PageOcrReasons {
                page: 1,
                reasons: vec![OCR_REASON_SUSPECTED_GARBLED_TEXT.to_string()],
            },
            PageOcrReasons {
                page: 2,
                reasons: vec![crate::OCR_REASON_SCANNED.to_string()],
            },
            PageOcrReasons {
                page: 3,
                reasons: vec![OCR_REASON_VECTOR_TEXT.to_string()],
            },
            PageOcrReasons {
                page: 4,
                reasons: vec![
                    OCR_REASON_SUSPECTED_GARBLED_TEXT.to_string(),
                    crate::OCR_REASON_SCANNED.to_string(),
                ],
            },
        ];

        assert_eq!(native_recovery_candidates(&routed, &reasons), vec![1, 3]);
    }

    #[test]
    fn native_recovery_requires_text_coverage_beyond_a_header() {
        let header = PdfiumTextPage {
            page: 1,
            page_width: 600.0,
            page_height: 800.0,
            items: (0..8)
                .map(|index| recovery_item("HEADER", index as f32 * 60.0, 740.0, 50.0, 12.0))
                .collect(),
        };
        assert!(!native_recovery_covers_page(&header));

        let complete = PdfiumTextPage {
            page: 1,
            page_width: 600.0,
            page_height: 800.0,
            items: vec![
                recovery_item("Top one", 40.0, 700.0, 180.0, 12.0),
                recovery_item("Top two", 260.0, 680.0, 180.0, 12.0),
                recovery_item("Middle one", 40.0, 400.0, 180.0, 12.0),
                recovery_item("Middle two", 260.0, 380.0, 180.0, 12.0),
                recovery_item("Bottom one", 40.0, 100.0, 180.0, 12.0),
                recovery_item("Bottom two", 260.0, 80.0, 180.0, 12.0),
            ],
        };
        assert!(native_recovery_covers_page(&complete));
    }

    #[test]
    fn uniform_case_guard_requires_structured_content() {
        let table_text = "STATUS CODE 100 READY ".repeat(20);
        let table_markdown = "|STATUS|CODE|\n|---|---|\n|READY|100|\n|READY|200|\n|READY|300|\n";
        assert!(is_uniform_case_structured_ascii(
            &table_text,
            table_markdown
        ));

        let prose = "THIS IS ORDINARY UPPERCASE PROSE WITH NATURAL WORDS ".repeat(20);
        let prose_markdown = prose
            .split_whitespace()
            .collect::<Vec<_>>()
            .chunks(8)
            .map(|line| line.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!is_uniform_case_structured_ascii(&prose, &prose_markdown));
    }

    #[test]
    fn recovered_markdown_drops_plain_duplicates_of_table_rows() {
        let markdown = "|Date|Value|Status|\n|---|---|---|\n|April 1|42|ok|\n\nApril 1 42 ok\n";

        assert_eq!(
            remove_duplicate_table_lines(markdown),
            "|Date|Value|Status|\n|---|---|---|\n|April 1|42|ok|\n\n"
        );
    }

    #[test]
    fn recovered_markdown_keeps_nonadjacent_repeated_table_text() {
        let markdown = "|Date|Value|Status|\n|---|---|---|\n|April 1|42|ok|\n\nSummary follows.\n\nApril 1 42 ok\n";

        assert_eq!(remove_duplicate_table_lines(markdown), markdown);
    }

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
