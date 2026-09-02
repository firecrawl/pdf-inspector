//! Layout-block recording for citation grounding.
//!
//! The Markdown convert loop already classifies every fragment it emits
//! (heading tier, list item, caption, code, table, image) — it just throws
//! the classification away once the text is pushed into the output string.
//! [`BlockRecorder`] captures those decisions as they happen: each emitted
//! fragment is recorded with its byte range in the raw (pre-postprocess)
//! output plus the union bbox of the source geometry.
//!
//! [`BlockRecorder::finish`] then assembles the final blocks payload. The
//! document-level postprocess pass ([`clean_markdown`]) rewrites bytes
//! (hyphenation, space collapsing, URL formatting), which would invalidate
//! recorded offsets — so each fragment is cleaned *individually* and the
//! payload's Markdown is reassembled from the cleaned fragments with the
//! original inter-block separators. `markdown_span` offsets are therefore
//! exact byte ranges into the payload's own `markdown` string. The default
//! `process_pdf` output is untouched: recording is opt-in and never changes
//! what the convert loop emits.

use crate::types::{TextItem, TextLine};

use super::postprocess::clean_markdown;
use super::MarkdownOptions;

/// Classification of one recorded fragment, mirroring the convert loop's
/// own emission branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawBlockKind {
    /// Markdown heading with its level (1–6).
    Heading(usize),
    /// Body paragraph, block quote, or short mono fragment demoted to text.
    Text,
    /// Bulleted or numbered list item (including wrapped continuations).
    ListItem,
    /// Figure/table caption or source citation line.
    Caption,
    /// Fenced code block.
    Code,
    /// Markdown pipe table.
    Table,
    /// Image placeholder (only emitted with `include_images`).
    Picture,
}

/// One block in the finalized payload: kind, source page, PDF-space bbox,
/// and its byte span in [`LayoutBlocksOutput::markdown`].
#[derive(Debug, Clone)]
pub(crate) struct RecordedBlock {
    pub(crate) kind: RawBlockKind,
    /// 1-indexed page the block was emitted from.
    pub(crate) page: u32,
    /// Union bbox `(x0, y0, x1, y1)` in PDF points (y-up, bottom-left
    /// origin), from the block's source items.
    pub(crate) bbox: Option<(f32, f32, f32, f32)>,
    /// `[start, end)` byte offsets into the finalized markdown.
    pub(crate) span: (usize, usize),
}

/// Finalized layout-blocks payload: the assembled markdown plus the blocks
/// whose `span` ranges index into it.
#[derive(Debug, Default)]
pub(crate) struct LayoutBlocksOutput {
    pub(crate) markdown: String,
    pub(crate) blocks: Vec<RecordedBlock>,
}

/// A fragment recorded against the raw convert-loop output.
#[derive(Debug, Clone)]
struct RawBlock {
    kind: RawBlockKind,
    page: u32,
    start: usize,
    end: usize,
    bbox: Option<(f32, f32, f32, f32)>,
}

/// Records emitted fragments during the convert loop.
#[derive(Debug, Default)]
pub(crate) struct BlockRecorder {
    blocks: Vec<RawBlock>,
    /// Whether the last block may still be extended (open paragraph or
    /// list item awaiting wrapped continuation lines).
    open: bool,
    finished: Option<LayoutBlocksOutput>,
}

