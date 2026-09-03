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

/// A text item with position information.
///
/// `x`, `y`, `width`, `height` describe the item's axis-aligned box in PDF
/// points, y-up. For ordinary horizontal text the box runs from the baseline
/// one em upward and spans the run's advance, which is what every consumer
/// historically assumed. A run shown with a rotated text matrix gets the
/// bounding box of its rotated glyph run instead — tall and thin for a
/// vertical margin stamp — and reports the angle in `rotation`.
///
/// # Coordinate frame
///
/// Items returned by the public position APIs (`extract_text_with_positions*`)
/// are relative to the page's **visible page box** —
/// `CropBox ∩ MediaBox` when the page has a CropBox, else the MediaBox; a
/// CropBox that does not overlap the MediaBox is ignored, and a page without
/// a MediaBox is measured against US Letter (see `extractor::page_box`) —
/// with the box's lower-left corner as the origin and `y` growing upward.
/// A renderer's page image and the region APIs use the same box from its
/// top-left corner with `y` growing downward, so flipping `y` by the box
/// height lets items and rendered regions be intersected directly. Raw
/// content-stream coordinates differ whenever the CropBox or MediaBox origin
/// is not `(0, 0)`. A page whose text is predominantly rotated has its frame
/// turned so the text reads left-to-right (`PageRotation`, reported by
/// `extract_text_with_positions_and_rotations_mem`) and the shift is turned
/// the same way; `/Rotate` is not applied. Inside the markdown pipeline
/// items stay in raw user space.
#[derive(Debug, Clone)]
pub struct TextItem {
    /// The text content
    pub text: String,
    /// Left edge of the item's box, in PDF points from the visible page
    /// box's left edge (see the coordinate frame note on [`TextItem`]).
    pub x: f32,
    /// Bottom edge of the item's box, in PDF points from the visible page
    /// box's bottom edge with `y` growing upward (see the coordinate frame
    /// note on [`TextItem`]). For horizontal text this is the baseline;
    /// descenders are not included. Image, link and form-field items carry
    /// their rect's bottom edge.
    pub y: f32,
    /// Horizontal extent of the box: the advance for horizontal text, the
    /// em size for a vertical run. Zero only for a horizontal run whose font
    /// carries no width information (advance unknown).
    pub width: f32,
    /// Vertical extent of the box: the rendered em size for horizontal
    /// text, the advance for a vertical run.
    pub height: f32,
    /// Rotation of the run's baseline in degrees, counter-clockwise from the
    /// page's +x axis, normalised to `[0, 360)`: `0` for ordinary
    /// left-to-right text, `90` for text reading bottom-to-top (a margin
    /// stamp rotated counter-clockwise), `270` for text reading
    /// top-to-bottom, `180` for upside-down text. Rotation-only matrices
    /// report exact multiples of 90; skewed matrices (deskewed OCR layers,
    /// diagonal watermarks) report fractional angles. A reflected text
    /// matrix has no rotation — its reading direction and its glyphs'
    /// orientation differ by a half turn — and reports how its glyphs
    /// stand: `0` for the mirrored-x matrix some producers paint
    /// right-to-left text with (upright glyphs reading left), `180` for a
    /// y-flipped one. A negative `Tf` size turns a run around and reads as
    /// `180`. `0` for items that don't come from a text matrix (images,
    /// links, form fields, OCR).
    /// On a page whose text is predominantly rotated the extractor turns
    /// the coordinate frame so the dominant runs read as `0`; upright
    /// strays then report `270` on a counter-clockwise page and `90` on a
    /// clockwise one.
    pub rotation: f32,
    /// Whether the run's advance came from font metrics. `false` when the
    /// font carries no width information (or, for an ActualText span, when
    /// the advance could not be recovered from the text matrix): the box's
    /// extent along the baseline is then an estimate of half an em per
    /// painted glyph (an ActualText span counts the glyphs it covers, not its
    /// replacement text), laid in the direction the run reads, rather than
    /// a measurement. A
    /// font that reports a genuine zero advance keeps `true`. Items that
    /// don't come from a text matrix (images, links, form fields, OCR)
    /// always report `true`.
    pub advance_known: bool,
    /// Font name: the `/BaseFont` family name ("ABCDEF+CMMI10"), which
    /// identifies the actual face (see `extractor::fonts::item_font_name`
    /// for the CID carve-out).
    pub font: String,
    /// The raw font resource tag ("F2", "T22") the item's show operator
    /// selected. This is exactly what `font` carried before 1.16.0, with
    /// the same caveats: the tag's namespace is the enclosing page or Form
    /// XObject's `/Resources` (the same tag on another page may name a
    /// different face), and an item merged from multiple runs keeps the
    /// first run's tag. Within one page it distinguishes font *programs*
    /// that share a family name (two subsets of the same face keep
    /// distinct tags), which family-keyed `font` cannot. Empty for items
    /// that don't originate from a content-stream show operator (images,
    /// links, form fields, OCR).
    pub font_tag: String,
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
    /// Signed baseline offset, in points, of a superscript/subscript glyph
    /// run from the baseline of the body text it is attached to. Zero for
    /// normal text. Positive = raised above the anchor's baseline
    /// (superscript: footnote and affiliation markers, exponents), negative =
    /// lowered (subscript: chemistry indices, math). Extraction sets it when
    /// a short run is small relative to a tightly adjacent larger neighbor
    /// and sits at a real baseline offset from it; a digit-only run beside a
    /// word is instead fused into that word as Unicode super/subscript
    /// characters ("H₂O", "word²") and never carries a shift. `y` stays the
    /// glyph's own baseline; [`TextItem::line_y`] gives the anchor's.
    pub baseline_shift: f32,
}

