//! Renderer-neutral page bitmap and coordinate types.

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
}

/// A point in PDF page space, measured in points from the bottom-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    /// Horizontal position in PDF points.
    pub x: f32,
    /// Vertical position in PDF points, increasing upward.
    pub y: f32,
}

/// Affine transform between top-left pixel space and PDF page space.
///
/// Renderers create this from the page-space images of the bitmap corners.
/// Keeping the coefficients in pdf-inspector makes [`RenderedPage`] neutral
/// to the renderer implementation that produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageTransform {
    forward: [f64; 6],
    inverse: [f64; 6],
    pixel_width: u32,
    pixel_height: u32,
}

impl PageTransform {
    /// Builds a transform from the PDF-space images of device corners
    /// `(0, 0)`, `(pixel_width, 0)`, and `(0, pixel_height)`.
    pub fn from_corners(
        pixel_width: u32,
        pixel_height: u32,
        origin: (f64, f64),
        x_axis: (f64, f64),
        y_axis: (f64, f64),
    ) -> Option<Self> {
        if pixel_width == 0 || pixel_height == 0 {
            return None;
        }

        let values = [origin.0, origin.1, x_axis.0, x_axis.1, y_axis.0, y_axis.1];
        if values.iter().any(|value| !value.is_finite()) {
            return None;
        }

        let width = f64::from(pixel_width);
        let height = f64::from(pixel_height);
        let a = (x_axis.0 - origin.0) / width;
        let c = (x_axis.1 - origin.1) / width;
        let b = (y_axis.0 - origin.0) / height;
        let d = (y_axis.1 - origin.1) / height;
        let (e, f) = origin;
        let forward = [a, b, c, d, e, f];
        if forward.iter().any(|coefficient| !coefficient.is_finite()) {
            return None;
        }
        let determinant = a * d - b * c;
        if determinant == 0.0 || !determinant.is_finite() {
            return None;
        }

        let inverse_a = d / determinant;
        let inverse_b = -b / determinant;
        let inverse_c = -c / determinant;
        let inverse_d = a / determinant;
        let inverse_e = -(inverse_a * e + inverse_b * f);
        let inverse_f = -(inverse_c * e + inverse_d * f);
        let inverse = [
            inverse_a, inverse_b, inverse_c, inverse_d, inverse_e, inverse_f,
        ];
        if inverse.iter().any(|coefficient| !coefficient.is_finite()) {
            return None;
        }

        Some(Self {
            forward,
            inverse,
            pixel_width,
            pixel_height,
        })
    }

    /// Width of the bitmap this transform describes.
    pub fn pixel_width(&self) -> u32 {
        self.pixel_width
    }

    /// Height of the bitmap this transform describes.
    pub fn pixel_height(&self) -> u32 {
        self.pixel_height
    }

    /// Converts a bitmap point to PDF page space.
    pub fn pixel_to_page(&self, x: f64, y: f64) -> PagePoint {
        let [a, b, c, d, e, f] = self.forward;
        PagePoint {
            x: (a * x + b * y + e) as f32,
            y: (c * x + d * y + f) as f32,
        }
    }

    /// Converts a PDF page-space point to bitmap coordinates.
    pub fn page_to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.inverse;
        (a * x + b * y + e, c * x + d * y + f)
    }
}

