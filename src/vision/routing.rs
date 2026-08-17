//! Page routing and renderer/OCR orchestration without Markdown fusion.

use std::collections::BTreeSet;
use std::error::Error;
use std::time::Instant;

use thiserror::Error;

use super::{OcrEngine, OcrMode, OcrOptions, OcrPage, PageRenderer, RenderOptions, RenderedPage};

/// A rendered page paired with OCR output in the same bitmap coordinate space.
#[derive(Debug)]
pub struct RoutedOcrPage {
    /// Renderer-owned bitmap and pixel↔PDF transform.
    pub rendered: RenderedPage,
    /// Positioned OCR spans for the bitmap.
    pub ocr: OcrPage,
}

/// Output of one selective OCR invocation.
#[derive(Debug)]
pub struct OcrRun {
    /// Pages processed in ascending document order.
    pub pages: Vec<RoutedOcrPage>,
    /// Total page-rendering wall time.
    pub render_time_ms: u64,
    /// Total engine wall time.
    pub ocr_time_ms: u64,
}

/// Selects 1-indexed pages for OCR.
///
/// `recommended_pages` comes from pdf-inspector's existing detector/text
/// quality signals. `selected_pages` is an optional user page filter. Results
/// are validated, deduplicated, and returned in document order.
pub fn route_ocr_pages(
    mode: OcrMode,
    page_count: u32,
    recommended_pages: &[u32],
    selected_pages: Option<&[u32]>,
) -> Result<Vec<u32>, OcrRoutingError> {
    match mode {
        OcrMode::Off => Ok(Vec::new()),
        OcrMode::Auto => {
            let mut routed = validated_page_set("recommended", recommended_pages, page_count)?;
            if let Some(selected) = selected_pages {
                let selected = validated_page_set("selected", selected, page_count)?;
                routed.retain(|page| selected.contains(page));
            }
            Ok(routed.into_iter().collect())
        }
        OcrMode::Force => {
            let routed = selected_pages
                .map(|pages| validated_page_set("selected", pages, page_count))
                .transpose()?
                .unwrap_or_else(|| (1..=page_count).collect());
            Ok(routed.into_iter().collect())
        }
    }
}

/// Renders and recognizes already-routed pages while retaining transforms for
/// the following fusion layer.
///
/// An empty page list returns without calling either dependency, which keeps
/// model resolution and inference lazy when Auto routing finds no OCR work.
#[allow(clippy::too_many_arguments)]
pub fn run_ocr_pages<R, O>(
    renderer: &R,
    engine: &O,
    pdf_bytes: &[u8],
    pages: &[u32],
    password: Option<&str>,
    render_options: &RenderOptions,
    ocr_options: &OcrOptions,
) -> Result<OcrRun, OcrRunError>
where
    R: PageRenderer,
    O: OcrEngine,
{
    if pages.is_empty() {
        return Ok(OcrRun {
            pages: Vec::new(),
            render_time_ms: 0,
            ocr_time_ms: 0,
        });
    }
    if ocr_options.mode == OcrMode::Off {
        return Err(OcrRunError::OcrDisabled);
    }

    let render_started = Instant::now();
    let rendered = renderer
        .render_pages(pdf_bytes, pages, password, render_options)
        .map_err(|source| OcrRunError::Render {
            source: Box::new(source),
        })?;
    let render_time_ms = elapsed_ms(render_started);
    validate_page_order("renderer", pages, rendered.iter().map(RenderedPage::page))?;

    let ocr_started = Instant::now();
    let recognized =
        engine
            .recognize(&rendered, ocr_options)
            .map_err(|source| OcrRunError::Ocr {
                source: Box::new(source),
            })?;
    let ocr_time_ms = elapsed_ms(ocr_started);
    validate_page_order(
        "OCR engine",
        pages,
        recognized.iter().map(|page| page.page_number),
    )?;

    Ok(OcrRun {
        pages: rendered
            .into_iter()
            .zip(recognized)
            .map(|(rendered, ocr)| RoutedOcrPage { rendered, ocr })
            .collect(),
        render_time_ms,
        ocr_time_ms,
    })
}

/// Invalid page routing or renderer/engine contract output.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OcrRoutingError {
    /// A page list contained zero or a page beyond the document.
    #[error("{source_name} OCR page {page} is outside the valid range 1..={page_count}")]
    InvalidPage {
        /// Page-list source.
        source_name: &'static str,
        /// Invalid 1-indexed page.
        page: u32,
        /// Document page count.
        page_count: u32,
    },
}

