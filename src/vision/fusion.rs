//! Geometry-aware OCR Markdown assembly and native-text fusion.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use thiserror::Error;

use crate::markdown::{
    complete_table_markdown_from_items, to_markdown_from_items_with_rects_and_page_count,
    MarkdownOptions,
};
use crate::text_quality::{detect_encoding_issues, is_cid_garbage, is_garbage_text};
use crate::types::{ItemType, PdfRect, TextItem};
use crate::PageMarkdown;

use super::{
    OcrRun, PageContentSource, PageProvenance, RoutedOcrPage, VisionTimings, DEFAULT_RENDER_DPI,
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

    /// Validates rendering and hosted-fallback thresholds without doing work.
    pub fn validate(&self) -> Result<(), OcrFusionError> {
        validate_options(self)
    }
}

/// Final Markdown and provenance for one page.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedPageMarkdown {
    /// 1-indexed document page number, matching OCR and provenance fields.
    pub page_number: u32,
    /// Final page Markdown.
    pub markdown: String,
    /// Native/OCR source, model, timing, and fallback metadata.
    pub provenance: PageProvenance,
}

/// Output of fusing a selective OCR run into native page extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedPages {
    /// Pages in the same order as the native input.
    pub pages: Vec<FusedPageMarkdown>,
    /// Batch rendering wall time from the OCR run.
    pub render_time_ms: u64,
    /// Batch OCR wall time from the OCR run.
    pub ocr_time_ms: u64,
}

/// Origin of a trustworthy native-text candidate retained for adaptive OCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeCandidateOrigin {
    /// Text produced by pdf-inspector's normal native extractor.
    Extractor,
    /// Positioned text independently recovered through PDFium.
    Pdfium,
}

