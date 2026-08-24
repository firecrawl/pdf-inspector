//! PDFium-backed implementation of the renderer-neutral page contract.

use std::path::Path;

use firecrawl_pdfium::{PageChar, Pdfium, PixelFormat, PixelPoint, RenderConfig};
use thiserror::Error;

use crate::types::{ItemType, TextItem};

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
    /// The PDFium shared library could not be discovered or loaded.
    #[error(
        "failed to load PDFium; install a compatible PDFium shared library or set PDFIUM_LIB_PATH to its path"
    )]
    PdfiumLoad {
        /// Dynamic loading failure.
        #[source]
        source: firecrawl_pdfium::Error,
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

/// Positioned native text recovered from one selected PDF page.
#[derive(Debug)]
pub(crate) struct PdfiumTextPage {
    pub(crate) page: u32,
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
    pub(crate) items: Vec<TextItem>,
}

impl PdfiumRenderer {
    /// Loads PDFium using `firecrawl-pdfium`'s documented discovery chain.
    pub fn load() -> Result<Self, RenderError> {
        Ok(Self {
            pdfium: Pdfium::load().map_err(|source| RenderError::PdfiumLoad { source })?,
        })
    }

    /// Loads PDFium from an explicit native library path.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, RenderError> {
        Ok(Self {
            pdfium: Pdfium::load_from_path(path)
                .map_err(|source| RenderError::PdfiumLoad { source })?,
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

    /// Extracts positioned native text from selected 1-indexed pages.
    ///
    /// This is deliberately separate from rendering: callers can probe a
    /// suspicious embedded text layer before paying for rasterization and
    /// OCR. A page-level text failure is treated as an unavailable recovery
    /// candidate so the caller can continue to its normal OCR fallback.
    pub(crate) fn extract_text_pages(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        password: Option<&str>,
    ) -> Result<Vec<PdfiumTextPage>, RenderError> {
        const MAX_TEXT_CHARS_PER_PAGE: usize = 250_000;

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

        let mut recovered = Vec::with_capacity(pages.len());
        for &page_number in pages {
            let page = document.page(page_number as usize - 1)?;
            let page_size = page.size();
            let text = match page.text_with_limit(MAX_TEXT_CHARS_PER_PAGE) {
                Ok(text) => text,
                Err(error) => {
                    log::debug!(
                        "page {page_number}: positioned native text recovery unavailable: {error}"
                    );
                    continue;
                }
            };
            recovered.push(PdfiumTextPage {
                page: page_number,
                page_width: page_size.width,
                page_height: page_size.height,
                items: text_chars_to_items(text.chars(), page_number),
            });
        }
        Ok(recovered)
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

fn text_chars_to_items(chars: &[PageChar], page: u32) -> Vec<TextItem> {
    #[derive(Debug, Clone, Copy)]
    struct Bounds {
        left: f64,
        bottom: f64,
        right: f64,
        top: f64,
    }

    fn flush(items: &mut Vec<TextItem>, text: &mut String, bounds: &mut Option<Bounds>, page: u32) {
        let Some(bounds) = bounds.take() else {
            text.clear();
            return;
        };
        if text.is_empty() {
            return;
        }
        let width = (bounds.right - bounds.left) as f32;
        let height = (bounds.top - bounds.bottom) as f32;
        let x = bounds.left as f32;
        let y = bounds.bottom as f32;
        if !x.is_finite()
            || !y.is_finite()
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
        {
            text.clear();
            return;
        }
        items.push(TextItem {
            text: std::mem::take(text),
            x,
            y,
            width,
            height,
            font: "PDFium native text".to_string(),
            font_tag: String::new(),
            font_size: height.max(1.0),
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        });
    }

    let mut items = Vec::new();
    let mut text = String::new();
    let mut bounds: Option<Bounds> = None;
    for character in chars {
        let Some(value) = character.unicode else {
            flush(&mut items, &mut text, &mut bounds, page);
            continue;
        };
        if value.is_whitespace() {
            flush(&mut items, &mut text, &mut bounds, page);
            continue;
        }

        let rect = character.loose_bounds.normalized();
        if !rect.left.is_finite()
            || !rect.bottom.is_finite()
            || !rect.right.is_finite()
            || !rect.top.is_finite()
            || rect.width() <= 0.0
            || rect.height() <= 0.0
        {
            flush(&mut items, &mut text, &mut bounds, page);
            continue;
        }
        text.push(value);
        bounds = Some(match bounds {
            Some(bounds) => Bounds {
                left: bounds.left.min(rect.left),
                bottom: bounds.bottom.min(rect.bottom),
                right: bounds.right.max(rect.right),
                top: bounds.top.max(rect.top),
            },
            None => Bounds {
                left: rect.left,
                bottom: rect.bottom,
                right: rect.right,
                top: rect.top,
            },
        });
    }
    flush(&mut items, &mut text, &mut bounds, page);
    items.sort_by(|first, second| {
        first
            .page
            .cmp(&second.page)
            .then(second.y.total_cmp(&first.y))
            .then(first.x.total_cmp(&second.x))
    });
    items
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
        for pixel in row[..row_bytes].as_chunks_mut::<3>().0 {
            pixel.swap(0, 2);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrawl_pdfium::{PagePoint, PageRect};

    fn page_char(value: char, bounds: PageRect) -> PageChar {
        PageChar {
            unicode: Some(value),
            code: value as u32,
            bounds,
            loose_bounds: bounds,
            origin: PagePoint::new(bounds.left, bounds.bottom),
        }
    }

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

    #[test]
    fn invalid_character_geometry_splits_text_runs() {
        let chars = [
            page_char('A', PageRect::new(0.0, 0.0, 8.0, 10.0)),
            page_char('X', PageRect::new(10.0, 0.0, 10.0, 10.0)),
            page_char('B', PageRect::new(20.0, 0.0, 28.0, 10.0)),
        ];

        let items = text_chars_to_items(&chars, 1);

        assert_eq!(
            items
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }

    #[test]
    fn coordinates_that_overflow_f32_are_discarded() {
        let left = f64::from(f32::MAX) * 2.0;
        let chars = [page_char(
            'A',
            PageRect::new(left, 0.0, left + 1.0e30, 10.0),
        )];

        assert!(text_chars_to_items(&chars, 1).is_empty());
    }
}