/// Failures while rendering and recognizing a routed page set.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OcrRunError {
    /// A non-empty route cannot execute with OCR disabled.
    #[error("cannot process routed pages while OCR mode is Off")]
    OcrDisabled,
    /// Page rasterization failed.
    #[error("page rendering failed: {source}")]
    Render {
        /// Renderer-specific failure.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// OCR inference failed.
    #[error("OCR inference failed: {source}")]
    Ocr {
        /// Engine-specific failure.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// A dependency returned the wrong count or order.
    #[error("{stage} returned pages {actual:?}; expected {expected:?}")]
    PageOrderMismatch {
        /// Dependency boundary that violated the contract.
        stage: &'static str,
        /// Requested 1-indexed pages.
        expected: Vec<u32>,
        /// Returned 1-indexed pages.
        actual: Vec<u32>,
    },
}

fn validated_page_set(
    source_name: &'static str,
    pages: &[u32],
    page_count: u32,
) -> Result<BTreeSet<u32>, OcrRoutingError> {
    let mut result = BTreeSet::new();
    for &page in pages {
        if page == 0 || page > page_count {
            return Err(OcrRoutingError::InvalidPage {
                source_name,
                page,
                page_count,
            });
        }
        result.insert(page);
    }
    Ok(result)
}

fn validate_page_order(
    stage: &'static str,
    expected: &[u32],
    actual: impl IntoIterator<Item = u32>,
) -> Result<(), OcrRunError> {
    let actual: Vec<u32> = actual.into_iter().collect();
    if actual == expected {
        Ok(())
    } else {
        Err(OcrRunError::PageOrderMismatch {
            stage,
            expected: expected.to_vec(),
            actual,
        })
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::{
        ImagePoint, ImageQuad, ModelIdentity, OcrSpan, PageTransform, RenderBufferError,
        RenderPixelFormat,
    };

    #[derive(Debug, Error)]
    #[error("fake failure")]
    struct FakeError;

    struct FakeRenderer;

    impl PageRenderer for FakeRenderer {
        type Error = FakeError;

        fn render_pages(
            &self,
            _pdf_bytes: &[u8],
            pages: &[u32],
            _password: Option<&str>,
            _options: &RenderOptions,
        ) -> Result<Vec<RenderedPage>, Self::Error> {
            pages
                .iter()
                .copied()
                .map(rendered_page)
                .collect::<Result<_, _>>()
                .map_err(|_| FakeError)
        }
    }

    struct FakeEngine {
        model: ModelIdentity,
    }

    impl FakeEngine {
        fn new() -> Self {
            Self {
                model: ModelIdentity::new("fake", "v1"),
            }
        }
    }

    impl OcrEngine for FakeEngine {
        type Error = FakeError;

        fn model(&self) -> &ModelIdentity {
            &self.model
        }

        fn recognize(
            &self,
            pages: &[RenderedPage],
            _options: &OcrOptions,
        ) -> Result<Vec<OcrPage>, Self::Error> {
            Ok(pages
                .iter()
                .map(|page| OcrPage {
                    page_number: page.page(),
                    spans: vec![OcrSpan {
                        text: format!("page {}", page.page()),
                        polygon: ImageQuad::new([
                            ImagePoint::new(0.0, 0.0),
                            ImagePoint::new(1.0, 0.0),
                            ImagePoint::new(1.0, 1.0),
                            ImagePoint::new(0.0, 1.0),
                        ]),
                        confidence: 0.9,
                        orientation_degrees: None,
                    }],
                    mean_confidence: Some(0.9),
                    model: self.model.clone(),
                    processing_time_ms: 1,
                    warnings: Vec::new(),
                })
                .collect())
        }
    }

    fn rendered_page(page: u32) -> Result<RenderedPage, RenderBufferError> {
        let transform =
            PageTransform::from_corners(1, 1, (0.0, 1.0), (1.0, 1.0), (0.0, 0.0)).unwrap();
        RenderedPage::new(
            page,
            1.0,
            1.0,
            1,
            1,
            3,
            RenderPixelFormat::Rgb8,
            vec![255; 3],
            transform,
        )
    }

    #[test]
    fn off_auto_and_force_route_expected_pages() {
        assert_eq!(
            route_ocr_pages(OcrMode::Off, 0, &[99], Some(&[0])).unwrap(),
            Vec::<u32>::new()
        );
        assert_eq!(
            route_ocr_pages(OcrMode::Auto, 5, &[5, 3, 3, 1], Some(&[2, 3, 5])).unwrap(),
            vec![3, 5]
        );
        assert_eq!(
            route_ocr_pages(OcrMode::Force, 4, &[], None).unwrap(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            route_ocr_pages(OcrMode::Force, 4, &[], Some(&[4, 2, 2])).unwrap(),
            vec![2, 4]
        );
    }

    #[test]
    fn routing_rejects_invalid_page_numbers() {
        assert!(matches!(
            route_ocr_pages(OcrMode::Auto, 2, &[0], None),
            Err(OcrRoutingError::InvalidPage { page: 0, .. })
        ));
        assert!(matches!(
            route_ocr_pages(OcrMode::Force, 2, &[], Some(&[3])),
            Err(OcrRoutingError::InvalidPage { page: 3, .. })
        ));
    }

    #[test]
    fn run_retains_render_transforms_and_input_order() {
        let options = OcrOptions::new().mode(OcrMode::Auto);
        let run = run_ocr_pages(
            &FakeRenderer,
            &FakeEngine::new(),
            b"pdf",
            &[2, 4],
            None,
            &RenderOptions::new(),
            &options,
        )
        .unwrap();
        assert_eq!(
            run.pages
                .iter()
                .map(|page| page.rendered.page())
                .collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert_eq!(run.pages[1].ocr.spans[0].text, "page 4");
        assert_eq!(run.pages[1].rendered.pixel_to_page(0.0, 0.0).y, 1.0);
    }

    #[test]
    fn empty_route_is_a_noop_even_when_ocr_is_off() {
        let run = run_ocr_pages(
            &FakeRenderer,
            &FakeEngine::new(),
            b"pdf",
            &[],
            None,
            &RenderOptions::new(),
            &OcrOptions::new(),
        )
        .unwrap();
        assert!(run.pages.is_empty());
        assert_eq!(run.render_time_ms, 0);
        assert_eq!(run.ocr_time_ms, 0);
    }
}
