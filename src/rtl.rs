//! Visual→logical restoration for RTL scripts (Hebrew, Arabic).
//!
//! PDF content streams store text in *visual* order: glyphs appear in each
//! show operation in the same left-to-right sequence they are painted on the
//! page. For RTL scripts that means extracted strings arrive
//! character-reversed relative to logical reading order — `תורת המספרים`
//! extracts as `םירפסמה תרות`.
//!
//! The Unicode bidi algorithm (UAX #9) maps logical order to visual order.
//! On the plain-text runs found in PDFs its L2 reordering is an involution:
//! running it once more over visual-order text, with the paragraph direction
//! the original layout used, restores logical order.
//!
//! The restoration runs **per text item**, at extraction time, because that
//! is the only level where the invariant holds unconditionally: a show
//! operation is always in paint order. Line assembly already orders items
//! logically (RTL lines sort right-to-left), so once each item is restored,
//! every downstream consumer — joining, headings, lists, table cells, links
//! — operates on logical text natively. Two refinements:
//!
//! 1. **No bracket mirroring.** ToUnicode maps mirrored glyphs back to their
//!    logical characters, so visual-order text already carries logical
//!    bracket codepoints in reversed positions — plain positional reversal
//!    restores them, and applying rule L4 (as `python-bidi` does) would
//!    break them. A conservative repair pass handles the rare writer that
//!    stores screen-form brackets instead: an unmatched closer followed by
//!    an unmatched opener of the same kind is swapped back.
//! 2. **RTL tables are logicalized** in a final pass over the rendered
//!    Markdown: in a majority-RTL table the grid detector's left-to-right
//!    columns are in reversed logical order, so cell order is flipped per
//!    row (only when every row has the same cell count, so unescaped `|`
//!    characters in cell text never scramble a row).
//!
//! Both entry points gate on the presence of RTL characters (with an ASCII
//! fast path), so LTR documents are byte-identical and pay one scan.

use std::borrow::Cow;

use unicode_bidi::{Level, ParagraphBidiInfo};

use crate::text_utils::{is_cjk_char, is_rtl_char};

/// Restore one extracted text item from visual (paint) order to logical
/// reading order. Returns the input unchanged unless it contains RTL
/// characters.
///
/// Must not be applied to text that is already logical (e.g. `ActualText`
/// replacements, which the PDF spec defines in logical content order).
pub(crate) fn restore_item_text(text: String) -> String {
    if text.is_ascii() || !text.chars().any(is_rtl_char) {
        return text;
    }

    // The paragraph direction the original layout used, estimated the same
    // way the extractor classifies line direction: RTL on a strict letter
    // majority. Latin-majority items keep LTR run order and only embedded
    // RTL runs reverse in place.
    let base = if is_rtl_majority(&mut text.chars()) {
        Level::rtl()
    } else {
        Level::ltr()
    };

    let bidi = ParagraphBidiInfo::new(&text, Some(base));
    let (levels, runs) = bidi.visual_runs(0..text.len());
    let mut reordered = String::with_capacity(text.len());
    for run in runs {
        if levels[run.start].is_rtl() {
            push_reversed(&mut reordered, &text[run]);
        } else {
            reordered.push_str(&text[run]);
        }
    }

    repair_inverted_brackets(reordered)
}

/// Reverse column order of majority-RTL Markdown tables so it matches
/// logical reading order. Cell text is already logical (restored per item);
/// only the per-row cell sequence is touched. No-op scan for documents
/// without RTL characters.
pub(crate) fn restore_visual_order(markdown: String) -> String {
    if markdown.is_ascii() || !markdown.chars().any(is_rtl_char) {
        return markdown;
    }

    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out: Vec<Cow<str>> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if is_table_row(lines[i]) {
            let start = i;
            while i < lines.len() && is_table_row(lines[i]) {
                i += 1;
            }
            process_table_block(&lines[start..i], &mut out);
        } else {
            out.push(Cow::Borrowed(lines[i]));
            i += 1;
        }
    }
    out.join("\n")
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// True when `text` is RTL-majority: such fragments read right-to-left, so
/// joins that iterate in ascending-X (paint) order must prepend rather than
/// append them. ASCII fast path keeps LTR-only callers cost-free.
pub(crate) fn joins_right_to_left(text: &str) -> bool {
    !text.is_ascii() && is_rtl_majority(&mut text.chars())
}

/// Append an item's text to a table cell buffer. Cell fills iterate items in
/// ascending-X (paint) order; for RTL-majority item text that is reversed
/// logical order, so such items are prepended instead, reconstructing
/// right-to-left reading order. The LTR path is byte-identical to the
/// original `push(' ')`/`push_str` pattern at every call site.
pub(crate) fn append_cell_text(cell: &mut String, text: &str) {
    if !cell.is_empty() && joins_right_to_left(text) {
        let mut swapped = String::with_capacity(text.len() + cell.len() + 1);
        swapped.push_str(text);
        swapped.push(' ');
        swapped.push_str(cell);
        *cell = swapped;
    } else {
        if !cell.is_empty() {
            cell.push(' ');
        }
        cell.push_str(text);
    }
}