impl BlockRecorder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record one fragment emitted at `raw[start..end]`.
    ///
    /// With `continuation`, the fragment extends the currently open block of
    /// the same kind (paragraph line joins, wrapped list items) instead of
    /// starting a new one. `extendable` keeps the block open for future
    /// continuations; one-shot emissions (headings, captions, tables) close
    /// it immediately.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_fragment(
        &mut self,
        kind: RawBlockKind,
        page: u32,
        start: usize,
        end: usize,
        bbox: Option<(f32, f32, f32, f32)>,
        continuation: bool,
        extendable: bool,
    ) {
        if continuation && self.open {
            if let Some(last) = self.blocks.last_mut() {
                if last.kind == kind {
                    last.end = last.end.max(end);
                    // Blocks are page-scoped in practice; if a continuation
                    // crosses pages, keep the first page's geometry.
                    if last.page == page {
                        last.bbox = union_bbox(last.bbox, bbox);
                    }
                    self.open = extendable;
                    return;
                }
            }
        }
        self.blocks.push(RawBlock {
            kind,
            page,
            start,
            end,
            bbox,
        });
        self.open = extendable;
    }

    /// Assemble the final payload from the raw convert-loop output.
    ///
    /// Each fragment is cleaned individually (same [`clean_markdown`]
    /// passes as the default pipeline) so recorded offsets stay exact, and
    /// fragments are re-joined with separators derived from the raw output
    /// (single newline between adjacent list items, blank line between
    /// paragraphs). Fragments that clean to nothing (e.g. a stray folio
    /// removed by `remove_page_numbers`) are dropped.
    pub(crate) fn finish(&mut self, raw: &str, options: &MarkdownOptions) {
        let raw_blocks = std::mem::take(&mut self.blocks);
        let mut markdown = String::new();
        let mut blocks = Vec::with_capacity(raw_blocks.len());
        // End of the previous block's trimmed content in the raw output;
        // the bytes from here to the next block's trimmed start form the
        // inter-block separator.
        let mut prev_content_end = 0usize;
        let mut have_prev = false;

        // `remove_page_numbers` decides by line isolation, and a single-item
        // fragment is always isolated — so a legitimate list item whose text
        // is page-number-shaped ("- 5 -") would be dropped here even though
        // the document-level pass keeps it (its list neighbors break the
        // isolation). List items are already positively classified content;
        // never run folio removal on them.
        let list_item_options = MarkdownOptions {
            remove_page_numbers: false,
            ..options.clone()
        };
        for block in raw_blocks {
            if block.start >= block.end || block.end > raw.len() {
                continue;
            }
            let Some(fragment) = raw.get(block.start..block.end) else {
                continue;
            };
            let content_start = block.start + (fragment.len() - fragment.trim_start().len());
            let content_end = block.start + fragment.trim_end().len();
            let fragment_options = if block.kind == RawBlockKind::ListItem {
                &list_item_options
            } else {
                options
            };
            let cleaned = clean_markdown(fragment.to_string(), fragment_options);
            let cleaned = cleaned.trim();
            if cleaned.is_empty() {
                // Removed entirely by postprocess (e.g. a folio line).
                prev_content_end = content_end.max(prev_content_end);
                continue;
            }

            if have_prev {
                let sep_src = raw
                    .get(prev_content_end.min(content_start)..content_start)
                    .unwrap_or("");
                if sep_src.chars().any(|c| !c.is_whitespace()) {
                    // Unrecorded non-whitespace between blocks (e.g. a
                    // `<!-- Page N -->` marker) is preserved verbatim.
                    markdown.push_str("\n\n");
                    markdown.push_str(sep_src.trim());
                    markdown.push_str("\n\n");
                } else if sep_src.matches('\n').count() >= 2 {
                    markdown.push_str("\n\n");
                } else {
                    markdown.push('\n');
                }
            }

            let span_start = markdown.len();
            markdown.push_str(cleaned);
            blocks.push(RecordedBlock {
                kind: block.kind,
                page: block.page,
                bbox: block.bbox,
                span: (span_start, markdown.len()),
            });
            prev_content_end = content_end;
            have_prev = true;
        }

        if !markdown.is_empty() {
            markdown.push('\n');
        }
        self.finished = Some(LayoutBlocksOutput { markdown, blocks });
    }

    /// Take the finalized payload (empty when the convert loop produced no
    /// output at all).
    pub(crate) fn take_output(&mut self) -> LayoutBlocksOutput {
        self.finished.take().unwrap_or_default()
    }
}

fn union_bbox(
    a: Option<(f32, f32, f32, f32)>,
    b: Option<(f32, f32, f32, f32)>,
) -> Option<(f32, f32, f32, f32)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

