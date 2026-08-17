//! PP-OCRv6 Small implementation backed by OAR and ONNX Runtime.

use std::path::PathBuf;
use std::time::Instant;

use image::RgbImage;
use oar_ocr::core::config::onnx::OrtSessionConfig;
use oar_ocr::oarocr::{OAROCRBuilder, OAROCR};
use oar_ocr::processors::BoundingBox;
use thiserror::Error;

use super::{
    ImagePoint, ImageQuad, ModelArtifactKind, ModelIdentity, ModelPaths, OcrEngine, OcrMode,
    OcrOptions, OcrPage, OcrSpan, RenderPixelFormat, RenderedPage,
};

/// Environment variable selecting the ONNX Runtime shared library.
pub const ONNX_RUNTIME_LIBRARY_ENV: &str = "ORT_DYLIB_PATH";

/// Failures while constructing or running the OAR OCR backend.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OarOcrError {
    /// A required file is missing from the resolved model set.
    #[error("resolved OCR model set is missing {kind:?}")]
    MissingModelArtifact {
        /// Missing artifact role.
        kind: ModelArtifactKind,
    },
    /// OCR was invoked while the caller explicitly disabled it.
    #[error("OCR is disabled; select Auto or Force before invoking the engine")]
    OcrDisabled,
    /// Confidence thresholds must match the normalized engine output range.
    #[error("minimum OCR confidence must be finite and between 0 and 1, got {value}")]
    InvalidMinimumConfidence {
        /// Invalid threshold.
        value: f32,
    },
    /// Bitmap dimension arithmetic exceeded the host address space.
    #[error("rendered page {page} bitmap dimensions overflow the host address space")]
    ImageSizeOverflow {
        /// 1-indexed page number.
        page: u32,
    },
    /// A validated renderer buffer could not be represented as an RGB image.
    #[error("rendered page {page} could not be converted to an RGB image")]
    InvalidImageBuffer {
        /// 1-indexed page number.
        page: u32,
    },
    /// The external ONNX Runtime shared library could not be loaded.
    #[error(
        "failed to load ONNX Runtime from {path}; install a compatible ONNX Runtime shared library or set ORT_DYLIB_PATH to its path: {source}"
    )]
    OnnxRuntimeLoad {
        /// Requested shared-library path or platform library name.
        path: PathBuf,
        /// Dynamic-loader failure.
        #[source]
        source: ort::LoadDynamicError,
    },
    /// OAR returned no result for a submitted page.
    #[error("OAR returned no result for rendered page {page}")]
    MissingPageResult {
        /// 1-indexed page number.
        page: u32,
    },
    /// OAR or ONNX Runtime rejected the models or failed during inference.
    #[error(transparent)]
    Backend(#[from] oar_ocr::core::OCRError),
}

/// CPU PP-OCRv6 Small engine using OAR's detection and recognition pipeline.
///
/// Construction accepts only [`ModelPaths`] that have already passed
/// pdf-inspector's manifest size and SHA-256 verification. OAR's independent
/// model auto-download feature is deliberately not enabled.
#[derive(Debug)]
pub struct OarOcrEngine {
    pipeline: OAROCR,
    model: ModelIdentity,
}

impl OarOcrEngine {
    /// Loads PP-OCRv6 Small from a resolved, verified model set.
    pub fn from_models(models: &ModelPaths) -> Result<Self, OarOcrError> {
        load_onnx_runtime()?;
        let detection = required_model(models, ModelArtifactKind::TextDetection)?;
        let recognition = required_model(models, ModelArtifactKind::TextRecognition)?;
        let dictionary = required_model(models, ModelArtifactKind::CharacterDictionary)?;

        let pipeline = OAROCRBuilder::new(detection, recognition, dictionary)
            .ort_session(ocr_session_config())
            // Document line crops often have very different widths. Keeping
            // CPU recognition batches at one avoids padding every crop to the
            // widest line, reducing both inference work and peak memory.
            .region_batch_size(1)
            .build()?;
        let model = ModelIdentity::new(models.manifest_id(), models.revision());
        Ok(Self { pipeline, model })
    }

