//! PDFium-backed page rendering for OCR.

use std::path::Path;

use firecrawl_pdfium::{
    PageRect as PdfiumPageRect, Pdfium, PixelFormat, PixelPoint, PixelRect, RenderConfig,
};
use thiserror::Error;

use crate::PdfRect;

/// Default rendering resolution for OCR.
pub const DEFAULT_RENDER_DPI: f32 = 150.0;

/// Default maximum size of one rendered page: 256 MiB.
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Pixel layout returned by [`RenderedPage`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderPixelFormat {
    /// Three bytes per pixel in red, green, blue order. This is the default
    /// because OCR preprocessors conventionally consume RGB images.
    #[default]
    Rgb8,
    /// Four bytes per pixel in red, green, blue, alpha order.
    Rgba8,
    /// One luminance byte per pixel.
    Gray8,
}

impl RenderPixelFormat {
    /// Number of bytes used by one pixel.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
            Self::Gray8 => 1,
        }
    }

    fn pdfium_format(self) -> PixelFormat {
        match self {
            // PDFium produces BGR directly; `RenderedPage::from_pdfium`
            // swaps the red and blue channels in place.
            Self::Rgb8 => PixelFormat::Bgr8,
            Self::Rgba8 => PixelFormat::Rgba8,
            Self::Gray8 => PixelFormat::Gray8,
        }
    }
}

/// Configuration for pages rendered as input to a local vision pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
    /// Output resolution. Defaults to 150 DPI.
    pub dpi: f32,
    /// Pixel layout. Defaults to three-channel RGB.
    pub pixel_format: RenderPixelFormat,
    /// Include PDF annotations in the rendered bitmap.
    pub annotations: bool,
    /// Include visible static AcroForm field appearances.
    pub form_fields: bool,
    /// Maximum allocation for each rendered page.
    pub max_output_bytes_per_page: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_RENDER_DPI,
            pixel_format: RenderPixelFormat::Rgb8,
            annotations: true,
            form_fields: true,
            max_output_bytes_per_page: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl RenderOptions {
    /// Creates local-rendering options with OCR-oriented defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the output resolution in dots per inch.
    pub fn dpi(mut self, dpi: f32) -> Self {
        self.dpi = dpi;
        self
    }

    /// Sets the output pixel layout.
    pub fn pixel_format(mut self, pixel_format: RenderPixelFormat) -> Self {
        self.pixel_format = pixel_format;
        self
    }

    /// Toggles annotation rendering.
    pub fn annotations(mut self, annotations: bool) -> Self {
        self.annotations = annotations;
        self
    }

    /// Toggles visible static form-field rendering.
    pub fn form_fields(mut self, form_fields: bool) -> Self {
        self.form_fields = form_fields;
        self
    }

    /// Sets the maximum allocation for each rendered page.
    pub fn max_output_bytes_per_page(mut self, bytes: u64) -> Self {
        self.max_output_bytes_per_page = bytes;
        self
    }

    fn pdfium_config(&self) -> RenderConfig {
        RenderConfig::new()
            .dpi(self.dpi)
            .pixel_format(self.pixel_format.pdfium_format())
            .annotations(self.annotations)
            .form_fields(self.form_fields)
            .max_output_bytes(self.max_output_bytes_per_page)
    }
}

/// A point in PDF page space, measured in points from the bottom-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    /// Horizontal position in PDF points.
    pub x: f32,
    /// Vertical position in PDF points, increasing upward.
    pub y: f32,
}

/// One rendered page with owned pixels and its pixel-to-PDF transform.
///
/// The value contains no live PDFium page or document handles. It can be
/// moved to an OCR worker and retained after [`PdfiumRenderer::render_pages`]
/// returns.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    page: u32,
    page_width: f32,
    page_height: f32,
    width: u32,
    height: u32,
    stride: usize,
    format: RenderPixelFormat,
    pixels: Vec<u8>,
    transform: firecrawl_pdfium::PageTransform,
}

impl RenderedPage {
    fn from_pdfium(
        page: u32,
        page_width: f32,
        page_height: f32,
        format: RenderPixelFormat,
        rendered: firecrawl_pdfium::RenderedPage,
    ) -> Self {
        let width = rendered.width();
        let height = rendered.height();
        let stride = rendered.stride();
        let transform = *rendered.transform();
        let mut pixels = rendered.into_pixels();

        if format == RenderPixelFormat::Rgb8 {
            bgr_to_rgb_in_place(&mut pixels, width, height, stride);
        }

        Self {
            page,
            page_width,
            page_height,
            width,
            height,
            stride,
            format,
            pixels,
            transform,
        }
    }

    /// 1-indexed page number.
    pub fn page(&self) -> u32 {
        self.page
    }

    /// Page width in PDF points after applying the page's `/Rotate` entry.
    pub fn page_width(&self) -> f32 {
        self.page_width
    }

    /// Page height in PDF points after applying the page's `/Rotate` entry.
    pub fn page_height(&self) -> f32 {
        self.page_height
    }

    /// Bitmap width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Bitmap height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of bytes between adjacent bitmap rows.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Pixel layout of [`pixels`](Self::pixels).
    pub fn format(&self) -> RenderPixelFormat {
        self.format
    }

    /// Owned bitmap bytes, with rows ordered top-to-bottom.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the page and returns its pixel buffer.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// Converts a bitmap point (top-left origin, y-down) to PDF page space
    /// (bottom-left origin, y-up).
    pub fn pixel_to_page(&self, x: f64, y: f64) -> PagePoint {
        let point = self.transform.pixel_to_page(PixelPoint::new(x, y));
        PagePoint {
            x: point.x as f32,
            y: point.y as f32,
        }
    }

