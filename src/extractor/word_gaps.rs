//! Word gaps written as character spacing, and letter gaps written as `TJ`
//! offsets.
//!
//! Some producers carry a word space as character spacing instead of a
//! space glyph: the two glyphs on either side of the boundary are shown as
//! one short string with a `Tc` as wide as a word space (`3 Tc (dt) Tj` for
//! the "d t" of "send to"), and the spacing is taken back where the glyphs
//! must kern together — with a positive `TJ` offset after the string, or by
//! positioning the next run that far before the pen. The string reads "dt",
//! and the line merges into "sendtoMars".
//!
//! Tracked display text is shown with the same wide spacing and must keep
//! its letters together; the geometry inside the string cannot tell the two
//! apart. The producer's next move can: tracking is never taken back. So a
//! string of two or three glyphs (a boundary, plus a one-letter word between
//! two boundaries) whose junction spacing is a word gap by the `TJ` offset
//! threshold is a candidate, and its junctions become spaces only once the
//! spacing after its last glyph is seen to be taken back.
//!
//! Tracked display text is also set as a `TJ` array with one glyph per
//! string and the letter spacing as the offset between them
//! (`[(V) -250 (A) -250 (L) -250 (L) -250 (E) -250 (Y)] TJ`). Judged one
//! offset at a time against the word-gap threshold, every letter becomes a
//! word. The run's own offsets tell letter gaps from word gaps: see
//! [`tj_tracking`].

use lopdf::{Object, StringFormat};

use super::fonts::get_operand_bytes;
use super::{get_number, is_spaceless_cjk, multiply_matrices};
use crate::text_utils::{expand_ligatures, is_rtl_char};
use crate::types::{FontWidthInfo, TextItem};

/// Glyphs in the longest string that carries word gaps this way: the last
/// glyph of one word, the first of the next, and a one-letter word between
/// two boundaries. A longer string with wide spacing is tracked text.
const MAX_BOUNDARY_GLYPHS: usize = 3;

/// Kerning allowance, in em, when matching the travel that takes the spacing
/// back against the spacing itself.
const KERN_ALLOWANCE_EM: f32 = 0.12;

/// Least share of the spacing a take-back must cover. Producers that take
/// the spacing back do so in full, less an intra-word kern; a kern alone,
/// between tracked glyphs, stays well under this.
const TAKE_BACK_MIN: f32 = 0.7;

/// `TJ` offset, in thousandths of the font size, beyond which pen travel
/// between two runs is a word space: 0.4 of the font's space width, at
/// least 0.08 em, and 0.12 em for a font without metrics.
pub(crate) fn word_gap_threshold(font_info: Option<&FontWidthInfo>) -> f32 {
    match font_info {
        Some(font_info) => {
            let space_em = font_info.space_width as f32 * font_info.units_scale;
            (space_em * 1000.0 * 0.4).max(80.0)
        }
        None => 120.0,
    }
}

/// Whether `spacing_ts` — the character spacing after a glyph shown at
/// `font_size`, plus the word spacing after a space code — is a word gap by
/// `threshold`, the way the `TJ` offset carrying the same travel would be.
/// A negative `Tf` size reads backwards and never qualifies.
pub(crate) fn spacing_is_word_gap(spacing_ts: f32, font_size: f32, threshold: f32) -> bool {
    font_size > 0.0 && spacing_ts / font_size * 1000.0 > threshold
}

/// Whether pen travel of `taken_back_ts` against the reading direction, right
/// after a glyph followed by `spacing_ts`, takes that spacing back: most of
/// it, and no more than the spacing plus a kerning allowance.
pub(crate) fn spacing_taken_back(taken_back_ts: f32, spacing_ts: f32, font_size: f32) -> bool {
    spacing_ts > 0.0
        && taken_back_ts >= spacing_ts * TAKE_BACK_MIN
        && taken_back_ts <= spacing_ts + font_size.abs() * KERN_ALLOWANCE_EM
}