    fn recognize_page(
        &self,
        page: &RenderedPage,
        options: &OcrOptions,
    ) -> Result<OcrPage, OarOcrError> {
        let started = Instant::now();
        let image = rendered_page_to_rgb(page)?;
        let result = self
            .pipeline
            .predict(vec![image])?
            .into_iter()
            .next()
            .ok_or(OarOcrError::MissingPageResult { page: page.page() })?;

        let mut spans = Vec::with_capacity(result.text_regions.len());
        let mut invalid_geometry = 0usize;
        let mut missing_recognition = 0usize;
        for region in result.text_regions {
            let (Some(text), Some(confidence)) = (region.text, region.confidence) else {
                missing_recognition += 1;
                continue;
            };
            if text.trim().is_empty() || !confidence.is_finite() {
                missing_recognition += 1;
                continue;
            }
            let confidence = confidence.clamp(0.0, 1.0);
            if confidence < options.minimum_confidence {
                continue;
            }

            let polygon = region.dt_poly.as_ref().unwrap_or(&region.bounding_box);
            let Some(polygon) = bounding_box_to_quad(polygon, page.width(), page.height()) else {
                invalid_geometry += 1;
                continue;
            };
            spans.push(OcrSpan {
                text: text.to_string(),
                polygon,
                confidence,
                orientation_degrees: region.orientation_angle,
            });
        }

        let mut warnings = Vec::new();
        if missing_recognition > 0 {
            warnings.push(format!(
                "discarded {missing_recognition} regions without usable recognition output"
            ));
        }
        if invalid_geometry > 0 {
            warnings.push(format!(
                "discarded {invalid_geometry} recognized regions with invalid geometry"
            ));
        }

        let mean_confidence = if spans.is_empty() {
            None
        } else {
            Some(spans.iter().map(|span| span.confidence).sum::<f32>() / spans.len() as f32)
        };
        let processing_time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(OcrPage {
            page_number: page.page(),
            spans,
            mean_confidence,
            model: self.model.clone(),
            processing_time_ms,
            warnings,
        })
    }
}

fn ocr_session_config() -> OrtSessionConfig {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    OrtSessionConfig::new()
        .with_intra_threads(available.min(4))
        .with_inter_threads(1)
        .with_parallel_execution(false)
}

fn load_onnx_runtime() -> Result<(), OarOcrError> {
    let path = onnx_runtime_library_path();
    drop(
        ort::init_from(&path).map_err(|source| OarOcrError::OnnxRuntimeLoad {
            path: path.clone(),
            source,
        })?,
    );
    Ok(())
}

pub(crate) fn onnx_runtime_library_path() -> PathBuf {
    std::env::var_os(ONNX_RUNTIME_LIBRARY_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_onnx_runtime_library)
}

fn default_onnx_runtime_library() -> PathBuf {
    #[cfg(target_os = "windows")]
    const NAME: &str = "onnxruntime.dll";
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    const NAME: &str = "libonnxruntime.so";
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const NAME: &str = "libonnxruntime.dylib";
    PathBuf::from(NAME)
}

impl OcrEngine for OarOcrEngine {
    type Error = OarOcrError;

    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn recognize(
        &self,
        pages: &[RenderedPage],
        options: &OcrOptions,
    ) -> Result<Vec<OcrPage>, Self::Error> {
        validate_options(options)?;

        pages
            .iter()
            .map(|page| self.recognize_page(page, options))
            .collect()
    }
}

fn validate_options(options: &OcrOptions) -> Result<(), OarOcrError> {
    if options.mode == OcrMode::Off {
        return Err(OarOcrError::OcrDisabled);
    }
    if !options.minimum_confidence.is_finite() || !(0.0..=1.0).contains(&options.minimum_confidence)
    {
        return Err(OarOcrError::InvalidMinimumConfidence {
            value: options.minimum_confidence,
        });
    }
    Ok(())
}

fn required_model(
    models: &ModelPaths,
    kind: ModelArtifactKind,
) -> Result<&std::path::Path, OarOcrError> {
    models
        .get(kind)
        .ok_or(OarOcrError::MissingModelArtifact { kind })
}

