//! Optional PDF page rasterization with bounded output buffers.

use hayro::hayro_interpret::{InterpreterSettings, InterpreterWarning};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, RenderCache, RenderSettings};
use std::collections::BTreeMap;
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

/// One image occurrence cropped from a rendered PDF page.
///
/// [`reference`](Self::reference) is the same URI emitted by Markdown when
/// [`crate::MarkdownOptions::include_images`] is enabled, so callers can join
/// the Markdown position to these pixels without matching resource names.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RenderedImage {
    /// Stable reference emitted in Markdown, for example `pdf-image:p1_i1`.
    pub reference: String,
    /// PDF XObject resource name, useful for diagnostics only.
    pub resource_name: String,
    /// Zero-based page index in the source PDF.
    pub page: u32,
    /// One-based image occurrence within the page, in content-stream order.
    pub occurrence: u32,
    /// Source placement as `[x, y, width, height]` in PDF points.
    pub bbox: [f32; 4],
    /// Width of the cropped output in pixels.
    pub width: u32,
    /// Height of the cropped output in pixels.
    pub height: u32,
    /// Opaque row-major RGBA8 pixels on a white background.
    pub pixels: Vec<u8>,
    /// Non-fatal warnings raised while rendering the containing page.
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

impl RenderWarning {
    /// Stable machine-readable identifier for bindings and logs.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedFont => "unsupported_font",
            Self::ImageDecodeFailure => "image_decode_failure",
        }
    }
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

    /// An extracted image placement cannot be mapped into its rendered page.
    #[error("image {reference} has invalid page bounds")]
    InvalidImageBounds { reference: String },
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
    /// PDF user space to rendered pixel space, including crop and rotation.
    transform: [f64; 6],
}

#[derive(Clone, Copy)]
struct PixelBounds {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

struct PreparedImage {
    reference: String,
    resource_name: String,
    occurrence: u32,
    bbox: [f32; 4],
    bounds: PixelBounds,
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
        rendered_pages.push(render_prepared_page(&pdf, &cache, prepared_page, scale)?);
    }

    Ok(rendered_pages)
}

/// Extract every positioned Image XObject as rendered RGBA8 pixels.
///
/// Each result carries a stable `pdf-image:...` reference. Enable
/// [`crate::MarkdownOptions::include_images`] when generating Markdown to emit
/// the same reference at the image's reading-order position.
///
/// Images are rendered as they appear on the page, so masks, clipping and PDF
/// stream encodings are resolved before the pixels reach the caller. Repeated
/// placements remain separate results because their page positions may differ.
pub fn extract_images_mem(
    pdf_bytes: &[u8],
    options: RenderOptions,
) -> Result<Vec<RenderedImage>, RenderError> {
    let image_items = crate::extractor::extract_image_items_mem_with_password(
        pdf_bytes,
        options.password.as_deref(),
    )
    .map_err(|error| match error {
        crate::PdfError::Encrypted => RenderError::Encrypted,
        _ => RenderError::Parse,
    })?;
    let image_count = image_items.len();
    let mut image_items_by_page = BTreeMap::<u32, Vec<_>>::new();
    for item in image_items {
        image_items_by_page
            .entry(item.page - 1)
            .or_default()
            .push(item);
    }
    let pages: Vec<_> = image_items_by_page.keys().copied().collect();
    validate_options(&pages, &options)?;
    if pages.is_empty() {
        return Ok(Vec::new());
    }

    let pdf = Pdf::new_with_password(
        pdf_bytes.to_vec(),
        options.password.as_deref().unwrap_or(""),
    )?;
    let scale = options.dpi / 72.0;
    let prepared_pages = pages
        .iter()
        .map(|&page| prepare_page(&pdf, page, scale).map(|(prepared, _)| prepared))
        .collect::<Result<Vec<_>, _>>()?;
    let mut prepared_images = BTreeMap::<u32, Vec<PreparedImage>>::new();
    let mut output_bytes = 0_u64;
    for page in &prepared_pages {
        let items = image_items_by_page
            .remove(&page.page)
            .expect("each prepared image page came from extracted items");
        let mut page_images = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            let occurrence = index as u32 + 1;
            let reference = crate::types::image_reference(item.page, occurrence);
            let bbox = [item.x, item.y, item.width, item.height];
            let bounds = image_pixel_bounds(page, bbox, &reference)?;
            let bytes = u64::from(bounds.right - bounds.left)
                .checked_mul(u64::from(bounds.bottom - bounds.top))
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or(RenderError::OutputTooLarge {
                    bytes: u64::MAX,
                    max: MAX_RENDER_OUTPUT_BYTES,
                })?;
            output_bytes = output_bytes
                .checked_add(bytes)
                .ok_or(RenderError::OutputTooLarge {
                    bytes: u64::MAX,
                    max: MAX_RENDER_OUTPUT_BYTES,
                })?;
            if output_bytes > MAX_RENDER_OUTPUT_BYTES {
                return Err(RenderError::OutputTooLarge {
                    bytes: output_bytes,
                    max: MAX_RENDER_OUTPUT_BYTES,
                });
            }
            let resource_name = item
                .text
                .strip_prefix("[Image: ")
                .and_then(|text| text.strip_suffix(']'))
                .unwrap_or(&item.text)
                .to_string();
            page_images.push(PreparedImage {
                reference,
                resource_name,
                occurrence,
                bbox,
                bounds,
            });
        }
        prepared_images.insert(page.page, page_images);
    }

    let cache = RenderCache::new();
    let mut images = Vec::with_capacity(image_count);
    for page in prepared_pages {
        let rendered = render_prepared_page(&pdf, &cache, page, scale)?;
        for image in prepared_images
            .remove(&page.page)
            .expect("each prepared page has image regions")
        {
            let (width, height, pixels) = crop_rgba(&rendered, image.bounds);
            images.push(RenderedImage {
                reference: image.reference,
                resource_name: image.resource_name,
                page: page.page,
                occurrence: image.occurrence,
                bbox: image.bbox,
                width,
                height,
                pixels,
                warnings: rendered.warnings.clone(),
            });
        }
    }

    Ok(images)
}

