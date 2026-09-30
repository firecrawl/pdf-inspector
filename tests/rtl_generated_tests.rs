//! Generated Arabic / RTL regression fixtures.
//!
//! There are no real Arabic documents in the test matrix — genuine ones are
//! almost all copyrighted or private, so nothing can be hosted (issue #218).
//! Instead of shipping sample files (or standing up a headless-Chrome
//! dependency in CI), these tests synthesize the PDFs in pure Rust at test
//! time, following the same hand-rolled `%PDF` + `ToUnicode` CMap approach the
//! repo already uses for the checked-in Hebrew fixtures that landed with
//! PR #440 ("reverse visual-order RTL text using glyph geometry").
//!
//! They are the regression guard for #440: base-alphabet RTL text stored in
//! *visual* (screen left-to-right) order must be reversed back to logical
//! reading order, while text stored in *logical* order must be left alone. A
//! codepoint-only trigger would corrupt one convention or the other, so both
//! storage conventions are generated and both must extract to the same logical
//! text.
//!
//! Assertions mirror the three that the issue calls out:
//!   1. digit groups (`126,248.34`) embedded in an Arabic line survive intact
//!      and unreversed,
//!   2. `لا` (lam-alef) sequences delivered as a single multi-character
//!      `ToUnicode` expansion round-trip character-identical,
//!   3. a negative control — the visual-order fixture must NOT come out in the
//!      raw stored (reversed) order, so a test that passes with #440 reverted
//!      would be caught.

use pdf_inspector::{extract_text_with_positions_mem, process_pdf_mem};

// ---------------------------------------------------------------------------
// Test vocabulary
// ---------------------------------------------------------------------------
//
// All words are base-alphabet Arabic (no presentation forms), so they take the
// #440 geometry-reversal path rather than the older NFKC/presentation-form path
// in `expand_ligatures`. Each of these words contains a `لا` (lam-alef) pair,
// which the generator always emits as a single glyph code mapped to the two
// code points U+0644 U+0627 — exercising the multi-character `ToUnicode`
// expansion that naive reversal breaks.

const SALAM: &str = "السلام"; // ا ل س ل ا م   (contains لا)
const KALAM: &str = "الكلام"; // ا ل ك ل ا م   (contains لا)
const KHILAL: &str = "خلال"; //  خ ل ا ل       (contains لا)
const NUMBER: &str = "126,248.34"; // ASCII digit group with separators

#[derive(Clone, Copy, PartialEq)]
enum Storage {
    /// Each show op stores its run's characters in screen (left-to-right)
    /// order — reversed relative to reading order — and ops walk the line
    /// left-to-right. This is how producers of *visible shaped* RTL text emit.
    Visual,
    /// Each show op stores one run in reading order and successive ops are
    /// positioned right-to-left across the line (the OCR-text-layer
    /// convention).
    Logical,
}

fn is_arabic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c))
}

/// Split a string into glyph "units": a `لا` (U+0644 U+0627) pair becomes one
/// unit (a single font glyph with a two-code-point `ToUnicode` mapping), every
/// other character is its own unit.
fn glyph_units(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut units = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{0644}' && i + 1 < chars.len() && chars[i + 1] == '\u{0627}' {
            units.push("\u{0644}\u{0627}".to_string());
            i += 2;
        } else {
            units.push(chars[i].to_string());
            i += 1;
        }
    }
    units
}

fn reversed(s: &str) -> String {
    s.chars().rev().collect()
}

/// How a token's characters are stored in a show op under `storage`. Arabic
/// runs are stored screen-order (reversed) for `Visual`; numbers are stored
/// left-to-right in *both* conventions (their bidi class is AN/EN — displayed
/// LTR even inside RTL).
fn stored_form(token: &str, storage: Storage) -> String {
    if is_arabic(token) && storage == Storage::Visual {
        reversed(token)
    } else {
        token.to_string()
    }
}

