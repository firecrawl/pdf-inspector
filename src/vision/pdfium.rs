//! PDFium-backed implementation of the renderer-neutral page contract.

use std::path::Path;

use firecrawl_pdfium::{Pdfium, PixelFormat, PixelPoint, RenderConfig};
use thiserror::Error;

use super::{
    PageRenderer, PageTransform, RenderBufferError, RenderOptions, RenderPixelFormat, RenderedPage,
};

impl RenderPixelFormat {
    fn pdfium_format(self) -> PixelFormat {
        match self {
            // PDFium produces BGR directly; `rendered_page_from_pdfium`
            // swaps the red and blue channels in place.
            Self::Rgb8 => PixelFormat::Bgr8,
            Self::Rgba8 => PixelFormat::Rgba8,
            Self::Gray8 => PixelFormat::Gray8,
        }
    }
}

impl RenderOptions {
    fn pdfium_config(&self) -> RenderConfig {
        RenderConfig::new()
            .dpi(self.dpi)
            .pixel_format(self.pixel_format.pdfium_format())
            .annotations(self.annotations)
            .form_fields(self.form_fields)
            .max_output_bytes(self.max_output_bytes_per_page)
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
    /// PDFium returned an internally inconsistent bitmap or transform.
    #[error(transparent)]
    Buffer(#[from] RenderBufferError),
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
    /// This inherent method mirrors [`PageRenderer`] so existing callers do
    /// not need to import the trait.
    pub fn render_pages(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        password: Option<&str>,
        options: &RenderOptions,
    ) -> Result<Vec<RenderedPage>, RenderError> {
        self.render_pages_impl(pdf_bytes, pages, password, options)
    }

    fn render_pages_impl(
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
            rendered_pages.push(rendered_page_from_pdfium(
                page_number,
                size.width,
                size.height,
                options.pixel_format,
                rendered,
            )?);
        }

        Ok(rendered_pages)
    }
}

impl PageRenderer for PdfiumRenderer {
    type Error = RenderError;

    fn render_pages(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        password: Option<&str>,
        options: &RenderOptions,
    ) -> Result<Vec<RenderedPage>, Self::Error> {
        self.render_pages_impl(pdf_bytes, pages, password, options)
    }
}

fn rendered_page_from_pdfium(
    page: u32,
    page_width: f32,
    page_height: f32,
    format: RenderPixelFormat,
    rendered: firecrawl_pdfium::RenderedPage,
) -> Result<RenderedPage, RenderBufferError> {
    let width = rendered.width();
    let height = rendered.height();
    let stride = rendered.stride();
    let pdfium_transform = *rendered.transform();
    let corner = |x, y| {
        let point = pdfium_transform.pixel_to_page(PixelPoint::new(x, y));
        (point.x, point.y)
    };
    let transform = PageTransform::from_corners(
        width,
        height,
        corner(0.0, 0.0),
        corner(f64::from(width), 0.0),
        corner(0.0, f64::from(height)),
    )
    .ok_or(RenderBufferError::InvalidTransform)?;
    let mut pixels = rendered.into_pixels();

    if format == RenderPixelFormat::Rgb8 {
        bgr_to_rgb_in_place(&mut pixels, width, height, stride)?;
    }

    RenderedPage::new(
        page,
        page_width,
        page_height,
        width,
        height,
        stride,
        format,
        pixels,
        transform,
    )
}

fn bgr_to_rgb_in_place(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
) -> Result<(), RenderBufferError> {
    let row_bytes = (width as usize)
        .checked_mul(RenderPixelFormat::Rgb8.bytes_per_pixel())
        .ok_or(RenderBufferError::SizeOverflow)?;
    if stride < row_bytes {
        return Err(RenderBufferError::InvalidStride {
            stride,
            minimum: row_bytes,
        });
    }
    let expected = stride
        .checked_mul(height as usize)
        .ok_or(RenderBufferError::SizeOverflow)?;
    if pixels.len() != expected {
        return Err(RenderBufferError::InvalidBufferLength {
            actual: pixels.len(),
            expected,
        });
    }

    for row in pixels.chunks_exact_mut(stride) {
        for pixel in row[..row_bytes].chunks_exact_mut(3) {
            pixel.swap(0, 2);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgr_pixels_are_converted_to_rgb_in_place() {
        let mut pixels = vec![1, 2, 3, 4, 5, 6];
        bgr_to_rgb_in_place(&mut pixels, 2, 1, 6).unwrap();
        assert_eq!(pixels, [3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn bgr_conversion_skips_row_padding() {
        let mut pixels = vec![1, 2, 3, 9, 7, 8, 9, 6];
        bgr_to_rgb_in_place(&mut pixels, 1, 2, 4).unwrap();
        assert_eq!(pixels, [3, 2, 1, 9, 9, 8, 7, 6]);
    }

    #[test]
    fn malformed_bgr_buffers_return_errors() {
        assert!(matches!(
            bgr_to_rgb_in_place(&mut [0; 6], 2, 1, 5),
            Err(RenderBufferError::InvalidStride { .. })
        ));
        assert!(matches!(
            bgr_to_rgb_in_place(&mut [0; 5], 1, 2, 3),
            Err(RenderBufferError::InvalidBufferLength { .. })
        ));
    }
}
