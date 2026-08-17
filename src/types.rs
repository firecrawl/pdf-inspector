//! Shared types used across the extraction and markdown pipelines.
//!
//! Centralises `TextItem`, `TextLine`, `PdfRect`, font-width / encoding
//! type aliases, and the `ItemType` enum so that every module can import
//! them from one place.

use std::collections::HashMap;

use crate::text_utils::should_join_items;

/// Result tuple returned by page-level text extraction: text items, rectangles, line segments,
/// and whether fonts with unresolvable gid-encoded glyphs were encountered.
pub(crate) type PageExtraction = (Vec<TextItem>, Vec<PdfRect>, Vec<PdfLine>);

// ── Font types (crate-internal) ──────────────────────────────────────

/// Font encoding map: maps byte codes to Unicode characters
pub(crate) type FontEncodingMap = HashMap<u8, char>;

/// All font encodings for a page
pub(crate) type PageFontEncodings = HashMap<String, FontEncodingMap>;

/// Font width information extracted from PDF font dictionaries
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct FontWidthInfo {
    /// Glyph widths: maps character code to width in font units
    pub(crate) widths: HashMap<u16, u16>,
    /// Default width for glyphs not in the widths table
    pub(crate) default_width: u16,
    /// Width of the space character (code 32) if known
    pub(crate) space_width: u16,
    /// Whether this is a CID font (2-byte character codes)
    pub(crate) is_cid: bool,
    /// Scale factor to convert font units to text space units.
    /// For Type1/TrueType: 0.001 (widths in 1000ths of em)
    /// For Type3: FontMatrix[0] (e.g., 0.00048828125 for 2048-unit grid)
    pub(crate) units_scale: f32,
    /// Writing mode: 0 = horizontal (default), 1 = vertical
    pub(crate) wmode: u8,
}

/// All font width info for a page, keyed by font resource name
pub(crate) type PageFontWidths = HashMap<String, FontWidthInfo>;

// ── Public types ─────────────────────────────────────────────────────

/// Type of extracted item
#[derive(Debug, Clone, Default)]
pub enum ItemType {
    /// Regular text content
    #[default]
    Text,
    /// Image placeholder
    Image,
    /// Hyperlink (with URL)
    Link(String),
    /// Form field (name: value)
    FormField,
}

/// Layout complexity analysis result.
///
/// Callers can use this to decide whether the extracted markdown is reliable
/// or whether the PDF should be routed to an OCR pipeline instead.
#[derive(Debug, Clone, Default)]
pub struct LayoutComplexity {
    /// True if any page has tables or multi-column text.
    pub is_complex: bool,
    /// 1-indexed pages where table borders were detected (rect count > 6).
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where 2+ text columns were detected.
    pub pages_with_columns: Vec<u32>,
}

/// A line segment from PDF path operators (`m`/`l`/`S`).
#[derive(Debug, Clone)]
pub struct PdfLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub page: u32,
}

/// A rectangle from a PDF `re` operator (cell boundary, border, etc.)
#[derive(Debug, Clone)]
pub struct PdfRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub page: u32,
}

/// A text item with position information
#[derive(Debug, Clone)]
pub struct TextItem {
    /// The text content
    pub text: String,
    /// X position on page
    pub x: f32,
    /// Y position on page (PDF coordinates, origin at bottom-left)
    pub y: f32,
    /// Width of text
    pub width: f32,
    /// Height (approximated from font size)
    pub height: f32,
    /// Font name
    pub font: String,
    /// Font size
    pub font_size: f32,
    /// Page number (1-indexed)
    pub page: u32,
    /// Whether the font is bold
    pub is_bold: bool,
    /// Whether the font is italic
    pub is_italic: bool,
    /// Whether the text is underlined (drawn rule/thin rect under the
    /// baseline — PDFs have no underline font flag, so this is detected
    /// geometrically after extraction; see `extractor::underline`).
    pub is_underline: bool,
    /// Whether the text is struck out (drawn rule/thin rect crossing the
    /// glyphs at mid x-height). Same geometric detection as underline,
    /// different vertical window; see `extractor::underline`.
    pub is_strikeout: bool,
    /// Type of item (text, image, link)
    pub item_type: ItemType,
    /// Marked Content ID from the content stream's BDC/BMC operator.
    /// Used to link this item to the PDF structure tree for tagged PDFs.
    pub mcid: Option<i64>,
}

