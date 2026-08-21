//! Character classification and text utility functions.
//!
//! Pure helpers that operate on characters, strings, or `TextItem` slices.
//! No PDF parsing happens here — these are shared across the extraction
//! and markdown pipelines.

use crate::types::TextItem;
use unicode_normalization::UnicodeNormalization;

/// Return whether text is an explicit page-number expression.
///
/// This strict form is suitable before layout, where removing one numeric item
/// from substantive text such as `Page 42 explains the result` would lose data.
pub(crate) fn is_explicit_page_number_expression(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    let is_number = |value: &str| {
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    };

    if trimmed.len() <= 4 && is_number(trimmed) {
        return true;
    }

    if trimmed.len() >= 3 && trimmed.starts_with('-') && trimmed.ends_with('-') {
        let inner = trimmed[1..trimmed.len() - 1].trim();
        if is_number(inner) {
            return true;
        }
    }

    let lowercase = trimmed.to_ascii_lowercase();
    if let Some(rest) = lowercase.strip_prefix("page") {
        let words: Vec<&str> = rest.split_whitespace().collect();
        if words.len() >= 3 && is_number(words[0]) && words[1] == "of" && is_number(words[2]) {
            return true;
        }
        if words.len() >= 2 && words[0] == "of" && is_number(words[1]) {
            return true;
        }
        return match words.as_slice() {
            [] | ["of"] => true,
            [number] => is_number(number),
            ["of", total] => is_number(total),
            [number, "of", total] => is_number(number) && is_number(total),
            _ => false,
        };
    }

    let words: Vec<&str> = lowercase.split_whitespace().collect();
    match words.as_slice() {
        [number, "of", total] => is_number(number) && is_number(total),
        _ => false,
    }
}

/// Return whether a completed Markdown line looks like a page number or a
/// labeled running header.
///
/// At this stage the complete line and surrounding breaks are available, so a
/// leading `Page N` remains compatible with the existing header cleanup even
/// when the PDF appends a chapter or document title.
pub(crate) fn is_page_number_line(text: &str) -> bool {
    if is_explicit_page_number_expression(text) {
        return true;
    }

    let lowercase = text.trim().to_ascii_lowercase();
    lowercase.strip_prefix("page").is_some_and(|rest| {
        let mut characters = rest.trim_start().chars().peekable();
        let mut has_page_number = false;
        while characters
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            has_page_number = true;
            characters.next();
        }

        has_page_number && characters.next().is_none_or(char::is_whitespace)
    })
}

/// Check if a character is CJK (Chinese, Japanese, Korean).
/// CJK languages don't use spaces between words, so word-boundary
/// heuristics should not apply when CJK characters are involved.
pub(crate) fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'   // Hangul Jamo
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{3130}'..='\u{318F}' // Hangul Compatibility Jamo
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{FF00}'..='\u{FFEF}' // Halfwidth and Fullwidth Forms
    )
}

pub(crate) fn is_rtl_char(c: char) -> bool {
    matches!(c,
        '\u{0590}'..='\u{05FF}'   // Hebrew
        | '\u{0600}'..='\u{06FF}' // Arabic
        | '\u{0700}'..='\u{074F}' // Syriac
        | '\u{0750}'..='\u{077F}' // Arabic Supplement
        | '\u{0780}'..='\u{07BF}' // Thaana
        | '\u{07C0}'..='\u{07FF}' // NKo
        | '\u{0800}'..='\u{083F}' // Samaritan
        | '\u{0840}'..='\u{085F}' // Mandaic
        | '\u{08A0}'..='\u{08FF}' // Arabic Extended-A
        | '\u{FB1D}'..='\u{FB4F}' // Hebrew Presentation Forms
        | '\u{FB50}'..='\u{FDFF}' // Arabic Presentation Forms-A
        | '\u{FE70}'..='\u{FEFF}' // Arabic Presentation Forms-B
    )
}

pub(crate) fn is_arabic_presentation_form(c: char) -> bool {
    // U+FEFF is BOM/ZWNJ, not an Arabic presentation form despite falling
    // in the Presentation Forms-B codepoint range.
    matches!(c, '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFE}')
}

pub(crate) fn is_rtl_text<I, S>(texts: I) -> bool
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    // Only letters vote: the RTL blocks embed weak-directionality characters
    // (Arabic-Indic digits, number separators, combining marks) that are bidi
    // class AN/NSM per UAX #9, not strong RTL — a digits-only line must stay
    // neutral, matching how ASCII digits don't vote LTR. Combining marks need
    // their own check: vowel points like U+064E are Other_Alphabetic, so
    // is_alphabetic() alone would let a marks-only line vote RTL.
    let (mut rtl, mut ltr) = (0u32, 0u32);
    for t in texts {
        for c in t.as_ref().chars() {
            if !c.is_alphabetic() || unicode_normalization::char::is_combining_mark(c) {
                continue;
            }
            if is_rtl_char(c) {
                rtl += 1;
            } else if !is_cjk_char(c) {
                ltr += 1;
            }
        }
    }
    rtl > 0 && rtl > ltr
}

/// Combining marks by general category (Mn) or nonzero canonical combining
/// class. Both signals are needed: Thaana vowel signs are Mn with ccc 0,
/// while some reordering marks are not Mn.
fn is_combining_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
        || unicode_normalization::char::canonical_combining_class(c) != 0
}

/// Sort a table cell's items into RTL reading order: baseline bands (2pt
/// tolerance) run top-to-bottom, items within a band run right-to-left with
/// embedded LTR phrases restored to screen order. Band-aware sorting keeps
/// sub/superscript baseline jitter from breaking a line's X order, which a
/// plain Y-then-X comparator would (`total_cmp` ties only on identical Y).
pub(crate) fn sort_rtl_cell_items<T>(
    items: &mut [T],
    x_of: impl Fn(&T) -> f32,
    y_of: impl Fn(&T) -> f32,
    text_of: impl Fn(&T) -> &str,
) {
    items.sort_by(|a, b| y_of(b).total_cmp(&y_of(a)));
    let mut start = 0;
    while start < items.len() {
        let y0 = y_of(&items[start]);
        let mut end = start + 1;
        while end < items.len() && (y_of(&items[end]) - y0).abs() <= 2.0 {
            end += 1;
        }
        items[start..end].sort_by(|a, b| x_of(b).total_cmp(&x_of(a)));
        restore_embedded_ltr_runs(&mut items[start..end], &text_of);
        start = end;
    }
}