/// Reverse the cell order of every row in a majority-RTL table block. Only
/// fires when the block is structurally uniform: unescaped `|` characters in
/// cell text make row lengths differ, and reversing such rows would scramble
/// them.
fn process_table_block<'a>(rows: &[&'a str], out: &mut Vec<Cow<'a, str>>) {
    let split_rows: Vec<Vec<&str>> = rows.iter().map(|r| split_cells(r)).collect();
    let uniform = split_rows.windows(2).all(|w| w[0].len() == w[1].len());
    let reverse_columns =
        rows.len() >= 2 && uniform && is_rtl_majority(&mut rows.iter().flat_map(|r| r.chars()));

    if !reverse_columns {
        out.extend(rows.iter().map(|r| Cow::Borrowed(*r)));
        return;
    }

    for mut cells in split_rows {
        if cells.len() > 3 {
            // `|a|b|` splits to ["", "a", "b", ""]: reverse the interior only.
            let last = cells.len() - 1;
            cells[1..last].reverse();
        }
        out.push(Cow::Owned(cells.join("|")));
    }
}

/// Split a table row on `|`, honoring the `\|` escape used for literal pipes.
fn split_cells(row: &str) -> Vec<&str> {
    let bytes = row.as_bytes();
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == b'|' {
            cells.push(&row[start..i]);
            start = i + 1;
        }
    }
    cells.push(&row[start..]);
    cells
}

/// Same convention as `text_utils::is_rtl_text`: RTL wins only a strict
/// majority over non-CJK LTR letters, mirroring how the extractor itself
/// decides line direction.
fn is_rtl_majority(chars: &mut dyn Iterator<Item = char>) -> bool {
    let (mut rtl, mut ltr) = (0u32, 0u32);
    for c in chars {
        if is_rtl_char(c) {
            rtl += 1;
        } else if c.is_alphabetic() && !is_cjk_char(c) {
            ltr += 1;
        }
    }
    rtl > 0 && rtl > ltr
}

/// Combining marks that must stay attached to the preceding base character
/// when a run is reversed (Hebrew points/accents, Arabic harakat, generic
/// combining diacritics). The Hebrew ranges skip the non-combining
/// punctuation interleaved with the points: maqaf (05BE), paseq (05C0),
/// sof pasuq (05C3), and nun hafukha (05C6) are ordinary class-R characters.
fn is_combining_mark(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'
        | '\u{05C1}'..='\u{05C2}'
        | '\u{05C4}'..='\u{05C5}'
        | '\u{05C7}'
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E4}'
        | '\u{06E7}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{FB1E}'
    )
}

/// Append `run` reversed, keeping base+combining-mark clusters intact and
/// deliberately not mirroring brackets (see module docs).
fn push_reversed(out: &mut String, run: &str) {
    let mut cluster_starts: Vec<usize> = run
        .char_indices()
        .filter(|&(i, c)| i == 0 || !is_combining_mark(c))
        .map(|(i, _)| i)
        .collect();
    cluster_starts.push(run.len());
    for pair in cluster_starts.windows(2).rev() {
        out.push_str(&run[pair[0]..pair[1]]);
    }
}

