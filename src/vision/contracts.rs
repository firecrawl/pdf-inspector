//! Public contracts between rendering, OCR, layout, and orchestration.

use std::error::Error;
use std::path::PathBuf;

use super::{RenderOptions, RenderedPage};

/// Selects when OCR may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OcrMode {
    /// Never run OCR. This is the default and preserves existing behavior.
    #[default]
    Off,
    /// Run OCR only on pages selected by pdf-inspector's OCR routing signals.
    Auto,
    /// Run OCR on every selected page, including pages with native text.
    Force,
}

/// Resource/quality profile for the OCR engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OcrProfile {
    /// Lowest latency and memory footprint.
    Edge,
    /// OCR-oriented balance of quality and CPU cost.
    #[default]
    Balanced,
    /// Highest quality within the lightweight model family.
    Quality,
}

/// Controls whether missing model artifacts may be fetched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelDownloadPolicy {
    /// Fetch a pinned artifact only after OCR has actually been selected.
    #[default]
    IfMissing,
    /// Never access the network; require an override or a warm model cache.
    Offline,
}

/// OCR engine configuration independent of a particular runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOptions {
    /// Page-level routing behavior.
    pub mode: OcrMode,
    /// Local quality/resource profile.
    pub profile: OcrProfile,
    /// Drop recognition spans below this confidence threshold.
    pub minimum_confidence: f32,
    /// Optional language hints understood by the selected engine.
    pub languages: Vec<String>,
    /// Optional directory containing an offline model set.
    pub model_directory: Option<PathBuf>,
    /// Whether a missing pinned artifact may be downloaded.
    pub model_downloads: ModelDownloadPolicy,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            mode: OcrMode::Off,
            profile: OcrProfile::Balanced,
            minimum_confidence: 0.0,
            languages: Vec::new(),
            model_directory: None,
            model_downloads: ModelDownloadPolicy::IfMissing,
        }
    }
}

impl OcrOptions {
    /// Creates OCR options with OCR disabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets page-level OCR routing.
    pub fn mode(mut self, mode: OcrMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the local resource/quality profile.
    pub fn profile(mut self, profile: OcrProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Sets the minimum accepted recognition confidence.
    pub fn minimum_confidence(mut self, minimum_confidence: f32) -> Self {
        self.minimum_confidence = minimum_confidence;
        self
    }

    /// Replaces the language hints passed to the OCR engine.
    pub fn languages(mut self, languages: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.languages = languages.into_iter().map(Into::into).collect();
        self
    }

    /// Uses an explicit model directory, suitable for offline packaging.
    pub fn model_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.model_directory = Some(directory.into());
        self
    }

    /// Sets the missing-model download policy.
    pub fn model_downloads(mut self, policy: ModelDownloadPolicy) -> Self {
        self.model_downloads = policy;
        self
    }
}

/// Configuration for an optional learned layout engine.
///
/// Layout inference is disabled by default. Existing deterministic layout,
/// table, and Markdown logic remains the assembly path when this is disabled.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutOptions {
    /// Whether the learned layout extension may run.
    pub enabled: bool,
    /// Drop layout regions below this confidence threshold.
    pub minimum_confidence: f32,
    /// Optional directory containing an offline layout model set.
    pub model_directory: Option<PathBuf>,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            minimum_confidence: 0.0,
            model_directory: None,
        }
    }
}

impl LayoutOptions {
    /// Creates layout options with learned layout disabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables or disables learned layout inference.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the minimum accepted region confidence.
    pub fn minimum_confidence(mut self, minimum_confidence: f32) -> Self {
        self.minimum_confidence = minimum_confidence;
        self
    }

    /// Uses an explicit layout model directory.
    pub fn model_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.model_directory = Some(directory.into());
        self
    }
}

/// A point in bitmap space, measured from the top-left in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImagePoint {
    /// Horizontal pixel coordinate.
    pub x: f32,
    /// Vertical pixel coordinate, increasing downward.
    pub y: f32,
}

impl ImagePoint {
    /// Creates a bitmap-space point.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Four-point polygon in bitmap coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImageQuad {
    /// Polygon points in engine-provided order.
    pub points: [ImagePoint; 4],
}

impl ImageQuad {
    /// Creates a four-point bitmap polygon.
    pub fn new(points: [ImagePoint; 4]) -> Self {
        Self { points }
    }
}

/// Stable identity for an inference model used in output provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentity {
    /// Model family/name, for example `pp-ocrv6-small`.
    pub name: String,
    /// Immutable model or artifact-set revision.
    pub revision: String,
}

impl ModelIdentity {
    /// Creates a model identity.
    pub fn new(name: impl Into<String>, revision: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            revision: revision.into(),
        }
    }
}

/// One positioned OCR recognition result in bitmap coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrSpan {
    /// Recognized text.
    pub text: String,
    /// Detection polygon in the original rendered page's pixel space.
    pub polygon: ImageQuad,
    /// Recognition confidence in the inclusive range 0–1.
    pub confidence: f32,
    /// Optional text-line orientation in clockwise degrees.
    pub orientation_degrees: Option<f32>,
}

