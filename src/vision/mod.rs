//! Optional native vision primitives used by OCR pipelines.
//!
//! The existing lopdf extractor remains the default path. Native page
//! rendering is available only with the `render-pdfium` feature. Engine
//! contracts are available with `vision`, while checksum-verified model
//! resolution is a separate `model-cache` feature. The `ocr-oar` feature adds
//! a CPU PP-OCRv6 Small implementation of [`OcrEngine`]. These remain separate
//! so browser WASM, text-only consumers, and renderer-only users take on no
//! model-management or inference dependencies.

#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
mod contracts;
#[cfg(all(feature = "model-download", not(target_arch = "wasm32")))]
mod download;
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
mod fusion;
#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
mod image_analysis;
#[cfg(all(feature = "model-cache", not(target_arch = "wasm32")))]
mod models;
#[cfg(all(feature = "ocr-oar", not(target_arch = "wasm32")))]
mod oar;
#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
mod pipeline;
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
mod render;
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
mod routing;

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
mod pdfium;

#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
pub use contracts::{
    ImagePoint, ImageQuad, ModelDownloadPolicy, ModelIdentity, OcrEngine, OcrMode, OcrOptions,
    OcrPage, OcrSpan, PageContentSource, PageProvenance, PageRenderer, VisionTimings,
};
#[cfg(all(feature = "model-download", not(target_arch = "wasm32")))]
pub use download::{HttpModelDownloadError, HttpModelDownloader, DEFAULT_MODEL_DOWNLOAD_TIMEOUT};
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
pub use fusion::{
    fuse_ocr_pages, ocr_page_to_markdown, FusedPageMarkdown, FusedPages, OcrFusionError,
    OcrFusionOptions,
};
#[cfg(all(feature = "model-cache", not(target_arch = "wasm32")))]
pub use models::{
    ModelAcquireError, ModelArtifact, ModelArtifactKind, ModelDownloader, ModelManifest,
    ModelPaths, ModelStore, ModelStoreError, PP_OCR_V6_SMALL,
};
#[cfg(all(feature = "ocr-oar", not(target_arch = "wasm32")))]
pub use oar::{OarOcrEngine, OarOcrError, ONNX_RUNTIME_LIBRARY_ENV};
#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
pub use pipeline::{
    process_pdf_with_ocr, process_pdf_with_ocr_mem, OcrPdfOptions, OcrPdfResult, OcrPipelineError,
};
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
pub use render::{
    PagePoint, PageTransform, RenderBufferError, RenderOptions, RenderPixelFormat, RenderedPage,
    DEFAULT_RENDER_DPI,
};
#[cfg(all(feature = "vision", not(target_arch = "wasm32")))]
pub use routing::{
    route_ocr_pages, run_ocr_pages, OcrRoutingError, OcrRun, OcrRunError, RoutedOcrPage,
};

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
pub use pdfium::{PdfiumRenderer, RenderError};
