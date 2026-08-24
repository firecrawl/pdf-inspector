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
    MarkdownOptions, PageMarkdown, PageOcrReasons, PdfError, OCR_REASON_SUSPECTED_GARBLED_TEXT,
    OCR_REASON_VECTOR_TEXT,
};

use super::fusion::{
    assess_native_candidate, fuse_ocr_pages_adaptive_with_routes, NativeCandidateOrigin,
    NativeFallbackCandidate, OcrFusionRoute,
};
use super::image_analysis::region_has_document_like_ink;
use super::oar::onnx_runtime_library_path;
use super::pdfium::PdfiumTextPage;
use super::{
    route_ocr_pages, run_ocr_pages, FusedPageMarkdown, FusedPages, HttpModelDownloadError,
    HttpModelDownloader, ModelAcquireError, ModelStore, ModelStoreError, OarOcrEngine, OarOcrError,
    OcrEngine, OcrFusionError, OcrFusionOptions, OcrMode, OcrOptions, OcrRoutingError, OcrRun,
    OcrRunError, PageRenderer, PdfiumRenderer, RenderError, RenderOptions, PP_OCR_V6_SMALL,
};

/// Bounds live rendered-page memory while preserving small OCR batches.
const OCR_PAGE_CHUNK_SIZE: usize = 4;

/// Pages rendered and held in memory per OCR batch. A parallel engine gets
/// three waves of work per batch so its workers are not starved at chunk
/// barriers; a sequential engine keeps the small memory-bounding default.
/// Engine-reported concurrency is a trait hook, so it is clamped before
/// sizing anything from it — this helper exists to bound rendered-page
/// memory and must not let an engine inflate it arbitrarily.
fn ocr_page_chunk_size(engine_concurrency: usize) -> usize {
    const MAX_ENGINE_CONCURRENCY: usize = 8;
    (engine_concurrency.clamp(1, MAX_ENGINE_CONCURRENCY) * 3).max(OCR_PAGE_CHUNK_SIZE)
}

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
    pub page_numbers: Option<BTreeSet<u32>>,
    /// Password for an encrypted PDF.
    pub password: Option<String>,
    /// Weak OCR threshold for recommending the hosted pipeline.
    ///
    /// Pages with incomplete native coverage may also recommend the hosted
    /// pipeline when confident OCR only duplicates the retained fragment.
    pub hosted_recommendation_confidence: f32,
}