/// Invalid renderer output rejected by [`RenderedPage::new`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderBufferError {
    /// Page numbers are 1-indexed.
    #[error("rendered page number must be at least 1")]
    InvalidPageNumber,
    /// Bitmap dimensions must be non-zero.
    #[error("rendered bitmap dimensions must be non-zero")]
    InvalidDimensions,
    /// Page dimensions must be positive finite numbers.
    #[error("rendered PDF page dimensions must be positive and finite")]
    InvalidPageDimensions,
    /// Transform dimensions must match the bitmap dimensions.
    #[error("coordinate transform dimensions do not match the rendered bitmap")]
    TransformDimensions,
    /// Renderer did not provide an invertible finite coordinate transform.
    #[error("renderer returned an invalid coordinate transform")]
    InvalidTransform,
    /// The stride cannot hold one active row of pixels.
    #[error("pixel stride {stride} is shorter than the active row size {minimum}")]
    InvalidStride {
        /// Supplied bytes per row.
        stride: usize,
        /// Minimum bytes required for one row.
        minimum: usize,
    },
    /// Pixel buffer size is inconsistent with height and stride.
    #[error("pixel buffer has {actual} bytes; expected {expected}")]
    InvalidBufferLength {
        /// Actual byte count.
        actual: usize,
        /// Required byte count.
        expected: usize,
    },
    /// Dimension arithmetic overflowed the host address space.
    #[error("rendered bitmap dimensions overflow the host address space")]
    SizeOverflow,
}

/// One rendered page with owned pixels and its pixel-to-PDF transform.
///
/// The value contains no live renderer, page, or document handles. It can be
/// moved to an OCR worker and retained after rendering returns.
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
    transform: PageTransform,
}

