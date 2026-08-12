//! Geometry-aware OCR Markdown assembly and native-text fusion.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use thiserror::Error;

use crate::markdown::{to_markdown_from_items_with_rects_and_page_count, MarkdownOptions};
use crate::types::{ItemType, TextItem};
use crate::PageMarkdown;

use super::{
    LocalOcrPage, LocalOcrRun, PageContentSource, PageProvenance, VisionTimings, DEFAULT_RENDER_DPI,
};

/// OCR assembly and hosted-fallback policy.
#[derive(Debug, Clone)]
pub struct OcrFusionOptions {
    /// Markdown conversion options applied to positioned OCR spans.
    pub markdown: MarkdownOptions,
    /// Render resolution recorded in page provenance.
    pub render_dpi: f32,
    /// Recommend the hosted pipeline below this mean confidence when native
    /// extraction already marked the page as requiring OCR.
    pub hosted_recommendation_confidence: f32,
}

impl Default for OcrFusionOptions {
    fn default() -> Self {
        Self {
            markdown: MarkdownOptions {
                include_page_numbers: false,
                strip_headers_footers: false,
                ..MarkdownOptions::default()
            },
            render_dpi: DEFAULT_RENDER_DPI,
            hosted_recommendation_confidence: 0.5,
        }
    }
}

impl OcrFusionOptions {
    /// Creates OCR fusion options with local defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces Markdown conversion options.
    pub fn markdown(mut self, markdown: MarkdownOptions) -> Self {
        self.markdown = markdown;
        self
    }

    /// Records the renderer resolution in provenance.
    pub fn render_dpi(mut self, render_dpi: f32) -> Self {
        self.render_dpi = render_dpi;
        self
    }

    /// Sets the weak-OCR confidence threshold for recommending hosted parsing.
    pub fn hosted_recommendation_confidence(mut self, confidence: f32) -> Self {
        self.hosted_recommendation_confidence = confidence;
        self
    }
}

/// Final Markdown and provenance for one page.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedPageMarkdown {
    /// 1-indexed document page number, matching OCR and provenance fields.
    pub page: u32,
    /// Final page Markdown.
    pub markdown: String,
    /// Native/OCR source, model, timing, and fallback metadata.
    pub provenance: PageProvenance,
}

/// Output of fusing a selective local OCR run into native page extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedPages {
    /// Pages in the same order as the native input.
    pub pages: Vec<FusedPageMarkdown>,
    /// Batch rendering wall time from the OCR run.
    pub render_time_ms: u64,
    /// Batch OCR wall time from the OCR run.
    pub ocr_time_ms: u64,
}

/// Converts positioned OCR spans to Markdown through pdf-inspector's existing
/// deterministic geometry, reading-order, table, and Markdown pipeline.
///
/// The page number is 1-indexed. `document_page_count` prevents a selected
/// page from being mistaken for a one-page document during page-number logic.
pub fn ocr_page_to_markdown(
    page: &LocalOcrPage,
    document_page_count: u32,
    options: &MarkdownOptions,
) -> String {
    let (items, _) = ocr_text_items(page);
    to_markdown_from_items_with_rects_and_page_count(
        items,
        options.clone(),
        &[],
        document_page_count,
    )
}