/// A line of text (grouped text items)
#[derive(Debug, Clone)]
pub struct TextLine {
    pub items: Vec<TextItem>,
    pub y: f32,
    pub page: u32,
    /// Adaptive join threshold from page-level letter-spacing detection.
    /// Default 0.10 for normal PDFs; higher for Canva-style PDFs.
    #[doc(hidden)]
    pub adaptive_threshold: f32,
}

impl TextLine {
    pub fn text(&self) -> String {
        self.text_with_formatting(false, false, false)
    }

    /// Get text with optional bold/italic/decorative markdown formatting.
    ///
    /// `format_decorations` enables both geometrically detected source
    /// decorations: underline (`<u>`) and strikeout (`<s>`).
    pub fn text_with_formatting(
        &self,
        format_bold: bool,
        format_italic: bool,
        format_decorations: bool,
    ) -> String {
        if !format_bold && !format_italic && !format_decorations {
            return self.text_plain();
        }

        let single_char_threshold = self.adaptive_threshold;

        // Resolve each item's *effective* underline up front, one run of
        // tightly-continuous fragments at a time, rather than toggling the
        // `<u>` tag per item as the render loop walks through. See
        // resolve_underline_by_word_group's doc comment for why.
        let effective_underline = self.resolve_underline_by_word_group(format_decorations);

        let mut result = String::new();
        let mut current_bold = false;
        let mut current_italic = false;
        let mut current_underline = false;
        let mut current_strikeout = false;

        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            let text_trimmed = text.trim();

            // Skip empty items
            if text_trimmed.is_empty() {
                continue;
            }

            // Determine spacing
            let needs_space = if i == 0 || result.is_empty() {
                false
            } else {
                let prev_item = &self.items[i - 1];
                self.needs_space_between(prev_item, item, &result, single_char_threshold)
            };

            // Preserve leading whitespace from the item text.
            // Items like " means any person" have a leading space that indicates
            // a word boundary. needs_space_between returns false for these (because
            // space_already_exists), but we still need to emit the space since
            // we push text_trimmed below (which strips it).
            let has_leading_space = text.starts_with(' ');

            // Check for style changes. Source decorations are exclusive:
            // `<u>`/`<s>` content stays free of `**`/`*` markers — consumers
            // (and the eval harnesses this feeds) match tag content literally,
            // and mixed nesting breaks that. A struck-and-underlined item is
            // emitted as struck text because deletion is the stronger semantic
            // distinction in redline documents.
            let item_strikeout = format_decorations && item.is_strikeout;
            let item_underline = effective_underline[i] && !item_strikeout;
            let item_bold = format_bold && item.is_bold && !item_underline && !item_strikeout;
            let item_italic = format_italic && item.is_italic && !item_underline && !item_strikeout;

            // Close previous styles if they change
            if current_italic && !item_italic {
                result.push('*');
                current_italic = false;
            }
            if current_bold && !item_bold {
                result.push_str("**");
                current_bold = false;
            }
            if current_underline && !item_underline {
                result.push_str("</u>");
                current_underline = false;
            }
            if current_strikeout && !item_strikeout {
                result.push_str("</s>");
                current_strikeout = false;
            }

            // Add space: either from spacing logic or preserved from item text
            if needs_space || (has_leading_space && !result.is_empty() && !result.ends_with(' ')) {
                result.push(' ');
            }

            // Open new styles
            if item_underline && !current_underline {
                result.push_str("<u>");
                current_underline = true;
            }
            if item_strikeout && !current_strikeout {
                result.push_str("<s>");
                current_strikeout = true;
            }
            if item_bold && !current_bold {
                result.push_str("**");
                current_bold = true;
            }
            if item_italic && !current_italic {
                result.push('*');
                current_italic = true;
            }

            result.push_str(text_trimmed);
        }

        // Close any remaining open styles
        if current_italic {
            result.push('*');
        }
        if current_bold {
            result.push_str("**");
        }
        if current_underline {
            result.push_str("</u>");
        }
        if current_strikeout {
            result.push_str("</s>");
        }