fn rendered_page_to_rgb(page: &RenderedPage) -> Result<RgbImage, OarOcrError> {
    let width = usize::try_from(page.width())
        .map_err(|_| OarOcrError::ImageSizeOverflow { page: page.page() })?;
    let height = usize::try_from(page.height())
        .map_err(|_| OarOcrError::ImageSizeOverflow { page: page.page() })?;
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or(OarOcrError::ImageSizeOverflow { page: page.page() })?;
    let input_bpp = page.format().bytes_per_pixel();
    let active_input_row = width
        .checked_mul(input_bpp)
        .ok_or(OarOcrError::ImageSizeOverflow { page: page.page() })?;
    let output_row = width
        .checked_mul(3)
        .ok_or(OarOcrError::ImageSizeOverflow { page: page.page() })?;

    let mut rgb = vec![0u8; output_len];
    for row in 0..height {
        let input_start = row * page.stride();
        let input = &page.pixels()[input_start..input_start + active_input_row];
        let output_start = row * output_row;
        let output = &mut rgb[output_start..output_start + output_row];
        match page.format() {
            RenderPixelFormat::Rgb8 => output.copy_from_slice(input),
            RenderPixelFormat::Rgba8 => {
                for (rgba, rgb) in input.chunks_exact(4).zip(output.chunks_exact_mut(3)) {
                    rgb.copy_from_slice(&rgba[..3]);
                }
            }
            RenderPixelFormat::Gray8 => {
                for (&gray, rgb) in input.iter().zip(output.chunks_exact_mut(3)) {
                    rgb.fill(gray);
                }
            }
        }
    }

    RgbImage::from_raw(page.width(), page.height(), rgb)
        .ok_or(OarOcrError::InvalidImageBuffer { page: page.page() })
}

fn bounding_box_to_quad(bounding_box: &BoundingBox, width: u32, height: u32) -> Option<ImageQuad> {
    let points: Vec<ImagePoint> = bounding_box
        .points
        .iter()
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .map(|point| {
            ImagePoint::new(
                point.x.clamp(0.0, width as f32),
                point.y.clamp(0.0, height as f32),
            )
        })
        .collect();

    if bounding_box.points.len() == 4 && points.len() == 4 && is_ordered_convex_quad(&points) {
        return Some(ImageQuad::new([points[0], points[1], points[2], points[3]]));
    }
    if points.len() < 3 {
        return None;
    }

    let min_x = points
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let max_x = points
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let max_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    Some(ImageQuad::new([
        ImagePoint::new(min_x, min_y),
        ImagePoint::new(max_x, min_y),
        ImagePoint::new(max_x, max_y),
        ImagePoint::new(min_x, max_y),
    ]))
}