/// Repair inverted bracket pairs left by writers that store screen-form
/// (mirrored) bracket glyphs: after reversal those surface as an unmatched
/// closer appearing before an unmatched opener of the same kind — a pattern
/// well-formed text never produces. Each such orphan pair is swapped back.
fn repair_inverted_brackets(text: String) -> String {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    if !text.contains(|c| PAIRS.iter().any(|&(o, cl)| c == o || c == cl)) {
        return text;
    }

    let mut chars: Vec<char> = text.chars().collect();
    let mut changed = false;
    for &(open, close) in &PAIRS {
        let mut stack: Vec<usize> = Vec::new();
        let mut orphan_closers: Vec<usize> = Vec::new();
        let mut orphan_openers: Vec<usize> = Vec::new();
        for (i, &c) in chars.iter().enumerate() {
            if c == open {
                stack.push(i);
            } else if c == close && stack.pop().is_none() {
                orphan_closers.push(i);
            }
        }
        orphan_openers.extend(stack);

        let mut openers = orphan_openers.into_iter().peekable();
        for closer in orphan_closers {
            while openers.peek().is_some_and(|&o| o < closer) {
                openers.next();
            }
            if let Some(opener) = openers.next() {
                chars[closer] = open;
                chars[opener] = close;
                changed = true;
            }
        }
    }

    if changed {
        chars.into_iter().collect()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate PDF visual-order storage of a logical string: full UAX #9
    /// logical→visual reordering with the given base direction, without
    /// glyph mirroring and with combining marks staying after their base —
    /// matching how ToUnicode-decoded text extracts from real writers.
    fn to_visual(logical: &str, rtl_base: bool) -> String {
        let base = if rtl_base { Level::rtl() } else { Level::ltr() };
        let bidi = ParagraphBidiInfo::new(logical, Some(base));
        let (levels, runs) = bidi.visual_runs(0..logical.len());
        let mut out = String::new();
        for run in runs {
            if levels[run.start].is_rtl() {
                push_reversed(&mut out, &logical[run]);
            } else {
                out.push_str(&logical[run]);
            }
        }
        out
    }

    fn restore(s: &str) -> String {
        restore_item_text(s.to_string())
    }

    #[test]
    fn ascii_item_passes_through_unchanged() {
        assert_eq!(restore("Hello, world 42%"), "Hello, world 42%");
    }

    #[test]
    fn pure_hebrew_item_is_reversed() {
        assert_eq!(restore("םירפסמה תרות"), "תורת המספרים");
    }

    #[test]
    fn involution_recovers_logical_order() {
        for logical in [
            "שלום עולם",
            "פרסום מיוחד דצמבר 2020",
            "המחיר הוא 42.5 אחוז",
            "ראה סעיף 10.4 להלן",
            "טל׳ 03-6400400",
        ] {
            let visual = to_visual(logical, true);
            assert_eq!(restore(&visual), logical, "failed for {logical:?}");
        }
    }

    #[test]
    fn latin_majority_item_keeps_run_order() {
        // Majority-LTR item: English stays in place, Hebrew run restored.
        let logical = "see תורת המספרים for details";
        let visual = to_visual(logical, false);
        assert_eq!(restore(&visual), logical);
    }

    #[test]
    fn tab_separated_words_restore() {
        // Word RTL export puts literal tabs between words inside one show op.
        assert_eq!(restore("\tתעה\tבתכ"), "כתב\tהעת\t");
    }

    #[test]
    fn unmirrored_writer_brackets_restore_by_reversal() {
        // Word-style writers store logical bracket codepoints in visual
        // positions: `(מיל׳)` arrives as `)׳לימ(`.
        assert_eq!(restore("ןורב יתיא )׳לימ( ל״את"), "תא״ל (מיל׳) איתי ברון");
    }

    #[test]
    fn mirrored_writer_brackets_repaired() {
        // A writer that stores screen forms produces an inverted orphan pair
        // after reversal; the repair pass swaps it back.
        let logical = "תא״ל (מיל׳) איתי ברון";
        let visual: String = to_visual(logical, true)
            .chars()
            .map(|c| match c {
                '(' => ')',
                ')' => '(',
                c => c,
            })
            .collect();
        assert_eq!(restore(&visual), logical);
    }

    #[test]
    fn maqaf_is_not_glued_to_previous_letter() {
        // U+05BE maqaf is punctuation, not a combining mark: `רב־תחומי`
        // must round-trip exactly.
        assert_eq!(restore("ימוחת־בר"), "רב־תחומי");
    }

    #[test]
    fn hebrew_points_stay_attached_under_reversal() {
        // Letters with niqqud: marks must follow their base after reversal.
        let logical = "\u{05D1}\u{05B0}\u{05E8}\u{05B5}\u{05D0}";
        let visual = to_visual(logical, true);
        assert_eq!(restore(&visual), logical);
    }

    #[test]
    fn ltr_document_markdown_passes_through_unchanged() {
        let doc = "# Title\n\nHello world.\n\n|a|b|\n|---|---|\n|1|2|\n";
        assert_eq!(restore_visual_order(doc.to_string()), doc);
    }

    #[test]
    fn rtl_table_columns_logicalized() {
        // Cell text is already logical (item-level restoration); only the
        // column order flips.
        let table = "|4|כללי|10.4|\n|---|---|---|\n|8|גגות|10.3|";
        let fixed = restore_visual_order(table.to_string());
        assert_eq!(fixed, "|10.4|כללי|4|\n|---|---|---|\n|10.3|גגות|8|");
    }

    #[test]
    fn ragged_rtl_table_keeps_column_order() {
        // Unescaped pipes in cell text make row lengths differ; column
        // reversal must not fire.
        let table = "|שלום|a|b|\n|---|---|\n|x|y|";
        assert_eq!(restore_visual_order(table.to_string()), table);
    }

    #[test]
    fn ltr_table_with_a_hebrew_cell_keeps_columns() {
        let table = "|Name|Value|\n|---|---|\n|שלום|42|";
        assert_eq!(restore_visual_order(table.to_string()), table);
    }

    #[test]
    fn escaped_pipes_do_not_split_cells() {
        let cells = split_cells("|a\\|b|שלום|");
        assert_eq!(cells, vec!["", "a\\|b", "שלום", ""]);
    }
}