impl TextItem {
    /// Baseline of the visual line this item belongs to: the glyphs'
    /// baseline for normal text, the anchor's baseline for a super/subscript
    /// glyph run (`baseline_shift` below it). Line grouping compares this
    /// instead of `y` so raised and lowered markers stay on their body line
    /// and upside-down runs of different sizes share theirs.
    pub fn line_y(&self) -> f32 {
        self.baseline_y() - self.baseline_shift
    }

    /// The y of the edge the glyphs stand on or hang from: `y` (the box's
    /// bottom edge) for a run within 45° of upright, `y + height` (the top
    /// edge) for one within 45° of upside-down (`is_upside_down()`, glyph
    /// orientation included for reflected matrices). Exact for level runs;
    /// for oblique ones the baseline is not horizontal and the edge is only
    /// an approximation of it, which is what line grouping then compares.
    /// Vertical runs return the box bottom `y`.
    pub fn baseline_y(&self) -> f32 {
        if self.is_upside_down() {
            self.y + self.height
        } else {
            self.y
        }
    }

    /// `true` for a glyph run flagged as a super- or subscript of a larger
    /// neighbor (non-zero `baseline_shift`).
    pub fn is_script(&self) -> bool {
        self.baseline_shift != 0.0
    }
}

impl TextItem {
    /// Whether the run reads along the page's x axis rather than its y
    /// axis: `rotation` closer to `0`/`180` than to `90`/`270`, the same
    /// 45° split the extractor uses to vote on page rotation. Layout
    /// heuristics that reason about baselines, word gaps, and column spans
    /// walk the x axis and assume this; rotated runs (margin stamps, chart
    /// axis titles, rotated table headers) return `false` and are kept out
    /// of them. Oblique runs (diagonal watermarks, deskewed OCR lines) are
    /// deliberately still `true`: the x-axis heuristics are the closest fit
    /// the pipeline has for them, exactly as before `rotation` existed, and
    /// callers needing the precise angle read `rotation` directly.
    pub fn is_horizontal(&self) -> bool {
        let r = self.rotation.rem_euclid(360.0);
        let vertical = (r > 45.0 && r < 135.0) || (r > 225.0 && r < 315.0);
        !vertical
    }

