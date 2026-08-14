//! Optional PDF page rasterization with bounded output buffers.

use hayro::hayro_interpret::{InterpreterSettings, InterpreterWarning};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, RenderCache, RenderSettings};
use std::sync::{Arc, Mutex};

/// Default DPI for page rasterization
pub const DEFAULT_RENDER_DPI: f32 = 200.0;

/// Hard cap on the requested DPI
pub const MAX_RENDER_DPI: f32 = 300.0;

/// Max output width or height, in pixels
pub const MAX_RENDER_DIMENSION: u32 = 16_384;

/// Max output area for a single page
pub const MAX_RENDER_PIXELS_PER_PAGE: u64 = 25_000_000;

/// Max combined RGBA8 bytes for one call
pub const MAX_RENDER_OUTPUT_BYTES: u64 = 100_000_000;

/// Max page entries for one call. Duplicates count separately, since
/// each entry produce its own buffer: for big documents is better to
/// render in batches and release the buffers in between.
pub const MAX_RENDER_PAGES_PER_REQUEST: usize = 1_024;

/// Options for [`render_pages_mem`].
#[derive(Clone)]
#[non_exhaustive]
pub struct RenderOptions {
    /// Output DPI. Must be finite, positive and at most [`MAX_RENDER_DPI`].
    pub dpi: f32,
    /// Password for an encrypted PDF
    pub password: Option<String>,
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("dpi", &self.dpi)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_RENDER_DPI,
            password: None,
        }
    }
}

impl RenderOptions {
    /// Default options at 200 DPI
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the output DPI
    pub fn dpi(mut self, dpi: f32) -> Self {
        self.dpi = dpi;
        self
    }

    /// Set the password for an encrypted PDF
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }
}

/// One rasterized PDF page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedPage {
    /// Zero-based page index in the source PDF
    pub page: u32,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Opaque row-major RGBA8, white background
    pub pixels: Vec<u8>,
    /// Non-fatal interpreter warnings for this page. An
    /// [`RenderWarning::ImageDecodeFailure`] means the pixels are not safe
    /// to OCR without an independent check.
    pub warnings: Vec<RenderWarning>,
}

/// A non-fatal issue reported while interpreting one rendered page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderWarning {
    /// The page uses a font kind we can't draw, glyphs may be missing
    UnsupportedFont,
    /// An image failed to decode, its pixels may be missing
    ImageDecodeFailure,
}

fn map_interpreter_warning(warning: InterpreterWarning) -> RenderWarning {
    match warning {
        InterpreterWarning::UnsupportedFont => RenderWarning::UnsupportedFont,
        InterpreterWarning::ImageDecodeFailure => RenderWarning::ImageDecodeFailure,
    }
}

/// An error returned while rasterizing PDF pages.
#[derive(Debug, thiserror::Error, PartialEq)]
#[non_exhaustive]
pub enum RenderError {
    /// The requested DPI is zero, negative, non-finite, or above the hard cap
    #[error("render DPI must be finite and greater than 0 and at most {max}, got {dpi}")]
    InvalidDpi { dpi: f32, max: f32 },

    /// Too many page entries were requested in one call
    #[error("requested {requested} page entries, maximum is {max}")]
    TooManyPages { requested: usize, max: usize },

    /// The PDF is encrypted or the supplied password is not accepted
    #[error("PDF is encrypted or the password is invalid")]
    Encrypted,

    /// The input could not be parsed as a PDF
    #[error("PDF parsing error")]
    Parse,

    /// A requested zero-based page index does not exist
    #[error("page index {page} is out of range for a PDF with {page_count} pages")]
    PageOutOfRange { page: u32, page_count: usize },

    /// A page has missing, non-finite, non-positive, or sub-pixel dimensions
    #[error("page {page} has invalid render dimensions {width_points} x {height_points} points")]
    InvalidPageDimensions {
        page: u32,
        width_points: f32,
        height_points: f32,
    },

    /// A scaled page width or height exceeds the hard cap
    #[error("page {page} would render at {width} x {height} pixels, maximum dimension is {max}")]
    PageDimensionsTooLarge {
        page: u32,
        width: u64,
        height: u64,
        max: u32,
    },

    /// A page's output area exceeds the hard cap
    #[error("page {page} would contain {pixels} pixels, maximum is {max}")]
    PagePixelsTooLarge { page: u32, pixels: u64, max: u64 },

    /// The combined RGBA8 output would exceed the hard cap
    #[error("rendered output would require {bytes} bytes, maximum is {max}")]
    OutputTooLarge { bytes: u64, max: u64 },

    /// The renderer returned a pixel buffer with an unexpected memory layout
    #[error("renderer returned an unsupported pixel-buffer layout")]
    PixelBufferLayout,
}

impl From<LoadPdfError> for RenderError {
    fn from(error: LoadPdfError) -> Self {
        match error {
            LoadPdfError::Decryption(_) => Self::Encrypted,
            LoadPdfError::Invalid => Self::Parse,
        }
    }
}

#[derive(Clone, Copy)]
struct PreparedPage {
    page: u32,
    width: u16,
    height: u16,
}