/// Whether `element`, the `TJ` array element after a candidate string, is a
/// positive offset that takes the spacing after the string's last glyph
/// back.
pub(crate) fn offset_takes_spacing_back(element: &Object, spacing_ts: f32, font_size: f32) -> bool {
    get_number(element).is_some_and(|offset| {
        offset > 0.0 && spacing_taken_back(offset / 1000.0 * font_size, spacing_ts, font_size)
    })
}

/// Whether `code` is the space code (32), the one the word spacing applies
/// to in the width formula.
fn is_space_code(code: &[u8]) -> bool {
    matches!(code, [0x20] | [0x00, 0x20])
}

/// Spacing the pen adds after `code`: the character spacing, plus the word
/// spacing after the space code.
fn spacing_after(code: &[u8], char_spacing: f32, word_spacing: f32) -> f32 {
    char_spacing
        + if is_space_code(code) {
            word_spacing
        } else {
            0.0
        }
}

/// Whether a word-gap junction between two glyphs takes a space: not next
/// to a space glyph, not inside Han/Kana text, which never spaces between
/// glyphs, not inside a right-to-left run, whose order and spacing the RTL
/// stages decide, and never before joining punctuation.
fn junction_takes_space(prev_last: Option<char>, next_first: Option<char>) -> bool {
    let (Some(prev), Some(next)) = (prev_last, next_first) else {
        return false;
    };
    if prev.is_whitespace() || next.is_whitespace() {
        return false;
    }
    if is_spaceless_cjk(prev) || is_spaceless_cjk(next) || is_rtl_char(prev) || is_rtl_char(next) {
        return false;
    }
    !matches!(
        next,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '%' | '\u{2019}' | '\u{201D}' | '»'
    )
}

/// A short string whose glyph junctions are word gaps by their character
/// spacing, waiting for the spacing after its last glyph to be taken back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WordGapCandidate {
    /// The string's text with a space at each word-gap junction.
    pub(crate) spaced_text: String,
    /// Spacing after the last glyph, in unscaled text-space units.
    pub(crate) trailing_spacing_ts: f32,
}

/// Read `raw`, shown with `char_spacing` and `word_spacing` at `font_size`,
/// as a word-gap candidate. `text` is the string's own decode and `decode`
/// decodes one code: the candidate stands only when every code decodes, and
/// the codes decode one at a time to the same text, so a decoder that reads
/// the string as a whole (UTF-16, a CMap choice still being sampled) or
/// drops a code keeps its item. Word spacing
/// counts only after a space code that paints a glyph: after a space glyph
/// it merely widens a gap the text already carries.
#[allow(clippy::too_many_arguments)]
pub(crate) fn word_gap_candidate(
    raw: &[u8],
    text: &str,
    font_info: Option<&FontWidthInfo>,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    threshold: f32,
    mut decode: impl FnMut(&Object) -> Option<(String, bool)>,
) -> Option<WordGapCandidate> {
    let code_len = if font_info.is_some_and(|font_info| font_info.is_cid) {
        2
    } else {
        1
    };
    if font_size <= 0.0 || !raw.len().is_multiple_of(code_len) {
        return None;
    }
    let codes: Vec<&[u8]> = raw.chunks_exact(code_len).collect();
    if !(2..=MAX_BOUNDARY_GLYPHS).contains(&codes.len()) {
        return None;
    }
    // The spacing after a glyph, with the word spacing only after a space
    // code whose glyph paints — a space glyph's gap is in the text already.
    let is_gap = |code: &[u8], paints: bool| {
        let spacing = if is_space_code(code) && !paints {
            char_spacing
        } else {
            spacing_after(code, char_spacing, word_spacing)
        };
        spacing_is_word_gap(spacing, font_size, threshold)
    };
    // A quick look before decoding code by code, taking every glyph as
    // painting; the spacing after the last glyph is the take-back's
    // business, not a junction.
    if !codes[..codes.len() - 1]
        .iter()
        .any(|code| is_gap(code, true))
    {
        return None;
    }

    let labels: Vec<String> = codes
        .iter()
        .map(|code| {
            decode(&Object::String(code.to_vec(), StringFormat::Literal)).map(|(label, _)| label)
        })
        .collect::<Option<_>>()?;
    if labels.concat() != text {
        return None;
    }

    let mut spaced_text = String::new();
    let mut spaces = 0usize;
    for (index, label) in labels.iter().enumerate() {
        spaced_text.push_str(label);
        let Some(next) = labels.get(index + 1) else {
            continue;
        };
        let paints = !label.chars().all(char::is_whitespace);
        if is_gap(codes[index], paints)
            && junction_takes_space(label.chars().last(), next.chars().next())
        {
            spaced_text.push(' ');
            spaces += 1;
        }
    }
    (spaces > 0).then(|| WordGapCandidate {
        spaced_text,
        trailing_spacing_ts: spacing_after(codes[codes.len() - 1], char_spacing, word_spacing),
    })
}