/// Build a single-page PDF from `lines`, each line a list of tokens in logical
/// (reading) order. A custom TrueType font carries a `ToUnicode` CMap so the
/// extractor recovers real Unicode; glyph *positions* (absolute `Tm` per op)
/// carry the geometry that #440's visual-vs-logical vote reads.
fn make_rtl_pdf(lines: &[Vec<&str>], storage: Storage) -> Vec<u8> {
    // 1. Collect every glyph unit across the document and assign each a byte
    //    code, building the ToUnicode bfchar table and the Widths array.
    let mut code_of: std::collections::BTreeMap<String, u8> = std::collections::BTreeMap::new();
    let mut next_code: u8 = 0x21;
    let mut assign = |unit: &str, code_of: &mut std::collections::BTreeMap<String, u8>| -> u8 {
        if let Some(&c) = code_of.get(unit) {
            c
        } else {
            let c = next_code;
            next_code += 1;
            code_of.insert(unit.to_string(), c);
            c
        }
    };

    // 2. Emit the content stream. Screen order (left-to-right) is the reverse
    //    of logical token order for an RTL line; x grows left-to-right.
    let mut content = String::from("BT\n/F1 12 Tf\n");
    let x0 = 90.0f32;
    let step = 72.0f32;
    let mut y = 700.0f32;

    for line in lines {
        let n = line.len();
        // screen_tokens[i] sits at x0 + i*step (i = 0 is the leftmost glyph).
        let screen_tokens: Vec<&str> = line.iter().rev().copied().collect();

        // Emission order sets the direction the vote sees:
        //   Visual  -> paint left-to-right  (increasing x) -> rightward votes
        //   Logical -> paint right-to-left  (decreasing x) -> leftward votes
        let order: Vec<usize> = match storage {
            Storage::Visual => (0..n).collect(),
            Storage::Logical => (0..n).rev().collect(),
        };

        for &i in &order {
            let token = screen_tokens[i];
            let x = x0 + (i as f32) * step;
            let stored = stored_form(token, storage);
            let mut hex = String::new();
            for unit in glyph_units(&stored) {
                let code = assign(&unit, &mut code_of);
                hex.push_str(&format!("{code:02X}"));
            }
            content.push_str(&format!("1 0 0 1 {x} {y} Tm <{hex}> Tj\n"));
        }
        y -= 40.0;
    }
    content.push_str("ET");

    // 3. ToUnicode CMap. Single-char units map to one UTF-16 code point; the
    //    لا unit maps to two (the multi-character expansion).
    let first_char = 0x21u8;
    let last_char = next_code.saturating_sub(1).max(first_char);
    let mut bfchar = String::new();
    let mut nchars = 0;
    for (unit, code) in &code_of {
        let dst: String = unit.chars().map(|c| format!("{:04X}", c as u32)).collect();
        bfchar.push_str(&format!("<{code:02X}> <{dst}>\n"));
        nchars += 1;
    }
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Test-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
         {nchars} beginbfchar\n{bfchar}endbfchar\n\
         endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend"
    );

    // Widths for every used code (all 500, like the Hebrew fixture).
    let widths: String = (first_char..=last_char)
        .map(|_| "500")
        .collect::<Vec<_>>()
        .join(" ");

    // 4. Assemble the objects with a byte-accurate xref (same shape as the
    //    repo's other hand-rolled fixtures).
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];
    fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &str) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body.as_bytes());
        pdf.extend_from_slice(b"\nendobj\n");
    }

    add_object(
        &mut pdf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 6 0 R >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        4,
        &format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /TestArabic /FirstChar {first_char} /LastChar {last_char} /Widths [{widths}] /ToUnicode 5 0 R >>"
        ),
    );
    add_object(
        &mut pdf,
        &mut offsets,
        5,
        &format!("<< /Length {} >>\nstream\n{}\nendstream", cmap.len(), cmap),
    );
    add_object(
        &mut pdf,
        &mut offsets,
        6,
        &format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ),
    );

    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len(),
            xref_start
        )
        .as_bytes(),
    );
    pdf
}

fn extract_markdown(pdf: &[u8]) -> String {
    process_pdf_mem(pdf)
        .expect("fixture should process")
        .markdown
        .expect("fixture should produce markdown")
}

/// Index of the first occurrence of `needle`, panicking with context if absent.
fn require_at(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("expected to find {needle:?} in extracted text:\n{haystack}"))
}

// ---------------------------------------------------------------------------
// Assertion 1 — digit groups survive reordering intact.
// ---------------------------------------------------------------------------

/// A digit group with a thousands separator and a decimal point, embedded in a
/// visual-order Arabic line, must come out byte-identical and unreversed — no
/// `126,248.34` -> `43.842,621`, no split separators, no migrated decimal
/// point. The surrounding Arabic still reverses to reading order around it.
#[test]
fn arabic_visual_order_keeps_digit_group_intact() {
    let pdf = make_rtl_pdf(&[vec![KALAM, NUMBER, KHILAL]], Storage::Visual);
    let md = extract_markdown(&pdf);

    assert!(
        md.contains(NUMBER),
        "digit group must survive intact, got:\n{md}"
    );
    assert!(
        !md.contains(&reversed(NUMBER)) && !md.contains("43.842,621"),
        "digit group must not be reversed, got:\n{md}"
    );
    // The two Arabic words must both be present in reading order (first token
    // reads first / rightmost), proving the number stayed put while the RTL
    // text around it was reordered.
    assert!(require_at(&md, KALAM) < require_at(&md, KHILAL));
    assert!(require_at(&md, NUMBER) > require_at(&md, KALAM));
    assert!(require_at(&md, NUMBER) < require_at(&md, KHILAL));
}

/// The same digit line stored in logical order must land on identical text: a
/// codepoint-only reversal trigger would flip the digits here.
#[test]
fn arabic_logical_order_keeps_digit_group_intact() {
    let pdf = make_rtl_pdf(&[vec![KALAM, NUMBER, KHILAL]], Storage::Logical);
    let md = extract_markdown(&pdf);

    assert!(
        md.contains(NUMBER),
        "digit group must survive intact, got:\n{md}"
    );
    assert!(require_at(&md, KALAM) < require_at(&md, KHILAL));
    assert!(require_at(&md, NUMBER) > require_at(&md, KALAM));
    assert!(require_at(&md, NUMBER) < require_at(&md, KHILAL));
}