/// Fuses a selective OCR run into per-page native Markdown.
///
/// OCR replaces pages whose native extraction was already rejected. On clean
/// native pages (for example in `Force` mode), normalized duplicate OCR blocks
/// are removed and only genuinely additional blocks are appended. Pages that
/// needed OCR but still have no credible local result recommend the hosted
/// document pipeline instead of silently presenting an empty result as final.
pub fn fuse_ocr_pages(
    native_pages: &[PageMarkdown],
    ocr_run: &LocalOcrRun,
    document_page_count: u32,
    options: &OcrFusionOptions,
) -> Result<FusedPages, OcrFusionError> {
    validate_options(options)?;

    let mut native_numbers = BTreeSet::new();
    for page in native_pages {
        let page_number = page
            .page
            .checked_add(1)
            .ok_or(OcrFusionError::PageOverflow)?;
        if !native_numbers.insert(page_number) {
            return Err(OcrFusionError::DuplicateNativePage { page: page_number });
        }
    }

    let mut ocr_by_page = BTreeMap::new();
    for page in &ocr_run.pages {
        let page_number = page.rendered.page();
        if !native_numbers.contains(&page_number) {
            return Err(OcrFusionError::UnexpectedOcrPage { page: page_number });
        }
        if ocr_by_page.insert(page_number, page).is_some() {
            return Err(OcrFusionError::DuplicateOcrPage { page: page_number });
        }
    }

    let render_shares = equal_time_shares(ocr_run.render_time_ms, ocr_run.pages.len());
    let render_by_page: BTreeMap<u32, u64> = ocr_run
        .pages
        .iter()
        .zip(render_shares)
        .map(|(page, share)| (page.rendered.page(), share))
        .collect();

    let mut pages = Vec::with_capacity(native_pages.len());
    for native in native_pages {
        let page_number = native.page + 1;
        let assembly_started = Instant::now();
        let mut warnings = Vec::new();

        let (markdown, source, ocr_model, ocr_confidence, ocr_ms, hosted_recommended) =
            if let Some(local) = ocr_by_page.get(&page_number) {
                let (ocr_items, discarded_spans) = ocr_text_items(local);
                if discarded_spans > 0 {
                    warnings.push(format!(
                        "discarded {discarded_spans} OCR spans with unusable text or geometry"
                    ));
                }
                warnings.extend(local.ocr.warnings.iter().cloned());
                let ocr_markdown = to_markdown_from_items_with_rects_and_page_count(
                    ocr_items,
                    options.markdown.clone(),
                    &[],
                    document_page_count,
                );
                let (markdown, source) = if native.markdown.trim().is_empty() || native.needs_ocr {
                    (ocr_markdown, PageContentSource::Ocr)
                } else {
                    merge_native_and_ocr(&native.markdown, &ocr_markdown)
                };
                let weak_ocr = local
                    .ocr
                    .mean_confidence
                    .is_none_or(|confidence| confidence < options.hosted_recommendation_confidence);
                let recommend_hosted = native.needs_ocr && (markdown.trim().is_empty() || weak_ocr);
                if native.needs_ocr && markdown.trim().is_empty() {
                    warnings.push("local OCR produced no usable text".to_string());
                }
                (
                    markdown,
                    source,
                    Some(local.ocr.model.clone()),
                    local.ocr.mean_confidence,
                    local.ocr.processing_time_ms,
                    recommend_hosted,
                )
            } else {
                if native.needs_ocr {
                    warnings.push("page was recommended for OCR but was not processed".to_string());
                }
                (
                    native.markdown.clone(),
                    PageContentSource::Native,
                    None,
                    None,
                    0,
                    native.needs_ocr,
                )
            };

        pages.push(FusedPageMarkdown {
            page: page_number,
            markdown,
            provenance: PageProvenance {
                page: page_number,
                source,
                ocr_model,
                layout_model: None,
                render_dpi: ocr_by_page
                    .contains_key(&page_number)
                    .then_some(options.render_dpi),
                ocr_confidence,
                timings: VisionTimings {
                    render_ms: render_by_page.get(&page_number).copied().unwrap_or(0),
                    ocr_ms,
                    layout_ms: 0,
                    assembly_ms: elapsed_ms(assembly_started),
                },
                warnings,
                hosted_recommended,
            },
        });
    }

    Ok(FusedPages {
        pages,
        render_time_ms: ocr_run.render_time_ms,
        ocr_time_ms: ocr_run.ocr_time_ms,
    })
}