    /// Whether the run reads along +x: `rotation` within 45° of `0`. The
    /// x-ascending walks (item merging, line assembly) assume this; an
    /// upside-down run is `is_horizontal()` but reads towards -x.
    pub fn is_upright(&self) -> bool {
        let r = self.rotation.rem_euclid(360.0);
        r <= 45.0 || r >= 315.0
    }

    /// Whether the run reads towards -x: `rotation` within 45° of `180`.
    pub fn is_upside_down(&self) -> bool {
        self.is_horizontal() && !self.is_upright()
    }

    /// The item's extent perpendicular to its reading direction: `height`
    /// for an unrotated item — identical to the historical value, and the
    /// only meaningful extent for image, link, and OCR boxes — and the
    /// rendered em (`font_size`) for any rotated run, whose axis-aligned
    /// box mixes the advance into both dimensions (a long diagonal run is
    /// not a tall line). Only content-stream runs carry a non-zero
    /// `rotation`, and they set `font_size` to exactly the em height the
    /// unrotated case reports.
    pub(crate) fn cross_extent(&self) -> f32 {
        if self.rotation == 0.0 {
            self.height
        } else {
            self.font_size
        }
    }
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

/// Gap, as a fraction of the larger font size, from which a script glyph and
/// its normal-sized neighbor are separate words. Attached markers sit at
/// ~0 gap (kerned ones slightly negative); a word space is ≥ 0.2 em.
const SCRIPT_WORD_GAP: f32 = 0.12;
/// Spacing at the edge of a super/subscript run — the single policy shared
/// by line rendering (`TextLine::text`) and table-cell joining. `None` when
/// neither item is a script run, so the caller's ordinary rules apply.
///
/// A run's glyphs arrive pre-joined by extraction, so only the boundary
/// between a run and its neighbor is decided here, by the measured gap: a
/// footnote marker hugs the word before it ("word<sup>1</sup>"), a leading
/// affiliation marker hugs the word after it ("<sup>1,2</sup>Hong Kong"),
/// and a word space after a marker survives ("<sup>2</sup> next"). Existing
/// whitespace, hyphen junctions, open brackets before and closing
/// punctuation after a run never take a space.
pub(crate) fn script_edge_needs_space(
    prev: &TextItem,
    item: &TextItem,
    result: &str,
    text: &str,
) -> Option<bool> {
    if !(prev.is_script() || item.is_script()) {
        return None;
    }
    if stacked_fraction_slash(prev, item) {
        return Some(false);
    }
    // Different visual lines (a wrapped table cell): a run at the end of
    // one line never attaches to the start of the next, whatever the x
    // overlap says.
    if (prev.line_y() - item.line_y()).abs() > prev.font_size.max(item.font_size) * 0.5 {
        return Some(true);
    }
    let curr = text.trim_start();
    // `result` ends with the closing tag when the previous item is a run,
    // so its raw text is inspected too for hyphens and open brackets.
    if result.ends_with([' ', '-', '(', '[', '{'])
        || prev.text.ends_with(' ')
        || prev.text.trim_end().ends_with(['-', '(', '[', '{'])
        || text.starts_with(' ')
        || curr.starts_with('-')
        || curr
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'))
    {
        return Some(false);
    }
    let gap = if prev.x <= item.x {
        item.x - (prev.x + prev.width)
    } else {
        prev.x - (item.x + item.width)
    };
    Some(gap >= prev.font_size.max(item.font_size) * SCRIPT_WORD_GAP)
}

/// A stacked case fraction: a digit-only superscript run directly followed
/// by a digit-only subscript run that overlaps it horizontally — the
/// numerator over the denominator, as TeX sets "3⅓". Rendered with a slash
/// between the runs (`3 <sup>1</sup>/<sub>3</sub>`) instead of the runs
/// being glued into one number.
pub(crate) fn stacked_fraction_slash(prev: &TextItem, item: &TextItem) -> bool {
    let digits = |t: &str| !t.is_empty() && t.chars().all(char::is_numeric);
    prev.baseline_shift > 0.0
        && item.baseline_shift < 0.0
        && (prev.line_y() - item.line_y()).abs() <= prev.font_size.max(item.font_size) * 0.5
        && digits(prev.text.trim())
        && digits(item.text.trim())
        && item.x < prev.x + prev.width
        && prev.x < item.x + item.width
}

/// Append an item's text, wrapping a super/subscript run in its tag.
/// Shared by line rendering and table-cell joining so both emit the same
/// markup for a run.
pub(crate) fn push_item_text(result: &mut String, item: &TextItem, text: &str) {
    let tag = if item.baseline_shift > 0.0 {
        "sup"
    } else if item.baseline_shift < 0.0 {
        "sub"
    } else {
        result.push_str(text);
        return;
    };
    result.push('<');
    result.push_str(tag);
    result.push('>');
    result.push_str(text);
    result.push_str("</");
    result.push_str(tag);
    result.push('>');
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
            let emit_space =
                needs_space || (has_leading_space && !result.is_empty() && !result.ends_with(' '));

            // A super/subscript run is wrapped in `<sup>`/`<sub>` (see
            // `text_plain`). It neither opens nor closes the other styles: a
            // footnote marker inside a bold name keeps the bold run intact
            // ("**Yibo Yan<sup>1</sup>, Jiahao Huo**") instead of splitting
            // it around the marker.
            let is_script = item.is_script();

            // Check for style changes. Source decorations are exclusive:
            // `<u>`/`<s>` content stays free of `**`/`*` markers — consumers
            // (and the eval harnesses this feeds) match tag content literally,
            // and mixed nesting breaks that. A struck-and-underlined item is
            // emitted as struck text because deletion is the stronger semantic
            // distinction in redline documents.
            let own_strikeout = format_decorations && item.is_strikeout;
            let own_underline = format_decorations && item.is_underline && !own_strikeout;
            let own_bold = format_bold && item.is_bold && !own_underline && !own_strikeout;
            let own_italic = format_italic && item.is_italic && !own_underline && !own_strikeout;
            // A script run inherits whatever body style is open around it
            // (see above) — its own bold/italic is noise (italic math indices
            // would shatter into `*<sub>t</sub>*` fragments) — but a run
            // carrying its own DECORATION, an underlined link marker in plain
            // text, keeps it: decorations are drawn ink, not font styling.
            let (item_strikeout, item_underline, item_bold, item_italic) = if is_script {
                (
                    current_strikeout,
                    current_underline,
                    current_bold,
                    current_italic,
                )
            } else {
                (own_strikeout, own_underline, own_bold, own_italic)
            };
            // A run's own decoration is emitted around the run itself and
            // closed right after it, so it never leaks onto the next run.
            let own_script_tag = if !is_script {
                None
            } else if own_strikeout && !current_strikeout {
                Some("s")
            } else if own_underline && !current_underline && !current_strikeout {
                Some("u")
            } else {
                None
            };

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
            if emit_space {
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

            if i > 0 && stacked_fraction_slash(&self.items[i - 1], item) {
                result.push('/');
            }
            match own_script_tag {
                Some(tag) => {
                    result.push('<');
                    result.push_str(tag);
                    result.push('>');
                    push_item_text(&mut result, item, text_trimmed);
                    result.push_str("</");
                    result.push_str(tag);
                    result.push('>');
                }
                None => push_item_text(&mut result, item, text_trimmed),
            }
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

    /// Get plain text without formatting.
    ///
    /// A super/subscript run (an item with a non-zero `baseline_shift`;
    /// extraction materializes each run as one item) is wrapped in
    /// `<sup>…</sup>` / `<sub>…</sub>`: without the tags the marker digits
    /// would be indistinguishable from the body text they follow
    /// ("Yibo Yan1,2,3" vs "Yibo Yan<sup>1,2,3</sup>").
    fn text_plain(&self) -> String {
        let single_char_threshold = self.adaptive_threshold;

        let mut result = String::new();
        for (i, item) in self.items.iter().enumerate() {
            if i > 0
                && self.needs_space_between(
                    &self.items[i - 1],
                    item,
                    &result,
                    single_char_threshold,
                )
            {
                result.push(' ');
            }
            if i > 0 && stacked_fraction_slash(&self.items[i - 1], item) {
                result.push('/');
            }
            push_item_text(&mut result, item, item.text.as_str());
        }
        result
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

        // Check if space already exists
        let prev_ends_with_space = result.ends_with(' ');
        let curr_starts_with_space = text.starts_with(' ');
        let space_already_exists = prev_ends_with_space || curr_starts_with_space;

        // Script runs flagged by extraction share one edge-spacing policy
        // with table cells (see `script_edge_needs_space`). The blanket
        // suppression below is for unflagged size changes only.
        if let Some(needs_space) = script_edge_needs_space(prev_item, item, result, text) {
            return needs_space;
        }

        // Detect subscript/superscript: smaller font size and/or Y offset
        let font_ratio = item.font_size / prev_item.font_size;
        let reverse_font_ratio = prev_item.font_size / item.font_size;
        let y_diff = (item.y - prev_item.y).abs();

        let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
        let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

        // Use position-based spacing detection
        let should_join = should_join_items(prev_item, item, single_char_threshold);

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
            font_tag: String::new(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: strikeout,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
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

    /// A body-text item at 12pt on the shared baseline.
    fn body(text: &str, x: f32, width: f32) -> TextItem {
        item(text, x, width, false)
    }

    /// A script run at 8pt, `shift` points off the 12pt body baseline
    /// (positive = raised), as `merge_subscript_items` materializes it: one
    /// item per run.
    fn script(text: &str, x: f32, width: f32, shift: f32) -> TextItem {
        let mut it = item(text, x, width, false);
        it.font_size = 8.0;
        it.height = 8.0;
        it.y += shift;
        it.baseline_shift = shift;
        it
    }

    #[test]
    fn script_run_is_wrapped_in_one_sup_span() {
        // Author block: "Yibo Yan" + the raised "1,2" run + body ", " +
        // "Jiahao Huo". The marker run is one <sup> span attached to the
        // name, and the body comma follows without a space.
        let line = line(vec![
            body("Yibo Yan", 10.0, 48.0),
            script("1,2", 58.0, 10.0, 4.3),
            body(",", 68.0, 3.0),
            body("Jiahao Huo", 74.5, 60.0),
        ]);
        assert_eq!(line.text(), "Yibo Yan<sup>1,2</sup>, Jiahao Huo");
        assert_eq!(
            line.text_with_formatting(true, true, true),
            "Yibo Yan<sup>1,2</sup>, Jiahao Huo"
        );
    }

    #[test]
    fn word_space_after_script_run_follows_geometry() {
        let line = line(vec![
            body("word", 10.0, 24.0),
            script("2", 34.0, 4.0, 4.0),
            body("next", 41.5, 24.0),
        ]);
        assert_eq!(line.text(), "word<sup>2</sup> next");

        // Tight junction after the marker: no space ("x<sup>2</sup>y").
        let line = super::TextLine {
            items: vec![
                body("x", 10.0, 6.0),
                script("2", 16.2, 4.0, 4.0),
                body("y", 20.4, 6.0),
            ],
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.1,
        };
        assert_eq!(line.text(), "x<sup>2</sup>y");
    }

    #[test]
    fn leading_script_run_attaches_to_following_word() {
        // Affiliation line: markers lead their institution, and a word space
        // before the run (after the previous institution's comma) survives.
        let line = line(vec![
            body("University,", 10.0, 60.0),
            script("1,2", 73.4, 10.2, 3.5),
            body("Hong Kong", 83.6, 54.0),
        ]);
        assert_eq!(line.text(), "University, <sup>1,2</sup>Hong Kong");
    }

    #[test]
    fn lowered_run_uses_sub_tag() {
        let line = line(vec![body("x", 10.0, 6.0), script("max", 16.0, 12.0, -2.4)]);
        assert_eq!(line.text(), "x<sub>max</sub>");
    }

    #[test]
    fn script_span_does_not_split_a_bold_run() {
        let mut name = body("Yibo Yan", 10.0, 48.0);
        name.is_bold = true;
        let mut rest = body(", Jiahao Huo", 62.0, 66.0);
        rest.is_bold = true;
        let line = line(vec![name, script("1", 58.0, 4.0, 4.3), rest]);
        assert_eq!(
            line.text_with_formatting(true, false, false),
            "**Yibo Yan<sup>1</sup>, Jiahao Huo**"
        );
        assert_eq!(line.text(), "Yibo Yan<sup>1</sup>, Jiahao Huo");
    }

    #[test]
    fn line_y_of_an_upside_down_run_is_its_baseline() {
        // A 180° run hangs from its box top: that is the baseline its line
        // groups by, a script offset still applies below it, and an upright
        // run keeps `y`.
        let mut run = item("x", 100.0, 20.0, false);
        run.y = 500.0;
        run.height = 10.0;
        run.rotation = 180.0;
        assert_eq!(run.baseline_y(), 510.0);
        assert_eq!(run.line_y(), 510.0);
        run.baseline_shift = 2.0;
        assert_eq!(run.line_y(), 508.0);
        run.rotation = 0.0;
        assert_eq!(run.baseline_y(), 500.0);
        assert_eq!(run.line_y(), 498.0);
    }

    #[test]
    fn separated_script_items_get_separate_spans() {
        // Two runs with a real gap between them (nothing else on the line)
        // are two spans with a space, never "<sup>1 2</sup>".
        let line = line(vec![
            script("1", 10.0, 4.0, 4.0),
            script("2", 40.0, 4.0, 4.0),
        ]);
        assert_eq!(line.text(), "<sup>1</sup> <sup>2</sup>");
    }

    #[test]
    fn touching_runs_of_different_size_are_separate_spans_without_space() {
        // Nested script: "n" (6pt) attached to the superscript "2" (8pt).
        let mut nested = script("n", 20.2, 3.0, 6.0);
        nested.font_size = 6.0;
        let line = line(vec![
            body("x", 10.0, 6.0),
            script("2", 16.2, 4.0, 4.0),
            nested,
        ]);
        assert_eq!(line.text(), "x<sup>2</sup><sup>n</sup>");
    }

    #[test]
    fn stacked_digit_fraction_renders_with_a_slash() {
        // "3 1/3 bits" set as a case fraction: numerator raised, denominator
        // lowered, both at the same x. Never "3 <sup>13</sup>".
        let mut num = script("1", 52.5, 3.7, 3.96);
        num.font_size = 7.4;
        let mut den = script("3", 52.5, 3.7, -4.0);
        den.font_size = 7.4;
        let line = line(vec![
            body("about 3", 10.0, 40.8),
            num,
            den,
            body("bits", 58.0, 20.0),
        ]);
        assert_eq!(line.text(), "about 3 <sup>1</sup>/<sub>3</sub> bits");
    }

    #[test]
    fn decorated_script_run_keeps_its_own_underline() {
        // An underlined (hyperlinked) footnote marker in plain text.
        let mut marker = script("1", 58.0, 4.0, 4.3);
        marker.is_underline = true;
        let line = line(vec![body("word", 10.0, 48.0), marker]);
        assert_eq!(
            line.text_with_formatting(false, false, true),
            "word<u><sup>1</sup></u>"
        );
    }

    #[test]
    fn own_decoration_does_not_leak_onto_the_next_run() {
        let mut first = script("1", 58.0, 4.0, 4.3);
        first.is_underline = true;
        let second = script("2", 80.0, 4.0, 4.3);
        let line = line(vec![body("word", 10.0, 48.0), first, second]);
        assert_eq!(
            line.text_with_formatting(false, false, true),
            "word<u><sup>1</sup></u> <sup>2</sup>"
        );
    }

    #[test]
    fn own_decoration_nests_inside_an_open_body_decoration() {
        // Underlined body text with a struck footnote marker: the strike
        // nests inside the underline instead of being dropped.
        let mut word = body("word", 10.0, 48.0);
        word.is_underline = true;
        let mut marker = script("1", 58.0, 4.0, 4.3);
        marker.is_strikeout = true;
        let line = line(vec![word, marker]);
        assert_eq!(
            line.text_with_formatting(false, false, true),
            "<u>word<s><sup>1</sup></s></u>"
        );
    }

    #[test]
    fn fraction_slash_needs_one_visual_line() {
        // Opposite-sign digit runs on different lines are not a fraction.
        let mut num = script("1", 52.5, 3.7, 3.96);
        num.font_size = 7.4;
        let mut den = script("3", 52.5, 3.7, -4.0);
        den.font_size = 7.4;
        den.y -= 12.0; // anchored to the next line's body
        assert!(!super::stacked_fraction_slash(&num, &den));
    }

    #[test]
    fn line_y_snaps_scripts_to_the_anchor_baseline() {
        let raised = script("1", 58.0, 4.0, 4.3);
        assert!((raised.y - 104.3).abs() < 1e-4);
        assert!((raised.line_y() - 100.0).abs() < 1e-4);
        assert!(raised.is_script());
        assert!(!body("Yibo Yan", 10.0, 48.0).is_script());
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

    #[test]
    fn is_horizontal_follows_the_baseline_quadrant() {
        let cases = [
            (0.0, true),
            (180.0, true),
            (90.0, false),
            (270.0, false),
            (44.0, true),
            (46.0, false),
            (134.0, false),
            (136.0, true),
            (359.5, true),
            (-90.0, false),
            (450.0, false),
        ];
        for (rotation, horizontal) in cases {
            let mut probe = item("x", 0.0, 10.0, false);
            probe.rotation = rotation;
            assert_eq!(
                probe.is_horizontal(),
                horizontal,
                "rotation {rotation} should be horizontal={horizontal}"
            );
        }
    }

    #[test]
    fn cross_extent_is_the_em_box_whatever_the_orientation() {
        let mut probe = item("x", 0.0, 10.0, false);
        probe.height = 12.0;
        assert_eq!(probe.cross_extent(), 12.0);
        probe.rotation = 90.0;
        probe.width = 12.0;
        probe.height = 200.0;
        assert_eq!(probe.cross_extent(), 12.0);
        // A long diagonal run: both box extents carry the advance, the em
        // is the font size.
        probe.rotation = 30.0;
        probe.width = 180.0;
        probe.height = 110.0;
        assert_eq!(probe.cross_extent(), 12.0);
    }

    #[test]
    fn upright_and_upside_down_split_the_horizontal_half_plane() {
        let mut probe = item("x", 0.0, 10.0, false);
        for (rotation, upright, upside_down) in [
            (0.0, true, false),
            (44.0, true, false),
            (316.0, true, false),
            (180.0, false, true),
            (136.0, false, true),
            (224.0, false, true),
            (90.0, false, false),
            (270.0, false, false),
        ] {
            probe.rotation = rotation;
            assert_eq!(probe.is_upright(), upright, "upright at {rotation}");
            assert_eq!(
                probe.is_upside_down(),
                upside_down,
                "upside_down at {rotation}"
            );
        }
    }
}
