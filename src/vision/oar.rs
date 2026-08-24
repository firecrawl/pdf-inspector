//! PP-OCRv6 Small implementation backed by OAR and ONNX Runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use image::RgbImage;
use oar_ocr::core::config::onnx::OrtSessionConfig;
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{EdgeProcessor, TextCroppingProcessor};
use oar_ocr::predictors::{TextDetectionPredictor, TextRecognitionPredictor};
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

/// Standard detection input cap. PP-OCR detection resizes each page so its
/// longest side fits this before inference; it is the PaddleOCR default and
/// is sufficient for ordinary body text at 150 DPI.
const DETECTION_LIMIT_STANDARD: u32 = 960;

/// Escalated detection input cap for dense fine-print pages. Beyond this the
/// measured recall plateaus while inference cost keeps growing.
const DETECTION_LIMIT_ESCALATED: u32 = 2560;

/// Hard ceiling protecting detection from out-of-memory on giant renders.
const DETECTION_MAXIMUM_SIDE: u32 = 4000;

/// Escalate only for pages dense with small text: at least this many detected
/// regions in the standard pass...
const ESCALATION_MINIMUM_REGIONS: usize = 80;

/// ...whose median height, at detection scale, is below this. Calibrated at
/// `unclip_ratio` 2.0 (the expansion inflates measured heights, so this
/// constant is coupled to [`detection_config`]): dense fine-print pages that
/// gain from escalation measure 12.0–14.2 px with 144+ regions; the nearest
/// non-gaining page above the region gate (an engineering drawing) measures
/// 15.7 px, and prose/typewriter pages measure 14.5 px+ with too few
/// regions to qualify at all.
const ESCALATION_MAXIMUM_MEDIAN_HEIGHT: f32 = 15.0;

/// One worker's model sessions: a standard-limit detector plus a recognizer,
/// and that worker's own lazily built escalated-limit detector.
/// Staged (detect, crop, recognize as separate calls) rather than OAROCR's
/// combined `predict` so an escalated page replaces only its detection pass —
/// recognition runs exactly once, on the final region set.
struct OcrWorker {
    detector: TextDetectionPredictor,
    recognizer: TextRecognitionPredictor,
    /// Built on this worker's first dense fine-print page. `None` inside the
    /// cell records a failed build so it is not retried per page.
    escalated: std::sync::OnceLock<Option<TextDetectionPredictor>>,
}

/// CPU PP-OCRv6 Small engine using OAR's detection and recognition components.
///
/// Construction accepts only [`ModelPaths`] that have already passed
/// pdf-inspector's manifest size and SHA-256 verification. OAR's independent
/// model auto-download feature is deliberately not enabled.
pub struct OarOcrEngine {
    workers: Vec<OcrWorker>,
    detection_path: PathBuf,
    intra_threads: usize,
    /// Present only when more than one worker exists; sized to match.
    pool: Option<rayon::ThreadPool>,
    model: ModelIdentity,
}

impl std::fmt::Debug for OarOcrEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OarOcrEngine")
            .field("workers", &self.workers.len())
            .field("parallel", &self.pool.is_some())
            .field("model", &self.model)
            .finish()
    }
}

/// Pages processed concurrently: one OAROCR pipeline (and its ONNX sessions)
/// per worker, because oar-ocr serializes each session behind a mutex.
/// Measured on CPU: workers beyond 3 stop scaling (memory-bandwidth bound)
/// and each worker is fastest with 2 intra-op threads.
fn pipeline_concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    (cores / 4).clamp(1, 3)
}

fn intra_threads_per_pipeline(concurrency: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    if concurrency > 1 {
        2
    } else {
        cores.min(4)
    }
}

/// True when a standard-limit detection pass over a downscaled page shows
/// dense, small text: the page deserves a second pass at the escalated limit.
fn should_escalate_detection(
    median_detection_height: f32,
    region_count: usize,
    downscale: f32,
) -> bool {
    downscale < 1.0
        && region_count >= ESCALATION_MINIMUM_REGIONS
        && median_detection_height < ESCALATION_MAXIMUM_MEDIAN_HEIGHT
}