/// Rasterize the selected pages of a PDF into opaque RGBA8 buffers.
///
/// `pages` holds zero-based indexes; results keep the caller order and
/// the duplicates. Everything (every page, the whole output budget) is
/// validated before the first page renders. An empty selection still
/// parses the PDF and returns an empty vec.
///
/// Only available with the `render` feature. CPU-only, no filesystem,
/// so it compiles for `wasm32-unknown-unknown` too.
pub fn render_pages_mem(
    pdf_bytes: &[u8],
    pages: &[u32],
    options: RenderOptions,
) -> Result<Vec<RenderedPage>, RenderError> {
    validate_options(pages, &options)?;

    let pdf = Pdf::new_with_password(
        pdf_bytes.to_vec(),
        options.password.as_deref().unwrap_or(""),
    )?;
    let scale = options.dpi / 72.0;
    let prepared = prepare_pages(&pdf, pages, scale)?;

    let cache = RenderCache::new();
    let mut rendered_pages = Vec::with_capacity(prepared.len());

    for prepared_page in prepared {
        // prepare_pages already validated every index against this same PDF,
        // but better to not have any indexing panic here anyway.
        let page =
            pdf.pages()
                .get(prepared_page.page as usize)
                .ok_or(RenderError::PageOutOfRange {
                    page: prepared_page.page,
                    page_count: pdf.pages().len(),
                })?;
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_sink = Arc::clone(&warnings);
        let interpreter_settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                let mut warnings = warning_sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let warning = map_interpreter_warning(warning);
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }),
            ..InterpreterSettings::default()
        };
        let pixmap = render(
            page,
            &cache,
            &interpreter_settings,
            &RenderSettings {
                x_scale: scale,
                y_scale: scale,
                width: Some(prepared_page.width),
                height: Some(prepared_page.height),
                bg_color: WHITE,
            },
        );

        let width = u32::from(pixmap.width());
        let height = u32::from(pixmap.height());
        let pixels = bytemuck::allocation::try_cast_vec(pixmap.take())
            .map_err(|_| RenderError::PixelBufferLayout)?;
        let warnings = warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        rendered_pages.push(RenderedPage {
            page: prepared_page.page,
            width,
            height,
            pixels,
            warnings,
        });
    }

    Ok(rendered_pages)
}

fn validate_options(pages: &[u32], options: &RenderOptions) -> Result<(), RenderError> {
    if !options.dpi.is_finite() || options.dpi <= 0.0 || options.dpi > MAX_RENDER_DPI {
        return Err(RenderError::InvalidDpi {
            dpi: options.dpi,
            max: MAX_RENDER_DPI,
        });
    }

    if pages.len() > MAX_RENDER_PAGES_PER_REQUEST {
        return Err(RenderError::TooManyPages {
            requested: pages.len(),
            max: MAX_RENDER_PAGES_PER_REQUEST,
        });
    }

    Ok(())
}

fn prepare_pages(pdf: &Pdf, pages: &[u32], scale: f32) -> Result<Vec<PreparedPage>, RenderError> {
    let page_count = pdf.pages().len();
    let mut total_bytes = 0_u64;
    let mut prepared = Vec::with_capacity(pages.len());

    for &page_index in pages {
        let page = pdf
            .pages()
            .get(page_index as usize)
            .ok_or(RenderError::PageOutOfRange {
                page: page_index,
                page_count,
            })?;
        let (width_points, height_points) = page.render_dimensions();
        let (width, height) = scaled_dimensions(width_points, height_points, scale).ok_or(
            RenderError::InvalidPageDimensions {
                page: page_index,
                width_points,
                height_points,
            },
        )?;

        if width > u64::from(MAX_RENDER_DIMENSION) || height > u64::from(MAX_RENDER_DIMENSION) {
            return Err(RenderError::PageDimensionsTooLarge {
                page: page_index,
                width,
                height,
                max: MAX_RENDER_DIMENSION,
            });
        }

        let pixels = width
            .checked_mul(height)
            .ok_or(RenderError::PagePixelsTooLarge {
                page: page_index,
                pixels: u64::MAX,
                max: MAX_RENDER_PIXELS_PER_PAGE,
            })?;
        if pixels > MAX_RENDER_PIXELS_PER_PAGE {
            return Err(RenderError::PagePixelsTooLarge {
                page: page_index,
                pixels,
                max: MAX_RENDER_PIXELS_PER_PAGE,
            });
        }

        let page_bytes = pixels.checked_mul(4).ok_or(RenderError::OutputTooLarge {
            bytes: u64::MAX,
            max: MAX_RENDER_OUTPUT_BYTES,
        })?;
        total_bytes = total_bytes
            .checked_add(page_bytes)
            .ok_or(RenderError::OutputTooLarge {
                bytes: u64::MAX,
                max: MAX_RENDER_OUTPUT_BYTES,
            })?;
        if total_bytes > MAX_RENDER_OUTPUT_BYTES {
            return Err(RenderError::OutputTooLarge {
                bytes: total_bytes,
                max: MAX_RENDER_OUTPUT_BYTES,
            });
        }

        prepared.push(PreparedPage {
            page: page_index,
            width: width as u16,
            height: height as u16,
        });
    }

    Ok(prepared)
}

fn scaled_dimensions(width_points: f32, height_points: f32, scale: f32) -> Option<(u64, u64)> {
    if !width_points.is_finite()
        || !height_points.is_finite()
        || width_points <= 0.0
        || height_points <= 0.0
    {
        return None;
    }

    // Match the same f32 multiplication Hayro does before to floor to pixel.
    let width = f64::from(width_points * scale);
    let height = f64::from(height_points * scale);
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return None;
    }

    Some((width.floor() as u64, height.floor() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_interpreter_warning_to_the_public_type() {
        assert_eq!(
            map_interpreter_warning(InterpreterWarning::UnsupportedFont),
            RenderWarning::UnsupportedFont
        );
        assert_eq!(
            map_interpreter_warning(InterpreterWarning::ImageDecodeFailure),
            RenderWarning::ImageDecodeFailure
        );
    }
}
