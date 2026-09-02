//! Table-cell text rendering.
//!
//! Every table detector assembles cell strings from positioned items; this
//! module is the one place that turns items into cell text, so all of them
//! render a super/subscript run the same way `TextLine::text` does
//! (`V<sub>f</sub>`, `$1,234<sup>1</sup>`) and share one spacing policy at
//! the run's edges (`crate::types::script_edge_needs_space`).

use crate::types::TextItem;

/// `text` (the caller's view of an item's text, trimmed or not) as it
/// appears in a table cell: a super/subscript run keeps its `<sup>`/`<sub>`
/// markup, exactly as `TextLine::text` renders it, so cell assembly paths
/// that join fragments themselves never flatten a marker into plain digits.
pub(crate) fn cell_fragment(item: &TextItem, text: &str) -> String {
    let mut fragment = String::new();
    crate::types::push_item_text(&mut fragment, item, text);
    fragment
}

/// Join cell items with subscript/superscript-aware spacing
/// Same logic as TextLine::text() but for table cells
pub(crate) fn join_cell_items(items: &[&TextItem]) -> String {
    let mut result = String::new();
    let mut last: Option<&TextItem> = None;

    for item in items {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }

        if let Some(prev_item) = last {
            // Don't add space before/after hyphens
            let prev_ends_with_hyphen = result.ends_with('-');
            let curr_is_hyphen = text == "-";
            let curr_starts_with_hyphen = text.starts_with('-');
            let prev_ends_with_open_delimiter =
                result.ends_with('(') || result.ends_with('[') || result.ends_with('{');
            let curr_starts_with_close_delimiter =
                text.starts_with(')') || text.starts_with(']') || text.starts_with('}');

            // Detect unflagged subscript/superscript: smaller font size and/or Y offset
            let font_ratio = item.font_size / prev_item.font_size;
            let reverse_font_ratio = prev_item.font_size / item.font_size;
            let y_diff = (item.y - prev_item.y).abs();

            // Current item is subscript/superscript (smaller than previous)
            let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
            // Previous item was subscript/superscript (returning to normal size)
            let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

            // A script run flagged by extraction renders as `<sup>`/`<sub>`
            // and takes the same edge-spacing policy as `TextLine::text`:
            // "V<sub>f</sub>", "$1,234<sup>1</sup>", "word<sup>2</sup> next".
            let needs_space =
                match crate::types::script_edge_needs_space(prev_item, item, &result, text) {
                    Some(needs_space) => needs_space,
                    None => {
                        !(prev_ends_with_hyphen
                            || curr_is_hyphen
                            || curr_starts_with_hyphen
                            || is_sub_super
                            || was_sub_super
                            || prev_ends_with_open_delimiter
                            || curr_starts_with_close_delimiter)
                    }
                };
            if needs_space {
                result.push(' ');
            }
            if crate::types::stacked_fraction_slash(prev_item, item) {
                result.push('/');
            }
            crate::types::push_item_text(&mut result, item, text);
        } else {
            crate::types::push_item_text(&mut result, item, text);
        }
        last = Some(item);
    }

    result
}

