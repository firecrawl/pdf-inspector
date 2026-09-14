//! `parse_simple_font_widths` must read the space width from the code that
//! actually carries `/space` when `/Encoding /Differences` moves it off 32.

use super::parse_simple_font_widths;
use lopdf::{dictionary, Document, Object};

/// A simple font whose `/Widths` covers codes 1..=120 with `default` for
/// every code, then the given overrides. `differences` becomes the
/// `/Encoding /Differences` array when present.
fn simple_font(
    default: i64,
    overrides: &[(u16, i64)],
    differences: Option<Vec<Object>>,
) -> (Document, lopdf::Dictionary) {
    let doc = Document::with_version("1.4");
    let first_char: u16 = 1;
    let last_char: u16 = 120;
    let widths: Vec<Object> = (first_char..=last_char)
        .map(|code| {
            overrides
                .iter()
                .find(|(c, _)| *c == code)
                .map_or(default, |(_, w)| *w)
                .into()
        })
        .collect();
    let mut font = dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "ABCDEF+SubsetSans",
        "FirstChar" => i64::from(first_char),
        "LastChar" => i64::from(last_char),
        "Widths" => widths,
    };
    if let Some(differences) = differences {
        font.set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => differences,
            },
        );
    }
    (doc, font)
}

fn space_width_of(doc: &Document, font: &lopdf::Dictionary) -> u16 {
    parse_simple_font_widths(doc, font)
        .expect("simple font with Widths parses")
        .space_width
}

#[test]
fn space_remapped_off_code_32_uses_its_own_width() {
    // The Start-Up guide's AkzidenzGrotesk subset: glyphs numbered from 1
    // in order of first use, `/space` at 26, code 32 present but unused.
    let (doc, font) = simple_font(
        556,
        &[(26, 288), (32, 0)],
        Some(vec![26.into(), Object::Name(b"space".to_vec())]),
    );
    assert_eq!(space_width_of(&doc, &font), 288);
}

#[test]
fn space_remapped_when_code_32_holds_another_glyph() {
    // Code 32 is a real glyph here (a letter), not the space.
    let (doc, font) = simple_font(
        556,
        &[(27, 271), (32, 611)],
        Some(vec![
            27.into(),
            Object::Name(b"space".to_vec()),
            32.into(),
            Object::Name(b"A".to_vec()),
        ]),
    );
    assert_eq!(space_width_of(&doc, &font), 271);
}

#[test]
fn no_encoding_keeps_code_32_width() {
    let (doc, font) = simple_font(556, &[(32, 278)], None);
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn named_encoding_keeps_code_32_width() {
    let (doc, mut font) = simple_font(556, &[(32, 278)], None);
    font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn differences_without_space_keep_code_32_width() {
    let (doc, font) = simple_font(
        556,
        &[(32, 278)],
        Some(vec![65.into(), Object::Name(b"Agrave".to_vec())]),
    );
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn explicit_space_at_code_32_is_unchanged() {
    let (doc, font) = simple_font(
        556,
        &[(32, 278)],
        Some(vec![32.into(), Object::Name(b"space".to_vec())]),
    );
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn zero_width_space_code_falls_back_to_default() {
    // Neither the remapped code nor code 32 has a usable width: the
    // pre-existing 250-unit default still applies.
    let (doc, font) = simple_font(
        556,
        &[(26, 0), (32, 0)],
        Some(vec![26.into(), Object::Name(b"space".to_vec())]),
    );
    assert_eq!(space_width_of(&doc, &font), 250);
}

#[test]
fn space_code_outside_widths_range_falls_back_to_default() {
    // `/space` mapped beyond LastChar: no metric to read.
    let (doc, font) = simple_font(
        556,
        &[(32, 0)],
        Some(vec![200.into(), Object::Name(b"space".to_vec())]),
    );
    assert_eq!(space_width_of(&doc, &font), 250);
}

#[test]
fn conflicting_space_widths_keep_code_32_width() {
    // Two codes claim the space with different advances: ambiguous, so the
    // code-32 metric stays in force.
    let (doc, font) = simple_font(
        556,
        &[(26, 288), (27, 300), (32, 278)],
        Some(vec![
            26.into(),
            Object::Name(b"space".to_vec()),
            Object::Name(b"space".to_vec()),
        ]),
    );
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn agreeing_duplicate_space_codes_use_the_shared_width() {
    let (doc, font) = simple_font(
        556,
        &[(26, 288), (27, 288), (32, 0)],
        Some(vec![
            26.into(),
            Object::Name(b"space".to_vec()),
            Object::Name(b"space".to_vec()),
        ]),
    );
    assert_eq!(space_width_of(&doc, &font), 288);
}

#[test]
fn indirect_differences_array_is_resolved() {
    let (mut doc, mut font) = simple_font(556, &[(26, 288), (32, 0)], None);
    let differences = doc.add_object(vec![26.into(), Object::Name(b"space".to_vec())]);
    let encoding = doc.add_object(dictionary! {
        "Type" => "Encoding",
        "Differences" => differences,
    });
    font.set("Encoding", encoding);
    assert_eq!(space_width_of(&doc, &font), 288);
}

#[test]
fn truetype_subset_with_remapped_space_uses_its_width() {
    // Same parser path as Type1; the other common simple-font subtype.
    let (doc, mut font) = simple_font(
        556,
        &[(1, 278), (32, 611)],
        Some(vec![1.into(), Object::Name(b"space".to_vec())]),
    );
    font.set("Subtype", Object::Name(b"TrueType".to_vec()));
    assert_eq!(space_width_of(&doc, &font), 278);
}

#[test]
fn type3_remapped_space_keeps_glyph_space_units() {
    // Type3 widths are in glyph space; the remapped width is taken as-is
    // and the FontMatrix scale still applies on top of it.
    let (doc, mut font) = simple_font(
        50,
        &[(26, 29), (32, 0)],
        Some(vec![26.into(), Object::Name(b"space".to_vec())]),
    );
    font.set("Subtype", Object::Name(b"Type3".to_vec()));
    font.set(
        "FontMatrix",
        vec![
            Object::Real(0.01),
            0.into(),
            0.into(),
            Object::Real(0.01),
            0.into(),
            0.into(),
        ],
    );
    let info = parse_simple_font_widths(&doc, &font).expect("Type3 font parses");
    assert_eq!(info.space_width, 29);
    assert!((info.units_scale - 0.01).abs() < 1e-6);
}

#[test]
fn type3_without_remapped_space_keeps_average_estimate() {
    // Control: no usable space metric on a non-standard scale still falls
    // back to the 45%-of-average estimate, not to the remap.
    let (doc, mut font) = simple_font(50, &[(32, 0)], None);
    font.set("Subtype", Object::Name(b"Type3".to_vec()));
    font.set(
        "FontMatrix",
        vec![
            Object::Real(0.01),
            0.into(),
            0.into(),
            Object::Real(0.01),
            0.into(),
            0.into(),
        ],
    );
    let info = parse_simple_font_widths(&doc, &font).expect("Type3 font parses");
    // 119 codes at 50 plus one at 0: average 49.58 → 22.
    assert_eq!(info.space_width, 22);
}