/// A candidate shown as one item, kept until the next run in the same
/// content stream shows whether the spacing after it was taken back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingWordGaps {
    /// Index of the candidate's item — the last one when it was shown.
    item: usize,
    spaced_text: String,
    /// The pen after the candidate, in device space.
    pen: (f32, f32),
    /// Device displacement of one unscaled text-space unit along the
    /// candidate's baseline.
    unit: (f32, f32),
    spacing_ts: f32,
    font_size: f32,
}

impl PendingWordGaps {
    /// The state for a string just shown as item `item`, when it is a
    /// candidate (see [`word_gap_candidate`]): `pen_tm` is the text matrix
    /// after the string, and `decode` decodes one of its codes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_shown_string(
        item: usize,
        raw: &[u8],
        text: &str,
        font_info: Option<&FontWidthInfo>,
        font_size: f32,
        char_spacing: f32,
        word_spacing: f32,
        pen_tm: &[f32; 6],
        ctm: &[f32; 6],
        horizontal_scale: f32,
        decode: impl FnMut(&Object) -> Option<(String, bool)>,
    ) -> Option<Self> {
        let candidate = word_gap_candidate(
            raw,
            text,
            font_info,
            font_size,
            char_spacing,
            word_spacing,
            word_gap_threshold(font_info),
            decode,
        )?;
        Some(Self::new(
            item,
            expand_ligatures(&candidate.spaced_text),
            pen_tm,
            ctm,
            horizontal_scale,
            candidate.trailing_spacing_ts,
            font_size,
        ))
    }

    /// `pen_tm` is the text matrix after the candidate was shown; `unit`
    /// follows from its linear part and the horizontal scaling.
    pub(crate) fn new(
        item: usize,
        spaced_text: String,
        pen_tm: &[f32; 6],
        ctm: &[f32; 6],
        horizontal_scale: f32,
        spacing_ts: f32,
        font_size: f32,
    ) -> Self {
        let pen = multiply_matrices(pen_tm, ctm);
        Self {
            item,
            spaced_text,
            pen: (pen[4], pen[5]),
            unit: (pen[0] * horizontal_scale, pen[1] * horizontal_scale),
            spacing_ts,
            font_size,
        }
    }

    /// The next run is about to be shown with its first painted glyph at
    /// `next_tm`: when it starts where the candidate's trailing spacing was
    /// taken back, the candidate's gaps were word spaces. A run at the pen
    /// or beyond it leaves the candidate as it is — tracking going on, or
    /// a word gap of its own — and so does anything shown in between (an
    /// image placeholder, a form's text).
    pub(crate) fn resolve(self, items: &mut [TextItem], next_tm: &[f32; 6], ctm: &[f32; 6]) {
        if self.item + 1 != items.len() {
            return;
        }
        let next = multiply_matrices(next_tm, ctm);
        let (dx, dy) = (next[4] - self.pen.0, next[5] - self.pen.1);
        let (ux, uy) = self.unit;
        let scale = ux * ux + uy * uy;
        if scale <= 0.0 {
            return;
        }
        // Travel in unscaled text-space units: along the baseline, and
        // across it — another line, or a script run.
        let along = (dx * ux + dy * uy) / scale;
        let across = (dx * uy - dy * ux) / scale;
        if across.abs() > self.font_size.abs() * 0.2 {
            return;
        }
        if spacing_taken_back(-along, self.spacing_ts, self.font_size) {
            items[self.item].text = self.spaced_text;
        }
    }
}