/// One piece of a cell string: plain text, or the content of a
/// `<sup>…</sup>` / `<sub>…</sub>` span.
enum Segment<'a> {
    Text(&'a str),
    Span(&'a str),
}

/// Split cell text at exact `<sup>`/`<sub>` openers (literal text such as
/// "<sum>" is never a span). An unmatched opener is dropped and the text after
/// it kept.
fn script_segments(text: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut rest = text;
    loop {
        let (open, tag, close) = match (rest.find("<sup>"), rest.find("<sub>")) {
            (Some(a), Some(b)) if a <= b => (a, "<sup>", "</sup>"),
            (Some(a), None) => (a, "<sup>", "</sup>"),
            (_, Some(b)) => (b, "<sub>", "</sub>"),
            (None, None) => break,
        };
        if open > 0 {
            segments.push(Segment::Text(&rest[..open]));
        }
        let body = &rest[open + tag.len()..];
        match body.find(close) {
            Some(i) => {
                segments.push(Segment::Span(&body[..i]));
                rest = &body[i + close.len()..];
            }
            None => rest = body,
        }
    }
    if !rest.is_empty() {
        segments.push(Segment::Text(rest));
    }
    segments
}

/// Footnote/annotation marker content: digits, brackets and marker symbols
/// ("1)", "(5)", "*", "†"). Letters ("f", "subc", "i") are label content.
fn is_marker_content(content: &str) -> bool {
    let t = content.trim();
    !t.is_empty()
        && t.chars().all(|c| {
            c.is_numeric()
                || matches!(
                    c,
                    '(' | ')' | '[' | ']' | '*' | '†' | '‡' | '§' | '¶' | ',' | ' '
                )
        })
}

fn has_script_markup(text: &str) -> bool {
    text.contains("<sup>") || text.contains("<sub>")
}

/// Cell text with the `<sup>`/`<sub>` tags removed but their content kept,
/// for length-based heuristics: `IV<sub>subc</sub>` measures as `IVsubc`.
pub(crate) fn strip_script_markup(text: &str) -> std::borrow::Cow<'_, str> {
    if !has_script_markup(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for segment in script_segments(text) {
        match segment {
            Segment::Text(t) | Segment::Span(t) => out.push_str(t),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Cell text for value classification: footnote-marker spans
/// (`<sup>1)</sup>`, `<sup>*</sup>`) removed entirely — a marker is an
/// annotation on the cell, not part of its value, so `12<sup>1)</sup>` is
/// the number 12 — while letter spans keep their content (`V<sub>f</sub>` →
/// `Vf`), so a label never collapses onto a shorter token such as the roman
/// numeral `V`.
pub(crate) fn strip_marker_spans(text: &str) -> std::borrow::Cow<'_, str> {
    if !has_script_markup(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for segment in script_segments(text) {
        match segment {
            Segment::Text(t) => out.push_str(t),
            Segment::Span(t) if !is_marker_content(t) => out.push_str(t),
            Segment::Span(_) => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Cell text with every `<sup>…</sup>` / `<sub>…</sub>` span removed, tags
/// and content alike: what the cell says outside its annotations.
pub(crate) fn strip_script_spans(text: &str) -> std::borrow::Cow<'_, str> {
    if !has_script_markup(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for segment in script_segments(text) {
        if let Segment::Text(t) = segment {
            out.push_str(t);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Whether the cell carries a script span with letter content — a label
/// like `V<sub>f</sub>` or `x<sub>i</sub>`, which is never a page number
/// however its letters read as roman numerals.
pub(crate) fn has_letter_script_span(text: &str) -> bool {
    has_script_markup(text)
        && script_segments(text)
            .iter()
            .any(|segment| matches!(segment, Segment::Span(t) if !is_marker_content(t)))
}

/// Append one item to a cell that is being built incrementally (row/column
/// grid fills, line-anchored rows, header bands). `last` is the item most
/// recently appended to this cell, `None` for an empty cell.
///
/// Plain fragments keep the builders' historical single-space join; when the
/// previous or the current item is a super/subscript run, the shared
/// script-edge policy decides instead, so "V" + lowered "f" becomes
/// `V<sub>f</sub>` rather than `V <sub>f</sub>`.
pub(crate) fn push_cell_item<'a>(
    cell: &mut String,
    last: &mut Option<&'a TextItem>,
    item: &'a TextItem,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    if !cell.is_empty() {
        let needs_space = match last {
            Some(prev) => crate::types::script_edge_needs_space(prev, item, cell, text),
            None => None,
        };
        if needs_space.unwrap_or(true) {
            cell.push(' ');
        }
        if last.is_some_and(|prev| crate::types::stacked_fraction_slash(prev, item)) {
            cell.push('/');
        }
    }
    crate::types::push_item_text(cell, item, text);
    *last = Some(item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn make_item(text: &str, x: f32, y: f32, font_size: f32) -> TextItem {
        TextItem {
            rotation: 0.0,
            advance_known: true,
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * font_size * 0.5,
            height: font_size,
            font: "TestFont".to_string(),
            font_tag: String::new(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    #[test]
    fn test_join_cell_items_single_item() {
        let item = make_item("Hello", 100.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&item]), "Hello");
    }

    #[test]
    fn test_join_cell_items_multiple_spaced() {
        let a = make_item("Hello", 100.0, 500.0, 10.0);
        let b = make_item("World", 150.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b]), "Hello World");
    }

    #[test]
    fn test_join_cell_items_hyphen_no_space() {
        let a = make_item("pre", 100.0, 500.0, 10.0);
        let b = make_item("-", 120.0, 500.0, 10.0);
        let c = make_item("fix", 130.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b, &c]), "pre-fix");
    }

    #[test]
    fn test_join_cell_items_parenthetical_no_inner_spaces() {
        let a = make_item("The first sentence", 100.0, 500.0, 10.0);
        let b = make_item("(", 190.0, 500.0, 10.0);
        let c = make_item("twice", 195.0, 500.0, 10.0);
        let d = make_item(")", 220.0, 500.0, 10.0);
        assert_eq!(
            join_cell_items(&[&a, &b, &c, &d]),
            "The first sentence (twice)"
        );
    }

    #[test]
    fn test_join_cell_items_subscript_no_space() {
        let a = make_item("H", 100.0, 500.0, 12.0);
        let b = make_item("2", 110.0, 497.0, 8.0); // smaller font, Y offset
        assert_eq!(join_cell_items(&[&a, &b]), "H2");
    }

    #[test]
    fn test_join_cell_items_empty_items_skipped() {
        let a = make_item("Hello", 100.0, 500.0, 10.0);
        let b = make_item("  ", 120.0, 500.0, 10.0);
        let c = make_item("World", 150.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b, &c]), "Hello World");
    }

    fn cell(text: &str, x: f32, width: f32, y: f32, font_size: f32, shift: f32) -> TextItem {
        TextItem {
            rotation: 0.0,
            advance_known: true,
            text: text.into(),
            x,
            y,
            width,
            height: font_size,
            font: "F1".into(),
            font_tag: String::new(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: shift,
        }
    }

    #[test]
    fn subscript_run_renders_with_sub_tag() {
        let v = cell("V", 100.0, 6.0, 500.0, 9.0, 0.0);
        let f = cell("f", 106.0, 2.2, 498.5, 6.0, -1.5);
        assert_eq!(join_cell_items(&[&v, &f]), "V<sub>f</sub>");
    }

    #[test]
    fn footnote_marker_after_number_stays_a_separate_span() {
        let n = cell("$1,234", 100.0, 33.0, 500.0, 10.0, 0.0);
        let m = cell("1", 133.0, 3.6, 503.5, 6.5, 3.5);
        assert_eq!(join_cell_items(&[&n, &m]), "$1,234<sup>1</sup>");
    }

    #[test]
    fn word_space_after_marker_follows_geometry() {
        let w = cell("word", 100.0, 24.0, 500.0, 10.0, 0.0);
        let m = cell("2", 124.0, 3.6, 503.5, 6.5, 3.5);
        let next = cell("next", 130.5, 24.0, 500.0, 10.0, 0.0);
        assert_eq!(join_cell_items(&[&w, &m, &next]), "word<sup>2</sup> next");
    }

    #[test]
    fn leading_marker_hugs_the_following_word() {
        let m = cell("3,4", 100.0, 9.5, 503.5, 7.0, 3.5);
        let w = cell("Institute", 109.5, 40.0, 500.0, 10.0, 0.0);
        assert_eq!(join_cell_items(&[&m, &w]), "<sup>3,4</sup>Institute");
    }

    #[test]
    fn push_cell_item_keeps_plain_space_join() {
        let a = cell("Hello", 100.0, 30.0, 500.0, 10.0, 0.0);
        let b = cell("World", 140.0, 30.0, 500.0, 10.0, 0.0);
        let mut text = String::new();
        let mut last = None;
        push_cell_item(&mut text, &mut last, &a, a.text.trim());
        push_cell_item(&mut text, &mut last, &b, b.text.trim());
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn push_cell_item_skips_whitespace_only_fragments() {
        let w = cell("word", 100.0, 24.0, 500.0, 10.0, 0.0);
        let blank = cell(" ", 124.0, 2.0, 500.0, 10.0, 0.0);
        let m = cell("1", 124.0, 3.6, 503.5, 6.5, 3.5);
        let mut text = String::new();
        let mut last = None;
        for it in [&w, &blank, &m] {
            push_cell_item(&mut text, &mut last, it, it.text.trim());
        }
        assert_eq!(text, "word<sup>1</sup>");
    }

    #[test]
    fn join_cell_items_spacing_uses_the_last_nonempty_item() {
        let w = cell("word", 100.0, 24.0, 500.0, 10.0, 0.0);
        let blank = cell(" ", 124.0, 2.0, 500.0, 10.0, 0.0);
        let m = cell("1", 124.0, 3.6, 503.5, 6.5, 3.5);
        assert_eq!(join_cell_items(&[&w, &blank, &m]), "word<sup>1</sup>");
    }

    #[test]
    fn script_at_a_wrapped_line_end_does_not_attach_to_the_next_line() {
        // Cell wraps: "value" + raised "1" on line 1, "next" starts line 2
        // under the marker. Different visual lines keep their separator.
        let v = cell("value", 100.0, 26.0, 500.0, 10.0, 0.0);
        let m = cell("1", 126.0, 3.6, 503.5, 6.5, 3.5);
        let next = cell("next", 100.0, 22.0, 488.0, 10.0, 0.0);
        assert_eq!(join_cell_items(&[&v, &m, &next]), "value<sup>1</sup> next");
        let mut text = String::new();
        let mut last = None;
        for it in [&v, &m, &next] {
            push_cell_item(&mut text, &mut last, it, it.text.trim());
        }
        assert_eq!(text, "value<sup>1</sup> next");
    }

    #[test]
    fn stacked_fraction_in_a_cell_renders_with_a_slash() {
        let three = cell("3", 100.0, 5.0, 500.0, 10.0, 0.0);
        let num = cell("1", 106.5, 3.7, 503.96, 7.4, 3.96);
        let den = cell("3", 106.5, 3.7, 496.0, 7.4, -4.0);
        assert_eq!(
            join_cell_items(&[&three, &num, &den]),
            "3 <sup>1</sup>/<sub>3</sub>"
        );
    }

    #[test]
    fn strip_script_markup_removes_only_the_tags() {
        assert_eq!(strip_script_markup("IV<sub>subc</sub>"), "IVsubc");
        assert_eq!(strip_script_markup("12<sup>1)</sup>"), "121)");
        assert_eq!(strip_script_markup("a <sum> b"), "a <sum> b");
    }

    #[test]
    fn strip_marker_spans_drops_markers_but_keeps_letter_content() {
        assert_eq!(strip_marker_spans("12<sup>1)</sup>"), "12");
        assert_eq!(strip_marker_spans("Total<sup>*</sup>"), "Total");
        assert_eq!(strip_marker_spans("x<sub>i</sub>"), "xi");
        assert_eq!(strip_marker_spans("V<sub>f</sub> m/s"), "Vf m/s");
        assert_eq!(strip_marker_spans("a<sup>1</sup>b<sub>2</sub>c"), "abc");
        // Literal text is never a span; an unmatched opener drops only itself.
        assert_eq!(strip_marker_spans("a <sum> b<sup>1</sup>"), "a <sum> b");
        assert_eq!(strip_marker_spans("odd <sup>1"), "odd 1");
        assert!(matches!(
            strip_marker_spans("plain"),
            std::borrow::Cow::Borrowed("plain")
        ));
    }

    #[test]
    fn strip_script_spans_keeps_only_the_base_text() {
        assert_eq!(strip_script_spans("12<sup>a</sup>"), "12");
        assert_eq!(strip_script_spans("V<sub>f</sub> m/s"), "V m/s");
        assert_eq!(strip_script_spans("plain"), "plain");
    }

    #[test]
    fn letter_script_spans_are_labels() {
        assert!(has_letter_script_span("V<sub>f</sub>"));
        assert!(has_letter_script_span("x<sub>i</sub>"));
        assert!(!has_letter_script_span("12<sup>1)</sup>"));
        assert!(!has_letter_script_span("plain 12"));
    }

    #[test]
    fn push_cell_item_attaches_script_runs_by_geometry() {
        let v = cell("V", 100.0, 6.0, 500.0, 9.0, 0.0);
        let f = cell("f", 106.0, 2.2, 498.5, 6.0, -1.5);
        let unit = cell("m/s", 112.0, 14.0, 500.0, 9.0, 0.0);
        let mut text = String::new();
        let mut last = None;
        for it in [&v, &f, &unit] {
            push_cell_item(&mut text, &mut last, it, it.text.trim());
        }
        assert_eq!(text, "V<sub>f</sub> m/s");
    }
}