/// Converts recognized line polygons to ordinary PDF-space text items.
fn ocr_text_items(page: &LocalOcrPage) -> (Vec<TextItem>, usize) {
    let mut discarded = 0usize;
    let mut items = Vec::with_capacity(page.ocr.spans.len());
    for span in &page.ocr.spans {
        if span.text.trim().is_empty() {
            discarded += 1;
            continue;
        }
        let Some((left, top, right, bottom)) = image_quad_bounds(
            &span.polygon.points,
            page.rendered.width(),
            page.rendered.height(),
        ) else {
            discarded += 1;
            continue;
        };
        let rect = page.rendered.pixel_rect_to_pdf_rect(
            f64::from(left),
            f64::from(top),
            f64::from(right - left),
            f64::from(bottom - top),
        );
        items.push(TextItem {
            text: span.text.trim().to_string(),
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            font: "OCR".to_string(),
            font_size: rect.height.max(1.0),
            page: page.rendered.page(),
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        });
    }
    // OCR engines do not share an ordering contract. Geometry gives the
    // deterministic top-to-bottom seed expected by the existing layout
    // pipeline, which can still replace it with column-aware reading order.
    items.sort_by(|first, second| {
        first
            .page
            .cmp(&second.page)
            .then(second.y.total_cmp(&first.y))
            .then(first.x.total_cmp(&second.x))
    });
    (items, discarded)
}

fn image_quad_bounds(
    points: &[super::ImagePoint; 4],
    width: u32,
    height: u32,
) -> Option<(f32, f32, f32, f32)> {
    if points
        .iter()
        .any(|point| !point.x.is_finite() || !point.y.is_finite())
    {
        return None;
    }
    let left = points
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min)
        .clamp(0.0, width as f32);
    let right = points
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max)
        .clamp(0.0, width as f32);
    let top = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min)
        .clamp(0.0, height as f32);
    let bottom = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .clamp(0.0, height as f32);
    (right > left && bottom > top).then_some((left, top, right, bottom))
}

fn merge_native_and_ocr(native: &str, ocr: &str) -> (String, PageContentSource) {
    let native_keys = comparison_units(native);
    let mut addition_keys = Vec::new();
    let mut additions: Vec<String> = Vec::new();
    for block in markdown_blocks(ocr) {
        let key = ComparisonKey::new(block);
        if key.is_empty()
            || native_keys.iter().any(|native| native.same_content(&key))
            || addition_keys
                .iter()
                .any(|existing: &ComparisonKey| existing.same_content(&key))
        {
            continue;
        }

        let addition = if let Some(native) = best_partial_overlap(&key, &native_keys) {
            let Some(novel) = novel_fragments(block, native) else {
                continue;
            };
            novel
        } else {
            block.trim_end().to_string()
        };
        let addition_key = ComparisonKey::new(&addition);
        if addition_key.is_empty()
            || native_keys
                .iter()
                .any(|native| native.same_content(&addition_key))
            || addition_keys
                .iter()
                .any(|existing: &ComparisonKey| existing.same_content(&addition_key))
        {
            continue;
        }
        addition_keys.push(addition_key);
        additions.push(addition);
    }

    if additions.is_empty() {
        (ensure_trailing_newline(native), PageContentSource::Native)
    } else {
        let mut result = native.trim_end_matches('\n').to_string();
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&additions.join("\n\n"));
        result.push('\n');
        (result, PageContentSource::Fused)
    }
}

fn markdown_blocks(markdown: &str) -> impl Iterator<Item = &str> {
    markdown
        .split("\n\n")
        .map(str::trim_end)
        .filter(|block| !block.trim().is_empty())
}

#[derive(Debug)]
struct ComparisonKey {
    tokens: Vec<String>,
    punctuation: String,
}

