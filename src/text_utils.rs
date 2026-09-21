//! Character classification and text utility functions.
//!
//! Pure helpers that operate on characters, strings, or `TextItem` slices.
//! No PDF parsing happens here — these are shared across the extraction
//! and markdown pipelines.

use crate::types::{ItemType, TextItem};

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

/// Whether a line reads right to left: by its own letters, or — for a line
/// holding right-to-left letters at all — by the page around it, so a line
/// that opens with a Latin name on a Hebrew page keeps the page's direction
/// and its Latin phrase falls into place. A Latin sentence that merely
/// quotes a right-to-left word keeps reading left to right: its outermost
/// letters on the page are Latin on both sides, which no right-to-left
/// paragraph displays.
pub(crate) fn rtl_line_base<T>(
    items: &[T],
    item_of: impl Fn(&T) -> &TextItem,
    page_rtl: bool,
) -> bool {
    if is_rtl_text(items.iter().map(|item| &item_of(item).text)) {
        return true;
    }
    if !page_rtl
        || !items
            .iter()
            .any(|item| item_of(item).text.chars().any(is_rtl_char))
    {
        return false;
    }
    let is_letter =
        |c: &char| c.is_alphabetic() && !unicode_normalization::char::is_combining_mark(*c);
    let leftmost = items
        .iter()
        .map(&item_of)
        .filter(|i| i.text.chars().any(|c| is_letter(&c)))
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .and_then(|i| i.text.chars().find(is_letter));
    let rightmost = items
        .iter()
        .map(&item_of)
        .filter(|i| i.text.chars().any(|c| is_letter(&c)))
        .max_by(|a, b| (a.x + a.width).total_cmp(&(b.x + b.width)))
        .and_then(|i| i.text.chars().rev().find(is_letter));
    !matches!((leftmost, rightmost), (Some(l), Some(r)) if !is_rtl_char(l) && !is_rtl_char(r))
}

/// Put the items of one line holding right-to-left text, given in screen
/// order (ascending `x`), into reading order: the Unicode Bidirectional
/// Algorithm's for a paragraph of the given base direction, with embedded
/// runs of the other direction reading their own way (see `crate::bidi`).
/// Item texts are already logical and stay as they are.
pub(crate) fn reorder_bidi_line<T: Clone>(
    items: &mut [T],
    item_of: impl Fn(&T) -> &TextItem,
    rtl_base: bool,
) {
    let order = crate::bidi::logical_line_order(
        items,
        |item| item_of(item).text.as_str(),
        |item| {
            let item = item_of(item);
            (item.x, item.width)
        },
        |item| item_of(item).font_size,
        |_| false,
        None,
        rtl_base,
    );
    let source: Vec<T> = items.to_vec();
    for (slot, (index, _)) in order.into_iter().enumerate() {
        items[slot] = source[index].clone();
    }
}

/// Sort a table cell's items into RTL reading order: baseline bands (2pt
/// tolerance) run top-to-bottom, items within a band read right-to-left with
/// embedded LTR phrases and numbers reading forwards. Band-aware sorting
/// keeps sub/superscript baseline jitter from breaking a line's X order,
/// which a plain Y-then-X comparator would (`total_cmp` ties only on
/// identical Y).
pub(crate) fn sort_rtl_cell_items<T: Clone>(items: &mut [T], item_of: impl Fn(&T) -> &TextItem) {
    items.sort_by(|a, b| item_of(b).line_y().total_cmp(&item_of(a).line_y()));
    let mut start = 0;
    while start < items.len() {
        let y0 = item_of(&items[start]).line_y();
        let mut end = start + 1;
        while end < items.len() && (item_of(&items[end]).line_y() - y0).abs() <= 2.0 {
            end += 1;
        }
        items[start..end].sort_by(|a, b| item_of(a).x.total_cmp(&item_of(b).x));
        reorder_bidi_line(&mut items[start..end], &item_of, true);
        start = end;
    }
}

/// Sort a line's items into reading order. `page_rtl` is the direction of
/// the surrounding page (see [`rtl_line_base`]).
pub(crate) fn sort_line_items(items: &mut [TextItem], page_rtl: bool) {
    // A line with right-to-left letters reads by the Unicode Bidirectional
    // Algorithm, whichever direction dominates it.
    if items.iter().any(|i| i.text.chars().any(is_rtl_char)) {
        let rtl_base = rtl_line_base(items, |i| i, page_rtl);
        items.sort_by(|a, b| a.x.total_cmp(&b.x));
        reorder_bidi_line(items, |i| i, rtl_base);
        return;
    }
    // An upside-down line of LTR runs (180°) reads towards -x: sort it by its
    // mirrored position so the fragments come out in reading order.
    // Non-text items on the line (links, form fields, images) are axis-aligned
    // boxes reporting 0° and say nothing about the reading direction.
    let mut text_runs = items
        .iter()
        .filter(|i| matches!(i.item_type, ItemType::Text));
    let upside_down = text_runs.clone().next().is_some() && text_runs.all(|i| i.is_upside_down());
    let key = |item: &TextItem| {
        if upside_down {
            -(item.x + item.width)
        } else {
            item.x
        }
    };
    items.sort_by(|a, b| key(a).total_cmp(&key(b)));
}