/// The positioned-item API must agree: the number is its own item and its text
/// is exactly the digit group.
#[test]
fn arabic_digit_group_intact_in_positioned_items() {
    for storage in [Storage::Visual, Storage::Logical] {
        let pdf = make_rtl_pdf(&[vec![KALAM, NUMBER, KHILAL]], storage);
        let items = extract_text_with_positions_mem(&pdf).expect("extract positioned text");
        assert!(
            items.iter().any(|it| it.text == NUMBER),
            "a positioned item should carry the intact digit group"
        );
        // No item may carry a reversed digit run.
        assert!(
            !items.iter().any(|it| it.text.contains(&reversed(NUMBER))),
            "no positioned item may carry a reversed digit group"
        );
    }
}

// ---------------------------------------------------------------------------
// Assertion 2 — lam-alef (multi-character ToUnicode) round-trip.
// ---------------------------------------------------------------------------

/// Each word arrives with its `لا` delivered as a single glyph whose ToUnicode
/// expands to two code points. After the full visual-order reversal the words
/// must be character-identical to their logical forms — this is where naive
/// reversal quietly breaks (the text still reads as Arabic, just wrong).
#[test]
fn arabic_visual_order_ligature_round_trip() {
    let pdf = make_rtl_pdf(&[vec![KHILAL, SALAM, KALAM]], Storage::Visual);
    let md = extract_markdown(&pdf);

    for word in [KHILAL, SALAM, KALAM] {
        assert!(
            md.contains(word),
            "ligature word {word:?} must round-trip character-identical, got:\n{md}"
        );
    }
    // Reading order preserved: خلال، السلام، الكلام.
    assert!(require_at(&md, KHILAL) < require_at(&md, SALAM));
    assert!(require_at(&md, SALAM) < require_at(&md, KALAM));
}

/// Logical-order storage of the same words must not be reversed: the lam-alef
/// expansion round-trips only if the run is left alone.
#[test]
fn arabic_logical_order_ligature_round_trip() {
    let pdf = make_rtl_pdf(&[vec![KHILAL, SALAM, KALAM]], Storage::Logical);
    let md = extract_markdown(&pdf);

    for word in [KHILAL, SALAM, KALAM] {
        assert!(
            md.contains(word),
            "ligature word {word:?} must round-trip character-identical, got:\n{md}"
        );
    }
    assert!(require_at(&md, KHILAL) < require_at(&md, SALAM));
    assert!(require_at(&md, SALAM) < require_at(&md, KALAM));
}

// ---------------------------------------------------------------------------
// Assertion 3 — negative control.
// ---------------------------------------------------------------------------

/// If #440's reversal is reverted, the visual-order words come out in their raw
/// stored (reversed) order. This test asserts the *wrong* forms are ABSENT and
/// the correct forms present, so a pipeline that merely runs (without the fix)
/// fails here — the point of a negative control.
#[test]
fn visual_order_negative_control_wrong_output_is_rejected() {
    let pdf = make_rtl_pdf(&[vec![SALAM, KALAM]], Storage::Visual);
    let md = extract_markdown(&pdf);

    // Correct, logical forms present.
    assert!(md.contains(SALAM) && md.contains(KALAM), "got:\n{md}");

    // Raw stored (reversed) forms — what extraction yields with the fix
    // reverted — must be absent.
    let wrong_salam = reversed(SALAM); // مالسلا
    let wrong_kalam = reversed(KALAM); // مالكلا
    assert!(
        !md.contains(&wrong_salam) && !md.contains(&wrong_kalam),
        "reversed (unfixed) forms must not appear — this is the negative control, got:\n{md}"
    );
}

/// Sanity anchor: the two storage conventions of the same logical document must
/// extract to the same set of words in the same reading order. This is the
/// property #440's own Hebrew fixtures assert, restated for Arabic.
#[test]
fn visual_and_logical_storage_agree() {
    let doc = vec![vec![KHILAL, SALAM, KALAM], vec![KALAM, NUMBER, KHILAL]];
    let visual = extract_markdown(&make_rtl_pdf(&doc, Storage::Visual));
    let logical = extract_markdown(&make_rtl_pdf(&doc, Storage::Logical));

    for token in [KHILAL, SALAM, KALAM, NUMBER] {
        assert!(
            visual.contains(token),
            "visual missing {token:?}:\n{visual}"
        );
        assert!(
            logical.contains(token),
            "logical missing {token:?}:\n{logical}"
        );
    }
    // Same relative reading order in both.
    assert!(require_at(&visual, SALAM) < require_at(&visual, KALAM));
    assert!(require_at(&logical, SALAM) < require_at(&logical, KALAM));
}