impl ComparisonKey {
    fn new(text: &str) -> Self {
        let mut tokens = Vec::new();
        let mut token = String::new();
        let mut punctuation = String::new();
        for character in text.chars().flat_map(char::to_lowercase) {
            if character.is_alphanumeric() {
                token.push(character);
            } else {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if !character.is_whitespace() {
                    punctuation.push(character);
                }
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
        Self {
            tokens,
            punctuation,
        }
    }

    fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.punctuation.is_empty()
    }

    fn same_content(&self, other: &Self) -> bool {
        if self.tokens.is_empty() || other.tokens.is_empty() {
            self.tokens.is_empty()
                && other.tokens.is_empty()
                && self.punctuation == other.punctuation
        } else {
            token_counts(&self.tokens) == token_counts(&other.tokens)
        }
    }
}

fn comparison_units(markdown: &str) -> Vec<ComparisonKey> {
    let mut units: Vec<_> = markdown_blocks(markdown).map(ComparisonKey::new).collect();
    units.extend(
        markdown
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ComparisonKey::new),
    );
    units
}

fn token_counts(tokens: &[String]) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for token in tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    counts
}

fn best_partial_overlap<'a>(
    ocr: &ComparisonKey,
    native: &'a [ComparisonKey],
) -> Option<&'a ComparisonKey> {
    if ocr.tokens.len() < 2 {
        return None;
    }
    native
        .iter()
        .filter(|key| !key.tokens.is_empty())
        .filter_map(|key| {
            let overlap = token_overlap(&ocr.tokens, &key.tokens);
            let substantial =
                overlap >= 2 && overlap * 2 >= ocr.tokens.len() && overlap * 2 >= key.tokens.len();
            substantial.then_some((overlap, key))
        })
        .max_by_key(|(overlap, _)| *overlap)
        .map(|(_, key)| key)
}

fn token_overlap(first: &[String], second: &[String]) -> usize {
    let first = token_counts(first);
    let second = token_counts(second);
    first
        .iter()
        .map(|(token, count)| (*count).min(second.get(token).copied().unwrap_or(0)))
        .sum()
}

fn novel_fragments(block: &str, native: &ComparisonKey) -> Option<String> {
    let mut available = token_counts(&native.tokens);
    let mut novel = Vec::new();
    for fragment in block.split_whitespace() {
        let key = ComparisonKey::new(fragment);
        let mut adds_content = false;
        for token in &key.tokens {
            match available.get_mut(token.as_str()) {
                Some(count) if *count > 0 => *count -= 1,
                _ => adds_content = true,
            }
        }
        if adds_content {
            novel.push(fragment);
        }
    }
    (!novel.is_empty()).then(|| novel.join(" "))
}

fn ensure_trailing_newline(markdown: &str) -> String {
    let mut result = markdown.trim_end_matches('\n').to_string();
    result.push('\n');
    result
}

fn equal_time_shares(total: u64, count: usize) -> Vec<u64> {
    if count == 0 {
        return Vec::new();
    }
    let count = count as u64;
    let base = total / count;
    let remainder = total % count;
    (0..count)
        .map(|index| base + u64::from(index < remainder))
        .collect()
}

