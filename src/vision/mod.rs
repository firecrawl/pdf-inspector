//! Optional native vision primitives used by local extraction pipelines.
//!
//! The existing lopdf extractor remains the default path. Native page
//! rendering is available only with the `render-pdfium` feature. Engine
//! contracts are available with `local-vision`, while checksum-verified model
//! resolution is a separate `model-cache` feature. These remain separate so
//! browser WASM, text-only consumers, and renderer-only users take on no model
//! management dependencies.

#[cfg(all(feature = "local-vision", not(target_arch = "wasm32")))]
mod contracts;
#[cfg(all(feature = "model-cache", not(target_arch = "wasm32")))]
mod models;
#[cfg(all(feature = "local-vision", not(target_arch = "wasm32")))]
mod render;

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
mod pdfium;

#[cfg(all(feature = "local-vision", not(target_arch = "wasm32")))]
pub use contracts::{
    ImagePoint, ImageQuad, LayoutEngine, LayoutOptions, LayoutPage, LayoutRegion, LayoutRegionKind,
    LocalOptions, ModelDownloadPolicy, ModelIdentity, OcrEngine, OcrMode, OcrOptions, OcrPage,
    OcrProfile, OcrSpan, PageContentSource, PageProvenance, PageRenderer, VisionTimings,
};
#[cfg(all(feature = "model-cache", not(target_arch = "wasm32")))]
pub use models::{
    ModelArtifact, ModelArtifactKind, ModelManifest, ModelPaths, ModelStore, ModelStoreError,
    PP_OCR_V6_SMALL,
};
#[cfg(all(feature = "local-vision", not(target_arch = "wasm32")))]
pub use render::{
    PagePoint, PageTransform, RenderBufferError, RenderOptions, RenderPixelFormat, RenderedPage,
};

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
pub use pdfium::{PdfiumRenderer, RenderError};