impl RenderedPage {
    /// Creates a renderer-neutral owned page after validating its buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        page: u32,
        page_width: f32,
        page_height: f32,
        width: u32,
        height: u32,
        stride: usize,
        format: RenderPixelFormat,
        pixels: Vec<u8>,
        transform: PageTransform,
    ) -> Result<Self, RenderBufferError> {
        if page == 0 {
            return Err(RenderBufferError::InvalidPageNumber);
        }
        if width == 0 || height == 0 {
            return Err(RenderBufferError::InvalidDimensions);
        }
        if page_width <= 0.0
            || page_height <= 0.0
            || !page_width.is_finite()
            || !page_height.is_finite()
        {
            return Err(RenderBufferError::InvalidPageDimensions);
        }
        if transform.pixel_width() != width || transform.pixel_height() != height {
            return Err(RenderBufferError::TransformDimensions);
        }

        let row_bytes = (width as usize)
            .checked_mul(format.bytes_per_pixel())
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

        Ok(Self {
            page,
            page_width,
            page_height,
            width,
            height,
            stride,
            format,
            pixels,
            transform,
        })
    }

    /// 1-indexed page number.
    pub fn page(&self) -> u32 {
        self.page
    }

    /// Page width in PDF points after applying the page's rotation.
    pub fn page_width(&self) -> f32 {
        self.page_width
    }

    /// Page height in PDF points after applying the page's rotation.
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

    /// Coordinate transform associated with the rendered page.
    pub fn transform(&self) -> PageTransform {
        self.transform
    }

    /// Converts a bitmap point (top-left origin, y-down) to PDF page space
    /// (bottom-left origin, y-up).
    pub fn pixel_to_page(&self, x: f64, y: f64) -> PagePoint {
        self.transform.pixel_to_page(x, y)
    }

    /// Converts a bitmap rectangle to the repository's existing PDF-space
    /// rectangle type. The returned page number remains 1-indexed.
    pub fn pixel_rect_to_pdf_rect(&self, x: f64, y: f64, width: f64, height: f64) -> PdfRect {
        let points = [
            self.transform.pixel_to_page(x, y),
            self.transform.pixel_to_page(x + width, y),
            self.transform.pixel_to_page(x, y + height),
            self.transform.pixel_to_page(x + width, y + height),
        ];
        let left = points
            .iter()
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let right = points
            .iter()
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let bottom = points
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        let top = points
            .iter()
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        PdfRect {
            x: left,
            y: bottom,
            width: right - left,
            height: top - bottom,
            page: self.page,
        }
    }

    /// Converts a PDF-space rectangle to bitmap coordinates
    /// `(x, y, width, height)` with a top-left origin.
    pub fn pdf_rect_to_pixel(&self, rect: &PdfRect) -> (f64, f64, f64, f64) {
        let left = f64::from(rect.x);
        let right = f64::from(rect.x + rect.width);
        let bottom = f64::from(rect.y);
        let top = f64::from(rect.y + rect.height);
        let points = [
            self.transform.page_to_pixel(left, bottom),
            self.transform.page_to_pixel(right, bottom),
            self.transform.page_to_pixel(left, top),
            self.transform.page_to_pixel(right, top),
        ];
        let min_x = points
            .iter()
            .map(|point| point.0)
            .fold(f64::INFINITY, f64::min);
        let max_x = points
            .iter()
            .map(|point| point.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = points
            .iter()
            .map(|point| point.1)
            .fold(f64::INFINITY, f64::min);
        let max_y = points
            .iter()
            .map(|point| point.1)
            .fold(f64::NEG_INFINITY, f64::max);
        (min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transform() -> PageTransform {
        PageTransform::from_corners(400, 200, (0.0, 100.0), (200.0, 100.0), (0.0, 0.0)).unwrap()
    }

    #[test]
    fn transform_maps_both_directions_at_non_identity_scale() {
        let transform = transform();
        let point = transform.pixel_to_page(100.0, 50.0);
        assert!((point.x - 50.0).abs() < 1e-6);
        assert!((point.y - 75.0).abs() < 1e-6);
        let pixel = transform.page_to_pixel(f64::from(point.x), f64::from(point.y));
        assert!((pixel.0 - 100.0).abs() < 1e-6);
        assert!((pixel.1 - 50.0).abs() < 1e-6);
    }

    #[test]
    fn rendered_page_accepts_padding_and_validates_length() {
        let page = RenderedPage::new(
            1,
            200.0,
            100.0,
            400,
            200,
            1_204,
            RenderPixelFormat::Rgb8,
            vec![0; 1_204 * 200],
            transform(),
        )
        .unwrap();
        assert_eq!(page.stride(), 1_204);

        assert!(matches!(
            RenderedPage::new(
                1,
                200.0,
                100.0,
                400,
                200,
                1_204,
                RenderPixelFormat::Rgb8,
                vec![0; 5],
                transform(),
            ),
            Err(RenderBufferError::InvalidBufferLength { .. })
        ));
    }

    #[test]
    fn rotated_transform_round_trips_rectangles() {
        let transform =
            PageTransform::from_corners(100, 200, (0.0, 0.0), (0.0, 100.0), (200.0, 0.0)).unwrap();
        let page = RenderedPage::new(
            1,
            200.0,
            100.0,
            100,
            200,
            300,
            RenderPixelFormat::Rgb8,
            vec![0; 300 * 200],
            transform,
        )
        .unwrap();
        let pdf = page.pixel_rect_to_pdf_rect(10.0, 20.0, 30.0, 40.0);
        let pixel = page.pdf_rect_to_pixel(&pdf);
        assert!((pixel.0 - 10.0).abs() < 1e-5);
        assert!((pixel.1 - 20.0).abs() < 1e-5);
        assert!((pixel.2 - 30.0).abs() < 1e-5);
        assert!((pixel.3 - 40.0).abs() < 1e-5);
    }

    #[test]
    fn skewed_transform_bounds_all_rectangle_corners() {
        let transform =
            PageTransform::from_corners(100, 100, (0.0, 100.0), (100.0, 125.0), (25.0, 0.0))
                .unwrap();
        let page = RenderedPage::new(
            1,
            125.0,
            125.0,
            100,
            100,
            300,
            RenderPixelFormat::Rgb8,
            vec![0; 30_000],
            transform,
        )
        .unwrap();

        let pdf = page.pixel_rect_to_pdf_rect(10.0, 20.0, 30.0, 40.0);
        assert!((pdf.x - 15.0).abs() < 1e-5);
        assert!((pdf.y - 42.5).abs() < 1e-5);
        assert!((pdf.width - 40.0).abs() < 1e-5);
        assert!((pdf.height - 47.5).abs() < 1e-5);

        let pixels = page.pdf_rect_to_pixel(&pdf);
        assert!(pixels.0 <= 10.0 && pixels.1 <= 20.0);
        assert!(pixels.0 + pixels.2 >= 40.0 && pixels.1 + pixels.3 >= 60.0);
    }
}