/// OCR output for one 1-indexed page.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPage {
    /// 1-indexed PDF page number.
    pub page: u32,
    /// Positioned recognition spans.
    pub spans: Vec<OcrSpan>,
    /// Mean confidence across accepted spans, when available.
    pub mean_confidence: Option<f32>,
    /// Exact model identity used for this result.
    pub model: ModelIdentity,
    /// OCR wall time for this page.
    pub processing_time_ms: u64,
    /// Non-fatal engine warnings.
    pub warnings: Vec<String>,
}

/// Normalized semantic class emitted by a learned layout engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutRegionKind {
    /// Body or other prose text.
    Text,
    /// Document heading or title.
    Heading,
    /// Table region.
    Table,
    /// Figure/image region.
    Figure,
    /// Figure or table caption.
    Caption,
    /// Header/footer/page furniture.
    Furniture,
    /// Model-specific class retained without changing the common taxonomy.
    Other(String),
}

/// One learned layout region in bitmap coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutRegion {
    /// Normalized semantic class.
    pub kind: LayoutRegionKind,
    /// Region polygon in the original rendered page's pixel space.
    pub polygon: ImageQuad,
    /// Model confidence in the inclusive range 0–1.
    pub confidence: f32,
    /// Optional model-provided reading-order position.
    pub reading_order: Option<u32>,
}

/// Learned layout output for one 1-indexed page.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutPage {
    /// 1-indexed PDF page number.
    pub page: u32,
    /// Semantic regions.
    pub regions: Vec<LayoutRegion>,
    /// Exact model identity used for this result.
    pub model: ModelIdentity,
    /// Layout inference wall time for this page.
    pub processing_time_ms: u64,
    /// Non-fatal engine warnings.
    pub warnings: Vec<String>,
}

/// How final page content was sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageContentSource {
    /// Trusted native PDF text only.
    Native,
    /// OCR output only.
    Ocr,
    /// Native and OCR spans were fused.
    Fused,
}

/// Per-page local processing timings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisionTimings {
    /// Rasterization wall time.
    pub render_ms: u64,
    /// OCR wall time.
    pub ocr_ms: u64,
    /// Optional learned layout wall time.
    pub layout_ms: u64,
    /// Native/OCR fusion and assembly wall time.
    pub assembly_ms: u64,
}

/// Source and model metadata retained for one processed page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageProvenance {
    /// 1-indexed PDF page number.
    pub page: u32,
    /// Final page-content source.
    pub source: PageContentSource,
    /// OCR model, when OCR ran.
    pub ocr_model: Option<ModelIdentity>,
    /// Learned layout model, when layout inference ran.
    pub layout_model: Option<ModelIdentity>,
    /// Render resolution used for local vision.
    pub render_dpi: Option<f32>,
    /// Mean accepted OCR confidence, when available.
    pub ocr_confidence: Option<f32>,
    /// Stage timings.
    pub timings: VisionTimings,
    /// Non-fatal warnings surfaced to downstream users.
    pub warnings: Vec<String>,
    /// True when this lightweight local path detected a case better suited to
    /// Firecrawl's hosted document pipeline.
    pub hosted_recommended: bool,
}

/// Converts selected PDF pages into renderer-neutral owned bitmaps.
pub trait PageRenderer: Send + Sync {
    /// Renderer-specific failure type.
    type Error: Error + Send + Sync + 'static;

    /// Renders selected 1-indexed pages in the same order as `pages`.
    fn render_pages(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        password: Option<&str>,
        options: &RenderOptions,
    ) -> Result<Vec<RenderedPage>, Self::Error>;
}

/// Recognizes positioned text from rendered pages.
pub trait OcrEngine: Send + Sync {
    /// Engine-specific failure type.
    type Error: Error + Send + Sync + 'static;

    /// Exact model identity used by this engine instance.
    fn model(&self) -> &ModelIdentity;

    /// Recognizes pages in batch and returns results in input order.
    fn recognize(
        &self,
        pages: &[RenderedPage],
        options: &OcrOptions,
    ) -> Result<Vec<OcrPage>, Self::Error>;
}

/// Optional learned semantic layout extension.
pub trait LayoutEngine: Send + Sync {
    /// Engine-specific failure type.
    type Error: Error + Send + Sync + 'static;

    /// Exact model identity used by this engine instance.
    fn model(&self) -> &ModelIdentity;

    /// Analyzes rendered pages, optionally using their OCR spans.
    fn analyze(
        &self,
        pages: &[RenderedPage],
        ocr: &[OcrPage],
        options: &LayoutOptions,
    ) -> Result<Vec<LayoutPage>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_defaults_never_enable_recognition() {
        let options = OcrOptions::default();
        assert_eq!(options.mode, OcrMode::Off);
    }

    #[test]
    fn offline_model_override_is_explicit() {
        let options = OcrOptions::new()
            .mode(OcrMode::Auto)
            .model_directory("/models/pp-ocr")
            .model_downloads(ModelDownloadPolicy::Offline);
        assert_eq!(options.mode, OcrMode::Auto);
        assert_eq!(options.model_downloads, ModelDownloadPolicy::Offline);
        assert_eq!(
            options.model_directory,
            Some(PathBuf::from("/models/pp-ocr"))
        );
    }
}