/// Detect if a font name indicates bold style: a bold word ("Bold",
/// "Black", "Heavy", "Demi", "Ultra", "SemiBold", "ExtraBold"), or one of
/// the foundry style abbreviations the weight-class parser reads ("-Bd",
/// "-Sb", "-SBd", "-Smbd", "-Dm", "-DmBd", "-Hv", "-Blk", "-XBd", "-XBlk",
/// "-Ult", "W6".."W9") — any name
/// [`font_weight_from_name`] puts at 600 or heavier is bold, so every face
/// the weight class calls bold this flag calls bold too. The abbreviations
/// are matched as whole tokens after the family name, in the mixed case
/// foundries write them: "Bookman" is not Book, "LT" is not Light and
/// "Hvar" is not Heavy. On top of that the flag keeps its older readings,
/// which the weight class does not share: a Medium face ("Arial-Medium",
/// URW's "-Medi") is bold here and 500 there, since some families use
/// Medium as their heavier weight.
pub fn is_bold_font(font_name: &str) -> bool {
    if font_weight_from_name(font_name).is_some_and(|weight| weight >= 600) {
        return true;
    }
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

/// Weight class named by a font's style tokens, on the 100..=900 scale
/// shared by CSS `font-weight` and the OS/2 `usWeightClass` field:
/// Thin 100, ExtraLight/UltraLight 200, Light 300, Regular/Book/Roman 400,
/// Medium 500, SemiBold/DemiBold 600, Bold 700, ExtraBold/UltraBold 800,
/// Black/Heavy/Ultra 900. `None` when the name carries no weight word.
///
/// The name is split at the separators producers use (`-`, `_`, `,`,
/// space) and at lowercase→uppercase boundaries, and each piece is matched
/// whole; a qualifier reaches the word after it across either kind of
/// seam ("ExtraLight", "Extra-Light", "Extra Light" are all 200). The
/// abbreviations of foundry style suffixes read ("-Md",
/// "-Lt", "-Bd", "-Sb", "-Dm", "-Hv", "-Blk", "-XBd", "-Ult", "-UltLt",
/// "-W1".."-W9") while "Bookman" is not Book. Abbreviations count only
/// after the family name (a leading "TH" is a Thai family, not Thin) and
/// only in the mixed case foundries write them in: an all-caps "LT" or
/// "MT" is the Linotype or Monotype acronym, not Light. Inside the family
/// name — the first separator-delimited token — a weight word counts only
/// when it ends the family or is followed by nothing but style words and
/// foundry marks ("ArialBlack", "TimesNewRomanPSMT", "HelveticaNeueLightItalic"),
/// never when it starts it or is followed by another word ("BlackChancery",
/// "BookAntiqua", "OldBlackLetter"). A piece written in one case ("BOLDMT")
/// is searched for the full words instead. The last weight word wins, since
/// style suffixes follow the family ("Bookman-Demi").
pub fn font_weight_from_name(font_name: &str) -> Option<u16> {
    // Subset tags ("ABCDEF+Face-Md") carry no style.
    let name = font_name
        .split_once('+')
        .map_or(font_name, |(_, rest)| rest);
    // The name's tokens and their pieces, walked as one sequence so a
    // qualifier reaches its word across a separator ("Foo-Extra-Light").
    let tokens: Vec<Vec<&str>> = name
        .split(['-', '_', ',', ' ', '.'])
        .filter(|token| !token.is_empty())
        .map(camel_pieces)
        .collect();
    let flat: Vec<(usize, usize)> = tokens
        .iter()
        .enumerate()
        .flat_map(|(t, pieces)| (0..pieces.len()).map(move |i| (t, i)))
        .collect();
    let lower = |&(t, i): &(usize, usize)| tokens[t][i].to_ascii_lowercase();
    let mut weight = None;
    let mut k = 0;
    while k < flat.len() {
        let (t, i) = flat[k];
        let piece = lower(&flat[k]);
        let next = flat.get(k + 1).map(lower).unwrap_or_default();
        // "Extra"/"Ultra"/"X" and "Semi"/"Demi" qualify the word after
        // them; on their own only "Ultra" and "Demi" name a weight.
        let extra = matches!(piece.as_str(), "extra" | "ultra" | "ult" | "x");
        let semi = matches!(piece.as_str(), "semi" | "demi" | "sm" | "dm");
        let qualified = match next.as_str() {
            "light" | "lt" if extra => Some(200),
            "bold" | "bd" if extra => Some(800),
            "black" | "blk" if extra => Some(900),
            "bold" | "bd" if semi => Some(600),
            _ => None,
        };
        let width = if qualified.is_some() { 2 } else { 1 };
        let whole = qualified.or_else(|| weight_word(&piece)).or_else(|| {
            (t > 0 || i > 0)
                .then(|| weight_abbreviation(tokens[t][i]))
                .flatten()
        });
        let found = if whole.is_some() {
            // A weight word inside the family name is a style only at its
            // end; a family that starts with one, or goes on with another
            // word after it, merely contains the word.
            let (last_t, last_i) = flat[k + width - 1];
            let rest = &tokens[last_t][last_i + 1..];
            let in_family = t == 0 && (i == 0 || !rest.iter().all(|rest| is_style_or_mark(rest)));
            (!in_family).then_some(whole).flatten()
        } else {
            // A piece written in one case has no seams to split at: look
            // for the words inside it, at its end when it is the family.
            let flat_case = tokens[t][i].chars().all(|c| !c.is_lowercase())
                || tokens[t][i].chars().all(|c| !c.is_uppercase());
            flat_case
                .then(|| weight_word_substring(&piece, t == 0))
                .flatten()
        };
        if found.is_some() {
            weight = found;
        }
        k += width;
    }
    weight
}

/// The pieces of one name token, split where a lowercase letter meets an
/// uppercase one ("BoldMT" → "Bold", "MT"; "UltLt" → "Ult", "Lt").
fn camel_pieces(token: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut previous_lower = false;
    for (index, c) in token.char_indices() {
        if c.is_uppercase() && previous_lower && index > start {
            pieces.push(&token[start..index]);
            start = index;
        }
        previous_lower = c.is_lowercase();
    }
    if start < token.len() {
        pieces.push(&token[start..]);
    }
    pieces
}

/// Whether a piece following a weight word inside a family name is a style
/// word or a foundry mark rather than another word of the family: slant and
/// width words, and all-caps acronyms such as "MT", "PS", "PSMT" or "LT".
fn is_style_or_mark(piece: &str) -> bool {
    if piece
        .chars()
        .all(|c| c.is_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    matches!(
        piece.to_ascii_lowercase().as_str(),
        "italic"
            | "oblique"
            | "it"
            | "ital"
            | "obl"
            | "condensed"
            | "cond"
            | "cn"
            | "cd"
            | "narrow"
            | "compressed"
            | "extended"
            | "ext"
            | "expanded"
            | "std"
            | "pro"
    )
}

/// Weight of one whole style word, lowercased.
fn weight_word(piece: &str) -> Option<u16> {
    Some(match piece {
        "thin" | "hairline" => 100,
        "extralight" | "ultralight" => 200,
        "light" => 300,
        "regular" | "book" | "roman" | "normal" => 400,
        "medium" => 500,
        "semibold" | "demibold" | "demi" => 600,
        "bold" => 700,
        "extrabold" | "ultrabold" => 800,
        "black" | "heavy" | "ultra" | "extrablack" => 900,
        _ => return None,
    })
}

/// Weight of one whole style abbreviation, as written: the short codes of
/// foundry style suffixes and the "W1".."W9" weight digit of Japanese
/// families, a hundredth of the weight class (Hiragino's W3 is 300, its W6
/// 600). The codes are written in mixed case ("Md", "Lt", "XBd"); an
/// all-caps piece is a family or foundry acronym ("LT" for Linotype, "MT"
/// for Monotype, "ITC") and is not read.
fn weight_abbreviation(piece: &str) -> Option<u16> {
    if let Some(digit) = piece.strip_prefix(['W', 'w']) {
        if let Some(n) = digit.parse::<u16>().ok().filter(|n| (1..=9).contains(n)) {
            return Some(n * 100);
        }
    }
    if !piece.chars().any(char::is_lowercase) || !piece.chars().any(char::is_uppercase) {
        return None;
    }
    Some(match piece.to_ascii_lowercase().as_str() {
        "th" => 100,
        "ultlt" | "xlt" => 200,
        "lt" => 300,
        "rg" | "reg" | "bk" => 400,
        "md" | "med" | "medi" => 500,
        "sb" | "sbd" | "smbd" | "dm" | "dmbd" => 600,
        "bd" => 700,
        "xbd" => 800,
        "ult" | "blk" | "hv" | "xblk" => 900,
        _ => return None,
    })
}

/// Full weight words inside one lowercased piece written in a single case
/// ("boldmt", "boldoblique"), where the case split above finds no seam.
/// Compound words are tried first so "extrabold" is not read as bold; the
/// last word in the piece wins. In the family name (`in_family`) only a
/// word ending the piece counts, foundry marks aside: "arialblack" is
/// black, "blackchancery" merely contains the word.
fn weight_word_substring(piece: &str, in_family: bool) -> Option<u16> {
    const COMPOUND: [(&str, u16); 6] = [
        ("extralight", 200),
        ("ultralight", 200),
        ("extrabold", 800),
        ("ultrabold", 800),
        ("semibold", 600),
        ("demibold", 600),
    ];
    let piece = if in_family {
        ["psmt", "mt", "ps"]
            .iter()
            .find_map(|mark| piece.strip_suffix(mark))
            .unwrap_or(piece)
    } else {
        piece
    };
    if let Some((_, weight)) = COMPOUND
        .iter()
        .find(|(word, _)| in_family && piece.ends_with(word) || !in_family && piece.contains(word))
    {
        return Some(*weight);
    }
    const WORDS: [(&str, u16); 13] = [
        ("black", 900),
        ("heavy", 900),
        ("ultra", 900),
        ("bold", 700),
        ("demi", 600),
        ("medium", 500),
        ("light", 300),
        ("regular", 400),
        ("roman", 400),
        ("book", 400),
        ("normal", 400),
        ("thin", 100),
        ("hairline", 100),
    ];
    if in_family {
        return WORDS
            .iter()
            .find(|(word, _)| piece.ends_with(word))
            .map(|(_, weight)| *weight);
    }
    WORDS
        .iter()
        .filter_map(|(word, weight)| piece.rfind(word).map(|at| (at, *weight)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, weight)| weight)
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
/// Also strips control and invisible formatting characters and normalizes
/// typographic spaces. Hebrew and Arabic presentation forms are left as they
/// are: they stand for letters of a run whose storage order is not known
/// yet, and are normalized once it is (see [`fix_visual_order_rtl`]).
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

    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // Explicit ligature expansion (also covers fonts whose ToUnicode
            // maps to these code points directly)
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

    result
}

/// A decoded show-op string qualifies as evidence in the geometric
/// visual-order vote when it holds an RTL letter. The vote reads the way
/// show operators walk along the line, which tells the two conventions
/// apart whatever else a string holds: a single letter reads the same in
/// either storage order, and a string that is mostly Latin — a whole line
/// shown by one operator, with a right-to-left word inside it — is the
/// typical output of a visual-order producer, whose pages would otherwise
/// cast no vote at all and keep that word reversed. Only letters count:
/// Arabic-Indic digits are stored left-to-right in both conventions, so a
/// bare number carries no evidence, and a string of nothing but combining
/// marks (vowel points shown apart from their letters) or a byte order
/// mark — code points the RTL blocks also hold — says nothing about the
/// order letters are stored in.
pub(crate) fn is_visual_rtl_candidate(text: &str) -> bool {
    text.chars().any(|c| {
        c.is_alphabetic() && !unicode_normalization::char::is_combining_mark(c) && is_rtl_char(c)
    })
}

/// A decoded show-op string that is evidence of visual storage on its own
/// when it is painted forwards and seen: a run of two or more right-to-left
/// letters shown with forward advances displays its letters in the order
/// they are stored, so a run meant to be read can only be stored in visual
/// order. A single letter reads the same either way. An invisible run — the
/// convention of OCR text layers, which store their words in logical order
/// — displays nothing and proves nothing; see [`render_mode_paints`].
pub(crate) fn is_visual_rtl_run(text: &str) -> bool {
    text.chars()
        .filter(|&c| {
            c.is_alphabetic()
                && !unicode_normalization::char::is_combining_mark(c)
                && is_rtl_char(c)
        })
        .nth(1)
        .is_some()
}

/// Whether a text render mode puts glyphs on the page: modes 3 and 7 paint
/// nothing (invisible text, clipping only).
pub(crate) fn render_mode_paints(mode: i32) -> bool {
    !matches!(mode, 3 | 7)
}

/// Whether a white fill hides a run shown in `mode`: only when the run is
/// painted with the fill alone (modes 0 and 4). A stroked run (1, 2, 5, 6)
/// shows its stroke whatever the fill, and modes 3 and 7 paint nothing at
/// all, which [`render_mode_paints`] says.
pub(crate) fn white_fill_hides(mode: i32, fill_is_white: bool) -> bool {
    fill_is_white && matches!(mode, 0 | 4)
}

/// Whether a page's right-to-left runs are stored in visual (screen
/// left-to-right) order.
///
/// PDF paints glyphs sequentially left-to-right, so producers of visible RTL
/// text emit each run's characters in screen order — reversed relative to
/// reading order — and walk the line's runs left-to-right. Producers that
/// keep logical order instead position each run explicitly, walking
/// right-to-left across the line (common in OCR text layers). The two
/// conventions are distinguished geometrically: candidate runs emitted
/// left-to-right along a shared baseline vote for visual storage,
/// right-to-left emission votes for logical storage. `logical_ops` carries
/// extra logical votes observed during parsing — show ops whose internal
/// glyph progression already walks right-to-left — and `visual_ops` extra
/// visual votes: visible runs of several RTL letters painted forwards
/// ([`is_visual_rtl_run`]), which display their letters in stored order and
/// so can only be visual storage when they are meant to be read. They
/// decide the case the walk alone gets wrong: a producer that shows its
/// runs in reading order — right to left across the line, one text object
/// each — with every run's glyphs stored in visual order. The walk of such
/// a page reads as logical storage, and every word would come out
/// backwards. A logical-order layer keeps its reading when it is invisible,
/// as OCR text layers are.
///
/// Votes are pooled per page deliberately: a page is written by one
/// producer, so its storage convention is uniform, while individual lines
/// are often single-run and carry no votes at all. Ties — including the
/// vote-less single-run case — read as visual: RTL text painted with
/// forward advances renders correctly only when stored in visual order, so
/// visual storage is the dominant convention.
fn stored_in_visual_order(
    items: &[TextItem],
    candidates: &[usize],
    logical_ops: u32,
    visual_ops: u32,
) -> bool {
    if candidates.is_empty() {
        return false;
    }
    let mut rightward = visual_ops;
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
    leftward <= rightward
}

/// Whether a page's right-to-left runs are stored in visual order (see
/// [`stored_in_visual_order`]), which is how `merge_text_items` then reads
/// its lines: the items of a line are taken in screen order and the line's
/// characters are put into logical order through the Unicode Bidirectional
/// Algorithm (`crate::bidi`) — RTL words spelled forwards again, embedded
/// Latin phrases and numbers kept left-to-right, mirrored brackets turned
/// back — before the fragments merge into words.
///
/// `logical_text_items` are items whose text is logical whatever the page
/// does (ActualText replacements). On a visual-order page they are turned
/// into display order here so every item of the page reads the same way.
pub(crate) fn fix_visual_order_rtl(
    items: &mut [TextItem],
    candidates: &[usize],
    logical_ops: u32,
    visual_ops: u32,
    logical_text_items: &[usize],
) -> bool {
    let page_rtl = is_rtl_text(items.iter().map(|i| &i.text));
    if !stored_in_visual_order(items, candidates, logical_ops, visual_ops) {
        // Runs of shaped glyphs — presentation forms — come out of a
        // shaping engine in display order whatever order the runs are
        // shown in. A page that shows its words in reading order still
        // holds each such word's glyphs backwards: read every one back on
        // its own.
        for (index, item) in items.iter_mut().enumerate() {
            if logical_text_items.contains(&index)
                || !item.text.chars().any(crate::bidi::is_presentation_form)
            {
                continue;
            }
            let rtl = page_rtl || is_rtl_text(std::iter::once(&item.text));
            let visual: Vec<crate::bidi::VisualChar> = item
                .text
                .chars()
                .map(|ch| crate::bidi::VisualChar { ch, item: None })
                .collect();
            item.text = crate::bidi::visual_to_logical(&visual, rtl)
                .into_iter()
                .map(|v| v.ch)
                .collect();
        }
        return false;
    }
    for &index in logical_text_items {
        let Some(item) = items.get_mut(index) else {
            continue;
        };
        if !item.text.chars().any(is_rtl_char) {
            continue;
        }
        let rtl = page_rtl || is_rtl_text(std::iter::once(&item.text));
        item.text = crate::bidi::logical_to_visual(&item.text, rtl);
    }
    true
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
    // A negative `Tf` size turns the glyphs around; their size is still
    // their size (the geometry reads the direction from the advance).
    base_size.abs() * scale
}

/// The item's horizontal extent for layout heuristics. The box already
/// holds an estimate for runs whose font carries no width metrics (laid
/// along the run at extraction, flagged by `TextItem::advance_known ==
/// false`), so a positive width is taken as is. A box without one gets the
/// classic half-em-per-character stand-in, as it always did — whether the
/// width is negative (a merged item whose fragments ran backwards) or a
/// measured zero: a glyph drawn with a zero advance (an Arabic hamza or a
/// combining mark positioned by hand) still covers a glyph's worth of page,
/// and column detection and region routing need that footprint or they
/// displace it into another line and split the word. The stand-in is a
/// layout extent only; the item's box keeps its measured width.
pub(crate) fn effective_width(item: &TextItem) -> f32 {
    if item.width > 0.0 {
        item.width
    } else {
        item.text.chars().count() as f32 * item.font_size * 0.5
    }
}

/// The item's vertical extent — the counterpart of `effective_width`.
pub(crate) fn effective_height(item: &TextItem) -> f32 {
    item.height
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
    // Only measured widths earn the tight threshold: a width-less font's
    // box is a half-em-per-glyph estimate (`advance_known == false`), which
    // stays on the loose heuristic it always used. So does a rotated pair:
    // a vertical run's `width` is its em, not its advance, and the x gap
    // below says nothing about how far apart the runs read.
    if prev_item.width > 0.0
        && prev_item.advance_known
        && prev_item.is_upright()
        && curr_item.is_upright()
    {
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
    fn font_weight_reads_full_style_words() {
        let cases = [
            ("Helvetica", None),
            ("ABCDEF+Helvetica-Bold", Some(700)),
            ("Arial-BoldMT", Some(700)),
            ("Arial,BoldItalic", Some(700)),
            ("TimesNewRomanPSMT", Some(400)),
            ("TimesNewRomanPS-BoldMT", Some(700)),
            ("Calibri-Light", Some(300)),
            ("SegoeUI-Semibold", Some(600)),
            ("OpenSans-ExtraBold", Some(800)),
            ("Roboto-Black", Some(900)),
            ("Lato-Heavy", Some(900)),
            ("Montserrat-Thin", Some(100)),
            ("Montserrat-ExtraLightItalic", Some(200)),
            ("Foo-MediumItalic", Some(500)),
            ("ITCAvantGardeStd-Demi", Some(600)),
            ("Bookman-Demi", Some(600)),
            ("CenturyGothic-Book", Some(400)),
            ("Roboto-Regular", Some(400)),
            ("NimbusRomNo9L-Medi", Some(500)),
            ("ARIALBOLD", Some(700)),
            ("HELVETICA-BOLDOBLIQUE", Some(700)),
            ("AVANTGARDEDEMI", Some(600)),
            ("ARIALTHIN", Some(100)),
            ("ARIALBLACK", Some(900)),
            ("ARIALBOLDMT", Some(700)),
            ("TIMESNEWROMANPSMT", Some(400)),
            ("helveticaneue-ultra", Some(900)),
            ("FrutigerLT-Roman", Some(400)),
            ("Frutiger LT 45 Light", Some(300)),
            ("ArialBlack", Some(900)),
            ("Arial Black", Some(900)),
            ("ArialBoldMT", Some(700)),
            ("HelveticaNeueLightItalic", Some(300)),
            ("GillSansUltraBold", Some(800)),
            ("Foo-Extra-Light", Some(200)),
            ("Foo-Ultra-Light", Some(200)),
            ("Foo-Semi-Bold", Some(600)),
            ("Foo-Demi-Bold", Some(600)),
            ("Open Sans Extra Bold", Some(800)),
            ("Foo-Extra-Black", Some(900)),
        ];
        for (name, expected) in cases {
            assert_eq!(font_weight_from_name(name), expected, "{name}");
        }
    }

    #[test]
    fn font_weight_reads_style_abbreviations_after_the_family() {
        let cases = [
            ("AAAAAB+HelveticaNeueLTStd-Md", Some(500)),
            ("HelveticaNeueLTStd-Lt", Some(300)),
            ("HelveticaNeueLTStd-Bd", Some(700)),
            ("HelveticaNeueLTStd-BdCn", Some(700)),
            ("HelveticaNeueLTStd-MdIt", Some(500)),
            ("HelveticaNeueLTStd-UltLt", Some(200)),
            ("HelveticaNeueLTStd-Hv", Some(900)),
            ("HelveticaNeueLTStd-Blk", Some(900)),
            ("HelveticaNeueLTStd-XBlk", Some(900)),
            ("HelveticaNeueLTStd-Th", Some(100)),
            ("FrutigerLTStd-Ult", Some(900)),
            ("ITCFranklinGothicStd-Dm", Some(600)),
            ("ITCFranklinGothicStd-DmCd", Some(600)),
            ("Foo-Sb", Some(600)),
            ("Foo-SBd", Some(600)),
            ("Foo-Smbd", Some(600)),
            ("Foo-XBd", Some(800)),
            ("Foo-XBdIt", Some(800)),
            ("Foo-Bk", Some(400)),
            ("Foo-Rg", Some(400)),
            ("HiraginoSans-W1", Some(100)),
            ("HiraginoSans-W2", Some(200)),
            ("HiraKakuProN-W3", Some(300)),
            ("HiraKakuProN-W6", Some(600)),
            ("KozMinPr6N-W9", Some(900)),
        ];
        for (name, expected) in cases {
            assert_eq!(font_weight_from_name(name), expected, "{name}");
        }
    }

    #[test]
    fn font_weight_ignores_width_style_and_family_words() {
        // Condensed and script faces, the all-caps "LT" and "MT" acronyms
        // of Linotype and Monotype, a Thai family's leading "TH", families
        // that start with or contain a weight word, and the weight digit
        // only after a "W".
        let cases = [
            "HelveticaNeueLTStd-Cn",
            "Roboto-Condensed",
            "Roboto-CondensedItalic",
            "SignPainter-HouseScript",
            "FZXXLB--B51-0",
            "FZHTB--B51-0",
            "Frutiger LT Std",
            "Helvetica LT Condensed",
            "Frutiger-LT",
            "Foo-MT",
            "THSarabunNew",
            "TH-Sarabun",
            "BlackadderITC",
            "Bookman",
            "BlackChancery",
            "Black Chancery",
            "BLACKCHANCERY",
            "BlackOak",
            "BookAntiqua",
            "OldBlackLetter",
            "LightRail",
            "HeavyMetal",
            "Foo-W95",
            "HiraginoSans-W0",
            "Wingdings",
            "ABCDEF+Tc1",
        ];
        for name in cases {
            assert_eq!(font_weight_from_name(name), None, "{name}");
        }
    }

    #[test]
    fn bold_font_reads_the_weight_parsers_style_words_and_abbreviations() {
        // Every name the weight parser puts at 600 or heavier is bold,
        // foundry abbreviations and weight digits included.
        for name in [
            "Bookman-Demi",
            "ITCAvantGardeStd-Demi",
            "ABCDEF+Face-Demi",
            "helveticaneue-ultra",
            "FrutigerLTStd-Ult",
            "Lato-Heavy",
            "Roboto-Black",
            "HelveticaNeueLTStd-Hv",
            "HelveticaNeueLTStd-Blk",
            "HelveticaNeueLTStd-XBlk",
            "Foo-XBd",
            "Foo-XBdIt",
            "Foo-Sb",
            "Foo-SBd",
            "Foo-Smbd",
            "ITCFranklinGothicStd-Dm",
            "ITCFranklinGothicStd-DmCd",
            "HiraKakuProN-W6",
            "KozMinPr6N-W9",
            "Open Sans Extra Bold",
            "Foo-Semi-Bold",
        ] {
            assert!(is_bold_font(name), "{name}");
        }
        // Lighter faces, and family names that merely contain the letters
        // of an abbreviation or a weight word, are not.
        for name in [
            "Bookman",
            "BookAntiqua",
            "Frutiger-LT",
            "Frutiger LT Std",
            "HelveticaNeueLTStd-Lt",
            "HelveticaNeueLTStd-UltLt",
            "HelveticaNeueLTStd-Md",
            "Montserrat-ExtraLight",
            "Foo-Semi",
            "Hvar",
            "Foo-Hvar",
            "THSarabunNew",
            "HiraKakuProN-W3",
            "Roboto-Regular",
            "Wingdings",
        ] {
            assert!(!is_bold_font(name), "{name}");
        }
    }

    #[test]
    fn bold_font_urw_medi_abbreviation() {
        // A Medium face keeps its older bold reading, which the weight class
        // (500) does not share.
        assert!(is_bold_font("Arial-Medium"));
        assert_eq!(font_weight_from_name("Arial-Medium"), Some(500));
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
    fn presentation_forms_pass_through_expansion() {
        // Presentation forms are letters of a run whose storage order is
        // not known yet: expansion leaves them for the page pass.
        let input = "\u{FEE1}\u{FEF3}";
        assert_eq!(expand_ligatures(input), input);
        // Base Arabic already in logical order passes through unchanged.
        let input = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"; // مرحبا
        assert_eq!(expand_ligatures(input), input);
    }

    #[test]
    fn latin_text_unaffected() {
        assert_eq!(expand_ligatures("Hello World"), "Hello World");
    }

    #[test]
    fn visual_rtl_candidate_classification() {
        // Multi-char base Hebrew: candidate
        assert!(is_visual_rtl_candidate("\u{05E9}\u{05DC}\u{05D5}\u{05DD}"));
        // Multi-char base Arabic: candidate
        assert!(is_visual_rtl_candidate("\u{0645}\u{0631}\u{062D}"));
        // Arabic presentation forms are letters too and vote like them
        assert!(is_visual_rtl_candidate("\u{FEDF}\u{FEE0}"));
        // A single RTL letter votes with its position like a longer run
        assert!(is_visual_rtl_candidate("\u{05E9}"));
        // Latin-dominant with an embedded RTL word: the operator's walk
        // along the line is evidence all the same
        assert!(is_visual_rtl_candidate("the word \u{05E9}\u{05DC} here"));
        // Pure Latin
        assert!(!is_visual_rtl_candidate("Hello"));
        // Arabic-Indic digits are stored left-to-right in both conventions:
        // a bare "٢٤" run carries no evidence
        assert!(!is_visual_rtl_candidate("\u{0662}\u{0664}"));
        assert!(!is_visual_rtl_candidate("\u{0663}\u{0665},\u{0660}"));
        assert!(!is_visual_rtl_candidate("\u{0663}\u{0665}\u{066B}\u{0660}"));
        // Nor do marks shown apart from their letters, or a byte order mark
        // (both lie in the RTL blocks)
        assert!(!is_visual_rtl_candidate("\u{05B4}"));
        assert!(!is_visual_rtl_candidate("\u{064E}\u{0651}"));
        assert!(!is_visual_rtl_candidate("\u{FEFF}"));
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

    fn make_rtl_item(text: &str, x: f32, y: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: 30.0,
            height: 12.0,
            font: "TestFont".to_string(),
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
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
    fn rightward_emission_reads_as_visual_storage() {
        // Ops painted left-to-right on one baseline = visual storage
        let mut items = vec![
            make_rtl_item("\u{05DD}\u{05DC}\u{05D5}\u{05E2}", 100.0, 700.0), // visual עולם
            make_rtl_item("\u{05DD}\u{05D5}\u{05DC}\u{05E9}", 160.0, 700.0), // visual שלום
        ];
        assert!(fix_visual_order_rtl(&mut items, &[0, 1], 0, 0, &[]));
        // The items themselves are left for the merge to read.
        assert_eq!(items[0].text, "\u{05DD}\u{05DC}\u{05D5}\u{05E2}");
    }

    #[test]
    fn leftward_emission_reads_as_logical_storage() {
        // Ops positioned right-to-left = logical storage (OCR layers)
        let mut items = vec![
            make_rtl_item("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", 160.0, 700.0),
            make_rtl_item("\u{05E2}\u{05D5}\u{05DC}\u{05DD}", 100.0, 700.0),
        ];
        assert!(!fix_visual_order_rtl(&mut items, &[0, 1], 0, 0, &[]));
    }

    #[test]
    fn visible_runs_painted_forwards_outvote_a_leftward_walk() {
        // Words shown in reading order, right to left across the line, each
        // holding its glyphs in visual order with forward advances: the walk
        // alone reads as logical storage, but a visible run of several
        // letters painted forwards can only be visual storage.
        let mut items = vec![
            make_rtl_item("\u{05DD}\u{05D5}\u{05DC}\u{05E9}", 160.0, 700.0), // visual שלום
            make_rtl_item("\u{05DD}\u{05DC}\u{05D5}\u{05E2}", 100.0, 700.0), // visual עולם
        ];
        assert!(fix_visual_order_rtl(&mut items, &[0, 1], 0, 2, &[]));
        // The same walk of an invisible text layer, whose runs display
        // nothing and cast no visual vote, still reads as logical storage.
        assert!(!fix_visual_order_rtl(&mut items, &[0, 1], 0, 0, &[]));
    }

    #[test]
    fn a_visual_rtl_run_holds_two_letters() {
        assert!(is_visual_rtl_run("\u{05E9}\u{05DC}"));
        assert!(is_visual_rtl_run("see \u{05E9}\u{05DC}\u{05D5}\u{05DD} 12"));
        // One letter reads the same either way; digits and marks are not letters.
        assert!(!is_visual_rtl_run("\u{05E9}"));
        assert!(!is_visual_rtl_run("\u{0661}\u{0662}"));
        assert!(!is_visual_rtl_run("\u{05E9}\u{05B0}"));
        assert!(render_mode_paints(0) && render_mode_paints(2) && render_mode_paints(4));
        assert!(!render_mode_paints(3) && !render_mode_paints(7));
        // A white fill hides filled text only; stroked text shows its stroke.
        assert!(white_fill_hides(0, true) && white_fill_hides(4, true));
        assert!(
            !white_fill_hides(1, true) && !white_fill_hides(2, true) && !white_fill_hides(5, true)
        );
        assert!(!white_fill_hides(0, false));
    }

    #[test]
    fn single_run_defaults_to_visual_storage() {
        // A single run gives no geometric votes; visible RTL painted with
        // forward advances can only be visual-order storage.
        let mut items = vec![make_rtl_item(
            "\u{05DD}\u{05D5}\u{05DC}\u{05E9}",
            100.0,
            700.0,
        )];
        assert!(fix_visual_order_rtl(&mut items, &[0], 0, 0, &[]));
        // No candidate at all: nothing to decide.
        assert!(!fix_visual_order_rtl(&mut items, &[], 0, 0, &[]));
    }

    #[test]
    fn logical_ops_outvote_rightward_emission() {
        // Extra logical evidence from op-internal geometry blocks reversal
        let logical = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let mut items = vec![
            make_rtl_item(logical, 100.0, 700.0),
            make_rtl_item(logical, 160.0, 700.0),
        ];
        assert!(!fix_visual_order_rtl(&mut items, &[0, 1], 2, 0, &[]));
    }

    #[test]
    fn cross_line_pairs_carry_no_vote() {
        // Different baselines carry no horizontal-direction information;
        // with no votes the default (visual) applies.
        let mut items = vec![
            make_rtl_item("\u{05D1}\u{05D0}", 160.0, 700.0),
            make_rtl_item("\u{05D3}\u{05D2}", 100.0, 650.0),
        ];
        assert!(fix_visual_order_rtl(&mut items, &[0, 1], 0, 0, &[]));
    }

    #[test]
    fn single_glyph_ops_vote_too() {
        // Glyph-by-glyph positioned text: one letter per op, walking
        // rightwards along the line.
        assert!(is_visual_rtl_candidate("\u{05E9}"));
        let mut items = vec![
            make_rtl_item("\u{05DD}", 100.0, 700.0),
            make_rtl_item("\u{05D5}", 106.0, 700.0),
            make_rtl_item("\u{05DC}", 112.0, 700.0),
            make_rtl_item("\u{05E9}", 118.0, 700.0),
        ];
        assert!(fix_visual_order_rtl(&mut items, &[0, 1, 2, 3], 0, 0, &[]));
        items.reverse();
        assert!(!fix_visual_order_rtl(&mut items, &[0, 1, 2, 3], 0, 0, &[]));
    }

    #[test]
    fn shaped_runs_on_a_logical_order_page_are_read_back_one_by_one() {
        // Words shown in reading order (walking leftwards), each holding
        // its shaped glyphs in display order: the page reads as logical
        // storage, and every run is turned round on its own. The forms
        // themselves are normalized later, when the words merge.
        let mut items = vec![
            make_rtl_item("\u{FEE6}\u{FEFB}", 160.0, 700.0), // لان displayed: noon-final, lam-alef
            make_rtl_item("\u{FEF3}\u{FEE1}", 100.0, 700.0), // مي displayed: yeh-initial, meem-medial
        ];
        assert!(!fix_visual_order_rtl(&mut items, &[0, 1], 0, 0, &[]));
        assert_eq!(items[0].text, "\u{FEFB}\u{FEE6}");
        assert_eq!(items[1].text, "\u{FEE1}\u{FEF3}");
    }

    #[test]
    fn actual_text_is_turned_into_display_order_on_visual_pages() {
        // An ActualText replacement is logical whatever the page does; on
        // a visual-order page it is rendered in display order so the whole
        // page reads one way.
        let mut items = vec![
            make_rtl_item("\u{05DD}\u{05DC}\u{05D5}\u{05E2}", 100.0, 700.0),
            make_rtl_item("\u{05E9}\u{05DC}\u{05D5}\u{05DD} 12", 160.0, 700.0),
        ];
        assert!(fix_visual_order_rtl(&mut items, &[0], 0, 0, &[1]));
        assert_eq!(items[1].text, "12 \u{05DD}\u{05D5}\u{05DC}\u{05E9}");
    }

    #[test]
    fn sort_line_items_reads_embedded_latin_forwards() {
        // Merged words of a line on a Hebrew page in screen order: the Latin
        // phrase keeps its order, the RTL words come right to left. By its
        // own letters the line is Latin-majority; the page's direction
        // decides.
        let mut items = vec![
            make_rtl_item("\u{05D1}", 100.0, 700.0),
            make_rtl_item("Financial", 120.0, 700.0),
            make_rtl_item("Stability", 180.0, 700.0),
            make_rtl_item("Board", 240.0, 700.0),
            make_rtl_item("\u{05E9}\u{05DC}", 290.0, 700.0),
        ];
        items.reverse();
        sort_line_items(&mut items, true);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "\u{05E9}\u{05DC}",
                "Financial",
                "Stability",
                "Board",
                "\u{05D1}"
            ]
        );
        // On a Latin page the same line reads left to right, the Hebrew
        // words as two embedded runs.
        sort_line_items(&mut items, false);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "\u{05D1}",
                "Financial",
                "Stability",
                "Board",
                "\u{05E9}\u{05DC}"
            ]
        );
    }

    #[test]
    fn sort_line_items_takes_the_page_direction_for_latin_led_lines() {
        // "IBM היא" displays with the Hebrew word at the left: by its own
        // letters the line is Latin-majority and would read "היא IBM"; on a
        // Hebrew page it reads "IBM היא".
        let mut items = vec![
            make_rtl_item("\u{05D4}\u{05D9}\u{05D0}", 100.0, 700.0),
            make_rtl_item("IBM", 140.0, 700.0),
        ];
        sort_line_items(&mut items, false);
        assert_eq!(items[0].text, "\u{05D4}\u{05D9}\u{05D0}");
        sort_line_items(&mut items, true);
        assert_eq!(items[0].text, "IBM");
        assert_eq!(items[1].text, "\u{05D4}\u{05D9}\u{05D0}");
    }

    #[test]
    fn sort_rtl_cell_items_respects_lines_and_jitter() {
        // Two wrapped lines of an RTL cell; the second line's items carry
        // sub/superscript baseline jitter (within the 2pt band). Lines must
        // stay separate top-to-bottom, each line must read right-to-left
        // despite the jitter, and LTR fragments from different visual lines
        // must never be reordered together.
        let mut items = vec![
            make_rtl_item("\u{05D5}\u{05DD}", 100.0, 688.0),
            make_rtl_item("CD", 160.0, 688.9),
            make_rtl_item("AB", 100.0, 700.0),
            make_rtl_item("\u{05E9}\u{05DC}", 160.0, 700.0),
        ];
        sort_rtl_cell_items(&mut items, |item| item);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            ["\u{05E9}\u{05DC}", "AB", "CD", "\u{05D5}\u{05DD}"],
            "line 1 right-to-left, then line 2 right-to-left"
        );
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
            legacy_symbol_rewrite: false,
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
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
                legacy_symbol_rewrite: false,
                font_size: fs,
                page: 1,
                is_bold: false,
                is_italic: false,
                font_weight: None,
                bold_source: None,
                fixed_pitch: None,
                is_underline: false,
                is_strikeout: false,
                rotation: 0.0,
                advance_known: true,
                item_type: ItemType::Text,
                mcid: None,
                baseline_shift: 0.0,
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
            legacy_symbol_rewrite: false,
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
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

    fn geometry_item(width: f32, font_size: f32, rotation: f32) -> TextItem {
        TextItem {
            baseline_shift: 0.0,
            text: "abcd".to_string(),
            x: 0.0,
            y: 0.0,
            width,
            height: font_size,
            rotation,
            advance_known: true,
            font: String::new(),
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn join_threshold_ignores_a_vertical_runs_em_width() {
        // For a vertical run `width` is its em, so the tight measured-width
        // path would read the x gap to the next run as a word boundary. The
        // pair falls back to the loose heuristic instead — which the same
        // geometry laid out upright does not.
        let mut prev = geometry_item(10.0, 30.0, 90.0);
        prev.text = "ab".to_string();
        prev.x = 100.0;
        prev.font_size = 10.0;
        let mut curr = geometry_item(10.0, 30.0, 90.0);
        curr.text = "cd".to_string();
        curr.x = 112.5;
        curr.font_size = 10.0;
        assert!(should_join_items(&prev, &curr, 0.1));

        prev.rotation = 0.0;
        prev.height = 10.0;
        curr.rotation = 0.0;
        curr.height = 10.0;
        assert!(!should_join_items(&prev, &curr, 0.1));
    }

    #[test]
    fn upside_down_lines_sort_right_to_left() {
        // A 180° run reads towards -x: the fragment painted first sits at
        // the largest x and must come first.
        let mut hello = geometry_item(30.0, 10.0, 180.0);
        hello.text = "HELLO".to_string();
        hello.x = 300.0;
        let mut world = geometry_item(30.0, 10.0, 180.0);
        world.text = "WORLD".to_string();
        world.x = 260.0;
        let mut items = vec![world.clone(), hello.clone()];
        sort_line_items(&mut items, false);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["HELLO", "WORLD"]);

        // Mixed or upright lines keep ascending x.
        items[0].rotation = 0.0;
        sort_line_items(&mut items, false);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["WORLD", "HELLO"]);

        // A link box on the line is axis-aligned (0°) and must not defeat the
        // mirrored sort of the text around it.
        let mut link = geometry_item(5.0, 10.0, 0.0);
        link.text = "link".to_string();
        link.x = 291.0;
        link.item_type = ItemType::Link("https://example.com/".to_string());
        let mut items = vec![world, link, hello];
        sort_line_items(&mut items, false);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["HELLO", "link", "WORLD"]);

        // An RTL line whose runs report 180° keeps the classic RTL order,
        // first word at the largest x: mirroring it would reverse the text.
        let mut first = geometry_item(30.0, 10.0, 180.0);
        first.text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}".to_string();
        first.x = 140.0;
        let mut second = geometry_item(30.0, 10.0, 180.0);
        second.text = "\u{05E2}\u{05D5}\u{05DC}\u{05DD}".to_string();
        second.x = 100.0;
        let mut items = vec![second.clone(), first.clone()];
        sort_line_items(&mut items, false);
        assert_eq!(items[0].text, first.text);
        assert_eq!(items[1].text, second.text);
    }

    #[test]
    fn extent_helpers_pass_the_box_through() {
        // Estimates for width-less fonts are laid into the box at extraction
        // and flagged, so the helpers never second-guess a positive width;
        // a box without one — measured zero or backwards — falls back to
        // half an em per character so layout has a footprint to reason with.
        let mut zero = geometry_item(0.0, 10.0, 0.0);
        zero.text = "ab".to_string();
        zero.font_size = 10.0;
        assert!(zero.advance_known);
        assert_eq!(
            (effective_width(&zero), effective_height(&zero)),
            (10.0, 10.0)
        );
        let mut backwards = zero.clone();
        backwards.width = -1.7;
        assert_eq!(effective_width(&backwards), 10.0);
        let mut unmeasured = zero.clone();
        unmeasured.advance_known = false;
        assert_eq!(effective_width(&unmeasured), 10.0);
        let mut estimated = geometry_item(20.0, 10.0, 0.0);
        estimated.advance_known = false;
        assert_eq!(effective_width(&estimated), 20.0);
        let mut short = geometry_item(6.0, 10.0, 45.0);
        short.height = 5.0;
        assert_eq!(
            (effective_width(&short), effective_height(&short)),
            (6.0, 5.0)
        );
    }
}