fn render_prepared_page<'a>(
    pdf: &'a Pdf,
    cache: &RenderCache<'a>,
    prepared: PreparedPage,
    scale: f32,
) -> Result<RenderedPage, RenderError> {
    // Preparation already validated every index against this same PDF.
    let page = pdf
        .pages()
        .get(prepared.page as usize)
        .ok_or(RenderError::PageOutOfRange {
            page: prepared.page,
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
        cache,
        &interpreter_settings,
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(prepared.width),
            height: Some(prepared.height),
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

    Ok(RenderedPage {
        page: prepared.page,
        width,
        height,
        pixels,
        warnings,
    })
}

fn image_pixel_bounds(
    page: &PreparedPage,
    [x, y, width, height]: [f32; 4],
    reference: &str,
) -> Result<PixelBounds, RenderError> {
    let point = |x: f32, y: f32| {
        (
            page.transform[0] * f64::from(x) + page.transform[2] * f64::from(y) + page.transform[4],
            page.transform[1] * f64::from(x) + page.transform[3] * f64::from(y) + page.transform[5],
        )
    };
    let corners = [
        point(x, y),
        point(x + width, y),
        point(x + width, y + height),
        point(x, y + height),
    ];
    let left = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min)
        .floor();
    let right = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil();
    let top = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min)
        .floor();
    let bottom = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil();
    if ![left, right, top, bottom]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(RenderError::InvalidImageBounds {
            reference: reference.to_string(),
        });
    }

    let left = left.clamp(0.0, f64::from(page.width)) as u32;
    let right = right.clamp(0.0, f64::from(page.width)) as u32;
    let top = top.clamp(0.0, f64::from(page.height)) as u32;
    let bottom = bottom.clamp(0.0, f64::from(page.height)) as u32;
    if left >= right || top >= bottom {
        return Err(RenderError::InvalidImageBounds {
            reference: reference.to_string(),
        });
    }

    Ok(PixelBounds {
        left,
        right,
        top,
        bottom,
    })
}

fn crop_rgba(page: &RenderedPage, bounds: PixelBounds) -> (u32, u32, Vec<u8>) {
    let crop_width = bounds.right - bounds.left;
    let crop_height = bounds.bottom - bounds.top;
    let row_bytes = crop_width as usize * 4;
    let mut pixels = Vec::with_capacity(row_bytes * crop_height as usize);
    for row in bounds.top..bounds.bottom {
        let start = (row as usize * page.width as usize + bounds.left as usize) * 4;
        pixels.extend_from_slice(&page.pixels[start..start + row_bytes]);
    }
    (crop_width, crop_height, pixels)
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
    let mut total_bytes = 0_u64;
    let mut prepared = Vec::with_capacity(pages.len());

    for &page_index in pages {
        let (page, page_bytes) = prepare_page(pdf, page_index, scale)?;
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
        prepared.push(page);
    }

    Ok(prepared)
}

fn prepare_page(
    pdf: &Pdf,
    page_index: u32,
    scale: f32,
) -> Result<(PreparedPage, u64), RenderError> {
    let page = pdf
        .pages()
        .get(page_index as usize)
        .ok_or(RenderError::PageOutOfRange {
            page: page_index,
            page_count: pdf.pages().len(),
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
    let transform = page.initial_transform(true).as_coeffs();
    let transform = [
        transform[0] * f64::from(scale),
        transform[1] * f64::from(scale),
        transform[2] * f64::from(scale),
        transform[3] * f64::from(scale),
        transform[4] * f64::from(scale),
        transform[5] * f64::from(scale),
    ];

    Ok((
        PreparedPage {
            page: page_index,
            width: width as u16,
            height: height as u16,
            transform,
        },
        page_bytes,
    ))
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