fn validate_options(options: &OcrFusionOptions) -> Result<(), OcrFusionError> {
    if !options.render_dpi.is_finite() || options.render_dpi <= 0.0 {
        return Err(OcrFusionError::InvalidRenderDpi {
            value: options.render_dpi,
        });
    }
    let confidence = options.hosted_recommendation_confidence;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(OcrFusionError::InvalidHostedConfidence { value: confidence });
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Invalid page sets or fusion options.
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum OcrFusionError {
    /// A 0-indexed native page could not be converted to 1-indexed form.
    #[error("native page number cannot be converted to a 1-indexed page")]
    PageOverflow,
    /// Native input repeated a page number.
    #[error("native page {page} appears more than once")]
    DuplicateNativePage {
        /// Repeated 1-indexed page number.
        page: u32,
    },
    /// OCR input repeated a page number.
    #[error("OCR page {page} appears more than once")]
    DuplicateOcrPage {
        /// Repeated 1-indexed page number.
        page: u32,
    },
    /// OCR returned a page that was not part of native extraction.
    #[error("OCR page {page} is not present in native page extraction")]
    UnexpectedOcrPage {
        /// Unexpected 1-indexed page number.
        page: u32,
    },
    /// Render resolution is non-finite or non-positive.
    #[error("render DPI must be positive and finite, got {value}")]
    InvalidRenderDpi {
        /// Invalid value.
        value: f32,
    },
    /// Hosted recommendation confidence is outside 0–1.
    #[error("hosted recommendation confidence must be between 0 and 1, got {value}")]
    InvalidHostedConfidence {
        /// Invalid value.
        value: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::{
        ImagePoint, ImageQuad, ModelIdentity, OcrPage, OcrSpan, PageTransform, RenderPixelFormat,
        RenderedPage,
    };

    fn native(page: u32, markdown: &str, needs_ocr: bool) -> PageMarkdown {
        PageMarkdown {
            page,
            markdown: markdown.to_string(),
            needs_ocr,
            ocr_reason: needs_ocr.then(|| "scanned".to_string()),
        }
    }

    fn rendered_page(page: u32) -> RenderedPage {
        let transform =
            PageTransform::from_corners(200, 100, (0.0, 100.0), (200.0, 100.0), (0.0, 0.0))
                .unwrap();
        RenderedPage::new(
            page,
            200.0,
            100.0,
            200,
            100,
            600,
            RenderPixelFormat::Rgb8,
            vec![255; 60_000],
            transform,
        )
        .unwrap()
    }

    fn span(text: &str, top: f32, confidence: f32) -> OcrSpan {
        OcrSpan {
            text: text.to_string(),
            polygon: ImageQuad::new([
                ImagePoint::new(10.0, top),
                ImagePoint::new(190.0, top),
                ImagePoint::new(190.0, top + 10.0),
                ImagePoint::new(10.0, top + 10.0),
            ]),
            confidence,
            orientation_degrees: None,
        }
    }

    fn local_page(page: u32, spans: Vec<OcrSpan>, confidence: Option<f32>) -> LocalOcrPage {
        LocalOcrPage {
            rendered: rendered_page(page),
            ocr: OcrPage {
                page,
                spans,
                mean_confidence: confidence,
                model: ModelIdentity::new("test-ocr", "v1"),
                processing_time_ms: 7,
                warnings: Vec::new(),
            },
        }
    }

    fn run(pages: Vec<LocalOcrPage>) -> LocalOcrRun {
        LocalOcrRun {
            pages,
            render_time_ms: 5,
            ocr_time_ms: 7,
        }
    }

    #[test]
    fn scanned_page_uses_geometry_ordered_ocr_and_provenance() {
        let native = [native(0, "", true)];
        let run = run(vec![local_page(
            1,
            vec![
                span("Second line", 30.0, 0.9),
                span("First line", 10.0, 0.9),
            ],
            Some(0.9),
        )]);

        let result = fuse_ocr_pages(&native, &run, 1, &OcrFusionOptions::new()).unwrap();

        assert!(
            result.pages[0].markdown.find("First").unwrap()
                < result.pages[0].markdown.find("Second").unwrap()
        );
        assert_eq!(result.pages[0].provenance.source, PageContentSource::Ocr);
        assert_eq!(result.pages[0].page, 1);
        assert_eq!(result.pages[0].page, result.pages[0].provenance.page);
        assert_eq!(
            result.pages[0].provenance.ocr_model.as_ref().unwrap().name,
            "test-ocr"
        );
        assert!(!result.pages[0].provenance.hosted_recommended);
    }

    #[test]
    fn force_mode_deduplicates_native_content() {
        let native = [native(0, "Hello, world!\n", false)];
        let run = run(vec![local_page(
            1,
            vec![span("Hello world", 10.0, 0.9)],
            Some(0.9),
        )]);

        let result = fuse_ocr_pages(&native, &run, 1, &OcrFusionOptions::new()).unwrap();

        assert_eq!(result.pages[0].markdown, "Hello, world!\n");
        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
    }

    #[test]
    fn force_mode_appends_only_additional_ocr_blocks() {
        let native = [native(0, "Native title\n", false)];
        let run = run(vec![local_page(
            1,
            vec![
                span("Native title", 10.0, 0.9),
                span("Image-only label", 30.0, 0.9),
            ],
            Some(0.9),
        )]);

        let result = fuse_ocr_pages(&native, &run, 1, &OcrFusionOptions::new()).unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Fused);
        assert_eq!(result.pages[0].markdown.matches("Native title").count(), 1);
        assert!(result.pages[0].markdown.contains("Image-only label"));
    }

    #[test]
    fn force_mode_preserves_punctuation_and_short_distinct_blocks() {
        let (markdown, source) = merge_native_and_ocr("Figure 1\n", "1\n\n***");
        assert_eq!(source, PageContentSource::Fused);
        assert_eq!(markdown, "Figure 1\n\n1\n\n***\n");
    }

    #[test]
    fn force_mode_extracts_only_novel_tokens_from_overlapping_blocks() {
        let (markdown, source) = merge_native_and_ocr("A C\n", "## A B C");
        assert_eq!(source, PageContentSource::Fused);
        assert_eq!(markdown, "A C\n\nB\n");
        assert_eq!(markdown.matches('A').count(), 1);
        assert_eq!(markdown.matches('C').count(), 1);
    }

    #[test]
    fn force_mode_deduplicates_reordered_tokens() {
        let native = [native(0, "Alpha Beta Gamma\n", false)];
        let run = run(vec![local_page(
            1,
            vec![span("Gamma Alpha Beta", 10.0, 0.9)],
            Some(0.9),
        )]);

        let result = fuse_ocr_pages(&native, &run, 1, &OcrFusionOptions::new()).unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
        assert_eq!(result.pages[0].markdown, "Alpha Beta Gamma\n");
    }

    #[test]
    fn fusion_preserves_native_leading_indentation() {
        let native = [native(0, "    indented code\n", false)];
        let duplicate = run(vec![local_page(
            1,
            vec![span("indented code", 10.0, 0.9)],
            Some(0.9),
        )]);
        let result = fuse_ocr_pages(&native, &duplicate, 1, &OcrFusionOptions::new()).unwrap();
        assert_eq!(result.pages[0].markdown, "    indented code\n");

        let addition = run(vec![local_page(
            1,
            vec![span("image label", 10.0, 0.9)],
            Some(0.9),
        )]);
        let result = fuse_ocr_pages(&native, &addition, 1, &OcrFusionOptions::new()).unwrap();
        assert_eq!(
            result.pages[0].markdown,
            "    indented code\n\nimage label\n"
        );
    }

    #[test]
    fn missing_or_weak_required_ocr_recommends_hosted() {
        let native = [native(0, "", true), native(1, "", true)];
        let run = run(vec![local_page(
            2,
            vec![span("uncertain", 10.0, 0.3)],
            Some(0.3),
        )]);

        let result = fuse_ocr_pages(&native, &run, 2, &OcrFusionOptions::new()).unwrap();

        assert!(result.pages[0].provenance.hosted_recommended);
        assert!(result.pages[1].provenance.hosted_recommended);
    }

    #[test]
    fn rejects_ocr_pages_outside_native_selection() {
        let error = fuse_ocr_pages(
            &[native(0, "text", false)],
            &run(vec![local_page(2, Vec::new(), None)]),
            2,
            &OcrFusionOptions::new(),
        )
        .unwrap_err();
        assert_eq!(error, OcrFusionError::UnexpectedOcrPage { page: 2 });
    }

    #[test]
    fn time_shares_preserve_batch_total() {
        assert_eq!(equal_time_shares(8, 3), vec![3, 3, 2]);
        assert_eq!(equal_time_shares(8, 0), Vec::<u64>::new());
    }
}