fn item_bbox(item: &TextItem) -> Option<(f32, f32, f32, f32)> {
    let x0 = item.x.min(item.x + item.width);
    let x1 = item.x.max(item.x + item.width);
    let y0 = item.y.min(item.y + item.height);
    let y1 = item.y.max(item.y + item.height);
    (x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite())
        .then_some((x0, y0, x1, y1))
}

/// Union bbox of a text line's items in PDF points (y-up).
pub(crate) fn line_bbox(line: &TextLine) -> Option<(f32, f32, f32, f32)> {
    line.items
        .iter()
        .fold(None, |acc, item| union_bbox(acc, item_bbox(item)))
}

/// Fold a text line's bbox into an accumulator (used while buffering code
/// lines whose fenced block is emitted later).
pub(crate) fn union_line_bbox(
    acc: Option<(f32, f32, f32, f32)>,
    line: &TextLine,
) -> Option<(f32, f32, f32, f32)> {
    union_bbox(acc, line_bbox(line))
}

/// Union bbox of the indexed items in PDF points (y-up). Used for table
/// blocks, whose `item_indices` reference the detection-time item slice.
pub(crate) fn items_bbox(items: &[TextItem], indices: &[usize]) -> Option<(f32, f32, f32, f32)> {
    indices
        .iter()
        .filter_map(|&idx| items.get(idx))
        .fold(None, |acc, item| union_bbox(acc, item_bbox(item)))
}