impl Default for OcrPdfOptions {
    fn default() -> Self {
        Self {
            render: RenderOptions::default(),
            ocr: OcrOptions::default(),
            markdown: MarkdownOptions::default(),
            page_numbers: None,
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
            .field("page_numbers", &self.page_numbers)
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

    /// Creates options with selective OCR enabled for recommended pages.
    pub fn auto() -> Self {
        Self::default().mode(OcrMode::Auto)
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
    pub fn page_numbers(mut self, pages: impl IntoIterator<Item = u32>) -> Self {
        self.page_numbers = Some(pages.into_iter().collect());
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
        .page_numbers
        .as_ref()
        .is_some_and(|pages| pages.contains(&0))
    {
        return Err(OcrPipelineError::InvalidSelectedPage { page: 0 });
    }

    let started = Instant::now();
    let selected_pages: Option<Vec<u32>> = options
        .page_numbers
        .as_ref()
        .map(|pages| pages.iter().copied().collect());
    let selected_pages_zero_indexed: Option<Vec<u32>> = selected_pages
        .as_ref()
        .map(|pages| pages.iter().map(|page| page - 1).collect());

    let mut page_markdown_options = options.markdown.clone();
    page_markdown_options.include_page_numbers = false;
    let extraction = crate::extract_pages_markdown_mem_for_ocr(
        buffer,
        selected_pages_zero_indexed.as_deref(),
        options.password.as_deref(),
        &page_markdown_options,
    )?;
    let mut native = extraction.result;
    let page_count = extraction.page_count;
    let extracted_supplemental_regions = extraction.supplemental_ocr_regions;
    if let Some(invalid) = selected_pages
        .as_ref()
        .and_then(|pages| pages.iter().copied().find(|page| *page > page_count))
    {
        return Err(OcrPipelineError::InvalidSelectedPage { page: invalid });
    }

    let initially_routed = route_ocr_pages(
        options.ocr.mode,
        page_count,
        &native.pages_needing_ocr,
        selected_pages.as_deref(),
    )?;

    // The OCR extractor retains clean native fragments on pages that still
    // require OCR. Remove them from the ordinary native result now so Off mode
    // preserves the existing suppression contract, while keeping a private
    // candidate for post-OCR comparison and geometry-aware fusion.
    let mut native_candidates = BTreeMap::<u32, NativeFallbackCandidate>::new();
    for page in &mut native.pages {
        if !page.needs_ocr || page.markdown.trim().is_empty() {
            continue;
        }
        let page_number = page.page + 1;
        let markdown = std::mem::take(&mut page.markdown);
        if may_preserve_extractor_candidate(page_number, &native.ocr_reasons_by_page) {
            if let Some(candidate) =
                assess_native_candidate(markdown, NativeCandidateOrigin::Extractor)
            {
                native_candidates.insert(page_number, candidate);
            }
        }
    }

    let mut fusion_routes: BTreeMap<u32, OcrFusionRoute> = initially_routed
        .iter()
        .map(|page| (*page, OcrFusionRoute::FullPage))
        .collect();
    if options.ocr.mode == OcrMode::Auto {
        for (&page, regions) in &extracted_supplemental_regions {
            fusion_routes
                .entry(page)
                .or_insert_with(|| OcrFusionRoute::SupplementalRegions(regions.clone()));
        }
    }
    let mut renderer = None;
    let mut recovered_natively = BTreeSet::new();
    if options.ocr.mode == OcrMode::Auto {
        let complete_recovery_candidates =
            native_recovery_candidates(&initially_routed, &native.ocr_reasons_by_page);
        let mut native_probe_pages: BTreeSet<u32> =
            complete_recovery_candidates.iter().copied().collect();
        native_probe_pages.extend(
            native_candidates
                .keys()
                .filter(|page| fusion_routes.contains_key(page))
                .copied(),
        );
        if !native_probe_pages.is_empty() {
            let native_renderer = PdfiumRenderer::load()?;
            let recovered = native_renderer.extract_text_pages(
                buffer,
                &native_probe_pages.iter().copied().collect::<Vec<_>>(),
                options.password.as_deref(),
            )?;
            renderer = Some(native_renderer);

            for page in recovered {
                let Some(markdown) =
                    credible_native_recovery(&page.items, page_count, &page_markdown_options)
                else {
                    continue;
                };
                let Some(candidate) =
                    assess_native_candidate(markdown, NativeCandidateOrigin::Pdfium)
                else {
                    continue;
                };
                let may_skip_ocr = complete_recovery_candidates.contains(&page.page)
                    && candidate.is_complete_recovery()
                    && native_recovery_covers_page(&page);
                if may_skip_ocr {
                    let Some(native_page) = native
                        .pages
                        .iter_mut()
                        .find(|entry| entry.page + 1 == page.page)
                    else {
                        continue;
                    };
                    native_page.markdown = candidate.markdown().to_string();
                    native_page.needs_ocr = false;
                    native_candidates.remove(&page.page);
                    if let Some(route) =
                        route_after_native_recovery(page.page, &extracted_supplemental_regions)
                    {
                        fusion_routes.insert(page.page, route);
                    } else {
                        fusion_routes.remove(&page.page);
                    }
                    recovered_natively.insert(page.page);
                } else {
                    match native_candidates.entry(page.page) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(candidate);
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            if candidate.is_stronger_than(entry.get()) {
                                entry.insert(candidate);
                            }
                        }
                    }
                }
            }
        }
    }

    let supplemental_preflight_ms = if options.ocr.mode == OcrMode::Auto
        && fusion_routes
            .values()
            .any(|route| matches!(route, OcrFusionRoute::SupplementalRegions(_)))
    {
        let native_renderer = match renderer {
            Some(renderer) => renderer,
            None => PdfiumRenderer::load()?,
        };
        let elapsed = filter_supplemental_routes_by_pixels(
            &native_renderer,
            buffer,
            options.password.as_deref(),
            &options.render,
            &mut fusion_routes,
        )?;
        renderer = Some(native_renderer);
        elapsed
    } else {
        0
    };

    // Derive the renderer input once, after every route mutation, so the
    // route map remains the single source of truth throughout planning.
    let routed: Vec<u32> = fusion_routes.keys().copied().collect();
    let fusion_options = OcrFusionOptions::new()
        .markdown(page_markdown_options)
        .render_dpi(options.render.dpi)
        .hosted_recommendation_confidence(options.hosted_recommendation_confidence);
    let mut fused = if routed.is_empty() {
        fuse_ocr_pages_adaptive_with_routes(
            &native.pages,
            &OcrRun {
                pages: Vec::new(),
                render_time_ms: 0,
                ocr_time_ms: 0,
            },
            page_count,
            &fusion_options,
            &native_candidates,
            &fusion_routes,
        )?
    } else {
        // Resolve the native renderer before any network request so a missing
        // PDFium installation cannot trigger a model download it cannot use.
        let renderer = match renderer {
            Some(renderer) => renderer,
            None => PdfiumRenderer::load()?,
        };
        let engine = cached_ocr_engine(&options.ocr)?;
        run_and_fuse_ocr_chunks(
            &renderer,
            engine.as_ref(),
            buffer,
            &routed,
            options.password.as_deref(),
            &options.render,
            &options.ocr,
            &native.pages,
            page_count,
            &fusion_options,
            &native_candidates,
            &fusion_routes,
        )?
    };
    fused.render_time_ms = fused
        .render_time_ms
        .saturating_add(supplemental_preflight_ms);
    for page in &mut fused.pages {
        if recovered_natively.contains(&page.page_number) {
            page.provenance
                .warnings
                .push("recovered a credible positioned native text layer before OCR".to_string());
        }
    }
    let pages_recommending_hosted = fused
        .pages
        .iter()
        .filter(|page| page.provenance.hosted_recommended)
        .map(|page| page.provenance.page_number)
        .collect();
    let markdown = assemble_document_markdown(&fused.pages, options.markdown.include_page_numbers);

    let mut pages_with_tables = native.pages_with_tables;
    for page in &fused.pages {
        if markdown_has_table(&page.markdown) && !pages_with_tables.contains(&page.page_number) {
            pages_with_tables.push(page.page_number);
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

fn route_after_native_recovery(
    page: u32,
    supplemental_regions: &BTreeMap<u32, Vec<crate::types::PdfRect>>,
) -> Option<OcrFusionRoute> {
    supplemental_regions
        .get(&page)
        .cloned()
        .map(OcrFusionRoute::SupplementalRegions)
}

/// Keep supplemental OCR lazy by cheaply checking rendered image regions for
/// document-like foreground ink before the model store or OCR engine is
/// touched. Full-page routes already have native extraction evidence and are
/// never filtered here.
fn filter_supplemental_routes_by_pixels(
    renderer: &PdfiumRenderer,
    pdf_bytes: &[u8],
    password: Option<&str>,
    render_options: &RenderOptions,
    routes: &mut BTreeMap<u32, OcrFusionRoute>,
) -> Result<u64, OcrPipelineError> {
    let pages: Vec<u32> = routes
        .iter()
        .filter_map(|(&page, route)| {
            matches!(route, OcrFusionRoute::SupplementalRegions(_)).then_some(page)
        })
        .collect();
    if pages.is_empty() {
        return Ok(0);
    }

    let started = Instant::now();
    let mut preflight_options = render_options.clone();
    preflight_options.dpi = preflight_options.dpi.min(96.0);
    for chunk in pages.chunks(OCR_PAGE_CHUNK_SIZE) {
        let rendered = renderer.render_pages(pdf_bytes, chunk, password, &preflight_options)?;
        for page in rendered {
            let Some(OcrFusionRoute::SupplementalRegions(regions)) = routes.get_mut(&page.page())
            else {
                continue;
            };
            regions.retain(|region| region_has_document_like_ink(&page, region));
        }
    }
    routes.retain(|_, route| {
        !matches!(route, OcrFusionRoute::SupplementalRegions(regions) if regions.is_empty())
    });
    Ok(elapsed_ms(started))
}

#[allow(clippy::too_many_arguments)]
fn run_and_fuse_ocr_chunks<R, O>(
    renderer: &R,
    engine: &O,
    pdf_bytes: &[u8],
    routed_pages: &[u32],
    password: Option<&str>,
    render_options: &RenderOptions,
    ocr_options: &OcrOptions,
    native_pages: &[PageMarkdown],
    document_page_count: u32,
    fusion_options: &OcrFusionOptions,
    native_candidates: &BTreeMap<u32, NativeFallbackCandidate>,
    fusion_routes: &BTreeMap<u32, OcrFusionRoute>,
) -> Result<FusedPages, OcrPipelineError>
where
    R: PageRenderer,
    O: OcrEngine,
{
    let routed: BTreeSet<u32> = routed_pages.iter().copied().collect();
    let native_by_number: BTreeMap<u32, &PageMarkdown> = native_pages
        .iter()
        .map(|page| (page.page + 1, page))
        .collect();
    let mut pages_by_number = BTreeMap::new();
    let mut render_time_ms = 0u64;
    let mut ocr_time_ms = 0u64;

    for chunk in routed_pages.chunks(ocr_page_chunk_size(engine.preferred_page_concurrency())) {
        let native_chunk = chunk
            .iter()
            .map(|page_number| {
                clone_native_page(
                    native_by_number
                        .get(page_number)
                        .copied()
                        .expect("every routed page has a native page"),
                )
            })
            .collect::<Vec<_>>();
        let run = run_ocr_pages(
            renderer,
            engine,
            pdf_bytes,
            chunk,
            password,
            render_options,
            ocr_options,
        )?;
        let fused = fuse_ocr_pages_adaptive_with_routes(
            &native_chunk,
            &run,
            document_page_count,
            fusion_options,
            native_candidates,
            fusion_routes,
        )?;
        render_time_ms = render_time_ms.saturating_add(fused.render_time_ms);
        ocr_time_ms = ocr_time_ms.saturating_add(fused.ocr_time_ms);
        for page in fused.pages {
            pages_by_number.insert(page.page_number, page);
        }
        // `run` and its rendered bitmaps are released before the next chunk.
    }

    let native_only = select_native_pages(native_pages, |page| !routed.contains(&page));
    if !native_only.is_empty() {
        let fused = fuse_ocr_pages_adaptive_with_routes(
            &native_only,
            &OcrRun {
                pages: Vec::new(),
                render_time_ms: 0,
                ocr_time_ms: 0,
            },
            document_page_count,
            fusion_options,
            native_candidates,
            fusion_routes,
        )?;
        for page in fused.pages {
            pages_by_number.insert(page.page_number, page);
        }
    }

    let pages = native_pages
        .iter()
        .map(|native| {
            pages_by_number
                .remove(&(native.page + 1))
                .expect("every native page is fused exactly once")
        })
        .collect();
    Ok(FusedPages {
        pages,
        render_time_ms,
        ocr_time_ms,
    })
}

fn select_native_pages(
    native_pages: &[PageMarkdown],
    include: impl Fn(u32) -> bool,
) -> Vec<PageMarkdown> {
    native_pages
        .iter()
        .filter(|page| include(page.page + 1))
        .map(clone_native_page)
        .collect()
}

fn clone_native_page(page: &PageMarkdown) -> PageMarkdown {
    PageMarkdown {
        page: page.page,
        markdown: page.markdown.clone(),
        needs_ocr: page.needs_ocr,
        ocr_reason: page.ocr_reason.clone(),
    }
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

fn may_preserve_extractor_candidate(page: u32, reasons: &[PageOcrReasons]) -> bool {
    reasons
        .iter()
        .find(|entry| entry.page == page)
        .is_some_and(|entry| {
            !entry.reasons.is_empty()
                && !entry
                    .reasons
                    .iter()
                    .any(|reason| reason == OCR_REASON_SUSPECTED_GARBLED_TEXT)
                && entry.reasons.iter().any(|reason| {
                    reason == crate::OCR_REASON_SCANNED || reason == OCR_REASON_VECTOR_TEXT
                })
        })
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
    let mut wide_table_rows = BTreeSet::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        let is_table_line = trimmed.starts_with('|') && trimmed.ends_with('|');
        if is_table_line {
            if !trimmed.contains("|---") && trimmed.matches('|').count() >= 4 {
                let canonical = canonical_table_text(trimmed);
                if !canonical.is_empty() {
                    if table_cell_count(trimmed) >= 8 {
                        wide_table_rows.insert(canonical.clone());
                    }
                    adjacent_table_row = Some(canonical);
                }
            }
        } else if trimmed.is_empty() {
            // Keep adjacency across the blank line emitted after a table.
        } else {
            let canonical = canonical_table_text(trimmed);
            let duplicate = wide_table_rows.contains(&canonical)
                || adjacent_table_row
                    .as_ref()
                    .is_some_and(|table_row| *table_row == canonical);
            adjacent_table_row = None;
            if duplicate {
                continue;
            }
            // Wide rows are only candidates inside the duplicate block that
            // immediately follows a table. Once unrelated prose begins, the
            // same text may be a legitimate later reference.
            wide_table_rows.clear();
        }
        output.push_str(line);
        output.push('\n');
    }
    while output.ends_with("\n\n\n") {
        output.pop();
    }
    output
}

fn table_cell_count(row: &str) -> usize {
    row.trim_matches('|')
        .split('|')
        .filter(|cell| !cell.trim().is_empty())
        .count()
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
            document.push_str(&format!("<!-- Page {} -->\n\n", page.page_number));
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
    fn chunk_size_scales_with_engine_concurrency() {
        // Sequential engines keep the memory-bounding default.
        assert_eq!(ocr_page_chunk_size(1), OCR_PAGE_CHUNK_SIZE);
        // Parallel engines get three waves of work per batch.
        assert_eq!(ocr_page_chunk_size(3), 9);
        assert_eq!(ocr_page_chunk_size(2), 6);
        // Engine-reported concurrency is untrusted: clamp before sizing so a
        // misbehaving engine cannot inflate rendered-page memory or overflow.
        assert_eq!(ocr_page_chunk_size(0), OCR_PAGE_CHUNK_SIZE);
        assert_eq!(ocr_page_chunk_size(usize::MAX), 24);
    }

    struct TrackingRenderer {
        batches: Mutex<Vec<Vec<u32>>>,
    }

    impl PageRenderer for TrackingRenderer {
        type Error = std::convert::Infallible;

        fn render_pages(
            &self,
            _pdf_bytes: &[u8],
            pages: &[u32],
            _password: Option<&str>,
            _options: &RenderOptions,
        ) -> Result<Vec<super::super::RenderedPage>, Self::Error> {
            self.batches.lock().unwrap().push(pages.to_vec());
            Ok(pages
                .iter()
                .map(|page| {
                    let transform = super::super::PageTransform::from_corners(
                        1,
                        1,
                        (0.0, 1.0),
                        (1.0, 1.0),
                        (0.0, 0.0),
                    )
                    .unwrap();
                    super::super::RenderedPage::new(
                        *page,
                        1.0,
                        1.0,
                        1,
                        1,
                        3,
                        super::super::RenderPixelFormat::Rgb8,
                        vec![255; 3],
                        transform,
                    )
                    .unwrap()
                })
                .collect())
        }
    }

    struct TrackingEngine {
        batches: Mutex<Vec<Vec<u32>>>,
        model: super::super::ModelIdentity,
    }

    impl OcrEngine for TrackingEngine {
        type Error = std::convert::Infallible;

        fn model(&self) -> &super::super::ModelIdentity {
            &self.model
        }

        fn recognize(
            &self,
            pages: &[super::super::RenderedPage],
            _options: &OcrOptions,
        ) -> Result<Vec<super::super::OcrPage>, Self::Error> {
            self.batches
                .lock()
                .unwrap()
                .push(pages.iter().map(super::super::RenderedPage::page).collect());
            Ok(pages
                .iter()
                .map(|page| super::super::OcrPage {
                    page_number: page.page(),
                    spans: Vec::new(),
                    mean_confidence: None,
                    model: self.model.clone(),
                    processing_time_ms: 0,
                    warnings: Vec::new(),
                })
                .collect())
        }
    }

    fn recovery_item(text: &str, x: f32, y: f32, width: f32, height: f32) -> crate::TextItem {
        crate::TextItem {
            text: text.to_string(),
            x,
            y,
            width,
            height,
            font: "PDFium native text".to_string(),
            font_tag: String::new(),
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
    fn high_level_ocr_bounds_rendering_and_inference_batches() {
        let renderer = TrackingRenderer {
            batches: Mutex::new(Vec::new()),
        };
        let engine = TrackingEngine {
            batches: Mutex::new(Vec::new()),
            model: super::super::ModelIdentity::new("test-ocr", "v1"),
        };
        let native_pages = (0..10)
            .map(|page| PageMarkdown {
                page,
                markdown: String::new(),
                needs_ocr: true,
                ocr_reason: Some(crate::OCR_REASON_SCANNED.to_string()),
            })
            .collect::<Vec<_>>();
        let routed_pages = (1..=10).collect::<Vec<_>>();
        let routes = routed_pages
            .iter()
            .map(|&page| (page, OcrFusionRoute::FullPage))
            .collect();
        let ocr_options = OcrOptions::new().mode(OcrMode::Force);

        let fused = run_and_fuse_ocr_chunks(
            &renderer,
            &engine,
            b"test",
            &routed_pages,
            None,
            &RenderOptions::new(),
            &ocr_options,
            &native_pages,
            10,
            &OcrFusionOptions::new(),
            &BTreeMap::new(),
            &routes,
        )
        .unwrap();

        let expected = vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10]];
        assert_eq!(*renderer.batches.lock().unwrap(), expected);
        assert_eq!(*engine.batches.lock().unwrap(), expected);
        assert_eq!(fused.pages.len(), 10);
        assert!(fused
            .pages
            .iter()
            .all(|page| page.provenance.hosted_recommended));
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
    fn native_recovery_retains_supplemental_image_regions() {
        let region = crate::types::PdfRect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 120.0,
            page: 2,
        };
        let supplemental = BTreeMap::from([(2, vec![region.clone()])]);

        let route = route_after_native_recovery(2, &supplemental).unwrap();
        let OcrFusionRoute::SupplementalRegions(regions) = route else {
            panic!("native recovery must leave image regions on a supplemental route");
        };
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].x, region.x);
        assert_eq!(regions[0].y, region.y);
        assert_eq!(regions[0].width, region.width);
        assert_eq!(regions[0].height, region.height);
        assert_eq!(regions[0].page, region.page);
        assert!(route_after_native_recovery(1, &supplemental).is_none());
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
    fn recovered_markdown_keeps_wide_table_text_after_unrelated_prose() {
        let markdown = "|Date|A|B|C|D|E|F|G|\n|---|---|---|---|---|---|---|---|\n|April 1|1|2|3|4|5|6|7|\n\nSummary follows.\n\nApril 1 1 2 3 4 5 6 7\n";

        assert_eq!(remove_duplicate_table_lines(markdown), markdown);
    }

    #[test]
    fn recovered_markdown_drops_contiguous_duplicate_block_of_wide_table_rows() {
        let markdown = "|Date|A|B|C|D|E|F|G|\n|---|---|---|---|---|---|---|---|\n|April 1|1|2|3|4|5|6|7|\n|April 2|8|9|10|11|12|13|14|\n\nApril 1 1 2 3 4 5 6 7\nApril 2 8 9 10 11 12 13 14\n\nSummary follows.\n";

        assert_eq!(
            remove_duplicate_table_lines(markdown),
            "|Date|A|B|C|D|E|F|G|\n|---|---|---|---|---|---|---|---|\n|April 1|1|2|3|4|5|6|7|\n|April 2|8|9|10|11|12|13|14|\n\n\nSummary follows.\n"
        );
    }

    #[test]
    fn extractor_candidates_require_clean_partial_content_reasons() {
        let reasons = [
            PageOcrReasons {
                page: 1,
                reasons: vec![crate::OCR_REASON_SCANNED.to_string()],
            },
            PageOcrReasons {
                page: 2,
                reasons: vec![OCR_REASON_SUSPECTED_GARBLED_TEXT.to_string()],
            },
            PageOcrReasons {
                page: 3,
                reasons: vec![OCR_REASON_VECTOR_TEXT.to_string()],
            },
            PageOcrReasons {
                page: 4,
                reasons: vec![
                    crate::OCR_REASON_SCANNED.to_string(),
                    OCR_REASON_SUSPECTED_GARBLED_TEXT.to_string(),
                ],
            },
        ];

        assert!(may_preserve_extractor_candidate(1, &reasons));
        assert!(!may_preserve_extractor_candidate(2, &reasons));
        assert!(may_preserve_extractor_candidate(3, &reasons));
        assert!(!may_preserve_extractor_candidate(4, &reasons));
        assert!(!may_preserve_extractor_candidate(5, &reasons));
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
        let result = process_pdf_with_ocr_mem(
            &bytes,
            OcrPdfOptions::new().page_numbers([2]).markdown(markdown),
        )
        .unwrap();

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].page_number, 2);
        assert_eq!(
            result.pages[0].page_number,
            result.pages[0].provenance.page_number
        );
        assert!(result.markdown.starts_with("<!-- Page 2 -->"));
    }

    #[test]
    fn off_mode_marks_unprocessed_scan_for_hosted_fallback() {
        let bytes = std::fs::read("tests/fixtures/scan_with_native_header_text.pdf").unwrap();
        let result = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new()).unwrap();

        assert!(result.pages_routed_to_ocr.is_empty());
        assert!(result.markdown.trim().is_empty());
        assert_eq!(result.pages_recommending_hosted, vec![1]);
    }

    #[test]
    fn partial_native_scan_text_is_retained_only_inside_ocr_orchestration() {
        let bytes = std::fs::read("tests/fixtures/scan_with_native_header_text.pdf").unwrap();
        let public = crate::extract_pages_markdown_mem(&bytes, None).unwrap();
        assert!(public.pages[0].needs_ocr);
        assert!(public.pages[0].markdown.trim().is_empty());

        let ocr = crate::extract_pages_markdown_mem_for_ocr(
            &bytes,
            None,
            None,
            &MarkdownOptions::default(),
        )
        .unwrap();
        assert!(ocr.result.pages[0].needs_ocr);
        assert!(ocr.result.pages[0]
            .markdown
            .contains("Order Detail Report by Account"));
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
        let error =
            process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().page_numbers([4])).unwrap_err();
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
