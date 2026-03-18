//! Character classification and text utility functions.
//!
//! Pure helpers that operate on characters, strings, or `TextItem` slices.
//! No PDF parsing happens here — these are shared across the extraction
//! and markdown pipelines.

use crate::types::TextItem;
use unicode_normalization::UnicodeNormalization;

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

fn is_arabic_presentation_form(c: char) -> bool {
    // U+FEFF is BOM/ZWNJ, not an Arabic presentation form despite falling
    // in the Presentation Forms-B codepoint range.
    matches!(c, '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFE}')
}

pub(crate) fn is_rtl_text<I, S>(texts: I) -> bool
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let (mut rtl, mut ltr) = (0u32, 0u32);
    for t in texts {
        for c in t.as_ref().chars() {
            if is_rtl_char(c) {
                rtl += 1;
            } else if c.is_alphabetic() && !is_cjk_char(c) {
                ltr += 1;
            }
        }
    }
    rtl > 0 && rtl > ltr
}

pub(crate) fn sort_line_items(items: &mut [TextItem]) {
    let rtl = is_rtl_text(items.iter().map(|i| &i.text));
    if rtl {
        items.sort_by(|a, b| b.x.partial_cmp(&a.x).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        items.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
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

/// Reverse visual-order Arabic text to logical order.
///
/// Pure RTL text (no ASCII alphanumerics) gets a simple character reversal.
/// Mixed content (embedded numbers or Latin words) splits into LTR and non-LTR
/// runs: run order is reversed, and only non-LTR runs are reversed internally.
fn reverse_visual_arabic(text: &str) -> String {
    // Check if there are any LTR runs (ASCII letters or digits)
    let has_ltr = text.chars().any(|c| c.is_ascii_alphanumeric());

    if !has_ltr {
        // Pure RTL: simple reversal
        return text.chars().rev().collect();
    }

    // Mixed content: split into runs of LTR (ASCII alphanumeric + adjacent
    // punctuation like '.', ',', '/', '-') vs non-LTR (Arabic + spaces + other).
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<(bool, String)> = Vec::new(); // (is_ltr, content)

    let mut i = 0;
    while i < chars.len() {
        let is_ltr = chars[i].is_ascii_alphanumeric()
            || (chars[i].is_ascii_punctuation() && is_adjacent_to_ascii_alnum(&chars, i));

        let mut run = String::new();
        while i < chars.len() {
            let c = chars[i];
            let c_is_ltr = c.is_ascii_alphanumeric()
                || (c.is_ascii_punctuation() && is_adjacent_to_ascii_alnum(&chars, i));
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
            result.extend(content.chars().rev());
        }
    }
    result
}

/// Check if the character at `idx` is adjacent to an ASCII alphanumeric character.
fn is_adjacent_to_ascii_alnum(chars: &[char], idx: usize) -> bool {
    (idx > 0 && chars[idx - 1].is_ascii_alphanumeric())
        || (idx + 1 < chars.len() && chars[idx + 1].is_ascii_alphanumeric())
}

/// Decode a PDF text string (ActualText, etc.) that may be UTF-16BE (BOM \xFE\xFF)
/// or PDFDocEncoding (Latin-1 superset).
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE with BOM
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
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
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

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

    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

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

        // Never join across column-scale gaps
        if gap > font_size * 3.0 {
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
        if let (Some(p), Some(c)) = (prev_last, curr_first) {
            let prev_is_numeric = p.is_ascii_digit() || p == ',' || p == '.';
            let curr_is_numeric = c.is_ascii_digit() || c == '%' || c == '.';
            if prev_is_numeric && curr_is_numeric {
                return gap < font_size * 0.3;
            }
            // Sign characters (+/-) followed by digits
            if (p == '+' || p == '-') && c.is_ascii_digit() {
                return gap < font_size * 0.3;
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

    /// Helper to create a single-char TextItem at a given x position with width.
    fn make_char_item(ch: char, x: f32, width: f32, font_size: f32) -> TextItem {
        TextItem {
            text: ch.to_string(),
            x,
            y: 100.0,
            width,
            height: font_size,
            font: "TestFont".to_string(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
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
                font_size: fs,
                page: 1,
                is_bold: false,
                is_italic: false,
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
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
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