impl NativeCandidateOrigin {
    fn description(self) -> &'static str {
        match self {
            Self::Extractor => "native extraction",
            Self::Pdfium => "PDFium native recovery",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TextCandidateQuality {
    alphanumeric_chars: usize,
    score: f32,
}

/// Clean native text retained while an ambiguous page is compared with OCR.
#[derive(Debug, Clone)]
pub(crate) struct NativeFallbackCandidate {
    markdown: String,
    quality: TextCandidateQuality,
    origin: NativeCandidateOrigin,
}

/// How OCR output for a routed page may participate in final fusion.
#[derive(Debug, Clone)]
pub(crate) enum OcrFusionRoute {
    /// The page requires ordinary full-page OCR replacement or adaptive fusion.
    FullPage,
    /// The native page is clean; only tables detected inside these image
    /// regions may be appended.
    SupplementalRegions(Vec<PdfRect>),
}

impl NativeFallbackCandidate {
    /// True when an independent native recovery is substantial enough to
    /// cancel OCR for recoverable font/vector routing reasons.
    pub(crate) fn is_complete_recovery(&self) -> bool {
        self.quality.alphanumeric_chars >= 40 && self.quality.score >= 0.68
    }

    pub(crate) fn markdown(&self) -> &str {
        &self.markdown
    }

    pub(crate) fn is_stronger_than(&self, other: &Self) -> bool {
        self.quality.alphanumeric_chars > other.quality.alphanumeric_chars
            || (self.quality.alphanumeric_chars == other.quality.alphanumeric_chars
                && self.quality.score > other.quality.score)
    }
}

/// Validates and scores native Markdown for possible post-OCR comparison.
///
/// This intentionally uses script-agnostic evidence. A native candidate only
/// needs to be trustworthy, not necessarily complete: a clean native header
/// can still be fused with an image-backed OCR body.
pub(crate) fn assess_native_candidate(
    markdown: String,
    origin: NativeCandidateOrigin,
) -> Option<NativeFallbackCandidate> {
    let quality = assess_text_candidate(&markdown)?;
    (quality.alphanumeric_chars >= 8).then_some(NativeFallbackCandidate {
        markdown,
        quality,
        origin,
    })
}

/// Converts positioned OCR spans to Markdown through pdf-inspector's existing
/// deterministic geometry, reading-order, table, and Markdown pipeline.
///
/// The page number is 1-indexed. `document_page_count` prevents a selected
/// page from being mistaken for a one-page document during page-number logic.
pub fn ocr_page_to_markdown(
    page: &RoutedOcrPage,
    document_page_count: u32,
    options: &MarkdownOptions,
) -> String {
    let (items, _) = ocr_text_items(page);
    let markdown = to_markdown_from_items_with_rects_and_page_count(
        items,
        options.clone(),
        &[],
        document_page_count,
    );
    preserve_ocr_line_breaks(&markdown, page)
}

/// Fuses a selective OCR run into per-page native Markdown.
///
/// OCR replaces pages whose native extraction was already rejected. On clean
/// native pages (for example in `Force` mode), normalized duplicate OCR blocks
/// are removed and only genuinely additional blocks are appended. Pages that
/// needed OCR but still have no credible OCR result, or whose confident OCR
/// only repeats an incomplete native fragment, recommend the hosted document
/// pipeline instead of silently presenting partial content as final.
pub fn fuse_ocr_pages(
    native_pages: &[PageMarkdown],
    ocr_run: &OcrRun,
    document_page_count: u32,
    options: &OcrFusionOptions,
) -> Result<FusedPages, OcrFusionError> {
    let routes = full_page_routes(ocr_run);
    fuse_ocr_pages_impl(
        native_pages,
        ocr_run,
        document_page_count,
        options,
        &BTreeMap::new(),
        &routes,
    )
}

/// OCR-pipeline fusion with trustworthy partial native candidates.
#[cfg(test)]
pub(crate) fn fuse_ocr_pages_adaptive(
    native_pages: &[PageMarkdown],
    ocr_run: &OcrRun,
    document_page_count: u32,
    options: &OcrFusionOptions,
    native_candidates: &BTreeMap<u32, NativeFallbackCandidate>,
) -> Result<FusedPages, OcrFusionError> {
    let routes = full_page_routes(ocr_run);
    fuse_ocr_pages_impl(
        native_pages,
        ocr_run,
        document_page_count,
        options,
        native_candidates,
        &routes,
    )
}

fn full_page_routes(ocr_run: &OcrRun) -> BTreeMap<u32, OcrFusionRoute> {
    ocr_run
        .pages
        .iter()
        .map(|page| (page.rendered.page(), OcrFusionRoute::FullPage))
        .collect()
}

/// OCR-pipeline fusion with an explicit route mode for every processed page.
pub(crate) fn fuse_ocr_pages_adaptive_with_routes(
    native_pages: &[PageMarkdown],
    ocr_run: &OcrRun,
    document_page_count: u32,
    options: &OcrFusionOptions,
    native_candidates: &BTreeMap<u32, NativeFallbackCandidate>,
    routes: &BTreeMap<u32, OcrFusionRoute>,
) -> Result<FusedPages, OcrFusionError> {
    fuse_ocr_pages_impl(
        native_pages,
        ocr_run,
        document_page_count,
        options,
        native_candidates,
        routes,
    )
}

fn fuse_ocr_pages_impl(
    native_pages: &[PageMarkdown],
    ocr_run: &OcrRun,
    document_page_count: u32,
    options: &OcrFusionOptions,
    native_candidates: &BTreeMap<u32, NativeFallbackCandidate>,
    routes: &BTreeMap<u32, OcrFusionRoute>,
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
                let route = routes
                    .get(&page_number)
                    .ok_or(OcrFusionError::MissingOcrRoute { page: page_number })?;
                let ocr_markdown = match route {
                    OcrFusionRoute::SupplementalRegions(regions) => {
                        let region_items = items_inside_regions(ocr_items, regions);
                        let table_markdown = complete_table_markdown_from_items(
                            region_items,
                            options.markdown.clone(),
                            document_page_count,
                        );
                        if table_markdown.is_empty() {
                            warnings.push(
                                "image-region OCR found no valid table; kept native content"
                                    .to_string(),
                            );
                        }
                        table_markdown
                    }
                    OcrFusionRoute::FullPage => {
                        let markdown = to_markdown_from_items_with_rects_and_page_count(
                            ocr_items,
                            options.markdown.clone(),
                            &[],
                            document_page_count,
                        );
                        preserve_ocr_line_breaks(&markdown, local)
                    }
                };
                let (markdown, source, adaptive_recommends_hosted) = if let Some(candidate) =
                    native_candidates
                        .get(&page_number)
                        .filter(|_| native.needs_ocr)
                {
                    let choice = choose_adaptive_content(
                        candidate,
                        &ocr_markdown,
                        local.ocr.mean_confidence,
                        options.hosted_recommendation_confidence,
                    );
                    warnings.push(choice.warning);
                    (choice.markdown, choice.source, choice.recommend_hosted)
                } else if native.markdown.trim().is_empty() || native.needs_ocr {
                    (ocr_markdown, PageContentSource::Ocr, false)
                } else {
                    let (markdown, source) = merge_native_and_ocr(&native.markdown, &ocr_markdown);
                    (markdown, source, false)
                };
                let weak_ocr = local
                    .ocr
                    .mean_confidence
                    .is_none_or(|confidence| confidence < options.hosted_recommendation_confidence);
                let recommend_hosted = native.needs_ocr
                    && (markdown.trim().is_empty() || weak_ocr || adaptive_recommends_hosted);
                if native.needs_ocr && markdown.trim().is_empty() {
                    warnings.push("OCR produced no usable text".to_string());
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
            page_number,
            markdown,
            provenance: PageProvenance {
                page_number,
                source,
                ocr_model,
                render_dpi: ocr_by_page
                    .contains_key(&page_number)
                    .then_some(options.render_dpi),
                ocr_confidence,
                timings: VisionTimings {
                    render_ms: render_by_page.get(&page_number).copied().unwrap_or(0),
                    ocr_ms,
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

struct AdaptiveContentChoice {
    markdown: String,
    source: PageContentSource,
    warning: String,
    recommend_hosted: bool,
}

fn choose_adaptive_content(
    native: &NativeFallbackCandidate,
    ocr: &str,
    ocr_confidence: Option<f32>,
    weak_ocr_threshold: f32,
) -> AdaptiveContentChoice {
    let ocr_quality = assess_text_candidate(ocr);
    let weak_ocr = ocr_confidence.is_none_or(|confidence| confidence < weak_ocr_threshold);
    if weak_ocr || ocr_quality.is_none() {
        return AdaptiveContentChoice {
            markdown: ensure_trailing_newline(native.markdown()),
            source: PageContentSource::Native,
            warning: format!(
                "kept trustworthy {} because OCR was weak or unusable",
                native.origin.description()
            ),
            recommend_hosted: true,
        };
    }

    let ocr_quality = ocr_quality.expect("checked above");
    let overlap = content_overlap(native.markdown(), ocr);
    let ocr_novel_chars = ocr_quality
        .alphanumeric_chars
        .saturating_sub(overlap.shared_chars);
    let material_novelty =
        ocr_novel_chars >= 2 && ocr_novel_chars * 8 >= ocr_quality.alphanumeric_chars.max(1);
    let native_substantially_covered =
        overlap.shared_chars * 4 >= native.quality.alphanumeric_chars.max(1) * 3;

    if native_substantially_covered && !material_novelty {
        return AdaptiveContentChoice {
            markdown: ensure_trailing_newline(native.markdown()),
            source: PageContentSource::Native,
            warning: format!(
                "kept trustworthy {} because OCR added no material coverage",
                native.origin.description()
            ),
            // The candidate exists only because this page was routed with
            // incomplete native coverage. Agreement between two partial
            // hypotheses preserves trustworthy text, but does not prove that
            // the rest of the page was recovered.
            recommend_hosted: true,
        };
    }

    // A shorter, materially lower-quality OCR hypothesis should not displace
    // exact native text even when its mean confidence happens to be high.
    if ocr_quality.score + 0.12 < native.quality.score
        && ocr_quality.alphanumeric_chars * 10
            <= native.quality.alphanumeric_chars.saturating_mul(11)
    {
        return AdaptiveContentChoice {
            markdown: ensure_trailing_newline(native.markdown()),
            source: PageContentSource::Native,
            warning: format!(
                "kept higher-quality {} after comparing OCR",
                native.origin.description()
            ),
            recommend_hosted: false,
        };
    }

    let (markdown, source) = merge_native_and_ocr(native.markdown(), ocr);
    let warning = match source {
        PageContentSource::Native => format!(
            "kept trustworthy {} because OCR duplicated its content",
            native.origin.description()
        ),
        PageContentSource::Fused => format!(
            "fused trustworthy {} with complementary OCR",
            native.origin.description()
        ),
        PageContentSource::Ocr => unreachable!("merge never returns OCR-only content"),
    };
    AdaptiveContentChoice {
        markdown,
        source,
        warning,
        recommend_hosted: false,
    }
}

#[derive(Debug, Clone, Copy)]
struct ContentOverlap {
    shared_chars: usize,
}

fn content_overlap(first: &str, second: &str) -> ContentOverlap {
    let mut first_counts = BTreeMap::<char, usize>::new();
    for character in normalized_content_chars(first) {
        *first_counts.entry(character).or_insert(0) += 1;
    }
    let mut second_counts = BTreeMap::<char, usize>::new();
    for character in normalized_content_chars(second) {
        *second_counts.entry(character).or_insert(0) += 1;
    }
    let shared_chars = first_counts
        .iter()
        .map(|(character, count)| (*count).min(second_counts.get(character).copied().unwrap_or(0)))
        .sum();
    ContentOverlap { shared_chars }
}

fn normalized_content_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
}

fn assess_text_candidate(markdown: &str) -> Option<TextCandidateQuality> {
    if markdown.trim().is_empty()
        || is_garbage_text(markdown)
        || is_cid_garbage(markdown)
        || detect_encoding_issues(markdown)
    {
        return None;
    }

    let alphanumeric_chars = markdown
        .chars()
        .filter(|character| character.is_alphanumeric())
        .count();
    if alphanumeric_chars == 0 {
        return None;
    }
    let visible_chars = markdown
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(1);
    let density = alphanumeric_chars as f32 / visible_chars as f32;
    let length_score = (alphanumeric_chars as f32 / 160.0).min(1.0);
    let nonempty_lines = markdown
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        .max(1);
    let line_score = (alphanumeric_chars as f32 / nonempty_lines as f32 / 12.0).min(1.0);
    let score = (0.45 + length_score * 0.25 + density * 0.20 + line_score * 0.10).min(1.0);
    Some(TextCandidateQuality {
        alphanumeric_chars,
        score,
    })
}

/// Converts recognized line polygons to ordinary PDF-space text items.
fn ocr_text_items(page: &RoutedOcrPage) -> (Vec<TextItem>, usize) {
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
            font_tag: String::new(),
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

fn preserve_ocr_line_breaks(markdown: &str, page: &RoutedOcrPage) -> String {
    let spans: Vec<(&str, f32, f32, f32, f32)> = page
        .ocr
        .spans
        .iter()
        .filter_map(|span| {
            let (left, top, right, bottom) = image_quad_bounds(
                &span.polygon.points,
                page.rendered.width(),
                page.rendered.height(),
            )?;
            (!span.text.trim().is_empty()).then_some((span.text.trim(), left, top, right, bottom))
        })
        .collect();
    if spans.len() < 2 {
        return markdown.to_string();
    }

    let mut line_heights: Vec<f32> = spans
        .iter()
        .map(|(_, _, top, _, bottom)| bottom - top)
        .filter(|height| height.is_finite() && *height > 0.0)
        .collect();
    if line_heights.is_empty() {
        return markdown.to_string();
    }
    line_heights.sort_by(f32::total_cmp);
    let median_height = line_heights[line_heights.len() / 2];

    // The Markdown converter owns reading order and may normalize syntax such
    // as list markers. Match every span back to its unique output occurrence,
    // then use Markdown order rather than imposing a second geometry sort.
    // If the mapping is incomplete or ambiguous, leave the converter output
    // untouched instead of risking a break at the wrong duplicate text.
    let mut mapped = Vec::with_capacity(spans.len());
    for (text, left, top, right, bottom) in spans {
        let Some((start, end)) = unique_markdown_span(markdown, text) else {
            return markdown.to_string();
        };
        mapped.push((start, end, left, top, right, bottom));
    }
    mapped.sort_by_key(|span| span.0);
    if mapped
        .windows(2)
        .any(|pair| pair[0].1 > pair[1].0 || pair[0].0 == pair[1].0)
    {
        return markdown.to_string();
    }

    let mut replacements = Vec::new();
    for pair in mapped.windows(2) {
        let (_, current_end, current_left, _, current_right, current_bottom) = pair[0];
        let (next_start, _, next_left, next_top, next_right, _) = pair[1];
        let overlap = (current_right.min(next_right) - current_left.max(next_left)).max(0.0);
        let narrowest_width = (current_right - current_left).min(next_right - next_left);
        let same_text_flow = narrowest_width > 0.0 && overlap >= narrowest_width * 0.2;
        let separated = next_top - current_bottom >= median_height * 0.65;
        let between = &markdown[current_end..next_start];
        if same_text_flow
            && separated
            && between.chars().all(char::is_whitespace)
            && !markdown_line_at(markdown, current_end).is_some_and(is_markdown_table_line)
            && !markdown_line_at(markdown, next_start).is_some_and(is_markdown_table_line)
        {
            replacements.push((current_end, next_start));
        }
    }

    let mut output = markdown.to_string();
    for (start, end) in replacements.into_iter().rev() {
        output.replace_range(start..end, "\n\n");
    }
    output
}

fn unique_markdown_span(markdown: &str, span_text: &str) -> Option<(usize, usize)> {
    let exact: Vec<_> = markdown.match_indices(span_text).collect();
    match exact.as_slice() {
        [(start, matched)] => return Some((*start, *start + matched.len())),
        [] => {}
        _ => return None,
    }

    let normalized = strip_list_marker(span_text)?;
    let matches: Vec<_> = markdown.match_indices(normalized).collect();
    match matches.as_slice() {
        [(start, matched)] => Some((*start, *start + matched.len())),
        _ => None,
    }
}

fn strip_list_marker(text: &str) -> Option<&str> {
    const BULLETS: &[char] = &['•', '●', '○', '◦', '▪', '–', '—'];
    let trimmed = text.trim_start();
    let remainder = trimmed
        .strip_prefix(BULLETS)?
        .trim_start_matches(char::is_whitespace);
    (!remainder.is_empty()).then_some(remainder)
}

fn markdown_line_at(markdown: &str, offset: usize) -> Option<&str> {
    if offset > markdown.len() || !markdown.is_char_boundary(offset) {
        return None;
    }
    let start = markdown[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = markdown[offset..]
        .find('\n')
        .map_or(markdown.len(), |index| offset + index);
    markdown.get(start..end)
}

fn is_markdown_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn items_inside_regions(items: Vec<TextItem>, regions: &[PdfRect]) -> Vec<TextItem> {
    items
        .into_iter()
        .filter(|item| {
            let center_x = item.x + item.width / 2.0;
            let center_y = item.y + item.height / 2.0;
            regions.iter().any(|region| {
                const EDGE_TOLERANCE_PT: f32 = 2.0;
                item.page == region.page
                    && center_x >= region.x - EDGE_TOLERANCE_PT
                    && center_x <= region.x + region.width + EDGE_TOLERANCE_PT
                    && center_y >= region.y - EDGE_TOLERANCE_PT
                    && center_y <= region.y + region.height + EDGE_TOLERANCE_PT
            })
        })
        .collect()
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
    /// OCR input did not have an explicit full-page or supplemental route.
    #[error("OCR page {page} has no fusion route")]
    MissingOcrRoute {
        /// Unrouted 1-indexed page number.
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

    fn routed_page(page: u32, spans: Vec<OcrSpan>, confidence: Option<f32>) -> RoutedOcrPage {
        RoutedOcrPage {
            rendered: rendered_page(page),
            ocr: OcrPage {
                page_number: page,
                spans,
                mean_confidence: confidence,
                model: ModelIdentity::new("test-ocr", "v1"),
                processing_time_ms: 7,
                warnings: Vec::new(),
            },
        }
    }

    fn positioned_span(text: &str, left: f32, top: f32, right: f32, bottom: f32) -> OcrSpan {
        OcrSpan {
            text: text.to_string(),
            polygon: ImageQuad::new([
                ImagePoint::new(left, top),
                ImagePoint::new(right, top),
                ImagePoint::new(right, bottom),
                ImagePoint::new(left, bottom),
            ]),
            confidence: 0.9,
            orientation_degrees: None,
        }
    }

    fn run(pages: Vec<RoutedOcrPage>) -> OcrRun {
        OcrRun {
            pages,
            render_time_ms: 5,
            ocr_time_ms: 7,
        }
    }

    fn native_candidate(markdown: &str) -> NativeFallbackCandidate {
        assess_native_candidate(markdown.to_string(), NativeCandidateOrigin::Extractor).unwrap()
    }

    #[test]
    fn supplemental_image_ocr_keeps_native_text_without_a_valid_table() {
        let native = [native(0, "Native article text\n", false)];
        let run = run(vec![routed_page(
            1,
            vec![positioned_span(
                "Figure-only OCR label",
                20.0,
                20.0,
                180.0,
                35.0,
            )],
            Some(0.95),
        )]);
        let routes = BTreeMap::from([(
            1,
            OcrFusionRoute::SupplementalRegions(vec![PdfRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
                page: 1,
            }]),
        )]);

        let result = fuse_ocr_pages_adaptive_with_routes(
            &native,
            &run,
            1,
            &OcrFusionOptions::new(),
            &BTreeMap::new(),
            &routes,
        )
        .unwrap();

        assert_eq!(result.pages[0].markdown, "Native article text\n");
        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
        assert!(result.pages[0].provenance.warnings[0].contains("no valid table"));
    }

    #[test]
    fn supplemental_image_ocr_fuses_a_table_inside_the_region() {
        let native = [native(0, "Native article text\n", false)];
        let mut spans = Vec::new();
        for (row, (left, right)) in [
            ("Metric", "Value"),
            ("Accuracy", "95%"),
            ("Recall", "93%"),
            ("Precision", "96%"),
        ]
        .into_iter()
        .enumerate()
        {
            let top = 10.0 + row as f32 * 20.0;
            spans.push(positioned_span(left, 10.0, top, 80.0, top + 10.0));
            spans.push(positioned_span(right, 110.0, top, 180.0, top + 10.0));
        }
        let run = run(vec![routed_page(1, spans, Some(0.95))]);
        let routes = BTreeMap::from([(
            1,
            OcrFusionRoute::SupplementalRegions(vec![PdfRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
                page: 1,
            }]),
        )]);

        let result = fuse_ocr_pages_adaptive_with_routes(
            &native,
            &run,
            1,
            &OcrFusionOptions::new(),
            &BTreeMap::new(),
            &routes,
        )
        .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Fused);
        assert!(result.pages[0].markdown.contains("|Metric|Value|"));
        assert!(result.pages[0].markdown.contains("|Accuracy|95%|"));
        assert_eq!(
            result.pages[0].markdown.matches("Native article").count(),
            1
        );
    }

    #[test]
    fn supplemental_table_detection_requires_multiple_rows() {
        let items = vec![
            TextItem {
                text: "Metric".to_string(),
                x: 10.0,
                y: 40.0,
                width: 50.0,
                height: 10.0,
                font: "OCR".to_string(),
                font_tag: String::new(),
                font_size: 10.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "Value".to_string(),
                x: 110.0,
                y: 40.0,
                width: 50.0,
                height: 10.0,
                font: "OCR".to_string(),
                font_tag: String::new(),
                font_size: 10.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];

        let mut options = MarkdownOptions::default();
        options.base_font_size = Some(10.0);
        assert!(complete_table_markdown_from_items(items, options, 1).is_empty());
    }

    #[test]
    fn supplemental_image_ocr_filters_spans_by_region_center() {
        let items = vec![
            TextItem {
                text: "inside".to_string(),
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 10.0,
                font: "OCR".to_string(),
                font_tag: String::new(),
                font_size: 10.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            },
            TextItem {
                text: "outside".to_string(),
                x: 120.0,
                y: 10.0,
                width: 20.0,
                height: 10.0,
                font: "OCR".to_string(),
                font_tag: String::new(),
                font_size: 10.0,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            },
        ];
        let regions = [PdfRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            page: 1,
        }];

        let filtered = items_inside_regions(items, &regions);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].text, "inside");
    }

    #[test]
    fn scanned_page_uses_geometry_ordered_ocr_and_provenance() {
        let native = [native(0, "", true)];
        let run = run(vec![routed_page(
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
        assert_eq!(result.pages[0].page_number, 1);
        assert_eq!(
            result.pages[0].page_number,
            result.pages[0].provenance.page_number
        );
        assert_eq!(
            result.pages[0].provenance.ocr_model.as_ref().unwrap().name,
            "test-ocr"
        );
        assert!(!result.pages[0].provenance.hosted_recommended);
    }

    #[test]
    fn ocr_assembly_preserves_well_separated_detected_rows() {
        let page = routed_page(
            1,
            vec![
                positioned_span("Column A Column B", 10.0, 10.0, 190.0, 20.0),
                positioned_span("First row value", 10.0, 35.0, 190.0, 45.0),
                positioned_span("Second row value", 10.0, 60.0, 190.0, 70.0),
            ],
            Some(0.9),
        );

        let markdown = ocr_page_to_markdown(&page, 1, &MarkdownOptions::default());

        assert!(markdown.contains("Column A Column B\n\n"), "{markdown:?}");
        assert!(markdown.contains("First row value\n\n"), "{markdown:?}");
    }

    #[test]
    fn line_break_recovery_uses_markdown_reading_order_for_columns() {
        let page = routed_page(
            1,
            vec![
                positioned_span("Left top", 10.0, 10.0, 90.0, 20.0),
                positioned_span("Right top", 110.0, 10.0, 190.0, 20.0),
                positioned_span("Left bottom", 10.0, 40.0, 90.0, 50.0),
                positioned_span("Right bottom", 110.0, 40.0, 190.0, 50.0),
            ],
            Some(0.9),
        );
        let markdown = "Left top Left bottom\n\nRight top Right bottom";

        let recovered = preserve_ocr_line_breaks(markdown, &page);

        assert_eq!(
            recovered,
            "Left top\n\nLeft bottom\n\nRight top\n\nRight bottom"
        );
    }

    #[test]
    fn line_break_recovery_preserves_tables_but_handles_page_prose() {
        let page = routed_page(
            1,
            vec![
                positioned_span("A", 10.0, 10.0, 90.0, 20.0),
                positioned_span("B", 110.0, 10.0, 190.0, 20.0),
                positioned_span("x", 10.0, 30.0, 90.0, 40.0),
                positioned_span("y", 110.0, 30.0, 190.0, 40.0),
                positioned_span("First prose", 10.0, 60.0, 190.0, 70.0),
                positioned_span("Second prose", 10.0, 90.0, 190.0, 100.0),
            ],
            Some(0.9),
        );
        let markdown = "| A | B |\n|---|---|\n| x | y |\n\nFirst prose Second prose";

        let recovered = preserve_ocr_line_breaks(markdown, &page);

        assert!(recovered.starts_with("| A | B |\n|---|---|\n| x | y |"));
        assert!(recovered.ends_with("First prose\n\nSecond prose"));
    }

    #[test]
    fn line_break_recovery_accepts_normalized_list_markers() {
        let page = routed_page(
            1,
            vec![
                positioned_span("• First item", 10.0, 10.0, 190.0, 20.0),
                positioned_span("Next paragraph", 10.0, 40.0, 190.0, 50.0),
            ],
            Some(0.9),
        );

        let recovered = preserve_ocr_line_breaks("- First item Next paragraph", &page);

        assert_eq!(recovered, "- First item\n\nNext paragraph");
    }

    #[test]
    fn line_break_recovery_accepts_white_bullet_list_markers() {
        let page = routed_page(
            1,
            vec![
                positioned_span("◦ First item", 10.0, 10.0, 190.0, 20.0),
                positioned_span("Next paragraph", 10.0, 40.0, 190.0, 50.0),
            ],
            Some(0.9),
        );

        let recovered = preserve_ocr_line_breaks("- First item Next paragraph", &page);

        assert_eq!(recovered, "- First item\n\nNext paragraph");
    }

    #[test]
    fn line_break_recovery_leaves_ambiguous_duplicates_unchanged() {
        let page = routed_page(
            1,
            vec![
                positioned_span("Repeated", 10.0, 10.0, 190.0, 20.0),
                positioned_span("Repeated", 10.0, 40.0, 190.0, 50.0),
            ],
            Some(0.9),
        );
        let markdown = "Repeated Repeated";

        assert_eq!(preserve_ocr_line_breaks(markdown, &page), markdown);
    }

    #[test]
    fn force_mode_deduplicates_native_content() {
        let native = [native(0, "Hello, world!\n", false)];
        let run = run(vec![routed_page(
            1,
            vec![span("Hello world", 10.0, 0.9)],
            Some(0.9),
        )]);

        let result = fuse_ocr_pages(&native, &run, 1, &OcrFusionOptions::new()).unwrap();

        assert_eq!(result.pages[0].markdown, "Hello, world!\n");
        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
    }

    #[test]
    fn adaptive_fallback_keeps_native_text_when_ocr_is_weak() {
        let native = [native(0, "", true)];
        let run = run(vec![routed_page(
            1,
            vec![span("Inv0ice total uncertain", 10.0, 0.3)],
            Some(0.3),
        )]);
        let candidates = BTreeMap::from([(
            1,
            native_candidate("Invoice total: $420.00\nPayment received\n"),
        )]);

        let result =
            fuse_ocr_pages_adaptive(&native, &run, 1, &OcrFusionOptions::new(), &candidates)
                .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
        assert_eq!(
            result.pages[0].markdown,
            "Invoice total: $420.00\nPayment received\n"
        );
        assert!(result.pages[0].provenance.hosted_recommended);
        assert!(result.pages[0].provenance.warnings[0].contains("OCR was weak"));
    }

    #[test]
    fn adaptive_fallback_prefers_exact_native_text_over_duplicate_ocr() {
        let native = [native(0, "", true)];
        let run = run(vec![routed_page(
            1,
            vec![span("Invoice total 420.00 Payment received", 10.0, 0.98)],
            Some(0.98),
        )]);
        let candidates = BTreeMap::from([(
            1,
            native_candidate("Invoice total: $420.00\nPayment received\n"),
        )]);

        let result =
            fuse_ocr_pages_adaptive(&native, &run, 1, &OcrFusionOptions::new(), &candidates)
                .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
        assert!(result.pages[0].provenance.hosted_recommended);
        assert_eq!(result.pages[0].markdown.matches("Invoice").count(), 1);
        assert!(result.pages[0].provenance.warnings[0].contains("no material coverage"));
    }

    #[test]
    fn adaptive_fallback_fuses_short_novel_ocr_content() {
        let native = [native(0, "", true)];
        let run = run(vec![routed_page(
            1,
            vec![span("Status ready 42", 10.0, 0.98)],
            Some(0.98),
        )]);
        let candidates = BTreeMap::from([(1, native_candidate("Status ready\n"))]);

        let result =
            fuse_ocr_pages_adaptive(&native, &run, 1, &OcrFusionOptions::new(), &candidates)
                .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Fused);
        assert!(result.pages[0].markdown.contains("42"));
        assert!(!result.pages[0].provenance.hosted_recommended);
    }

    #[test]
    fn adaptive_fallback_recommends_hosted_for_high_confidence_garbage_ocr() {
        let native = [native(0, "", true)];
        let garbage = "a@@b%%c&&d==e~~".repeat(12);
        let run = run(vec![routed_page(
            1,
            vec![span(&garbage, 10.0, 0.99)],
            Some(0.99),
        )]);
        let candidates = BTreeMap::from([(1, native_candidate("Invoice total 420\n"))]);

        let result =
            fuse_ocr_pages_adaptive(&native, &run, 1, &OcrFusionOptions::new(), &candidates)
                .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
        assert!(result.pages[0].provenance.hosted_recommended);
        assert!(!result.pages[0].markdown.contains("@@"));
    }

    #[test]
    fn adaptive_fallback_fuses_native_header_with_scanned_body() {
        let native = [native(0, "", true)];
        let run = run(vec![routed_page(
            1,
            vec![
                span("Quarterly account report", 10.0, 0.96),
                span("March revenue 420 units", 30.0, 0.96),
                span("April revenue 510 units", 50.0, 0.96),
            ],
            Some(0.96),
        )]);
        let candidates = BTreeMap::from([(1, native_candidate("# Quarterly account report\n"))]);

        let result =
            fuse_ocr_pages_adaptive(&native, &run, 1, &OcrFusionOptions::new(), &candidates)
                .unwrap();

        assert_eq!(result.pages[0].provenance.source, PageContentSource::Fused);
        assert_eq!(result.pages[0].markdown.matches("Quarterly").count(), 1);
        assert!(result.pages[0].markdown.contains("March revenue 420 units"));
        assert!(result.pages[0].markdown.contains("April revenue 510 units"));
        assert!(result.pages[0].provenance.warnings[0].contains("complementary"));
    }

    #[test]
    fn native_candidate_scoring_is_multilingual_and_rejects_garbage() {
        assert!(assess_native_candidate(
            "請求書 合計金額 4200円\n支払済み\n".to_string(),
            NativeCandidateOrigin::Extractor,
        )
        .is_some());
        assert!(assess_native_candidate(
            "a@@b%%c&&d==e~~".repeat(12),
            NativeCandidateOrigin::Extractor,
        )
        .is_none());
    }

    #[test]
    fn force_mode_appends_only_additional_ocr_blocks() {
        let native = [native(0, "Native title\n", false)];
        let run = run(vec![routed_page(
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
        let run = run(vec![routed_page(
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
        let duplicate = run(vec![routed_page(
            1,
            vec![span("indented code", 10.0, 0.9)],
            Some(0.9),
        )]);
        let result = fuse_ocr_pages(&native, &duplicate, 1, &OcrFusionOptions::new()).unwrap();
        assert_eq!(result.pages[0].markdown, "    indented code\n");

        let addition = run(vec![routed_page(
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
        let run = run(vec![routed_page(
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
            &run(vec![routed_page(2, Vec::new(), None)]),
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
