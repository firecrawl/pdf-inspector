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
//! Restoration happens at two levels, because visual order is imposed twice.
//!
//! **Within an item** (`restore_item_text`), at extraction time — the only
//! level where the invariant holds unconditionally, since a show operation is
//! always in paint order. Two refinements here:
//!
//! 1. **No bracket mirroring.** ToUnicode maps mirrored glyphs back to their
//!    logical characters, so visual-order text already carries logical
//!    bracket codepoints in reversed positions — plain positional reversal
//!    restores them, and applying rule L4 (as `python-bidi` does) would
//!    break them. A conservative repair pass handles the rare writer that
//!    stores screen-form brackets instead: an unmatched closer followed by
//!    an unmatched opener of the same kind is swapped back.
//! 2. **Combining marks** stay attached to their base under reversal, while
//!    Hebrew punctuation interleaved with the points is not treated as
//!    combining.
//!
//! **Across a sequence of items** (`order_rtl_sequence` and its callers) —
//! a line, a table cell, a merged run. Restoring each item is not enough: a
//! left-to-right run whose glyphs land in *separate* items, such as an
//! equation or a Latin phrase, is reversed by the surrounding right-to-left
//! ordering, and no item-level pass can see it because each item is a no-op
//! on its own. So the sequence is reversed as a whole and embedded LTR runs
//! are put back in ascending order.
//!
//! The direction decision always belongs to the **container** — the line,
//! the cell, the run — never to the individual fragment. Deciding per
//! fragment lands an LTR fragment on the wrong side of its RTL neighbours.
//!
//! Finally, **RTL tables are logicalized** in a pass over the rendered
//! Markdown: in a majority-RTL table the grid detector's left-to-right
//! columns are in reversed logical order, so cell order is flipped per row
//! (only when every row has the same cell count, so unescaped `|` characters
//! in cell text never scramble a row, and never inside a code fence).
//!
//! Every entry point gates on the presence of RTL characters (with an ASCII
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
    let mut in_fence = false;
    while i < lines.len() {
        // Code fences are copied through untouched: an extracted source
        // listing can hold pipe-delimited lines that are not a table, and
        // reversing them would corrupt the code.
        if is_code_fence(lines[i]) {
            in_fence = !in_fence;
            out.push(Cow::Borrowed(lines[i]));
            i += 1;
        } else if !in_fence && is_table_row(lines[i]) {
            let start = i;
            while i < lines.len() && is_table_row(lines[i]) && !is_code_fence(lines[i]) {
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

/// A Markdown code-fence delimiter (``` or ~~~), allowing an info string.
fn is_code_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
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

/// Put embedded left-to-right runs back in ascending order after a
/// paint-order sequence has been reversed for a right-to-left container.
///
/// Reversing a sequence is right for the RTL fragments themselves, but it
/// also reverses runs that read left-to-right — an equation split across
/// items, a Latin phrase, a URL. A run is a maximal span of elements
/// carrying no RTL characters, trimmed to the outermost elements that carry
/// a strong LTR letter so that punctuation merely abutting RTL text stays on
/// the side it was painted on.
pub(crate) fn restore_embedded_ltr_runs<T>(
    seq: &mut [T],
    has_rtl: impl Fn(&T) -> bool,
    has_strong_ltr: impl Fn(&T) -> bool,
) {
    let mut i = 0;
    while i < seq.len() {
        if has_rtl(&seq[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < seq.len() && !has_rtl(&seq[i]) {
            i += 1;
        }
        let first = (start..i).find(|&k| has_strong_ltr(&seq[k]));
        let last = (start..i).rev().find(|&k| has_strong_ltr(&seq[k]));
        if let (Some(first), Some(last)) = (first, last) {
            if last > first {
                seq[first..=last].reverse();
            }
        }
    }
}

/// Reorder a paint-order (ascending-X) sequence into right-to-left logical
/// reading order, keeping embedded left-to-right runs readable.
pub(crate) fn order_rtl_sequence<T>(
    seq: &mut [T],
    has_rtl: impl Fn(&T) -> bool,
    has_strong_ltr: impl Fn(&T) -> bool,
) {
    seq.reverse();
    restore_embedded_ltr_runs(seq, has_rtl, has_strong_ltr);
}

fn text_has_rtl(text: &&str) -> bool {
    text.chars().any(is_rtl_char)
}

fn text_has_strong_ltr(text: &&str) -> bool {
    text.chars().any(char::is_alphabetic)
}

/// Collects a table cell's fragments in paint (ascending-X) order and renders
/// them in logical reading order once the cell is complete.
///
/// The direction decision belongs to the cell, not to the individual
/// fragment. In an RTL cell the whole fragment sequence reverses, so an LTR
/// fragment sitting between two RTL ones keeps its position in the sequence
/// and only the sequence order flips. Deciding per fragment — prepend when
/// the fragment is RTL, append otherwise — lands such a fragment on the wrong
/// side of its neighbours.
#[derive(Default, Clone, Debug)]
pub(crate) struct CellText {
    fragments: Vec<String>,
}

impl CellText {
    pub(crate) fn push(&mut self, text: &str) {
        self.fragments.push(text.to_string());
    }

    /// Render the cell. The LTR path folds fragments exactly as the original
    /// `push(' ')`/`push_str` pattern did, including its handling of empty
    /// fragments, so LTR documents stay byte-identical.
    pub(crate) fn finish(&self) -> String {
        let mut ordered: Vec<&str> = self.fragments.iter().map(String::as_str).collect();
        if crate::text_utils::is_rtl_text(ordered.iter()) {
            order_rtl_sequence(&mut ordered, text_has_rtl, text_has_strong_ltr);
        }
        let mut out = String::new();
        for fragment in ordered {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(fragment);
        }
        out
    }
}

/// Put each visual line of a table cell into reading order.
///
/// Items must already be in paint order — top-to-bottom, then left-to-right.
/// Direction is decided per line rather than per cell, because one cell can
/// hold a Hebrew line and a Latin line, and reversing the whole cell because
/// its Hebrew line holds the majority would scramble the Latin one.
pub(crate) fn order_cell_lines(items: &mut [(usize, &crate::types::TextItem)]) {
    let mut start = 0;
    while start < items.len() {
        let y = items[start].1.y;
        let mut end = start + 1;
        while end < items.len() && (items[end].1.y - y).abs() < items[end].1.font_size * 0.5 {
            end += 1;
        }
        let line = &mut items[start..end];
        if crate::text_utils::is_rtl_text(line.iter().map(|(_, item)| &item.text)) {
            order_rtl_sequence(
                line,
                |(_, item)| item.text.chars().any(is_rtl_char),
                |(_, item)| item.text.chars().any(char::is_alphabetic),
            );
        }
        start = end;
    }
}

/// Join a paint-order run of item fragments into logical reading order.
///
/// Each fragment carries whether a word gap separates it from the previous
/// one. Fragments that touch are pieces of a single word, so they are joined
/// first — reordering must never split a word — and only the resulting word
/// sequence is put into reading order. As with cells, the direction belongs
/// to the run, not to the individual fragment.
pub(crate) fn join_run_fragments(fragments: &[(String, bool)]) -> String {
    let mut words: Vec<String> = Vec::new();
    for (text, word_gap) in fragments {
        match words.last_mut() {
            // Pieces of one word: an RTL word's pieces are painted
            // left-to-right, so a later piece precedes the earlier one.
            Some(word) if !word_gap => {
                if joins_right_to_left(text) {
                    let mut merged = String::with_capacity(text.len() + word.len());
                    merged.push_str(text);
                    merged.push_str(word);
                    *word = merged;
                } else {
                    word.push_str(text);
                }
            }
            _ => words.push(text.clone()),
        }
    }

    let mut ordered: Vec<&str> = words.iter().map(String::as_str).collect();
    if crate::text_utils::is_rtl_text(ordered.iter()) {
        order_rtl_sequence(&mut ordered, text_has_rtl, text_has_strong_ltr);
    }
    ordered.join(" ")
}

/// Render a grid of accumulated cells.
pub(crate) fn finish_cells(cells: &[Vec<CellText>]) -> Vec<Vec<String>> {
    cells
        .iter()
        .map(|row| row.iter().map(CellText::finish).collect())
        .collect()
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

#[cfg(test)]
mod mixed_direction {
    use super::*;
    use crate::types::{ItemType, TextItem};

    const HEB_A: &str = "\u{5D0}\u{5D1}\u{5D2}"; // אבג
    const HEB_B: &str = "\u{5D3}\u{5D4}\u{5D5}"; // דהו
    const HEB_LONG: &str = "\u{5D1}\u{5E8}\u{5D0}\u{5E9}\u{5D9}\u{5EA}"; // בראשית
                                                                         // Eight letters — outvotes the seven of "New York".
    const HEB_MAJORITY: &str = "\u{5D1}\u{5E8}\u{5D0}\u{5E9}\u{5D9}\u{5EA}\u{5D0}\u{5D1}";

    fn item(text: &str, x: f32, y: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width: 10.0,
            height: 12.0,
            font: "F1".into(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    /// An RTL cell whose fragments alternate direction: the sequence reverses
    /// as a whole, so the Latin fragment keeps its place between its Hebrew
    /// neighbours rather than being pushed to one end.
    #[test]
    fn rtl_cell_keeps_latin_between_hebrew() {
        let mut cell = CellText::default();
        cell.push(HEB_A); // x = 10
        cell.push("Latin"); // x = 50
        cell.push(HEB_B); // x = 90
        assert_eq!(cell.finish(), format!("{HEB_B} Latin {HEB_A}"));
    }

    /// A multi-item Latin phrase inside an RTL cell keeps its own word order.
    #[test]
    fn rtl_cell_keeps_multi_item_latin_phrase_readable() {
        // Hebrew long enough to hold the strict letter majority against the
        // seven Latin letters of "New York".
        let mut cell = CellText::default();
        cell.push("New"); // x = 10
        cell.push("York"); // x = 40
        cell.push(HEB_MAJORITY); // x = 90
        assert_eq!(cell.finish(), format!("{HEB_MAJORITY} New York"));
    }

    /// An LTR cell is joined exactly as the pre-RTL code did, empty fragments
    /// included, so LTR documents stay byte-identical.
    #[test]
    fn ltr_cell_join_is_unchanged() {
        let mut cell = CellText::default();
        cell.push("alpha");
        cell.push("");
        cell.push("beta");
        assert_eq!(cell.finish(), "alpha  beta");
    }

    /// A cell holding a Hebrew line above a Latin line must reverse only the
    /// Hebrew one — deciding direction for the whole cell scrambles the Latin.
    #[test]
    fn mixed_direction_cell_orders_each_line_separately() {
        let hebrew_line_y = 100.0;
        let latin_line_y = 80.0;
        let binding = [
            item(HEB_A, 10.0, hebrew_line_y),
            item(HEB_B, 40.0, hebrew_line_y),
            item("Hello", 10.0, latin_line_y),
            item("World", 40.0, latin_line_y),
        ];
        let mut items: Vec<(usize, &TextItem)> = binding.iter().enumerate().collect();
        order_cell_lines(&mut items);
        let order: Vec<&str> = items.iter().map(|(_, i)| i.text.as_str()).collect();
        // Hebrew line reverses; the Latin line below it does not.
        assert_eq!(order, vec![HEB_B, HEB_A, "Hello", "World"]);
    }

    /// Pieces of one word touch (no word gap) and must be reassembled before
    /// any reordering, so a split RTL word is never turned inside out.
    #[test]
    fn run_fragments_reassemble_split_rtl_word_then_order() {
        // בראשית split across two show operations. The logical tail ית is
        // painted leftmost, so it arrives first in ascending-X order and the
        // piece to its right must precede it once reassembled.
        let fragments = vec![
            ("\u{5D9}\u{5EA}".into(), false),               // ית
            ("\u{5D1}\u{5E8}\u{5D0}\u{5E9}".into(), false), // בראש
            ("Tail".into(), true),
        ];
        assert_eq!(join_run_fragments(&fragments), format!("Tail {HEB_LONG}"));
    }

    /// An LTR run joins in paint order with spaces only at word gaps.
    #[test]
    fn run_fragments_ltr_join_is_unchanged() {
        let fragments = vec![
            ("Hel".into(), false),
            ("lo".into(), false),
            ("World".into(), true),
        ];
        assert_eq!(join_run_fragments(&fragments), "Hello World");
    }

    /// Pipe-delimited lines inside a fenced code block are not a table and
    /// must survive untouched.
    #[test]
    fn fenced_code_block_is_not_treated_as_a_table() {
        let markdown = format!("```\n|{HEB_A}|{HEB_B}|\n|{HEB_A}|{HEB_B}|\n```\n");
        assert_eq!(restore_visual_order(markdown.clone()), markdown);
    }

    /// The same lines outside a fence are a table and do get logicalized.
    #[test]
    fn table_outside_a_fence_still_reverses() {
        let markdown = format!("|{HEB_A}|{HEB_B}|\n|{HEB_A}|{HEB_B}|\n");
        let restored = restore_visual_order(markdown.clone());
        assert_ne!(restored, markdown);
        assert!(restored.starts_with(&format!("|{HEB_B}|{HEB_A}|")));
    }

    /// Longer Hebrew than Latin, so the line holds the RTL majority: the
    /// embedded equation still reads left-to-right.
    #[test]
    fn embedded_equation_survives_line_ordering() {
        let mut items = vec![
            item(HEB_LONG, 300.0, 700.0),
            item("x", 100.0, 700.0),
            item("\u{2208}", 130.0, 700.0), // ∈
            item("G", 160.0, 700.0),
        ];
        crate::text_utils::sort_line_items(&mut items);
        let order: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(order, vec![HEB_LONG, "x", "\u{2208}", "G"]);
    }
}