/// Median detected-region height in detection-input pixels: original-image
/// heights multiplied by the downscale detection applied.
fn median_detection_height(heights: &mut [f32], downscale: f32) -> f32 {
    if heights.is_empty() {
        return f32::MAX;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = heights.len() / 2;
    let median = if heights.len().is_multiple_of(2) {
        (heights[middle - 1] + heights[middle]) / 2.0
    } else {
        heights[middle]
    };
    median * downscale
}

/// Detection preprocessing config at a given input cap.
///
/// Supplying an explicit config suppresses OAROCR's "general" text-type
/// overrides, so every field the override would have set must be pinned
/// here to match what the combined pipeline ran with before the staged
/// split: score 0.3 and box 0.6 (equal to [`TextDetectionConfig`]'s
/// defaults) and unclip 2.0 (the default is 1.5 — leaving it would
/// silently shrink detection-box expansion and risk clipping edge glyphs).
fn detection_config(detection_limit: u32) -> TextDetectionConfig {
    TextDetectionConfig {
        limit_side_len: Some(detection_limit),
        limit_type: Some(oar_ocr::processors::LimitType::Max),
        max_side_len: Some(DETECTION_MAXIMUM_SIDE),
        unclip_ratio: 2.0,
        ..Default::default()
    }
}

fn build_detector(
    detection: &std::path::Path,
    detection_limit: u32,
    intra_threads: usize,
) -> Result<TextDetectionPredictor, OarOcrError> {
    Ok(TextDetectionPredictor::builder()
        .with_config(detection_config(detection_limit))
        .with_ort_config(ocr_session_config(intra_threads))
        .build(detection)?)
}

fn build_workers(
    detection: &std::path::Path,
    recognition: &std::path::Path,
    dictionary: &std::path::Path,
    count: usize,
    intra_threads: usize,
) -> Result<Vec<OcrWorker>, OarOcrError> {
    let mut workers = Vec::with_capacity(count);
    for _ in 0..count {
        let detector = build_detector(detection, DETECTION_LIMIT_STANDARD, intra_threads)?;
        let recognizer = TextRecognitionPredictor::builder()
            .dict_path(dictionary)
            .with_ort_config(ocr_session_config(intra_threads))
            .build(recognition)?;
        workers.push(OcrWorker {
            detector,
            recognizer,
            escalated: std::sync::OnceLock::new(),
        });
    }
    Ok(workers)
}

impl OarOcrEngine {
    /// Loads PP-OCRv6 Small from a resolved, verified model set.
    pub fn from_models(models: &ModelPaths) -> Result<Self, OarOcrError> {
        load_onnx_runtime()?;
        let detection = required_model(models, ModelArtifactKind::TextDetection)?;
        let recognition = required_model(models, ModelArtifactKind::TextRecognition)?;
        let dictionary = required_model(models, ModelArtifactKind::CharacterDictionary)?;

        let concurrency = pipeline_concurrency();
        let intra_threads = intra_threads_per_pipeline(concurrency);
        let workers = build_workers(
            detection,
            recognition,
            dictionary,
            concurrency,
            intra_threads,
        )?;
        let pool = if concurrency > 1 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(concurrency)
                .build()
                .ok()
        } else {
            None
        };
        let model = ModelIdentity::new(models.manifest_id(), models.revision());
        Ok(Self {
            workers,
            detection_path: detection.to_path_buf(),
            intra_threads,
            pool,
            model,
        })
    }

    /// This worker's escalated-limit detector, built on first use.
    fn escalated_detector<'w>(&self, worker: &'w OcrWorker) -> Option<&'w TextDetectionPredictor> {
        worker
            .escalated
            .get_or_init(|| {
                match build_detector(
                    &self.detection_path,
                    DETECTION_LIMIT_ESCALATED,
                    self.intra_threads,
                ) {
                    Ok(detector) => Some(detector),
                    Err(error) => {
                        log::warn!(
                            "escalated OCR detection unavailable, keeping standard pass: {error}"
                        );
                        None
                    }
                }
            })
            .as_ref()
    }

    /// Detects text regions for one page: a standard-limit pass first, then —
    /// for pages the standard limit demonstrably under-resolves — a second
    /// pass at the escalated limit whose boxes replace the first. Pages whose
    /// render dwarfs even the escalated limit skip the standard pass outright.
    fn detect_boxes(
        &self,
        page: &RenderedPage,
        image: &Arc<RgbImage>,
        worker: &OcrWorker,
    ) -> Result<Vec<BoundingBox>, OarOcrError> {
        let longest_side = page.width().max(page.height()) as f32;

        // A page more than twice the standard limit loses over half its
        // resolution before detection even runs; go straight to the escalated
        // detector instead of paying a doomed standard pass.
        if longest_side > (DETECTION_LIMIT_STANDARD * 2) as f32 {
            if let Some(escalated) = self.escalated_detector(worker) {
                log::debug!(
                    "page {}: direct escalated detection (render {longest_side}px)",
                    page.page(),
                );
                match detect_with(escalated, image, page.page()) {
                    Ok(boxes) => return Ok(boxes),
                    Err(error) => {
                        // Same degradation as the adaptive branch below: a
                        // failing escalated pass falls back to standard
                        // detection instead of failing the page outright.
                        // Return the standard boxes directly — the adaptive
                        // trigger would only re-invoke the detector that
                        // just failed (repeating an OOM on a dense page).
                        log::warn!(
                            "page {}: direct escalated detection failed, using standard pass: {error}",
                            page.page()
                        );
                        return detect_with(&worker.detector, image, page.page());
                    }
                }
            }
        }

        let detections = detect_with(&worker.detector, image, page.page())?;

        // Dense fine-print pages (broadsheets, pricing sheets) lose most of
        // their text when detection downscales them to the standard limit.
        // When the standard pass shows many regions of tiny detection-scale
        // height, rerun detection at the escalated limit.
        let downscale = (DETECTION_LIMIT_STANDARD as f32 / longest_side).min(1.0);
        let mut heights: Vec<f32> = detections.iter().map(polygon_height).collect();
        let median = median_detection_height(&mut heights, downscale);
        log::trace!(
            "page {}: standard pass {} regions, median height {:.1}px at detection scale",
            page.page(),
            detections.len(),
            median
        );
        if should_escalate_detection(median, detections.len(), downscale) {
            log::debug!(
                "page {}: escalating detection ({} regions, median height {:.1}px at detection scale)",
                page.page(),
                detections.len(),
                median
            );
            if let Some(escalated) = self.escalated_detector(worker) {
                match detect_with(escalated, image, page.page()) {
                    Ok(escalated_boxes) => return Ok(escalated_boxes),
                    Err(error) => {
                        log::warn!(
                            "page {}: escalated detection failed, keeping standard pass: {error}",
                            page.page()
                        );
                    }
                }
            }
        }
        Ok(detections)
    }

    fn recognize_page(
        &self,
        page: &RenderedPage,
        options: &OcrOptions,
        worker: usize,
    ) -> Result<OcrPage, OarOcrError> {
        let started = Instant::now();
        let worker = &self.workers[worker % self.workers.len()];
        let image = Arc::new(rendered_page_to_rgb(page)?);
        let boxes = self.detect_boxes(page, &image, worker)?;
        // Reading order, matching what the combined pipeline produced.
        let boxes = oar_ocr::processors::sort_quad_boxes(&boxes);

        // Same rotation-aware cropping the combined pipeline uses.
        let crops =
            TextCroppingProcessor::new(true).process((Arc::clone(&image), boxes.clone()))?;
        drop(image);

        let recognizer = &worker.recognizer;
        let mut spans = Vec::with_capacity(boxes.len());
        let mut invalid_geometry = 0usize;
        let mut missing_recognition = 0usize;
        for (bounding_box, crop) in boxes.iter().zip(crops) {
            let Some(crop) = crop else {
                invalid_geometry += 1;
                continue;
            };
            // One crop per call: document line crops often have very
            // different widths, and batching pads every crop to the widest
            // line. Measured on CPU, batched recognition (even width-sorted)
            // is 2–3× slower than per-crop calls.
            let crop = Arc::try_unwrap(crop).unwrap_or_else(|shared| (*shared).clone());
            let recognized = recognizer.predict(vec![crop])?;
            let (Some(text), Some(confidence)) = (
                recognized.texts.into_iter().next(),
                recognized.scores.into_iter().next(),
            ) else {
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

            let Some(polygon) = bounding_box_to_quad(bounding_box, page.width(), page.height())
            else {
                invalid_geometry += 1;
                continue;
            };
            spans.push(OcrSpan {
                text,
                polygon,
                confidence,
                // The combined pipeline's orientation_angle came from the
                // text-line-orientation classifier, a model this engine has
                // never loaded — it was structurally None before the staged
                // split too (the staged/combined A/B was byte-identical).
                // Region rotation is still carried by the polygon itself.
                orientation_degrees: None,
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

/// Runs one detector over one page image and returns its region polygons.
fn detect_with(
    detector: &TextDetectionPredictor,
    image: &Arc<RgbImage>,
    page_number: u32,
) -> Result<Vec<BoundingBox>, OarOcrError> {
    let mut result = detector.predict(vec![(**image).clone()])?;
    if result.detections.is_empty() {
        return Err(OarOcrError::MissingPageResult { page: page_number });
    }
    Ok(result
        .detections
        .swap_remove(0)
        .into_iter()
        .map(|detection| detection.bbox)
        .collect())
}

/// Vertical extent of a detection polygon in original-image pixels.
fn polygon_height(polygon: &BoundingBox) -> f32 {
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for point in &polygon.points {
        min_y = min_y.min(point.y);
        max_y = max_y.max(point.y);
    }
    if max_y > min_y {
        max_y - min_y
    } else {
        0.0
    }
}

fn ocr_session_config(intra_threads: usize) -> OrtSessionConfig {
    OrtSessionConfig::new()
        .with_intra_threads(intra_threads.max(1))
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

        let Some(pool) = self.pool.as_ref().filter(|_| pages.len() > 1) else {
            return pages
                .iter()
                .map(|page| self.recognize_page(page, options, 0))
                .collect();
        };
        pool.install(|| {
            use rayon::prelude::*;
            pages
                .par_iter()
                .map(|page| {
                    let worker = rayon::current_thread_index().unwrap_or(0);
                    self.recognize_page(page, options, worker)
                })
                .collect()
        })
    }

    fn preferred_page_concurrency(&self) -> usize {
        // Without a pool, recognition runs sequentially regardless of worker
        // count — report that honestly so the pipeline doesn't render
        // oversized page batches for parallelism that isn't there.
        if self.pool.is_some() {
            self.workers.len()
        } else {
            1
        }
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
                for (rgba, rgb) in input
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(output.as_chunks_mut::<3>().0)
                {
                    rgb.copy_from_slice(&rgba[..3]);
                }
            }
            RenderPixelFormat::Gray8 => {
                for (&gray, rgb) in input.iter().zip(output.as_chunks_mut::<3>().0) {
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
        let concurrency = pipeline_concurrency();
        let config = ocr_session_config(intra_threads_per_pipeline(concurrency));
        assert!((1..=4).contains(&config.intra_threads.unwrap()));
        assert_eq!(config.inter_threads, Some(1));
        assert_eq!(config.parallel_execution, Some(false));
        // Zero requests are clamped so a session always has a thread.
        assert_eq!(ocr_session_config(0).intra_threads, Some(1));
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

    #[test]
    fn escalation_fires_for_dense_fine_print_pages() {
        // Measured cases (at unclip 2.0) that gain from escalation: dense
        // tiled ad pages (12.3–14.2px, 158–286 regions) and a dense pricing
        // sheet (12.0px, 144 regions), all downscaled by the standard limit.
        assert!(should_escalate_detection(14.2, 186, 0.55));
        assert!(should_escalate_detection(13.1, 286, 0.55));
        assert!(should_escalate_detection(12.3, 158, 0.55));
        assert!(should_escalate_detection(12.0, 144, 0.55));
    }

    #[test]
    fn escalation_skips_ordinary_pages() {
        // Academic prose: too few regions (and tall enough at unclip 2.0).
        assert!(!should_escalate_detection(14.5, 47, 0.58));
        // Engineering drawing: many regions but tall enough text.
        assert!(!should_escalate_detection(15.7, 205, 0.58));
        // Typewriter scan: tall text, few regions.
        assert!(!should_escalate_detection(17.5, 77, 0.55));
        // Page not downscaled at all: escalation cannot add pixels.
        assert!(!should_escalate_detection(9.0, 300, 1.0));
    }

    #[test]
    fn median_detection_height_scales_and_handles_empty() {
        let mut heights = vec![30.0, 10.0, 20.0];
        assert_eq!(median_detection_height(&mut heights, 0.5), 10.0);
        // Even counts average the two middle values instead of picking the
        // upper one, so borderline pages don't skew away from escalation.
        let mut even = vec![10.0, 12.0, 14.0, 30.0];
        assert_eq!(median_detection_height(&mut even, 1.0), 13.0);
        let mut empty: Vec<f32> = Vec::new();
        assert_eq!(median_detection_height(&mut empty, 0.5), f32::MAX);
    }

    #[test]
    fn concurrency_derivations_stay_in_bounds() {
        let concurrency = pipeline_concurrency();
        assert!((1..=3).contains(&concurrency));
        assert!(intra_threads_per_pipeline(2) == 2);
        assert!((1..=4).contains(&intra_threads_per_pipeline(1)));
    }
}