/// Least typical letter gap, as a share of the word-gap threshold, for a
/// `TJ` array to read as tracked: below it the offsets are kerning, and the
/// thresholds apply to them as they are.
const TRACKING_MIN: f32 = 0.5;

/// Most typical letter gap, as a share of the word-gap threshold, for a
/// `TJ` array to read as tracked — about 1.2 space widths. Wider offsets
/// between single glyphs are word or column spacing.
const TRACKING_MAX: f32 = 3.0;

/// Typical letter gap, as a share of the word-gap threshold, from which a
/// glyph-per-string array is as likely a run of one-letter words as a
/// tracked word — about 0.75 of a space width. From there tracking is read
/// only in capitals, digits, punctuation and Han/Kana: the display
/// convention such tracking follows.
const TRACKING_NEEDS_CAPITALS: f32 = 1.875;

/// Whether `raw` shows exactly one glyph: one code, two bytes per code for
/// CID fonts.
fn is_single_glyph(raw: &[u8], font_info: Option<&FontWidthInfo>) -> bool {
    let code_len = if font_info.is_some_and(|font_info| font_info.is_cid) {
        2
    } else {
        1
    };
    raw.len() == code_len
}

/// Lower median: the middle value, or the lower of the two middle values.
fn lower_median(sorted: &[f32]) -> f32 {
    sorted[(sorted.len() - 1) / 2]
}