        result
    }

    /// Get plain text without formatting
    fn text_plain(&self) -> String {
        let single_char_threshold = self.adaptive_threshold;

        let mut result = String::new();
        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            if i == 0 {
                result.push_str(text);
            } else {
                let prev_item = &self.items[i - 1];
                if self.needs_space_between(prev_item, item, &result, single_char_threshold) {
                    result.push(' ');
                }
                result.push_str(text);
            }
        }
        result
    }

    /// Resolve each item's effective underline flag, one run of tightly
    /// continuous fragments (a single visual word split across several
    /// positioned-text items) at a time, instead of toggling `<u>` per
    /// item as it's encountered.
    ///
    /// A single visual word can be split across several content-stream
    /// text-showing operators, and the source PDF can carry the
    /// underline flag on only one fragment — e.g. an underline rectangle
    /// whose geometry happens to cover only part of a word's width, a
    /// rendering artifact rather than intentional partial-word styling.
    /// Layout-level merging (`extractor::mod`'s `merge_text_items`)
    /// already refuses to combine such items across a style difference,
    /// so they arrive here as adjacent, unmerged items.
    ///
    /// Deciding each fragment's tag independently produces `<u>We</u>alth`
    /// for a word like "Wealth". A first attempt fixed that by having a
    /// continuation fragment inherit whichever underline state was
    /// already open while rendering — but that makes the *rendered*
    /// result depend on which fragment in the run happens to carry the
    /// flag: the same source pattern renders differently depending on
    /// fragment order (flag on the first fragment extends underline onto
    /// later ones; flag only on a later fragment gets silently dropped).
    /// Grouping each run up front and deciding it once, by majority
    /// character count across the whole run, removes that
    /// order-dependence: the same run always resolves to the same
    /// underline status regardless of which fragment(s) within it happen
    /// to carry the flag. See #397.
    fn resolve_underline_by_word_group(&self, format_decorations: bool) -> Vec<bool> {
        let n = self.items.len();
        if !format_decorations || n == 0 {
            return vec![false; n];
        }

        let mut resolved = vec![false; n];
        let mut run_start = 0usize;
        for i in 1..n {
            if !Self::is_tight_word_continuation(
                &self.items[i - 1],
                &self.items[i],
                self.adaptive_threshold,
            ) {
                Self::assign_run_underline(&self.items[run_start..i], &mut resolved[run_start..i]);
                run_start = i;
            }
        }
        Self::assign_run_underline(&self.items[run_start..n], &mut resolved[run_start..n]);
        resolved
    }

    /// True when `curr` continues the same visual word as `prev` — tight
    /// enough geometric contiguity (direction-aware, so this also holds
    /// for RTL text) that these are split fragments of one word rather
    /// than merely two items a general prose-spacing heuristic happens to
    /// join without inserting a space (punctuation attachment,
    /// hyphenation, kerning — all unrelated reasons for "no space" that
    /// are NOT "these are the same word").
    ///
    /// Strikeout is always a hard boundary: a struck-through fragment
    /// must never inherit an underlined neighbor's status (that would
    /// emit overlapping `<u>`/`<s>` spans), and by the same logic a
    /// non-struck fragment must never be grouped with a struck one.
    ///
    /// A trailing/leading closing-punctuation character (on either side
    /// of the join) is excluded even when geometrically touching — a
    /// period right after a word isn't part of that word, regardless of
    /// kerning. `'` is deliberately excluded from that punctuation set:
    /// unlike `.`/`,`/etc., an apostrophe can legitimately sit *inside* a
    /// word (a contraction split across fragments, e.g. "can" + "'t"),
    /// so always treating it as a boundary would reintroduce the same
    /// mid-word split this function exists to prevent.
    ///
    /// Also requires agreement with `should_join_items` — the same
    /// no-space decision the render loop itself uses to decide spacing.
    /// A tight geometric gap alone isn't sufficient: `should_join_items`
    /// deliberately treats some near-zero-gap pairs as separate words
    /// regardless of geometry (CID fonts emit one word per operator with
    /// gaps ≈ 0; a label ending in `:` followed by its value). Grouping
    /// those as one run here would apply/drop underline across a real
    /// word boundary the render loop itself inserts a space at.
    fn is_tight_word_continuation(prev: &TextItem, curr: &TextItem, threshold: f32) -> bool {
        if prev.is_strikeout || curr.is_strikeout {
            return false;
        }
        if prev.width <= 0.0 {
            return false;
        }
        let gap = if prev.x <= curr.x {
            curr.x - (prev.x + prev.width)
        } else {
            prev.x - (curr.x + curr.width)
        };
        if gap.abs() >= prev.font_size * 0.02 {
            return false;
        }
        let is_join_punct = |c: char| matches!(c, '.' | ',' | ';' | '!' | '?' | ')' | ']' | '}');
        if curr.text.starts_with(' ') || prev.text.ends_with(' ') {
            return false;
        }
        if curr
            .text
            .trim_start()
            .chars()
            .next()
            .is_some_and(is_join_punct)
        {
            return false;
        }
        if prev
            .text
            .trim_end()
            .chars()
            .last()
            .is_some_and(is_join_punct)
        {
            return false;
        }
        if !should_join_items(prev, curr, threshold) {
            return false;
        }
        true
    }

    /// Decide a run's group-level underline status by majority character
    /// count (ties favor `false`, the conservative "don't fabricate
    /// styling" default), then assign it to every item in the run.
    fn assign_run_underline(run: &[TextItem], resolved: &mut [bool]) {
        let mut underlined_chars = 0usize;
        let mut plain_chars = 0usize;
        for item in run {
            let len = item.text.trim().chars().count();
            if item.is_underline {
                underlined_chars += len;
            } else {
                plain_chars += len;
            }
        }
        let group_underline = underlined_chars > plain_chars;
        resolved.fill(group_underline);
    }

    /// Determine if a space is needed between two items
    fn needs_space_between(
        &self,
        prev_item: &TextItem,
        item: &TextItem,
        result: &str,
        single_char_threshold: f32,
    ) -> bool {
        let text = item.text.as_str();

        // Don't add space before/after hyphens for hyphenated words
        let prev_ends_with_hyphen = result.ends_with('-');
        let curr_is_hyphen = text.trim() == "-";
        let curr_starts_with_hyphen = text.starts_with('-');

        // Detect subscript/superscript: smaller font size and/or Y offset
        let font_ratio = item.font_size / prev_item.font_size;
        let reverse_font_ratio = prev_item.font_size / item.font_size;
        let y_diff = (item.y - prev_item.y).abs();

        let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
        let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

        // Use position-based spacing detection
        let should_join = should_join_items(prev_item, item, single_char_threshold);

        // Check if space already exists
        let prev_ends_with_space = result.ends_with(' ');
        let curr_starts_with_space = text.starts_with(' ');
        let space_already_exists = prev_ends_with_space || curr_starts_with_space;

        // Add space unless one of these conditions applies
        !(prev_ends_with_hyphen
            || curr_is_hyphen
            || curr_starts_with_hyphen
            || is_sub_super
            || was_sub_super
            || should_join
            || space_already_exists)
    }
}