    /// Converts a bitmap rectangle to the repository's existing PDF-space
    /// rectangle type. The returned page number remains 1-indexed.
    pub fn pixel_rect_to_pdf_rect(&self, x: f64, y: f64, width: f64, height: f64) -> PdfRect {
        let rect = self
            .transform
            .pixel_rect_to_page(PixelRect::new(x, y, width, height));
        PdfRect {
            x: rect.left as f32,
            y: rect.bottom as f32,
            width: rect.width() as f32,
            height: rect.height() as f32,
            page: self.page,
        }
    }

    /// Converts a PDF-space rectangle to bitmap coordinates
    /// `(x, y, width, height)` with a top-left origin.
    pub fn pdf_rect_to_pixel(&self, rect: &PdfRect) -> (f64, f64, f64, f64) {
        let rect = self.transform.page_rect_to_pixel(PdfiumPageRect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.x + rect.width),
            f64::from(rect.y + rect.height),
        ));
        (rect.x, rect.y, rect.width, rect.height)
    }
}

/// Errors produced by the optional local renderer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RenderError {
    /// Page numbers in pdf-inspector APIs are 1-indexed, so zero is invalid.
    #[error("page numbers are 1-indexed; page 0 is invalid")]
    InvalidPageNumber,
    /// The requested 1-indexed page is not present in the document.
    #[error("page {page} is out of bounds for a {page_count}-page document")]
    PageOutOfBounds {
        /// Requested 1-indexed page.
        page: u32,
        /// Number of pages in the document.
        page_count: usize,
    },
    /// PDFium loading, document parsing, form setup, or rendering failed.
    #[error(transparent)]
    Pdfium(#[from] firecrawl_pdfium::Error),
}

/// Loaded PDFium renderer used to prepare pages for OCR.
///
/// PDFium calls are safe from concurrent threads but serialize inside the
/// underlying binding. Returned [`RenderedPage`] values are ordinary owned
/// data and can be processed concurrently after rendering.
#[derive(Debug, Clone, Copy)]
pub struct PdfiumRenderer {
    pdfium: Pdfium,
}

impl PdfiumRenderer {
    /// Loads PDFium using `firecrawl-pdfium`'s documented discovery chain.
    pub fn load() -> Result<Self, RenderError> {
        Ok(Self {
            pdfium: Pdfium::load()?,
        })
    }

    /// Loads PDFium from an explicit native library path.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, RenderError> {
        Ok(Self {
            pdfium: Pdfium::load_from_path(path)?,
        })
    }

    /// Path of the active PDFium library, if it was loaded from a concrete
    /// file rather than through the system loader.
    pub fn loaded_from(&self) -> Option<&Path> {
        self.pdfium.loaded_from()
    }

    /// Renders selected 1-indexed pages in the same order as `pages`.
    ///
    /// The PDF is parsed once for the full batch. Passing an empty page list
    /// returns immediately without parsing or allocating.
    pub fn render_pages(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        password: Option<&str>,
        options: &RenderOptions,
    ) -> Result<Vec<RenderedPage>, RenderError> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        if pages.contains(&0) {
            return Err(RenderError::InvalidPageNumber);
        }

        let document = self.pdfium.load_document(pdf_bytes.to_vec(), password)?;
        let page_count = document.page_count();

        if let Some(&page) = pages.iter().find(|&&page| page as usize > page_count) {
            return Err(RenderError::PageOutOfBounds { page, page_count });
        }

        if options.form_fields {
            document.enable_form_rendering()?;
        }

        let config = options.pdfium_config();
        let mut rendered_pages = Vec::with_capacity(pages.len());
        for &page_number in pages {
            let page = document.page(page_number as usize - 1)?;
            let size = page.size();
            let rendered = page.render(&config)?;
            rendered_pages.push(RenderedPage::from_pdfium(
                page_number,
                size.width,
                size.height,
                options.pixel_format,
                rendered,
            ));
        }

        Ok(rendered_pages)
    }
}

fn bgr_to_rgb_in_place(pixels: &mut [u8], width: u32, height: u32, stride: usize) {
    let row_bytes = width as usize * RenderPixelFormat::Rgb8.bytes_per_pixel();
    assert!(stride >= row_bytes, "pixel stride is shorter than one row");
    assert_eq!(
        pixels.len(),
        stride * height as usize,
        "pixel buffer length does not match stride and height"
    );

    for row in pixels.chunks_exact_mut(stride) {
        for pixel in row[..row_bytes].chunks_exact_mut(3) {
            pixel.swap(0, 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_ocr_oriented_and_bounded() {
        let options = RenderOptions::default();
        assert_eq!(options.dpi, 150.0);
        assert_eq!(options.pixel_format, RenderPixelFormat::Rgb8);
        assert!(options.annotations);
        assert!(options.form_fields);
        assert_eq!(options.max_output_bytes_per_page, 256 * 1024 * 1024);
    }

    #[test]
    fn bgr_pixels_are_converted_to_rgb_in_place() {
        let mut pixels = vec![1, 2, 3, 4, 5, 6];
        bgr_to_rgb_in_place(&mut pixels, 2, 1, 6);
        assert_eq!(pixels, [3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn bgr_conversion_skips_row_padding() {
        let mut pixels = vec![1, 2, 3, 9, 7, 8, 9, 6];
        bgr_to_rgb_in_place(&mut pixels, 1, 2, 4);
        assert_eq!(pixels, [3, 2, 1, 9, 9, 8, 7, 6]);
    }
}