pub(crate) fn sort_line_items(items: &mut [TextItem]) {
    let rtl = is_rtl_text(items.iter().map(|i| &i.text));
    if rtl {
        items.sort_by(|a, b| b.x.total_cmp(&a.x));
        restore_embedded_ltr_runs(items, |i| i.text.as_str());
    } else {
        items.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
}

/// Reorder a right-to-left-sorted run of same-baseline items so embedded LTR
/// phrases read in screen order — an item-granularity approximation of BiDi
/// reordering. Within a maximal stretch of consecutive items carrying no RTL
/// characters, the span from the first to the last item holding an
/// alphanumeric is an LTR phrase: on screen it reads left-to-right, so
/// emitting it right-to-left would reverse its words. Neutral items at the
/// stretch's edges join the phrase only when they carry a bracket (BiDi
/// pairs brackets with the text they enclose); other neutrals — sentence
/// periods, commas — take the paragraph's RTL direction and stay put.
pub(crate) fn restore_embedded_ltr_runs<T>(items: &mut [T], text_of: impl Fn(&T) -> &str) {
    // An Arabic-Indic number split across items reads left-to-right even
    // though its digits live in the RTL blocks: rejoin maximal runs of
    // ADJACENT digit items in screen order, bridging decimal/thousands
    // delimiters — Arabic or ASCII — that sit inside or between the pieces.
    // These runs deliberately stay out of the Latin-phrase grouping below:
    // the brackets and operators around an Arabic number belong to the
    // surrounding RTL flow, unlike a Latin phrase's own brackets.
    let is_numeric_sep = |c: char| is_arabic_numeric_separator(c) || matches!(c, '.' | ',');
    let is_arabic_number_item = |t: &T| {
        let mut has_digit = false;
        for c in text_of(t).chars() {
            if is_arabic_indic_digit(c) {
                has_digit = true;
            } else if !is_numeric_sep(c) && !c.is_whitespace() {
                return false;
            }
        }
        has_digit
    };
    // A delimiter emitted as its own item joins the number only when digit
    // pieces flank it — a lone sentence period stays in the RTL flow.
    let is_number_delim_item = |t: &T| {
        let mut has_sep = false;
        for c in text_of(t).chars() {
            if is_numeric_sep(c) {
                has_sep = true;
            } else if !c.is_whitespace() {
                return false;
            }
        }
        has_sep
    };
    let mut i = 0;
    while i < items.len() {
        if !is_arabic_number_item(&items[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() {
            if is_arabic_number_item(&items[j]) {
                j += 1;
            } else if is_number_delim_item(&items[j])
                && j + 1 < items.len()
                && is_arabic_number_item(&items[j + 1])
            {
                j += 2;
            } else {
                break;
            }
        }
        if j - i >= 2 {
            items[i..j].reverse();
        }
        i = j;
    }

    let has_rtl = |t: &T| text_of(t).chars().any(is_rtl_char);
    let has_bracket = |t: &T| text_of(t).chars().any(|c| "()[]{}<>".contains(c));
    let mut i = 0;
    while i < items.len() {
        if has_rtl(&items[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() && !has_rtl(&items[j]) {
            j += 1;
        }
        let alnum = |t: &T| text_of(t).chars().any(char::is_alphanumeric);
        let first = items[i..j].iter().position(&alnum).map(|p| i + p);
        let last = items[i..j].iter().rposition(&alnum).map(|p| i + p);
        if let (Some(mut first), Some(mut last)) = (first, last) {
            while first > i && has_bracket(&items[first - 1]) {
                first -= 1;
            }
            while last + 1 < j && has_bracket(&items[last + 1]) {
                last += 1;
            }
            if last > first {
                items[first..=last].reverse();
            }
        }
        i = j;
    }
}

/// Detect if a font name indicates bold style
/// Common patterns: "Bold", "Bd", "Black", "Heavy", "Demi", "Semi" (semi-bold)
pub fn is_bold_font(font_name: &str) -> bool {
    let lower = font_name.to_lowercase();

    // Check for common bold indicators
    // Note: Need to be careful with "Oblique" not matching "Obl" + false positive for bold
    lower.contains("bold")
        || lower.contains("-bd")
        || lower.contains("_bd")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("demibold")
        || lower.contains("semibold")
        || lower.contains("demi-bold")
        || lower.contains("semi-bold")
        || lower.contains("extrabold")
        || lower.contains("ultrabold")
        || lower.contains("medium") && !lower.contains("mediumitalic") // Some fonts use Medium for semi-bold
        // URW Type 1 fonts abbreviate Medium as "Medi" (e.g. NimbusRomNo9L-Medi,
        // the Times-Bold substitute in LaTeX documents; -MediItal is bold italic).
        || lower.contains("-medi") && !lower.contains("mediumital")
}

/// Detect if a font name indicates italic/oblique style
/// Common patterns: "Italic", "It", "Oblique", "Obl", "Slant", "Inclined"
pub fn is_italic_font(font_name: &str) -> bool {
    let lower = font_name.to_lowercase();

    // Check for common italic indicators
    lower.contains("italic")
        || lower.contains("oblique")
        || lower.contains("-it")
        || lower.contains("_it")
        || lower.contains("slant")
        || lower.contains("inclined")
        || lower.contains("kursiv") // German for italic
}

/// Expand Unicode ligature characters to their component characters.
/// This makes extracted text more searchable and semantically correct.
/// Also applies NFKC normalization (converts Arabic presentation forms to base
/// characters, decomposes Latin ligatures, etc.) and reverses visual-order
/// Arabic text back to logical order when presentation forms are detected.
pub(crate) fn expand_ligatures(text: &str) -> String {
    // Strip null bytes and other control characters (except newline/tab)
    let text = if text
        .bytes()
        .any(|b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
    {
        text.chars()
            .filter(|&c| c >= ' ' || c == '\n' || c == '\r' || c == '\t')
            .collect::<String>()
    } else {
        text.to_string()
    };

    // Detect Arabic presentation forms before normalization — their presence
    // signals visual-order storage that needs reversal after NFKC.
    let had_presentation_forms = text.chars().any(is_arabic_presentation_form);

    // Apply NFKC normalization only when Arabic presentation forms are present.
    // This converts forms (U+FB50-FDFF, U+FE70-FEFF) back to base Arabic
    // (U+0600-06FF). We avoid broad NFKC on all non-ASCII text because it
    // would convert NBSP (U+00A0) to regular space, breaking downstream logic.
    // Latin ligatures are already handled by the explicit match arms below.
    let text = if had_presentation_forms {
        text.nfkc().collect::<String>()
    } else {
        text
    };

    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // Keep explicit ligature expansion as fallback for fonts that bypass
            // NFKC (e.g. custom ToUnicode mappings to PUA codepoints)
            '\u{FB00}' => result.push_str("ff"),
            '\u{FB01}' => result.push_str("fi"),
            '\u{FB02}' => result.push_str("fl"),
            '\u{FB03}' => result.push_str("ffi"),
            '\u{FB04}' => result.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => result.push_str("st"),
            // Strip invisible Unicode characters that pollute markdown output
            '\u{00AD}' => {}              // soft hyphen
            '\u{200B}' => {}              // zero-width space
            '\u{FEFF}' => {}              // BOM / zero-width no-break space
            '\u{200C}' | '\u{200D}' => {} // ZWNJ / ZWJ
            '\u{2060}' => {}              // word joiner
            // Normalize typographic spaces to ASCII space so downstream
            // spacing logic (should_join_items) can detect word boundaries.
            // Excludes NBSP (U+00A0) which is common in PDFs and handled
            // correctly by existing coordinate-based spacing.
            '\u{2000}'..='\u{200A}' => result.push(' '), // en/em/thin/hair spaces etc.
            _ => result.push(ch),
        }
    }

    // If the original text had Arabic presentation forms, the characters are in
    // visual (LTR screen) order. After NFKC normalization, reverse to restore
    // logical reading order.
    if had_presentation_forms {
        result = reverse_visual_arabic(&result);
    }

    result
}

/// Digits that fall inside the Arabic codepoint block but are stored and
/// displayed left-to-right like ASCII digits (bidi class AN): Arabic-Indic
/// (٠-٩) and Extended Arabic-Indic (۰-۹). Reversal must keep their runs
/// intact — reversing them corrupts every number in the document.
fn is_arabic_indic_digit(c: char) -> bool {
    matches!(c, '\u{0660}'..='\u{0669}' | '\u{06F0}'..='\u{06F9}')
}

/// Arabic decimal (U+066B) and thousands (U+066C) separators — punctuation
/// that lives inside numbers and must stay with the forward-ordered digit
/// run around it.
fn is_arabic_numeric_separator(c: char) -> bool {
    matches!(c, '\u{066B}' | '\u{066C}')
}

fn is_forward_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric() || is_arabic_indic_digit(c)
}

/// Reverse visual-order Arabic text to logical order.
///
/// Pure RTL text (no forward-ordered alphanumerics) gets a simple character
/// reversal. Mixed content (embedded numbers or Latin words) splits into LTR
/// and non-LTR runs: run order is reversed, and only non-LTR runs are
/// reversed internally.
fn reverse_visual_arabic(text: &str) -> String {
    // Check if there are any LTR runs (Latin letters or digits of either
    // numbering system)
    let has_ltr = text.chars().any(is_forward_alnum);

    if !has_ltr {
        // Pure RTL: simple reversal
        return reverse_keeping_marks(text);
    }

    // Mixed content: split into runs of LTR (forward-ordered alphanumerics +
    // adjacent punctuation like '.', ',', '/', '-') vs non-LTR (Arabic +
    // spaces + other).
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<(bool, String)> = Vec::new(); // (is_ltr, content)

    let is_ltr_punct = |chars: &[char], i: usize| {
        (chars[i].is_ascii_punctuation() || is_arabic_numeric_separator(chars[i]))
            && is_adjacent_to_alnum(chars, i)
    };
    let mut i = 0;
    while i < chars.len() {
        let is_ltr = is_forward_alnum(chars[i]) || is_ltr_punct(&chars, i);

        let mut run = String::new();
        while i < chars.len() {
            let c = chars[i];
            let c_is_ltr = is_forward_alnum(c) || is_ltr_punct(&chars, i);
            if c_is_ltr != is_ltr {
                break;
            }
            run.push(c);
            i += 1;
        }
        runs.push((is_ltr, run));
    }

    // Reverse run order and reverse non-LTR runs internally
    runs.reverse();
    let mut result = String::with_capacity(text.len());
    for (is_ltr, content) in &runs {
        if *is_ltr {
            result.push_str(content);
        } else {
            result.push_str(&reverse_keeping_marks(content));
        }
    }
    result
}

/// Reverse a string's characters keeping combining marks attached to their
/// base — a naive `chars().rev()` would detach vowel points (Hebrew nikud,
/// Arabic harakat) from the letters they modify. Paired brackets are
/// mirrored: visual-order storage records the on-screen glyph, which is the
/// mirror of the logical character on the other side of the reversal.
fn reverse_keeping_marks(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut end = chars.len();
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        if i == 0 || !is_combining_mark(chars[i]) {
            out.extend(chars[i..end].iter().map(|&c| mirror_bracket(c)));
            end = i;
        }
    }
    out
}

fn mirror_bracket(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        _ => c,
    }
}

/// Check if the character at `idx` is adjacent to a forward-ordered
/// alphanumeric character (ASCII or Arabic-Indic digit).
fn is_adjacent_to_alnum(chars: &[char], idx: usize) -> bool {
    (idx > 0 && is_forward_alnum(chars[idx - 1]))
        || (idx + 1 < chars.len() && is_forward_alnum(chars[idx + 1]))
}

/// A decoded show-op string qualifies for geometric visual-order RTL fixing
/// when RTL characters dominate and at least two are present — a single RTL
/// character reads the same in either storage order. Arabic presentation
/// forms are excluded: their presence already triggers reversal inside
/// `expand_ligatures`, so flagging them here would reverse twice.
/// Arabic-Indic digits don't count toward the threshold: they're stored
/// left-to-right in both conventions, so a bare number carries no evidence
/// and must never be reversed.
pub(crate) fn is_visual_rtl_candidate(text: &str) -> bool {
    if text.chars().any(is_arabic_presentation_form) {
        return false;
    }
    text.chars()
        .filter(|&c| is_rtl_char(c) && !is_arabic_indic_digit(c) && !is_arabic_numeric_separator(c))
        .count()
        >= 2
        && is_rtl_text(std::iter::once(text))
}

/// Reverse multi-character RTL runs stored in visual (screen left-to-right)
/// order back to logical reading order.
///
/// PDF paints glyphs sequentially left-to-right, so producers of visible RTL
/// text emit each run's characters in screen order — reversed relative to
/// reading order — and walk the line's runs left-to-right. Producers that
/// keep logical order instead position each run explicitly, walking
/// right-to-left across the line (common in OCR text layers), and must not
/// be reversed. The two conventions are distinguished geometrically:
/// candidate runs emitted left-to-right along a shared baseline vote for
/// visual storage, right-to-left emission votes for logical storage.
/// `logical_ops` carries extra logical votes observed during parsing — show
/// ops whose internal glyph progression already walks right-to-left.
///
/// Votes are pooled per page deliberately: a page is written by one
/// producer, so its storage convention is uniform, while individual lines
/// are often single-run and carry no votes at all. Scoping the decision per
/// line would push most lines onto the no-vote default, and that default
/// (reverse) is exactly what corrupts logical-order OCR layers.
pub(crate) fn fix_visual_order_rtl(items: &mut [TextItem], candidates: &[usize], logical_ops: u32) {
    if candidates.is_empty() {
        return;
    }
    let mut rightward = 0u32;
    let mut leftward = logical_ops;
    for pair in candidates.windows(2) {
        let (a, b) = (&items[pair[0]], &items[pair[1]]);
        // Same-baseline pairs only: emission order across lines says nothing
        // about horizontal storage direction.
        if (a.y - b.y).abs() > a.height.max(b.height).max(1.0) * 0.5 {
            continue;
        }
        if b.x > a.x + 0.1 {
            rightward += 1;
        } else if b.x < a.x - 0.1 {
            leftward += 1;
        }
    }
    // Ties — including the vote-less single-run case — reverse: RTL text
    // painted with forward advances renders correctly only when stored in
    // visual order, so visual storage is the dominant convention.
    if leftward > rightward {
        return;
    }
    for &idx in candidates {
        items[idx].text = reverse_visual_arabic(&items[idx].text);
    }
}

/// Decode a PDF text string (ActualText, etc.) that may be UTF-16BE (BOM \xFE\xFF)
/// or PDFDocEncoding (Latin-1 superset).
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE with BOM
        let utf16: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| u16::from_be_bytes(*chunk))
            .collect();
        String::from_utf16_lossy(&utf16)
    } else {
        // PDFDocEncoding — identical to Latin-1 for the byte range we care about
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Compute effective font size from base size and text matrix
/// Text matrix is [a, b, c, d, tx, ty] where a,d are scale factors
pub(crate) fn effective_font_size(base_size: f32, text_matrix: &[f32; 6]) -> f32 {
    // The scale factor is typically the magnitude of the transformation
    // For most PDFs, text_matrix[0] (a) is the horizontal scale
    // and text_matrix[3] (d) is the vertical scale
    let scale_x = (text_matrix[0].powi(2) + text_matrix[1].powi(2)).sqrt();
    let scale_y = (text_matrix[2].powi(2) + text_matrix[3].powi(2)).sqrt();
    // Use the larger of the two scales (usually they're equal for non-rotated text)
    let scale = scale_x.max(scale_y);
    base_size * scale
}

/// Estimate the width of a text item, falling back to a character-count heuristic when width is 0.
pub(crate) fn effective_width(item: &TextItem) -> f32 {
    if item.width > 0.0 {
        item.width
    } else {
        item.text.chars().count() as f32 * item.font_size * 0.5
    }
}

pub(crate) fn is_cid_font(font: &str) -> bool {
    font.starts_with("C2_") || font.starts_with("C0_")
}

/// Detect and fix Canva-style letter-spacing within text items.
///
/// Canva-generated PDFs render text character-by-character with CSS-style
/// letter-spacing. The TJ handler inserts spaces between each character,
/// producing items like `"a r i b"` instead of `"arib"`. This function
/// detects such items by checking if the text follows a strict pattern of
/// alternating single characters and spaces, then removes the spurious spaces.
///
/// Only activates when ≥50% of items on the page are letter-spaced, to avoid
/// false positives on normal PDFs with short items like `"a b"`.
///
/// Returns the adaptive join threshold for this page: DEFAULT (0.10) for normal
/// pages, or a higher Otsu-derived threshold for Canva-style pages.
pub(crate) fn fix_letterspaced_items(items: &mut [TextItem]) -> f32 {
    const DEFAULT: f32 = 0.10;

    if items.is_empty() {
        return DEFAULT;
    }

    // Check if the item text matches "x y z" pattern (single chars separated by spaces)
    fn is_letterspaced(text: &str) -> bool {
        let trimmed = text.trim();
        let chars: Vec<char> = trimmed.chars().collect();
        // Need at least 3 chars: "a b" = ['a', ' ', 'b']
        if chars.len() < 3 {
            return false;
        }
        // Pattern: non-space, space, non-space, space, ...
        chars
            .iter()
            .enumerate()
            .all(|(i, &c)| if i % 2 == 0 { c != ' ' } else { c == ' ' })
    }

    // Count how many items are letter-spaced vs total non-trivial items
    let mut letterspaced_count = 0u32;
    let mut total_text_items = 0u32;
    for item in items.iter() {
        let trimmed = item.text.trim();
        if trimmed.is_empty() || trimmed.len() < 3 {
            continue;
        }
        total_text_items += 1;
        if is_letterspaced(&item.text) {
            letterspaced_count += 1;
        }
    }

    // Only fix if ≥50% of substantial items are letter-spaced
    if total_text_items < 4 || letterspaced_count * 2 < total_text_items {
        // Second detection path: per-character rendering without embedded spaces.
        // Canva sometimes emits each character as a separate TextItem (no "a b c"
        // pattern within items). Detect by checking if >50% of items are single chars.
        let single_char_count = items
            .iter()
            .filter(|i| i.text.trim().chars().count() == 1)
            .count();
        if items.len() >= 10 && single_char_count * 2 >= items.len() {
            let threshold = compute_canva_join_threshold(items);
            if threshold > 0.40 {
                return threshold;
            }
        }
        return DEFAULT;
    }
    // Compute threshold BEFORE removing spaces. Since we've confirmed this
    // is a Canva-style page (≥50% letterspaced), use the ungated variant
    // that includes all pairs — the char-count guard in the normal function
    // would filter out long letterspaced items like "i s s i o n" (11 chars).
    let threshold = compute_canva_join_threshold(items);

    // Remove spaces from letter-spaced items
    for item in items.iter_mut() {
        if is_letterspaced(&item.text) {
            let fixed: String = item.text.chars().filter(|&c| c != ' ').collect();
            item.text = fixed;
        }
    }

    threshold
}

/// Compute join threshold for a confirmed Canva-style page.
///
/// Uses `median × 1.55` on the gap/font_size ratio distribution. The page-level
/// threshold is used for multi-char item pairs; single-char pairs use
/// character-width–based joining in `should_join_items` instead.
fn compute_canva_join_threshold(items: &[TextItem]) -> f32 {
    const DEFAULT: f32 = 0.10;
    const MIN_SAMPLES: usize = 8;

    let ratios = collect_gap_ratios(items);
    if ratios.len() < MIN_SAMPLES {
        return DEFAULT;
    }

    let mut sorted: Vec<f32> = ratios;
    sorted.sort_by(|a, b| a.total_cmp(b));

    if sorted[sorted.len() - 1] < 0.40 || sorted[0] < 0.40 {
        return DEFAULT;
    }

    let median = sorted[sorted.len() / 2];
    (median * 1.55).clamp(0.50, 2.0)
}

/// Collect positive gap/font_size ratios from adjacent item pairs,
/// filtering out CJK, zero-width, and out-of-range values.
fn collect_gap_ratios(items: &[TextItem]) -> Vec<f32> {
    let mut ratios: Vec<f32> = Vec::new();
    for pair in items.windows(2) {
        let prev = &pair[0];
        let curr = &pair[1];

        let prev_c = prev.text.trim().chars().last();
        let curr_c = curr.text.trim().chars().next();
        if prev_c.is_some_and(is_cjk_char) || curr_c.is_some_and(is_cjk_char) {
            continue;
        }

        if prev.width <= 0.0 || prev.font_size <= 0.0 {
            continue;
        }

        let gap = if prev.x <= curr.x {
            curr.x - (prev.x + prev.width)
        } else {
            prev.x - (curr.x + curr.width)
        };

        let ratio = gap / prev.font_size;

        if (0.0..=3.0).contains(&ratio) {
            ratios.push(ratio);
        }
    }
    ratios
}

/// Compute an adaptive join threshold for text items on a line.
///
/// Uses Otsu's method on the gap/font_size ratio distribution to find the
/// natural split between intra-word and inter-word gaps. With per-pair
/// char-count guard (both items ≥ 5 chars → skip). Used only in tests;
/// production code uses `compute_canva_join_threshold` via `fix_letterspaced_items`.
#[cfg(test)]
fn compute_single_char_join_threshold(items: &[TextItem]) -> f32 {
    const DEFAULT: f32 = 0.10;
    const MIN_SAMPLES: usize = 8;

    // Collect gap/font_size ratios for adjacent pairs involving at least one
    // short fragment (< 5 chars). This detects per-character rendering
    // (Canva-style) without being fooled by uniform word-level spacing.
    let mut ratios: Vec<f32> = Vec::new();
    for pair in items.windows(2) {
        let prev = &pair[0];
        let curr = &pair[1];

        let prev_chars = prev.text.trim().chars().count();
        let curr_chars = curr.text.trim().chars().count();

        // Require at least one item to be a short fragment.
        // Pairs of long words (both ≥ 5 chars) indicate normal text.
        if prev_chars >= 5 && curr_chars >= 5 {
            continue;
        }

        // Skip CJK pairs
        let prev_c = prev.text.trim().chars().last();
        let curr_c = curr.text.trim().chars().next();
        if prev_c.is_some_and(is_cjk_char) || curr_c.is_some_and(is_cjk_char) {
            continue;
        }

        if prev.width <= 0.0 || prev.font_size <= 0.0 {
            continue;
        }

        let gap = if prev.x <= curr.x {
            curr.x - (prev.x + prev.width)
        } else {
            prev.x - (curr.x + curr.width)
        };

        let ratio = gap / prev.font_size;

        // Skip negative gaps and huge gaps (> 3× font_size)
        if !(0.0..=3.0).contains(&ratio) {
            continue;
        }

        ratios.push(ratio);
    }

    if ratios.len() < MIN_SAMPLES {
        return DEFAULT;
    }

    ratios.sort_by(|a, b| a.total_cmp(b));

    // If all gaps are tight (max < 0.40), use default — normal PDF
    let max_ratio = ratios[ratios.len() - 1];
    if max_ratio < 0.40 {
        return DEFAULT;
    }

    // If the minimum gap is below 0.40, there's a mix of tight and wide gaps,
    // meaning this isn't a uniform letter-spacing PDF — use default.
    // Canva-style letter-spacing has min gaps ≈ 0.5× font_size; normal
    // justified text gaps are ≈ 0.15–0.30× font_size.
    if ratios[0] < 0.40 {
        return DEFAULT;
    }

    // All gaps are wide (≥0.25× font_size) — Canva-style letter-spacing.
    // Use Otsu to find the split between intra-word and inter-word gaps.
    let n = ratios.len() as f32;
    let total_sum: f32 = ratios.iter().sum();

    let mut best_threshold = DEFAULT;
    let mut best_variance = f32::NEG_INFINITY;

    let mut w0: f32 = 0.0;
    let mut sum0: f32 = 0.0;

    for i in 0..ratios.len() - 1 {
        w0 += 1.0;
        sum0 += ratios[i];

        let w1 = n - w0;
        if w1 == 0.0 {
            break;
        }

        let mean0 = sum0 / w0;
        let mean1 = (total_sum - sum0) / w1;
        let variance = w0 * w1 * (mean0 - mean1).powi(2);

        // Only consider thresholds at value boundaries (skip duplicates)
        if i + 1 < ratios.len() && (ratios[i + 1] - ratios[i]).abs() < 1e-6 {
            continue;
        }

        if variance > best_variance {
            best_variance = variance;
            // Place threshold midway between the two classes
            best_threshold = (ratios[i] + ratios[i + 1]) / 2.0;
        }
    }

    best_threshold.clamp(0.05, 2.0)
}

/// Determine if two adjacent text items should be joined without a space
/// based on their physical positions on the page and character case.
/// Uses a hybrid approach: position-based with case-aware thresholds.
/// CID fonts emit one word per text operator with gaps ≈ 0 between words.
/// Non-CID (Type1/TrueType) fonts emit phrases or fragments.
pub(crate) fn should_join_items(
    prev_item: &TextItem,
    curr_item: &TextItem,
    single_char_threshold: f32,
) -> bool {
    // If either text explicitly has leading/trailing spaces, respect them
    if prev_item.text.ends_with(' ') || curr_item.text.starts_with(' ') {
        return false;
    }

    // Get the last character of previous and first character of current
    let prev_last = prev_item.text.trim_end().chars().last();
    let curr_first = curr_item.text.trim_start().chars().next();

    // Always join if current starts with punctuation that typically follows without space
    // e.g., "www" + ".com" → "www.com", not "www .com"
    if let Some(c) = curr_first {
        if matches!(c, '.' | ',' | ';' | '!' | '?' | ')' | ']' | '}' | '\'') {
            return true;
        }
    }

    // After colons, add space if followed by alphanumeric (typical label:value pattern)
    // e.g., "Clave:" + "T9N2I6" → "Clave: T9N2I6"
    if let (Some(p), Some(c)) = (prev_last, curr_first) {
        if p == ':' && c.is_alphanumeric() {
            return false;
        }
    }

    // When we have accurate width from font metrics, use a tight threshold
    if prev_item.width > 0.0 {
        let gap = if prev_item.x <= curr_item.x {
            // LTR: prev is left of curr
            curr_item.x - (prev_item.x + prev_item.width)
        } else {
            // RTL: prev is right of curr
            prev_item.x - (curr_item.x + curr_item.width)
        };
        let font_size = prev_item.font_size;

        // Never join across column-scale gaps or large overlaps.
        // Large negative gaps arise when Tc/Tw inflate item widths past
        // where adjacent items actually start.
        if gap > font_size * 3.0 || gap < -font_size {
            return false;
        }

        // CID fonts (C2_*, C0_*) emit one word per text operator with gaps ≈ 0
        // between words. Detect these and add spaces. Only applies to CID fonts —
        // non-CID fonts (Type1/TrueType) emit phrases or fragments with small gaps
        // from positioning imprecision and should NOT trigger this.
        // Skip for CJK text — CJK languages don't use spaces between words.
        let prev_chars = prev_item.text.trim().chars().count();
        let curr_chars = curr_item.text.trim().chars().count();
        let prev_last_char = prev_item.text.trim().chars().last();
        let curr_first_char = curr_item.text.trim().chars().next();
        let is_cjk =
            prev_last_char.is_some_and(is_cjk_char) || curr_first_char.is_some_and(is_cjk_char);

        if !is_cjk && gap >= 0.0 && gap < font_size * 0.01 && is_cid_font(&prev_item.font) {
            let prev_word_count = prev_item.text.split_whitespace().count();

            if prev_word_count >= 3 {
                // Multi-word phrase from a line-level CID operator — likely mid-word boundary
                return gap < font_size * 0.15;
            }

            // CID font: each text operator is a separate word. Always add space.
            return false;
        }

        // Numeric continuity: digits, commas, periods, and percent signs that
        // are positioned close together are almost always a single number.
        // e.g., "34,20" + "8" → "34,208", "+13." + "0" + "%" → "+13.0%"
        // Use a generous threshold since word spaces in numbers are rare.
        // The lower bound (-font_size) rejects large overlaps caused by
        // Tc/Tw–inflated item widths that make adjacent items appear to
        // occupy the same space.
        if let (Some(p), Some(c)) = (prev_last, curr_first) {
            let prev_is_numeric = p.is_ascii_digit() || p == ',' || p == '.';
            let curr_is_numeric = c.is_ascii_digit() || c == '%' || c == '.';
            if prev_is_numeric && curr_is_numeric {
                return gap > -font_size && gap < font_size * 0.3;
            }
            // Sign characters (+/-) followed by digits
            if (p == '+' || p == '-') && c.is_ascii_digit() {
                return gap > -font_size && gap < font_size * 0.3;
            }
        }

        // When the adaptive threshold indicates Canva-style letter-spacing
        // (all gaps wide), use character-width–based joining.
        //
        // Canva renders text character-by-character with CSS-style letter-spacing.
        // For single-char prev items, gap/char_width gives a clean separation
        // (~0.9–1.05 for letter gaps, ~1.5+ for word gaps).
        // For multi-char prev, avg_char_width normalizes for character mix.
        // Multi→multi pairs use the page-level threshold (gap/font_size).
        if single_char_threshold > 0.20 {
            if prev_chars == 1 {
                // Single-char prev: its rendered width is an accurate reference
                return gap < prev_item.width * 1.25;
            }
            if curr_chars == 1 {
                // Multi→single: avg char width of prev normalises for
                // wide/narrow character mix (e.g. "ilw" includes i,l,w)
                let avg_char_width = prev_item.width / prev_chars as f32;
                return gap < avg_char_width * 1.25;
            }
            // Both multi-char: use page-level threshold
            return gap < font_size * single_char_threshold;
        }

        // Single-character fragment joined to a multi-character item: use a
        // moderately generous threshold to rejoin split words like "b" + "illion"
        // or "C" + "ultural". Gap near 0 = same word; gap ~0.2+ = different words.
        if (prev_chars == 1) != (curr_chars == 1) {
            return gap < font_size * 0.20;
        }

        // Both single-char: per-glyph positioning (character-by-character rendering).
        // Intra-word gaps are ≈ 0, word boundaries are ≈ 0.15× font_size.
        // For numeric chars (digits within "100,000"), use generous threshold.
        // For alphabetic, use tight threshold (0.10) to reliably detect word
        // boundaries in per-character PDFs like SEC filings.
        if prev_chars == 1 && curr_chars == 1 {
            if let (Some(p), Some(c)) = (prev_last, curr_first) {
                let p_numeric = p.is_ascii_digit() || matches!(p, ',' | '.' | '%' | '+' | '-');
                let c_numeric = c.is_ascii_digit() || matches!(c, ',' | '.' | '%');
                if p_numeric && c_numeric {
                    return gap < font_size * 0.25;
                }
            }
            return gap < font_size * single_char_threshold;
        }

        // With accurate widths, a gap < 15% of font size means glyphs are
        // adjacent (same word). Anything larger is a deliberate space.
        // For multi-char items with a lowercase→lowercase junction, use a
        // slightly wider threshold (0.18) to avoid mid-word space injection
        // with imprecise CID font metrics (e.g. "enterta"+"inment").
        // All-caps or mixed-case junctions keep the tighter 0.15 threshold
        // to preserve word boundaries (e.g. "LCOE"+"WITH").
        if prev_item.text.trim().chars().count() >= 2 && curr_item.text.trim().chars().count() >= 2
        {
            let prev_ends_lower = prev_item
                .text
                .trim()
                .chars()
                .last()
                .is_some_and(|c| c.is_lowercase());
            let curr_starts_lower = curr_item
                .text
                .trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_lowercase());
            if prev_ends_lower && curr_starts_lower {
                return gap < font_size * 0.18;
            }
        }
        return gap < font_size * 0.15;
    }

    // Fallback: estimate width from font size heuristics
    let char_width = prev_item.font_size * 0.45;

    let prev_text_len = prev_item.text.chars().count() as f32;
    let estimated_prev_width = prev_text_len * char_width;

    // Calculate expected end position of previous item
    let prev_end_x = prev_item.x + estimated_prev_width;

    // Calculate gap between items
    let gap = curr_item.x - prev_end_x;

    // Never join across column-scale gaps (fallback path)
    if gap > char_width * 6.0 {
        return false;
    }

    // CJK text: always join adjacent items — CJK languages don't use spaces between words.
    // The Latin case-based heuristics below would incorrectly insert spaces within CJK words.
    let is_cjk = prev_last.is_some_and(is_cjk_char) || curr_first.is_some_and(is_cjk_char);
    if is_cjk {
        return gap < char_width * 0.8;
    }

    // Use different thresholds based on character case
    // Same-case sequences (ALL CAPS or all lowercase) are more likely to be
    // word fragments that got split. Mixed case suggests word boundaries.
    match (prev_last, curr_first) {
        (Some(p), Some(c)) if p.is_alphabetic() && c.is_alphabetic() => {
            let same_case =
                (p.is_uppercase() && c.is_uppercase()) || (p.is_lowercase() && c.is_lowercase());
            if same_case {
                // Same case: use generous threshold (likely same word fragment)
                // e.g., "CONST" + "ANCIA" → "CONSTANCIA"
                gap < char_width * 0.8
            } else if p.is_lowercase() && c.is_uppercase() {
                // Lowercase to uppercase transition (e.g., "presente" → "CONSTANCIA")
                // This is typically a word boundary. In Spanish/English, words don't
                // transition from lowercase to uppercase mid-word.
                // Always add a space for this case, regardless of position.
                false
            } else {
                // Uppercase to lowercase (e.g., "REGISTRO" → "para")
                // Use stricter threshold (likely word boundary)
                gap < char_width * 0.3
            }
        }
        _ => {
            // Non-alphabetic: use moderate threshold
            gap < char_width * 0.5
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    #[test]
    fn bold_font_urw_medi_abbreviation() {
        // URW Type 1 fonts (LaTeX default Times) abbreviate Medium as "Medi"
        assert!(is_bold_font("NROFIU+NimbusRomNo9L-Medi"));
        assert!(is_bold_font("NimbusRomNo9L-MediItal"));
        assert!(!is_bold_font("DSSZWN+NimbusRomNo9L-Regu"));
        assert!(!is_bold_font("NimbusRomNo9L-ReguItal"));
        // Medium-Italic exclusion still holds
        assert!(!is_bold_font("Foo-MediumItalic"));
    }

    #[test]
    fn strip_soft_hyphen() {
        assert_eq!(expand_ligatures("con\u{00AD}tent"), "content");
    }

    #[test]
    fn strip_zero_width_space() {
        assert_eq!(expand_ligatures("hello\u{200B}world"), "helloworld");
    }

    #[test]
    fn strip_bom() {
        assert_eq!(expand_ligatures("\u{FEFF}text"), "text");
    }

    #[test]
    fn strip_zwnj_zwj_word_joiner() {
        assert_eq!(expand_ligatures("a\u{200C}b\u{200D}c\u{2060}d"), "abcd");
    }

    #[test]
    fn ligature_plus_invisible_chars() {
        assert_eq!(expand_ligatures("\u{FB01}rst\u{00AD}ly"), "firstly");
    }

    #[test]
    fn ligatures_still_expand() {
        assert_eq!(expand_ligatures("\u{FB00}\u{FB01}\u{FB02}"), "fffifl");
    }

    #[test]
    fn normalize_typographic_spaces() {
        // EM SPACE, EN SPACE, THIN SPACE → ASCII space
        assert_eq!(expand_ligatures("•\u{2003}text"), "• text");
        assert_eq!(expand_ligatures("a\u{2002}b"), "a b");
        assert_eq!(expand_ligatures("x\u{2009}y"), "x y");
    }

    #[test]
    fn nbsp_preserved() {
        // NBSP (U+00A0) should NOT be normalized
        assert_eq!(expand_ligatures("a\u{00A0}b"), "a\u{00A0}b");
    }

    #[test]
    fn nfkc_arabic_presentation_forms() {
        // Arabic Presentation Form-B: FEE1 = MEEM medial, FEF3 = YEH initial
        // NFKC maps these to base Arabic + reversal restores logical order
        let input = "\u{FEE1}\u{FEF3}"; // visual order: medial meem, initial yeh
        let result = expand_ligatures(input);
        // After NFKC: base Arabic chars; after reversal: logical order
        assert!(
            !result.chars().any(is_arabic_presentation_form),
            "presentation forms should be normalized: {result:?}"
        );
        assert!(
            result.chars().any(|c| matches!(c, '\u{0600}'..='\u{06FF}')),
            "should contain base Arabic characters: {result:?}"
        );
    }

    #[test]
    fn no_reversal_for_base_arabic() {
        // Base Arabic already in logical order — no presentation forms means no reversal
        let input = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"; // مرحبا
        let result = expand_ligatures(input);
        assert_eq!(result, input, "base Arabic should pass through unchanged");
    }

    #[test]
    fn latin_text_unaffected() {
        assert_eq!(expand_ligatures("Hello World"), "Hello World");
    }

    #[test]
    fn reverse_visual_arabic_pure_rtl() {
        // Pure RTL: simple reversal
        let input = "\u{0628}\u{0627}"; // ba (visual order)
        let result = reverse_visual_arabic(input);
        assert_eq!(result, "\u{0627}\u{0628}"); // ab (logical order)
    }

    #[test]
    fn reverse_visual_arabic_with_ltr_run() {
        // Mixed: Arabic + embedded number "123" + Arabic
        // Visual order: أ 123 ب  → runs: [أ], [123], [ب]
        // Reversed runs: [ب], [123], [أ]
        // Non-LTR reversed internally: ب, 123, أ
        let input = "\u{0623}123\u{0628}";
        let result = reverse_visual_arabic(input);
        assert_eq!(result, "\u{0628}123\u{0623}");
    }

    #[test]
    fn arabic_presentation_form_detection() {
        // Presentation Forms-A range
        assert!(is_arabic_presentation_form('\u{FB50}'));
        assert!(is_arabic_presentation_form('\u{FDFF}'));
        // Presentation Forms-B range (excludes U+FEFF which is BOM)
        assert!(is_arabic_presentation_form('\u{FE70}'));
        assert!(is_arabic_presentation_form('\u{FEFE}'));
        assert!(!is_arabic_presentation_form('\u{FEFF}'));
        // Base Arabic — NOT presentation form
        assert!(!is_arabic_presentation_form('\u{0645}'));
        // Latin
        assert!(!is_arabic_presentation_form('A'));
    }

    #[test]
    fn reverse_keeping_marks_attaches_combining_marks() {
        // Visual "בָא" (bet+qamats, alef) reversed must keep the qamats on bet:
        // "אבָ" — not migrate it onto alef.
        let input = "\u{05D1}\u{05B8}\u{05D0}";
        assert_eq!(reverse_keeping_marks(input), "\u{05D0}\u{05D1}\u{05B8}");
    }

    #[test]
    fn reverse_keeping_marks_mirrors_brackets() {
        // Visual-order storage records the on-screen (mirrored) bracket glyph;
        // reversal must mirror it back.
        let input = "(\u{05D0}\u{05D1})";
        assert_eq!(reverse_keeping_marks(input), "(\u{05D1}\u{05D0})");
    }

    #[test]
    fn visual_rtl_candidate_classification() {
        // Multi-char base Hebrew: candidate
        assert!(is_visual_rtl_candidate("\u{05E9}\u{05DC}\u{05D5}\u{05DD}"));
        // Multi-char base Arabic: candidate
        assert!(is_visual_rtl_candidate("\u{0645}\u{0631}\u{062D}"));
        // Single RTL char: reads the same either way — not a candidate
        assert!(!is_visual_rtl_candidate("\u{05E9}"));
        // Arabic presentation forms: expand_ligatures already reverses these
        assert!(!is_visual_rtl_candidate("\u{FEDF}\u{FEE0}"));
        // Latin-dominant with embedded RTL: internal order is Latin's
        assert!(!is_visual_rtl_candidate("the word \u{05E9}\u{05DC} here"));
        // Pure Latin
        assert!(!is_visual_rtl_candidate("Hello"));
        // Arabic-Indic digits are stored left-to-right in both conventions:
        // a bare "٢٤" run must never be reversed into "٤٢"
        assert!(!is_visual_rtl_candidate("\u{0662}\u{0664}"));
        assert!(!is_visual_rtl_candidate("\u{0663}\u{0665},\u{0660}"));
    }

    #[test]
    fn rtl_text_direction_ignores_marks_on_both_sides() {
        // Marks must not count as RTL: one heavily pointed Hebrew letter
        // must not out-vote a longer Latin word in a mixed cell
        assert!(!is_rtl_text(
            ["AB", "\u{05D1}\u{05B8}\u{05B8}\u{05B8}\u{05B8}"].iter()
        ));
        // ...and marks must not count as LTR either (they carry
        // Other_Alphabetic): a vocalized RTL cell (3 letters, 3 points)
        // still out-votes a short Latin item
        assert!(is_rtl_text(
            ["\u{05E9}\u{05B8}\u{05DC}\u{05B8}\u{05DD}\u{05B8}", "ab"].iter()
        ));
        // Thaana vowel signs are Mn with combining class 0 — still marks:
        // they must not count as RTL letters
        assert!(!is_rtl_text(
            ["ABC", "\u{078C}\u{07A6}\u{07A6}\u{07A6}"].iter()
        ));
    }

    #[test]
    fn arabic_numeric_separators_stay_with_digits() {
        // U+066B decimal / U+066C thousands separators live inside numbers:
        // the whole number is one forward run and must not split or reverse.
        let decimal = "\u{0663}\u{0665}\u{066B}\u{0660}"; // ٣٥٫٠
        assert_eq!(reverse_visual_arabic(decimal), decimal);
        assert!(!is_visual_rtl_candidate(decimal));
        let thousands = "\u{0661}\u{066C}\u{0660}\u{0660}\u{0660}"; // ١٬٠٠٠
        assert_eq!(reverse_visual_arabic(thousands), thousands);
    }

    #[test]
    fn reverse_visual_arabic_keeps_arabic_indic_digit_runs() {
        // Visual storage of "٢٤ ساعة" (24 hours): letters are reversed on
        // screen but the digit run stays left-to-right, like ASCII digits.
        let input = "\u{0629}\u{0639}\u{0627}\u{0633} \u{0662}\u{0664}"; // ةعاس ٢٤
        let result = reverse_visual_arabic(input);
        assert_eq!(
            result,
            "\u{0662}\u{0664} \u{0633}\u{0627}\u{0639}\u{0629}" // ٢٤ ساعة
        );
        // Decimal fragment with punctuation glued to digits stays intact
        let decimal = "\u{0663}\u{0665},\u{0660}"; // ٣٥,٠
        assert_eq!(reverse_visual_arabic(decimal), decimal);
    }

    fn make_rtl_item(text: &str, x: f32, y: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: 30.0,
            height: 12.0,
            font: "TestFont".to_string(),
            font_tag: String::new(),
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

    #[test]
    fn fix_visual_order_rtl_reverses_rightward_emission() {
        // Ops painted left-to-right on one baseline = visual storage
        let mut items = vec![
            make_rtl_item("\u{05DD}\u{05DC}\u{05D5}\u{05E2}", 100.0, 700.0), // visual עולם
            make_rtl_item("\u{05DD}\u{05D5}\u{05DC}\u{05E9}", 160.0, 700.0), // visual שלום
        ];
        fix_visual_order_rtl(&mut items, &[0, 1], 0);
        assert_eq!(items[0].text, "\u{05E2}\u{05D5}\u{05DC}\u{05DD}"); // עולם
        assert_eq!(items[1].text, "\u{05E9}\u{05DC}\u{05D5}\u{05DD}"); // שלום
    }

    #[test]
    fn fix_visual_order_rtl_keeps_leftward_emission() {
        // Ops positioned right-to-left = logical storage (OCR layers)
        let logical_a = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}"; // שלום
        let logical_b = "\u{05E2}\u{05D5}\u{05DC}\u{05DD}"; // עולם
        let mut items = vec![
            make_rtl_item(logical_a, 160.0, 700.0),
            make_rtl_item(logical_b, 100.0, 700.0),
        ];
        fix_visual_order_rtl(&mut items, &[0, 1], 0);
        assert_eq!(items[0].text, logical_a);
        assert_eq!(items[1].text, logical_b);
    }

    #[test]
    fn fix_visual_order_rtl_defaults_to_reversal_without_votes() {
        // A single run gives no geometric votes; visible RTL painted with
        // forward advances can only be visual-order storage.
        let mut items = vec![make_rtl_item(
            "\u{05DD}\u{05D5}\u{05DC}\u{05E9}",
            100.0,
            700.0,
        )];
        fix_visual_order_rtl(&mut items, &[0], 0);
        assert_eq!(items[0].text, "\u{05E9}\u{05DC}\u{05D5}\u{05DD}");
    }

    #[test]
    fn fix_visual_order_rtl_logical_ops_outvote() {
        // Extra logical evidence from op-internal geometry blocks reversal
        let logical = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let mut items = vec![
            make_rtl_item(logical, 100.0, 700.0),
            make_rtl_item(logical, 160.0, 700.0),
        ];
        fix_visual_order_rtl(&mut items, &[0, 1], 2);
        assert_eq!(items[0].text, logical);
        assert_eq!(items[1].text, logical);
    }

    #[test]
    fn fix_visual_order_rtl_ignores_cross_line_pairs() {
        // Different baselines carry no horizontal-direction information;
        // with no votes the default (reverse) applies.
        let mut items = vec![
            make_rtl_item("\u{05D1}\u{05D0}", 160.0, 700.0),
            make_rtl_item("\u{05D3}\u{05D2}", 100.0, 650.0),
        ];
        fix_visual_order_rtl(&mut items, &[0, 1], 0);
        assert_eq!(items[0].text, "\u{05D0}\u{05D1}");
        assert_eq!(items[1].text, "\u{05D2}\u{05D3}");
    }

    #[test]
    fn restore_embedded_ltr_runs_screen_order() {
        // RTL-sorted line: hebrew, ")", "KM1", "(", hebrew — the bracketed
        // acronym must come back in screen order: "(", "KM1", ")".
        let mut items = vec![
            "\u{05E9}\u{05DC}".to_string(),
            ")".to_string(),
            "KM1".to_string(),
            "(".to_string(),
            "\u{05D5}\u{05DD}".to_string(),
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items[1], "(");
        assert_eq!(items[2], "KM1");
        assert_eq!(items[3], ")");
    }

    #[test]
    fn restore_embedded_ltr_runs_rejoins_split_arabic_indic_numbers() {
        // An Arabic-Indic number split across items sits in the RTL block
        // but reads left-to-right: after the descending-X sort it must
        // rejoin in screen order like Latin digits would.
        let mut items = vec![
            "\u{0645}\u{0631}".to_string(), // مر
            "\u{0664}".to_string(),         // ٤
            "\u{0662}".to_string(),         // ٢
            "\u{062D}\u{0628}".to_string(), // حب
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items[1], "\u{0662}");
        assert_eq!(items[2], "\u{0664}");
    }

    #[test]
    fn restore_embedded_ltr_runs_bridges_number_delimiters() {
        // "٢٤.٥" split around an ASCII decimal point: descending-X order is
        // [٥][.][٢٤]; the delimiter is flanked by digit pieces, so the whole
        // number rejoins in screen order.
        let mut items = vec![
            "\u{0665}".to_string(),
            ".".to_string(),
            "\u{0662}\u{0664}".to_string(),
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items, ["\u{0662}\u{0664}", ".", "\u{0665}"]);

        // A lone period NOT flanked by a digit piece stays in the RTL flow
        let mut items = vec![
            "\u{0662}\u{0664}".to_string(),
            ".".to_string(),
            "\u{0645}\u{0631}".to_string(),
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items[0], "\u{0662}\u{0664}");
        assert_eq!(items[1], ".");
    }

    #[test]
    fn restore_embedded_ltr_runs_leaves_edge_neutrals() {
        // A sentence-final period after a year stays in the RTL flow:
        // [hebrew]["2020"]["."] must not become [hebrew]["."]["2020"].
        let mut items = vec![
            "\u{05DC}\u{05E9}\u{05E0}\u{05EA}".to_string(),
            "2020".to_string(),
            ".".to_string(),
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items[1], "2020");
        assert_eq!(items[2], ".");
    }

    #[test]
    fn sort_rtl_cell_items_respects_lines_and_jitter() {
        // Two wrapped lines of an RTL cell; the second line's items carry
        // sub/superscript baseline jitter (within the 2pt band). Lines must
        // stay separate top-to-bottom, each line must read right-to-left
        // despite the jitter, and LTR fragments from different visual lines
        // must never be reordered together.
        let mut items = vec![
            (100.0f32, 688.0f32, "\u{05D5}\u{05DD}".to_string()),
            (160.0f32, 688.9f32, "CD".to_string()),
            (100.0f32, 700.0f32, "AB".to_string()),
            (160.0f32, 700.0f32, "\u{05E9}\u{05DC}".to_string()),
        ];
        sort_rtl_cell_items(
            &mut items,
            |(x, _, _)| *x,
            |(_, y, _)| *y,
            |(_, _, t)| t.as_str(),
        );
        let texts: Vec<&str> = items.iter().map(|(_, _, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            ["\u{05E9}\u{05DC}", "AB", "CD", "\u{05D5}\u{05DD}"],
            "line 1 right-to-left, then line 2 right-to-left"
        );
    }

    #[test]
    fn restore_embedded_ltr_runs_multiword_phrase() {
        // Descending-x sort put the LTR phrase words backwards; restore
        // screen order.
        let mut items = vec![
            "\u{05E9}\u{05DC}".to_string(),
            "Board".to_string(),
            "Stability".to_string(),
            "Financial".to_string(),
        ];
        restore_embedded_ltr_runs(&mut items, |s| s.as_str());
        assert_eq!(items[1], "Financial");
        assert_eq!(items[2], "Stability");
        assert_eq!(items[3], "Board");
    }

    /// Helper to create a single-char TextItem at a given x position with width.
    fn make_char_item(ch: char, x: f32, width: f32, font_size: f32) -> TextItem {
        TextItem {
            text: ch.to_string(),
            x,
            y: 100.0,
            width,
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
        }
    }

    #[test]
    fn otsu_threshold_sec_style_tight_gaps() {
        // SEC-style: intra-word gaps ≈ 0, word gap ≈ 0.15× font_size
        // All gaps tight → should return default 0.10
        let fs = 12.0;
        let char_w = fs * 0.5;
        let mut items = Vec::new();
        // 15 chars with gap ≈ 0 (intra-word)
        for i in 0..15 {
            let x = 100.0 + i as f32 * (char_w + fs * 0.01);
            items.push(make_char_item('a', x, char_w, fs));
        }
        // Word gap
        let word_x = items.last().unwrap().x + char_w + fs * 0.15;
        items.push(make_char_item('b', word_x, char_w, fs));
        // 5 more tight chars
        for i in 1..5 {
            let x = word_x + i as f32 * (char_w + fs * 0.01);
            items.push(make_char_item('c', x, char_w, fs));
        }

        let threshold = compute_single_char_join_threshold(&items);
        // Max gap is 0.15, but most are 0.01 → max < 0.20 → default
        assert!(
            (threshold - 0.10).abs() < 0.01,
            "SEC-style should return default ~0.10, got {threshold}"
        );
    }

    #[test]
    fn otsu_threshold_canva_style_wide_gaps() {
        // Canva-style: intra-word gaps ≈ 0.6× font_size, word gaps ≈ 1.2× font_size
        let fs = 12.0;
        let char_w = fs * 0.5;
        let intra_gap = fs * 0.6;
        let word_gap = fs * 1.2;
        let mut items = Vec::new();

        // Word 1: 8 chars with intra-word spacing
        for i in 0..8 {
            let x = 100.0 + i as f32 * (char_w + intra_gap);
            items.push(make_char_item('K', x, char_w, fs));
        }
        // Word gap
        let word_x = items.last().unwrap().x + char_w + word_gap;
        items.push(make_char_item('T', word_x, char_w, fs));
        // Word 2: 7 more chars
        for i in 1..7 {
            let x = word_x + i as f32 * (char_w + intra_gap);
            items.push(make_char_item('o', x, char_w, fs));
        }

        let threshold = compute_single_char_join_threshold(&items);
        // Should find threshold between 0.6 and 1.2 → roughly 0.9
        assert!(
            threshold > 0.5 && threshold < 1.1,
            "Canva-style should find threshold ~0.9, got {threshold}"
        );
    }

    #[test]
    fn otsu_threshold_few_samples_returns_default() {
        // < 8 single-char pairs → default
        let fs = 12.0;
        let char_w = fs * 0.5;
        let items: Vec<TextItem> = (0..5)
            .map(|i| make_char_item('x', 100.0 + i as f32 * (char_w + 1.0), char_w, fs))
            .collect();

        let threshold = compute_single_char_join_threshold(&items);
        assert!(
            (threshold - 0.10).abs() < 0.01,
            "few samples should return default 0.10, got {threshold}"
        );
    }

    #[test]
    fn fix_letterspaced_items_returns_adaptive_threshold() {
        // Simulate Canva page with many letter-spaced items and word gaps.
        // Needs ≥8 inter-item gaps for the threshold to be computed.
        let fs = 12.0;
        let char_w = fs * 0.5;
        let letter_gap = fs * 0.6; // 0.6× font_size between items
        let word_gap = fs * 1.2; // 1.2× font_size between words

        let words: Vec<&str> = vec![
            "H e l l o",
            "W o r l d",
            "F o o",
            "B a r",
            "B a z",
            "Q u x",
            "T e s t",
            "D a t a",
            "M o r e",
            "T e x t",
        ];

        let mut items = Vec::new();
        let mut x = 100.0;
        for (wi, word) in words.iter().enumerate() {
            let char_count = word.chars().filter(|c| !c.is_whitespace()).count();
            let w = char_count as f32 * char_w + (char_count - 1) as f32 * letter_gap;
            items.push(TextItem {
                text: word.to_string(),
                x,
                y: 100.0,
                width: w,
                height: fs,
                font: "TestFont".to_string(),
                font_tag: String::new(),
                font_size: fs,
                page: 1,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            });
            // Alternate between letter-gap and word-gap to create bimodal distribution
            x += w + if wi % 3 == 2 { word_gap } else { letter_gap };
        }

        let threshold = fix_letterspaced_items(&mut items);

        // Threshold should be above default (Canva-style detected)
        assert!(
            threshold > 0.50,
            "Canva page should get threshold > 0.50, got {threshold}"
        );

        // Spaces should be removed from letter-spaced items
        assert_eq!(items[0].text, "Hello");
        assert_eq!(items[1].text, "World");
        assert_eq!(items[2].text, "Foo");
        assert_eq!(items[9].text, "Text");
    }

    #[test]
    fn canva_style_items_join_correctly() {
        // Simulate Canva PDF: "Hello" with 0.6× font_size letter-spacing
        let fs = 12.0;
        let char_w = fs * 0.5;
        let intra_gap = fs * 0.6;
        let word_gap = fs * 1.2;

        let mut items = Vec::new();
        let chars = ['H', 'e', 'l', 'l', 'o'];
        for (i, &ch) in chars.iter().enumerate() {
            let x = 100.0 + i as f32 * (char_w + intra_gap);
            items.push(make_char_item(ch, x, char_w, fs));
        }
        // Space then "W"
        let w_x = items.last().unwrap().x + char_w + word_gap;
        items.push(make_char_item('W', w_x, char_w, fs));
        let chars2 = ['o', 'r', 'l', 'd'];
        for (i, &ch) in chars2.iter().enumerate() {
            let x = w_x + (i + 1) as f32 * (char_w + intra_gap);
            items.push(make_char_item(ch, x, char_w, fs));
        }

        let threshold = compute_single_char_join_threshold(&items);

        // Intra-word pairs should join
        assert!(
            should_join_items(&items[0], &items[1], threshold),
            "H+e should join with threshold {threshold}"
        );
        assert!(
            should_join_items(&items[3], &items[4], threshold),
            "l+o should join with threshold {threshold}"
        );
        // Word boundary should NOT join
        assert!(
            !should_join_items(&items[4], &items[5], threshold),
            "o+W (word boundary) should NOT join with threshold {threshold}"
        );
    }

    /// Helper to create a multi-char TextItem at a given position.
    fn make_text_item(text: &str, x: f32, width: f32, font_size: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y: 100.0,
            width,
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
        }
    }

    #[test]
    fn canva_width_based_single_char_prev_join() {
        // Canva-style: single-char prev uses gap/prev.width < 1.25
        let fs = 12.0;
        let threshold = 0.90; // Canva page threshold

        // "K" (w=7.9) → "a" (gap=8.12): letter gap, ratio=1.028 → JOIN
        let k = make_text_item("K", 100.0, 7.9, fs);
        let a = make_text_item("a", 115.9, 6.0, fs);
        assert!(
            should_join_items(&k, &a, threshold),
            "K→a: gap/width={:.3}, should join",
            (a.x - (k.x + k.width)) / k.width
        );

        // "f" (w=4.0) → "K" (gap=10.47): word boundary, ratio=2.618 → SPLIT
        let f = make_text_item("f", 193.0, 4.0, fs);
        let k2 = make_text_item("K", 207.47, 7.9, fs);
        assert!(
            !should_join_items(&f, &k2, threshold),
            "f→K: gap/width={:.3}, should split",
            (k2.x - (f.x + f.width)) / f.width
        );
    }

    #[test]
    fn canva_width_based_multi_to_single_join() {
        // Multi→single: uses avg_char_width of prev
        let fs = 12.0;
        let threshold = 0.90;

        // "ilw" (w=23.6, 3 chars) → "a" (gap=9.42): intra-word, avg=7.87, ratio=1.197 → JOIN
        let ilw = make_text_item("ilw", 320.0, 23.6, fs);
        let a = make_text_item("a", 353.0, 6.0, fs);
        assert!(
            should_join_items(&ilw, &a, threshold),
            "ilw→a: avg_ratio={:.3}, should join (intra-word 'railway')",
            (a.x - (ilw.x + ilw.width)) / (ilw.width / 3.0)
        );

        // "rich" (w=34.8, 4 chars) → "m" (gap=14.01): word boundary, avg=8.7, ratio=1.610 → SPLIT
        let rich = make_text_item("rich", 229.0, 34.8, fs);
        let m = make_text_item("m", 277.8, 10.7, fs);
        assert!(
            !should_join_items(&rich, &m, threshold),
            "rich→m: avg_ratio={:.3}, should split (word boundary)",
            (m.x - (rich.x + rich.width)) / (rich.width / 4.0)
        );
    }

    #[test]
    fn canva_width_based_multi_to_multi_page_threshold() {
        // Multi→multi: uses page-level threshold (gap/font_size < threshold)
        let fs = 12.0;
        let threshold = 0.90;

        // "rib" (w=25.0) → "ib" (gap=7.01): intra-word, r=0.584 → JOIN
        let rib = make_text_item("rib", 236.0, 25.0, fs);
        let ib = make_text_item("ib", 268.0, 14.0, fs);
        assert!(
            should_join_items(&rib, &ib, threshold),
            "rib→ib: ratio={:.3}, should join (intra-word)",
            (ib.x - (rib.x + rib.width)) / fs
        );

        // "ized" (w=35.9) → "fo" (gap=13.92): word boundary, r=1.160 → SPLIT
        let ized = make_text_item("ized", 142.0, 35.9, fs);
        let fo = make_text_item("fo", 191.8, 13.8, fs);
        assert!(
            !should_join_items(&ized, &fo, threshold),
            "ized→fo: ratio={:.3}, should split (word boundary)",
            (fo.x - (ized.x + ized.width)) / fs
        );
    }
}