fn is_ordered_convex_quad(points: &[ImagePoint]) -> bool {
    if points.len() != 4 {
        return false;
    }
    let mut orientation = 0.0_f32;
    for index in 0..4 {
        let first = points[index];
        let second = points[(index + 1) % 4];
        let third = points[(index + 2) % 4];
        let cross = (second.x - first.x) * (third.y - second.y)
            - (second.y - first.y) * (third.x - second.x);
        if cross.abs() <= f32::EPSILON {
            return false;
        }
        if orientation == 0.0 {
            orientation = cross.signum();
        } else if cross.signum() != orientation {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use oar_ocr::processors::Point;

    use super::*;
    use crate::vision::PageTransform;

    #[test]
    fn cpu_session_budget_is_bounded_for_small_ocr_models() {
        let config = ocr_session_config();
        assert!((1..=4).contains(&config.intra_threads.unwrap()));
        assert_eq!(config.inter_threads, Some(1));
        assert_eq!(config.parallel_execution, Some(false));
    }

    fn page(format: RenderPixelFormat, stride: usize, pixels: Vec<u8>) -> RenderedPage {
        let transform =
            PageTransform::from_corners(2, 2, (0.0, 2.0), (2.0, 2.0), (0.0, 0.0)).unwrap();
        RenderedPage::new(1, 2.0, 2.0, 2, 2, stride, format, pixels, transform).unwrap()
    }

    #[test]
    fn converts_padded_rgb_without_exposing_padding() {
        let page = page(
            RenderPixelFormat::Rgb8,
            8,
            vec![1, 2, 3, 4, 5, 6, 99, 99, 7, 8, 9, 10, 11, 12, 99, 99],
        );
        let image = rendered_page_to_rgb(&page).unwrap();
        assert_eq!(image.as_raw(), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn converts_rgba_and_gray_to_rgb() {
        let rgba = page(
            RenderPixelFormat::Rgba8,
            8,
            vec![1, 2, 3, 44, 4, 5, 6, 55, 7, 8, 9, 66, 10, 11, 12, 77],
        );
        assert_eq!(
            rendered_page_to_rgb(&rgba).unwrap().as_raw(),
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );

        let gray = page(RenderPixelFormat::Gray8, 2, vec![1, 2, 3, 4]);
        assert_eq!(
            rendered_page_to_rgb(&gray).unwrap().as_raw(),
            &[1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4]
        );
    }

    #[test]
    fn preserves_quads_and_clamps_them_to_the_bitmap() {
        let bbox = BoundingBox::new(vec![
            Point::new(-1.0, 2.0),
            Point::new(11.0, 2.0),
            Point::new(11.0, 9.0),
            Point::new(-1.0, 9.0),
        ]);
        let quad = bounding_box_to_quad(&bbox, 10, 8).unwrap();
        assert_eq!(quad.points[0], ImagePoint::new(0.0, 2.0));
        assert_eq!(quad.points[2], ImagePoint::new(10.0, 8.0));
    }

    #[test]
    fn reduces_polygons_to_a_stable_axis_aligned_quad() {
        let bbox = BoundingBox::new(vec![
            Point::new(2.0, 1.0),
            Point::new(7.0, 2.0),
            Point::new(8.0, 6.0),
            Point::new(5.0, 9.0),
            Point::new(1.0, 5.0),
        ]);
        let quad = bounding_box_to_quad(&bbox, 10, 10).unwrap();
        assert_eq!(quad.points[0], ImagePoint::new(1.0, 1.0));
        assert_eq!(quad.points[2], ImagePoint::new(8.0, 9.0));
    }

    #[test]
    fn normalizes_unordered_or_partially_invalid_quads() {
        let unordered = BoundingBox::new(vec![
            Point::new(1.0, 1.0),
            Point::new(8.0, 8.0),
            Point::new(8.0, 1.0),
            Point::new(1.0, 8.0),
        ]);
        let quad = bounding_box_to_quad(&unordered, 10, 10).unwrap();
        assert_eq!(quad.points[0], ImagePoint::new(1.0, 1.0));
        assert_eq!(quad.points[1], ImagePoint::new(8.0, 1.0));
        assert_eq!(quad.points[2], ImagePoint::new(8.0, 8.0));

        let partially_invalid = BoundingBox::new(vec![
            Point::new(8.0, 8.0),
            Point::new(f32::NAN, 4.0),
            Point::new(1.0, 8.0),
            Point::new(8.0, 1.0),
            Point::new(1.0, 1.0),
        ]);
        let quad = bounding_box_to_quad(&partially_invalid, 10, 10).unwrap();
        assert_eq!(quad.points[0], ImagePoint::new(1.0, 1.0));
        assert_eq!(quad.points[1], ImagePoint::new(8.0, 1.0));
        assert_eq!(quad.points[2], ImagePoint::new(8.0, 8.0));
    }

    #[test]
    fn refuses_disabled_or_invalid_options_before_inference() {
        assert!(matches!(
            validate_options(&OcrOptions::new()),
            Err(OarOcrError::OcrDisabled)
        ));
        for value in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            let options = OcrOptions::new()
                .mode(OcrMode::Force)
                .minimum_confidence(value);
            assert!(matches!(
                validate_options(&options),
                Err(OarOcrError::InvalidMinimumConfidence { .. })
            ));
        }
        assert!(validate_options(
            &OcrOptions::new()
                .mode(OcrMode::Auto)
                .minimum_confidence(1.0)
        )
        .is_ok());
    }
}
