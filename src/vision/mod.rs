//! Optional native vision primitives used by local extraction pipelines.
//!
//! The existing lopdf extractor remains the default path. Native page
//! rendering is available only with the `render-pdfium` feature and is kept
//! separate so browser WASM and text-only consumers do not take on PDFium.

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
mod pdfium;

#[cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]
pub use pdfium::{
    PagePoint, PdfiumRenderer, RenderError, RenderOptions, RenderPixelFormat, RenderedPage,
};