#[cfg(test)]
mod formatting_tests {
    use super::{ItemType, TextItem, TextLine};

    fn item(text: &str, x: f32, width: f32, strikeout: bool) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y: 100.0,
            width,
            height: 12.0,
            font: "F1".to_string(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: strikeout,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn line(items: Vec<TextItem>) -> TextLine {
        TextLine {
            items,
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.1,
        }
    }

    #[test]
    fn formatting_emits_semantic_strikeout() {
        let line = line(vec![item("deleted", 10.0, 42.0, true)]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted</s>"
        );
    }

    #[test]
    fn formatting_closes_strikeout_before_live_text() {
        let line = line(vec![
            item("keep", 10.0, 24.0, false),
            item("remove", 40.0, 42.0, true),
            item("keep", 88.0, 24.0, false),
        ]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "keep <s>remove</s> keep"
        );
    }

    #[test]
    fn formatting_coalesces_adjacent_struck_items() {
        let line = line(vec![
            item("deleted", 10.0, 42.0, true),
            item("words", 58.0, 30.0, true),
        ]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted words</s>"
        );
    }

    #[test]
    fn underline_does_not_toggle_mid_word() {
        // Regression for #397: a single visual word ("Wealth") split
        // across three positioned items at the content-stream level, with
        // the underline flag set on only the middle fragment ("We") — a
        // real pattern from source PDFs where an underline rectangle
        // covers only part of a word's width. The items are contiguous
        // (zero gap, no space anywhere), so this must render as one
        // undivided word, not a `<u>` tag opening/closing mid-word.
        let title = item("The Institutional", 45.0, 200.0, false);
        // Real word gap before "We" (new word starts here) ...
        let mut we = item("We", title.x + title.width + 6.0, 24.0, false);
        we.is_underline = true;
        // ... zero gap before "alth" — it's a continuation of the same
        // word ("We" + "alth" = "Wealth") ...
        let alth = item("alth", we.x + we.width, 40.0, false);
        // ... then a real word gap before the next, separate word.
        let landscape = item("Landscape", alth.x + alth.width + 6.0, 100.0, false);

        let line = line(vec![title, we, alth, landscape]);

        let result = line.text_with_formatting(false, false, true);
        assert!(
            !result.contains("</u>alth") && !result.contains("We</u>"),
            "the underline tag must not close in the middle of \"Wealth\", got: {result:?}"
        );
        assert!(
            result.contains("Wealth"),
            "the word \"Wealth\" must render undivided, got: {result:?}"
        );
    }

    #[test]
    fn underline_word_group_resolution_is_order_independent() {
        // Regression for a PR #408 review finding: serially inheriting
        // "whichever underline state is already open" made the rendered
        // result depend on which fragment in a run happened to carry the
        // flag. Two equal-length fragments ("Wea" / "lth", tied 3 vs 3
        // characters) with the flag on either side must resolve to the
        // *same* output — majority-by-character-count ties favor `false`
        // regardless of which side carried the flag.
        let mut flag_first = item("Wea", 45.0, 60.0, false);
        flag_first.is_underline = true;
        let flag_first_rest = item("lth", flag_first.x + flag_first.width, 40.0, false);
        let a = line(vec![flag_first, flag_first_rest]).text_with_formatting(false, false, true);

        let flag_second = item("Wea", 45.0, 60.0, false);
        let mut flag_second_rest = item("lth", flag_second.x + flag_second.width, 40.0, false);
        flag_second_rest.is_underline = true;
        let b = line(vec![flag_second, flag_second_rest]).text_with_formatting(false, false, true);

        assert_eq!(
            a, b,
            "a tied same-word split must resolve identically regardless of \
             which fragment carries the underline flag"
        );
        assert_eq!(a, "Wealth", "a tie should favor not-underlined");
    }

    #[test]
    fn underline_strikeout_fragment_isolates_neighbors_from_its_own_flag() {
        // Regression for a PR #408 review finding: without treating
        // strikeout as a hard grouping boundary, a struck-through
        // fragment's own (hidden, struck-out) underline flag could still
        // sway the majority vote for its *neighboring* plain-text
        // fragments — even though the struck fragment itself never
        // renders `<u>` (the render loop always excludes underline on a
        // struck item). Here the struck middle fragment is long and
        // flagged, heavily outweighing its plain neighbors by character
        // count; if it weren't a hard boundary, that flag would leak into
        // the surrounding run and incorrectly underline the (visible,
        // non-struck) "We" and "alth" fragments too.
        let we = item("We", 45.0, 20.0, false);
        let mut struck_middle = item("XXXXXXXX", we.x + we.width, 80.0, true);
        struck_middle.is_underline = true;
        let alth = item("alth", struck_middle.x + struck_middle.width, 40.0, false);

        let line = line(vec![we, struck_middle, alth]);
        let result = line.text_with_formatting(false, false, true);

        assert!(
            !result.contains("<u>We") && !result.contains("alth</u>"),
            "a struck fragment's flag must not leak underline onto its plain \
             neighbors, got: {result:?}"
        );
    }

    #[test]
    fn underline_run_ends_at_trailing_punctuation_on_the_previous_item() {
        // Regression for a PR #408 review finding: the punctuation
        // boundary check only looked at the *next* item's leading
        // character. When the *previous* item's own text already ends
        // with closing punctuation (e.g. it was merged into one item
        // earlier in the pipeline), a tightly adjacent following
        // fragment must still not be swept into the same underline run.
        let mut sentence = item("Wealth.", 45.0, 70.0, false);
        sentence.is_underline = true;
        let extra = item("Extra", sentence.x + sentence.width, 50.0, false);

        let line = line(vec![sentence, extra]);
        let result = line.text_with_formatting(false, false, true);

        assert_eq!(result, "<u>Wealth.</u>Extra");
    }

    #[test]
    fn underline_apostrophe_does_not_split_a_contraction() {
        // Regression for a PR #408 review finding: treating `'` as
        // always a word boundary breaks contractions split across
        // fragments (e.g. "can" + "'t") — reintroducing the exact
        // mid-word toggle bug this PR fixes, just at the apostrophe.
        let mut can = item("can", 45.0, 40.0, false);
        can.is_underline = true;
        let t = item("'t", can.x + can.width, 20.0, false);

        let line = line(vec![can, t]);
        let result = line.text_with_formatting(false, false, true);

        assert_eq!(result, "<u>can't</u>");
    }

    #[test]
    fn underline_grouping_agrees_with_should_join_items_on_cid_fonts() {
        // Regression for a PR #408 review finding: the run-grouping
        // check used only geometric contiguity, disagreeing with
        // `should_join_items` (used by the render loop's own spacing
        // decision) on CID fonts, which emit one word per text operator
        // with gaps ≈ 0 between *separate* words — the tight-geometry
        // check alone would have wrongly grouped these two genuinely
        // distinct words ("Alpha " and "Beta") into a single underline
        // run, applying/dropping underline across a real word boundary
        // the render loop itself inserts a space at.
        let mut alpha = item("Alpha", 45.0, 40.0, false);
        alpha.font = "C2_0".to_string();
        alpha.is_underline = true;
        let mut beta = item("Beta", alpha.x + alpha.width, 40.0, false);
        beta.font = "C2_0".to_string();
        beta.is_underline = false;

        let line = line(vec![alpha, beta]);
        let result = line.text_with_formatting(false, false, true);

        assert_eq!(
            result, "<u>Alpha</u> Beta",
            "CID-font word-per-operator items must stay separate words, not \
             merge into one underline run just because their gap is near zero"
        );
    }

    #[test]
    fn underline_word_continuation_is_direction_aware_for_rtl() {
        // Regression for a PR #408 review finding: the geometric
        // contiguity check was LTR-only (`curr.x - (prev.x + prev.width)`),
        // which reports a large, non-tight gap for genuinely touching RTL
        // fragments (positioned right-to-left, so curr.x < prev.x).
        let mut prev = item("Weal", 100.0, 20.0, false);
        prev.is_underline = true;
        // RTL contiguity: curr's right edge (x + width) touches prev.x.
        // Unequal fragment lengths (4 vs 2 chars) so the majority-by-char
        // rule has a clear winner rather than a tie.
        let curr = item("th", 80.0, 20.0, false);

        let line = line(vec![prev, curr]);
        let result = line.text_with_formatting(false, false, true);

        assert_eq!(
            result, "<u>Wealth</u>",
            "RTL-contiguous fragments must still be grouped as one word"
        );
    }

    #[test]
    fn underline_still_toggles_at_a_real_word_boundary() {
        // Sanity check that the #397 fix doesn't suppress underline
        // entirely — a genuinely separate, space-divided word must still
        // get its own `<u>` span when it's the one flagged.
        let plain = item("Plain", 10.0, 40.0, false);
        let mut underlined = item("Underlined", 60.0, 70.0, false);
        underlined.is_underline = true;

        let line = line(vec![plain, underlined]);

        assert_eq!(
            line.text_with_formatting(false, false, true),
            "Plain <u>Underlined</u>"
        );
    }

    #[test]
    fn strikeout_takes_precedence_over_other_styles() {
        let mut decorated = item("deleted", 10.0, 42.0, true);
        decorated.is_bold = true;
        decorated.is_italic = true;
        decorated.is_underline = true;
        let line = line(vec![decorated]);

        assert_eq!(
            line.text_with_formatting(true, true, true),
            "<s>deleted</s>"
        );
        assert_eq!(line.text(), "deleted");
    }
}