/// Tracking, in thousandths of the font size, read from a `TJ` array that
/// shows tracked display text: one glyph per string, with the letter
/// spacing as the offset between strings. Judged one at a time against the
/// word-gap threshold, such offsets make a word of every letter; the caller
/// judges each offset over the tracking instead, so the letter gaps stay
/// inside the word and only a gap wider by a word gap ends it.
///
/// The run's own offsets tell letter gaps from word gaps: the letter gaps
/// cluster around one value and a word gap stands a space width above the
/// cluster. Gaps above the lower median by more than `space_threshold` are
/// word gaps; the tracking is the median of the rest.
///
/// `None` — the offsets are read as they are — when the array is not such a
/// run: a string of two glyphs or more (words, or kerned pairs), fewer than
/// two junctions, or a typical gap under [`TRACKING_MIN`] (kerning) or over
/// [`TRACKING_MAX`] (word and column spacing) thresholds. A typical gap of
/// [`TRACKING_NEEDS_CAPITALS`] thresholds or more is ambiguous with a run of
/// one-letter words and counts as tracking only when every glyph is a
/// capital, a digit, punctuation or Han/Kana; `decode` decodes one string
/// element and is consulted only then. Offsets are read the same way at a
/// negative `Tf` size, as the thresholds are.
pub(crate) fn tj_tracking(
    array: &[Object],
    font_info: Option<&FontWidthInfo>,
    space_threshold: f32,
    mut decode: impl FnMut(&Object) -> Option<(String, bool)>,
) -> Option<f32> {
    // Junction gaps between consecutive glyph strings, positive when the
    // offset widens the gap (a negative `TJ` number moves the pen on).
    let mut gaps: Vec<f32> = Vec::new();
    let mut strings: Vec<&Object> = Vec::new();
    let mut pending = 0.0f32;
    for element in array {
        if let Some(offset) = get_number(element) {
            pending -= offset;
            continue;
        }
        let Some(raw) = get_operand_bytes(element) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        if !is_single_glyph(raw, font_info) {
            return None;
        }
        if !strings.is_empty() {
            gaps.push(pending);
        }
        pending = 0.0;
        strings.push(element);
    }
    if gaps.len() < 2 {
        return None;
    }
    let mut sorted = gaps;
    sorted.sort_by(|a, b| a.total_cmp(b));
    let seed = lower_median(&sorted);
    let letter_gaps: Vec<f32> = sorted
        .iter()
        .copied()
        .filter(|gap| *gap <= seed + space_threshold)
        .collect();
    let tracking = lower_median(&letter_gaps);
    if tracking < space_threshold * TRACKING_MIN || tracking > space_threshold * TRACKING_MAX {
        return None;
    }
    if tracking >= space_threshold * TRACKING_NEEDS_CAPITALS {
        for element in strings {
            let (text, _) = decode(element)?;
            if text
                .chars()
                .any(|c| c.is_alphabetic() && !c.is_uppercase() && !is_spaceless_cjk(c))
            {
                return None;
            }
        }
    }
    Some(tracking)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;
    use std::collections::HashMap;

    /// 600-unit glyphs; the space is 300 units wide, so the word-gap
    /// threshold is 120 thousandths.
    fn font() -> FontWidthInfo {
        let mut widths = HashMap::new();
        for code in 0u16..=255 {
            widths.insert(code, if code == 32 { 300 } else { 600 });
        }
        FontWidthInfo {
            widths,
            default_width: 600,
            space_width: 300,
            is_cid: false,
            units_scale: 0.001,
            wmode: 0,
        }
    }

    fn latin(code: &Object) -> Option<(String, bool)> {
        match code {
            Object::String(bytes, _) => {
                Some((bytes.iter().map(|&b| b as char).collect::<String>(), false))
            }
            _ => None,
        }
    }

    fn candidate(raw: &[u8], char_spacing: f32, word_spacing: f32) -> Option<WordGapCandidate> {
        let font = font();
        let text: String = raw.iter().map(|&b| b as char).collect();
        word_gap_candidate(
            raw,
            &text,
            Some(&font),
            10.0,
            char_spacing,
            word_spacing,
            word_gap_threshold(Some(&font)),
            latin,
        )
    }

    fn spaced(text: &str, trailing_spacing_ts: f32) -> Option<WordGapCandidate> {
        Some(WordGapCandidate {
            spaced_text: text.to_string(),
            trailing_spacing_ts,
        })
    }

    fn item(text: &str) -> TextItem {
        TextItem {
            text: text.to_string(),
            x: 72.0,
            y: 700.0,
            width: 18.0,
            height: 10.0,
            font: "F1".to_string(),
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    #[test]
    fn threshold_follows_the_space_width_with_a_floor() {
        assert_eq!(word_gap_threshold(Some(&font())), 120.0);
        let mut narrow = font();
        narrow.space_width = 100;
        assert_eq!(word_gap_threshold(Some(&narrow)), 80.0);
        assert_eq!(word_gap_threshold(None), 120.0);
    }

    #[test]
    fn a_short_string_with_word_gap_spacing_is_a_candidate() {
        // 2 Tc at 10pt is 0.2 em: a word gap after each glyph.
        assert_eq!(candidate(b"dt", 2.0, 0.0), spaced("d t", 2.0));
        // A one-letter word between two boundaries.
        assert_eq!(candidate(b"sad", 2.0, 0.0), spaced("s a d", 2.0));
        // Under the threshold, or negative: ordinary advance.
        assert_eq!(candidate(b"dt", 1.0, 0.0), None);
        assert_eq!(candidate(b"dt", -1.0, 0.0), None);
        // Nothing to separate in one glyph; four glyphs are tracked text.
        assert_eq!(candidate(b"d", 3.0, 0.0), None);
        assert_eq!(candidate(b"dtoM", 3.0, 0.0), None);
    }

    #[test]
    fn junctions_that_take_no_space_leave_no_candidate() {
        assert_eq!(candidate(b"t,", 2.0, 0.0), None);
        assert_eq!(candidate(b",b", 2.0, 0.0), spaced(", b", 2.0));
        // Next to a space glyph the text already has its gap.
        assert_eq!(candidate(b"a b", 2.0, 3.0), None);

        let han = |code: &Object| match code {
            Object::String(bytes, _) if bytes == b"A" => Some(("\u{4E2D}".to_string(), false)),
            Object::String(_, _) => Some(("\u{6587}".to_string(), false)),
            _ => None,
        };
        let font = font();
        assert_eq!(
            word_gap_candidate(
                b"AB",
                "\u{4E2D}\u{6587}",
                Some(&font),
                10.0,
                2.0,
                0.0,
                word_gap_threshold(Some(&font)),
                han
            ),
            None
        );
    }

    #[test]
    fn word_spacing_counts_only_after_a_space_code_that_paints() {
        // Word spacing without a space code is no spacing at all.
        assert_eq!(candidate(b"dt", 0.0, 5.0), None);
        // Another code's whitespace does not hide the word spacing after a
        // space code that paints: code 32 shows "a", code 9 a blank.
        let mixed = |code: &Object| match code {
            Object::String(bytes, _) if bytes == b" " => Some(("a".to_string(), false)),
            Object::String(bytes, _) if bytes == b"\t" => Some((" ".to_string(), false)),
            other => latin(other),
        };
        let mixed_font = font();
        assert_eq!(
            word_gap_candidate(
                b"\t b",
                " ab",
                Some(&mixed_font),
                10.0,
                0.0,
                3.0,
                word_gap_threshold(Some(&mixed_font)),
                mixed
            ),
            spaced(" a b", 0.0)
        );
        // A font whose code 32 shows a letter: the word spacing after it is
        // a gap, and the last glyph carries only the character spacing.
        let differences = |code: &Object| match code {
            Object::String(bytes, _) if bytes == b" " => Some(("a".to_string(), false)),
            other => latin(other),
        };
        let font = font();
        assert_eq!(
            word_gap_candidate(
                b"t b",
                "tab",
                Some(&font),
                10.0,
                0.5,
                3.0,
                word_gap_threshold(Some(&font)),
                differences
            ),
            spaced("ta b", 0.5)
        );
    }

    #[test]
    fn a_decoder_that_reads_the_whole_string_keeps_its_item() {
        // A code the decoder drops leaves a partial text: no candidate.
        let dropping = |code: &Object| match code {
            Object::String(bytes, _) if bytes == b"\x01" => None,
            other => latin(other),
        };
        let font = font();
        assert_eq!(
            word_gap_candidate(
                b"\x01dt",
                "dt",
                Some(&font),
                10.0,
                2.0,
                0.0,
                word_gap_threshold(Some(&font)),
                dropping
            ),
            None
        );
        // The string decodes to a ligature the codes cannot reproduce.
        assert_eq!(
            word_gap_candidate(
                b"fi",
                "\u{FB01}",
                Some(&font),
                10.0,
                2.0,
                0.0,
                word_gap_threshold(Some(&font)),
                latin
            ),
            None
        );
    }

    #[test]
    fn taking_the_spacing_back_means_most_of_it_up_to_a_kern_more() {
        assert!(spacing_taken_back(3.0, 3.0, 10.0));
        assert!(spacing_taken_back(2.1, 3.0, 10.0));
        // A kern between tracked glyphs: two thirds of the spacing.
        assert!(!spacing_taken_back(2.0, 3.0, 10.0));
        assert!(spacing_taken_back(4.2, 3.0, 10.0));
        assert!(!spacing_taken_back(4.3, 3.0, 10.0));
        assert!(!spacing_taken_back(0.0, 3.0, 10.0));
        assert!(!spacing_taken_back(1.0, 0.0, 10.0));
    }

    fn pending(items: &[TextItem]) -> PendingWordGaps {
        // "dt" shown at 72 with 3pt spacing: the pen rests at 90.
        PendingWordGaps::new(
            items.len() - 1,
            "d t".to_string(),
            &[1.0, 0.0, 0.0, 1.0, 90.0, 700.0],
            &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            1.0,
            3.0,
            10.0,
        )
    }

    #[test]
    fn a_next_run_that_takes_the_spacing_back_spaces_the_candidate() {
        let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut items = vec![item("dt")];
        pending(&items).resolve(&mut items, &[1.0, 0.0, 0.0, 1.0, 87.0, 700.0], &identity);
        assert_eq!(items[0].text, "d t");

        // At the pen: nothing taken back, left alone.
        let mut items = vec![item("dt")];
        pending(&items).resolve(&mut items, &[1.0, 0.0, 0.0, 1.0, 90.0, 700.0], &identity);
        assert_eq!(items[0].text, "dt");

        // Another line, or a run shown in between.
        let mut items = vec![item("dt")];
        pending(&items).resolve(&mut items, &[1.0, 0.0, 0.0, 1.0, 87.0, 688.0], &identity);
        assert_eq!(items[0].text, "dt");
        let mut items = vec![item("dt")];
        let pending = pending(&items);
        items.push(item("x"));
        pending.resolve(&mut items, &[1.0, 0.0, 0.0, 1.0, 87.0, 700.0], &identity);
        assert_eq!(items[0].text, "dt");

        // Horizontal scaling halves the travel per text-space unit.
        let mut items = vec![item("dt")];
        PendingWordGaps::new(
            0,
            "d t".to_string(),
            &[1.0, 0.0, 0.0, 1.0, 81.0, 700.0],
            &identity,
            0.5,
            3.0,
            10.0,
        )
        .resolve(&mut items, &[1.0, 0.0, 0.0, 1.0, 79.5, 700.0], &identity);
        assert_eq!(items[0].text, "d t");
    }

    /// A `TJ` array from a compact spec: strings in parentheses, numbers
    /// as offsets — `"(V) -250 (A) -250 (L)"`.
    fn tj(spec: &str) -> Vec<Object> {
        spec.split_whitespace()
            .map(
                |token| match token.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
                    Some(text) => Object::String(text.as_bytes().to_vec(), StringFormat::Literal),
                    None => Object::Real(token.parse().unwrap()),
                },
            )
            .collect()
    }

    /// Tracking read from `spec` with the 600-unit test font: its space is
    /// 300 units, so the word-gap threshold is 120 thousandths.
    fn tracking(spec: &str) -> Option<f32> {
        let font = font();
        tj_tracking(
            &tj(spec),
            Some(&font),
            word_gap_threshold(Some(&font)),
            latin,
        )
    }

    #[test]
    fn tracked_display_runs_read_their_letter_spacing() {
        // Uniform tracking, tracking with kerning on top, and a word gap a
        // space width above the letter gaps: the tracking is the typical
        // letter gap each time.
        assert_eq!(
            tracking("(V) -250 (A) -250 (L) -250 (L) -250 (E) -250 (Y)"),
            Some(250.0)
        );
        assert_eq!(
            tracking("(V) -216 (A) -333 (L) -166 (L) -250 (E) -290 (Y)"),
            Some(250.0)
        );
        assert_eq!(
            tracking(
                "(V) -216 (A) -333 (L) -166 (L) -250 (E) -290 (Y) -560 (R) -240 (O) -260 (A) -250 (D)"
            ),
            Some(250.0)
        );
        // A junction without an offset is a letter gap of zero; it does
        // not unseat the typical gap.
        assert_eq!(
            tracking("(V) (A) -250 (L) -250 (L) -250 (E) -250 (Y)"),
            Some(250.0)
        );
        // Light tracking on a mixed-case word needs no capitals.
        assert_eq!(
            tracking("(V) -120 (a) -140 (l) -100 (l) -120 (e) -130 (y)"),
            Some(120.0)
        );
    }

    #[test]
    fn a_space_width_between_single_glyphs_is_tracking_only_in_capitals() {
        // 0.75 of the space width and more: one-letter words look the same
        // in lowercase, so only the display convention reads as tracking.
        assert_eq!(tracking("(a) -300 (b) -300 (c) -300 (d)"), None);
        assert_eq!(tracking("(A) -300 (b) -300 (c) -300 (d)"), None);
        assert_eq!(tracking("(U) -300 (S) -300 (A)"), Some(300.0));
        assert_eq!(tracking("(2) -300 (0) -300 (2) -300 (4)"), Some(300.0));
        let han = |code: &Object| match code {
            Object::String(bytes, _) if bytes == b"A" => Some(("\u{4E2D}".to_string(), false)),
            Object::String(_, _) => Some(("\u{6587}".to_string(), false)),
            _ => None,
        };
        let font = font();
        assert_eq!(
            tj_tracking(
                &tj("(A) -300 (B) -300 (A)"),
                Some(&font),
                word_gap_threshold(Some(&font)),
                han
            ),
            Some(300.0)
        );
        // Below that, tracking is read whatever the case.
        assert_eq!(tracking("(a) -200 (b) -200 (c) -200 (d)"), Some(200.0));
    }

    #[test]
    fn offsets_that_are_not_tracking_are_left_alone() {
        // Words positioned by offsets, and a kerned pair in a word.
        assert_eq!(tracking("(The) -258 (quick) -300 (brown)"), None);
        assert_eq!(tracking("(A) 83 (VALLEY)"), None);
        // Kerning between single glyphs, with a word gap among them: the
        // typical gap is no tracking, so every offset is read as it is.
        assert_eq!(
            tracking("(T) 20 (h) -5 (e) -278 (q) 10 (u) -3 (i) -8 (c) (k)"),
            None
        );
        assert_eq!(tracking("(A) 20 (V) -333 (I) 10 (S) -333 (A)"), None);
        assert_eq!(tracking("(A) -30 (B) -20 (C) -40 (D)"), None);
        // Column-wide positioning, and too few junctions to tell.
        assert_eq!(tracking("(A) -1500 (B) -1500 (C)"), None);
        assert_eq!(tracking("(A) -250 (B)"), None);
        assert_eq!(tracking("(A)"), None);
        assert_eq!(tracking(""), None);
    }

    #[test]
    fn cid_fonts_count_two_bytes_per_glyph() {
        let mut cid = font();
        cid.is_cid = true;
        let threshold = word_gap_threshold(Some(&cid));
        let glyph = |code: u16| Object::String(code.to_be_bytes().to_vec(), StringFormat::Literal);
        let array = vec![
            glyph(0x41),
            Object::Integer(-250),
            glyph(0x42),
            Object::Integer(-250),
            glyph(0x43),
        ];
        assert_eq!(
            tj_tracking(&array, Some(&cid), threshold, latin),
            Some(250.0)
        );
        // Two glyphs in one string: a word, not a tracked run.
        let pair = Object::String(vec![0, 0x41, 0, 0x42], StringFormat::Literal);
        let array = vec![
            pair,
            Object::Integer(-250),
            glyph(0x43),
            Object::Integer(-250),
            glyph(0x44),
        ];
        assert_eq!(tj_tracking(&array, Some(&cid), threshold, latin), None);
    }
}