/// Bbox of a single (image) item in PDF points (y-up).
pub(crate) fn single_item_bbox(item: &TextItem) -> Option<(f32, f32, f32, f32)> {
    item_bbox(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{
        to_markdown_from_items_with_rects_and_lines,
        to_markdown_with_layout_blocks_from_items_with_rects_and_lines, MarkdownDocumentContext,
        MarkdownOptions,
    };
    use crate::types::ItemType;
    use std::collections::{HashMap, HashSet};

    fn make_item(text: &str, x: f32, y: f32, font_size: f32, page: u32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * font_size * 0.5,
            height: font_size,
            font: "Helvetica".to_string(),
            font_tag: String::new(),
            font_size,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn context() -> MarkdownDocumentContext<'static> {
        use once_cell::sync::Lazy;
        static THRESHOLDS: Lazy<HashMap<u32, f32>> = Lazy::new(HashMap::new);
        MarkdownDocumentContext {
            page_thresholds: &THRESHOLDS,
            struct_roles: None,
            struct_tables: &[],
            page_count: 1,
            prefiltered_page_number_pages: None,
            prefiltered_page_number_mask: None,
            precomputed_chart_regions: None,
        }
    }

    fn sample_items() -> Vec<TextItem> {
        vec![
            make_item("Big Title", 72.0, 720.0, 24.0, 1),
            make_item("First paragraph line at body size.", 72.0, 680.0, 12.0, 1),
            make_item(
                "Second wrapped line of the paragraph.",
                72.0,
                666.0,
                12.0,
                1,
            ),
            make_item("• Item one", 72.0, 620.0, 12.0, 1),
            make_item("• Item two", 72.0, 606.0, 12.0, 1),
            make_item("Figure 1: A caption line.", 72.0, 560.0, 12.0, 1),
        ]
    }

    fn assert_spans_are_valid(markdown: &str, blocks: &[RecordedBlock]) {
        let mut prev_end = 0usize;
        for block in blocks {
            let (start, end) = block.span;
            assert!(
                start >= prev_end && start < end && end <= markdown.len(),
                "invalid span {:?} (prev_end {prev_end}) in {markdown:?}",
                block.span
            );
            assert!(
                !markdown[start..end].trim().is_empty(),
                "span {:?} slices to whitespace in {markdown:?}",
                block.span
            );
            prev_end = end;
        }
    }

    #[test]
    fn records_headings_paragraphs_lists_and_captions() {
        let (_, output) = to_markdown_with_layout_blocks_from_items_with_rects_and_lines(
            sample_items(),
            MarkdownOptions::default(),
            &[],
            &[],
            context(),
        );
        let kinds: Vec<RawBlockKind> = output.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RawBlockKind::Heading(1),
                RawBlockKind::Text,
                RawBlockKind::ListItem,
                RawBlockKind::ListItem,
                RawBlockKind::Caption,
            ],
            "unexpected block kinds in {:?}",
            output.markdown
        );
        assert_spans_are_valid(&output.markdown, &output.blocks);

        let slice = |idx: usize| {
            let (start, end) = output.blocks[idx].span;
            &output.markdown[start..end]
        };
        assert_eq!(slice(0), "# Big Title");
        assert_eq!(
            slice(1),
            "First paragraph line at body size. Second wrapped line of the paragraph."
        );
        assert_eq!(slice(2), "- Item one");
        assert_eq!(slice(3), "- Item two");
        assert_eq!(slice(4), "Figure 1: A caption line.");
    }

    #[test]
    fn paragraph_bbox_unions_wrapped_lines() {
        let (_, output) = to_markdown_with_layout_blocks_from_items_with_rects_and_lines(
            sample_items(),
            MarkdownOptions::default(),
            &[],
            &[],
            context(),
        );
        let paragraph = &output.blocks[1];
        assert_eq!(paragraph.page, 1);
        let (x0, y0, _, y1) = paragraph.bbox.expect("paragraph bbox");
        // Second line's baseline (666) through first line's top (680 + 12).
        assert!(x0 <= 72.0 && y0 <= 666.0 && y1 >= 692.0, "{:?}", paragraph);
    }

    #[test]
    fn records_picture_blocks_when_images_are_included() {
        let mut image = make_item("[Image: Im0]", 100.0, 400.0, 0.0, 1);
        image.width = 200.0;
        image.height = 150.0;
        image.item_type = ItemType::Image;
        let mut items = sample_items();
        items.push(image);

        let options = MarkdownOptions {
            include_images: true,
            ..MarkdownOptions::default()
        };
        let (_, output) = to_markdown_with_layout_blocks_from_items_with_rects_and_lines(
            items,
            options,
            &[],
            &[],
            context(),
        );
        let picture = output
            .blocks
            .iter()
            .find(|b| b.kind == RawBlockKind::Picture)
            .expect("picture block");
        assert_eq!(picture.bbox, Some((100.0, 400.0, 300.0, 550.0)));
        let (start, end) = picture.span;
        assert!(output.markdown[start..end].starts_with("![Image:"));
        assert_spans_are_valid(&output.markdown, &output.blocks);
    }

    #[test]
    fn recording_does_not_change_default_markdown() {
        let plain = to_markdown_from_items_with_rects_and_lines(
            sample_items(),
            MarkdownOptions::default(),
            &[],
            &[],
            context(),
        );
        let (recorded, _) = to_markdown_with_layout_blocks_from_items_with_rects_and_lines(
            sample_items(),
            MarkdownOptions::default(),
            &[],
            &[],
            context(),
        );
        assert_eq!(plain, recorded);
    }

    #[test]
    fn finish_drops_fragments_that_clean_to_nothing() {
        let mut recorder = BlockRecorder::new();
        let raw = "Hello world.\n\n17\n\nGoodbye.\n";
        recorder.push_fragment(RawBlockKind::Text, 1, 0, 12, None, false, true);
        // A stray folio: `remove_page_numbers` cleans it to nothing.
        recorder.push_fragment(RawBlockKind::Text, 1, 14, 16, None, false, true);
        recorder.push_fragment(RawBlockKind::Text, 2, 18, 26, None, false, true);
        recorder.finish(raw, &MarkdownOptions::default());
        let output = recorder.take_output();

        assert_eq!(output.markdown, "Hello world.\n\nGoodbye.\n");
        assert_eq!(output.blocks.len(), 2);
        assert_eq!(output.blocks[0].span, (0, 12));
        assert_eq!(output.blocks[1].span, (14, 22));
        assert_eq!(&output.markdown[14..22], "Goodbye.");
    }

    #[test]
    fn finish_keeps_page_number_shaped_list_items() {
        // "- 5 -" is a page-number expression, but as a classified list item
        // it is content. The document-level pass keeps it because its list
        // neighbors break line isolation; the per-fragment pass must not
        // drop it just because a lone fragment is always "isolated".
        let mut recorder = BlockRecorder::new();
        let raw = "- one\n- 5 -\n- two\n";
        recorder.push_fragment(RawBlockKind::ListItem, 1, 0, 6, None, false, true);
        recorder.push_fragment(RawBlockKind::ListItem, 1, 6, 12, None, false, true);
        recorder.push_fragment(RawBlockKind::ListItem, 1, 12, 18, None, false, true);
        recorder.finish(raw, &MarkdownOptions::default());
        let output = recorder.take_output();

        assert_eq!(output.markdown, "- one\n- 5 -\n- two\n");
        assert_eq!(output.blocks.len(), 3);
        assert_eq!(&output.markdown[6..11], "- 5 -");
    }

    #[test]
    fn finish_preserves_single_newline_list_separators() {
        let mut recorder = BlockRecorder::new();
        let raw = "- one\n- two\n\nAfter list.\n";
        recorder.push_fragment(RawBlockKind::ListItem, 1, 0, 6, None, false, true);
        recorder.push_fragment(RawBlockKind::ListItem, 1, 6, 12, None, false, true);
        recorder.push_fragment(RawBlockKind::Text, 1, 13, 24, None, false, true);
        recorder.finish(raw, &MarkdownOptions::default());
        let output = recorder.take_output();

        assert_eq!(output.markdown, "- one\n- two\n\nAfter list.\n");
        assert_spans_are_valid(&output.markdown, &output.blocks);
        assert_eq!(&output.markdown[6..11], "- two");
    }

    #[test]
    fn continuation_extends_the_open_block() {
        let mut recorder = BlockRecorder::new();
        let raw = "- item wraps here\n";
        recorder.push_fragment(
            RawBlockKind::ListItem,
            1,
            0,
            7,
            Some((72.0, 600.0, 120.0, 612.0)),
            false,
            true,
        );
        recorder.push_fragment(
            RawBlockKind::ListItem,
            1,
            7,
            18,
            Some((90.0, 586.0, 150.0, 598.0)),
            true,
            true,
        );
        recorder.finish(raw, &MarkdownOptions::default());
        let output = recorder.take_output();

        assert_eq!(output.blocks.len(), 1);
        assert_eq!(output.blocks[0].bbox, Some((72.0, 586.0, 150.0, 612.0)));
        assert_eq!(
            &output.markdown[output.blocks[0].span.0..output.blocks[0].span.1],
            "- item wraps here"
        );
    }

    #[test]
    fn unrecorded_page_markers_are_preserved_as_separators() {
        let mut recorder = BlockRecorder::new();
        let raw = "First page text.\n\n\n\n<!-- Page 2 -->\n\nSecond page text.\n";
        recorder.push_fragment(RawBlockKind::Text, 1, 0, 16, None, false, true);
        recorder.push_fragment(RawBlockKind::Text, 2, 37, 54, None, false, true);
        recorder.finish(raw, &MarkdownOptions::default());
        let output = recorder.take_output();

        assert_eq!(
            output.markdown,
            "First page text.\n\n<!-- Page 2 -->\n\nSecond page text.\n"
        );
        assert_spans_are_valid(&output.markdown, &output.blocks);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let (_, output) = to_markdown_with_layout_blocks_from_items_with_rects_and_lines(
            Vec::new(),
            MarkdownOptions::default(),
            &[],
            &[],
            context(),
        );
        assert!(output.markdown.is_empty());
        assert!(output.blocks.is_empty());
        let _ = HashSet::<u32>::new();
    }
}
