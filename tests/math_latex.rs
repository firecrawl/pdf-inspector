use lopdf::{dictionary, Document, Object, Stream};
use pdf_inspector::{extract_math_latex_in_regions_mem, MathGate, PositionFrame};

/// One page, 600x400pt, whose fonts are named after the TeX fonts they
/// stand in for. Every glyph advances 500/1000 em so positions follow the
/// operators directly.
fn pdf(content: &str, rotate: Option<i64>) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut fonts = lopdf::Dictionary::new();
    for (name, base) in [
        ("F1", "ABCDEF+CMMI10"),
        ("F2", "ABCDEF+CMR10"),
        ("F3", "ABCDEF+CMR7"),
        ("F4", "ABCDEF+CMEX10"),
        ("F5", "ABCDEF+CMMI7"),
        ("F6", "Courier"),
    ] {
        let id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => base,
            "FirstChar" => 0, "LastChar" => 255,
            "Widths" => Object::Array((0..=255).map(|_| 500.into()).collect()),
        });
        fonts.set(name, id);
    }
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let mut page = dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 600.into(), 400.into()],
        "Resources" => dictionary! { "Font" => fonts },
        "Contents" => content_id,
    };
    if let Some(rotate) = rotate {
        page.set("Rotate", rotate);
    }
    let page_id = doc.add_object(page);
    doc.objects.insert(
        pages_id,
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page_id.into()] }.into(),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save");
    out
}

fn read(bytes: &[u8], rect: [f32; 4], frame: PositionFrame) -> (String, MathGate) {
    let result = extract_math_latex_in_regions_mem(bytes, &[(0, vec![rect])], frame).expect("read");
    let region = &result[0].regions[0];
    (region.latex.clone(), region.gate)
}

const SCRIPTS: &str = "BT /F1 10 Tf 100 300 Td (x) Tj ET
BT /F3 7 Tf 105 304 Td (2) Tj ET
BT /F2 10 Tf 110 300 Td (+) Tj ET
BT /F1 10 Tf 117 300 Td (y) Tj ET
BT /F5 7 Tf 122 297 Td (i) Tj ET";

#[test]
fn baselines_and_sizes_become_scripts() {
    let bytes = pdf(SCRIPTS, None);
    let (latex, gate) = read(&bytes, [95.0, 88.0, 140.0, 106.0], PositionFrame::Sheet);
    assert_eq!(gate, MathGate::Pass);
    assert_eq!(latex, "x^2+y_i");
}

#[test]
fn display_frame_reads_rects_on_the_rotated_page() {
    let bytes = pdf(SCRIPTS, Some(90));
    // Turned clockwise, the sheet's [95, 88, 140, 106] lands at
    // x = 400 - 106 .. 400 - 88 and y = 95 .. 140 on the rendered page.
    let (latex, gate) = read(&bytes, [294.0, 95.0, 312.0, 140.0], PositionFrame::Display);
    assert_eq!(gate, MathGate::Pass);
    assert_eq!(latex, "x^2+y_i");
}

#[test]
fn glyphs_under_and_over_a_large_operator_are_its_limits() {
    // cmex 0x58 is the display summation sign, hanging below its origin.
    let content = "BT /F4 10 Tf 100 305 Td <58> Tj ET
BT /F5 7 Tf 100 288 Td (i) Tj ET
BT /F3 7 Tf 102.5 288 Td (=1) Tj ET
BT /F5 7 Tf 102 307 Td (n) Tj ET
BT /F1 10 Tf 112 296 Td (a) Tj ET";
    let bytes = pdf(content, None);
    let (latex, gate) = read(&bytes, [95.0, 85.0, 125.0, 115.0], PositionFrame::Sheet);
    assert_eq!(gate, MathGate::Pass);
    assert_eq!(latex, "\\sum_{i=1}^na");
}

#[test]
fn upright_limit_words_take_limits() {
    let content = "BT /F2 10 Tf 100 300 Td (lim) Tj ET
BT /F5 7 Tf 103 292 Td (n) Tj ET
BT /F1 10 Tf 118 300 Td (a) Tj ET";
    let bytes = pdf(content, None);
    let (latex, _) = read(&bytes, [95.0, 88.0, 130.0, 110.0], PositionFrame::Sheet);
    assert_eq!(latex, "\\lim_na");
}

#[test]
fn a_page_set_in_a_text_face_is_not_read() {
    let bytes = pdf("BT /F6 10 Tf 100 300 Td (x+y=1) Tj ET", None);
    let (latex, gate) = read(&bytes, [95.0, 88.0, 140.0, 106.0], PositionFrame::Sheet);
    assert_eq!(gate, MathGate::NonMathFonts);
    assert_eq!(latex, "");
}

#[test]
fn an_invisible_text_layer_is_not_read() {
    let bytes = pdf("BT 3 Tr /F1 10 Tf 100 300 Td (xy) Tj ET", None);
    let (latex, gate) = read(&bytes, [95.0, 88.0, 140.0, 106.0], PositionFrame::Sheet);
    assert_eq!(gate, MathGate::Invisible);
    assert_eq!(latex, "");
}

#[test]
fn an_empty_region_reports_no_glyphs() {
    let bytes = pdf(SCRIPTS, None);
    let (_, gate) = read(&bytes, [300.0, 200.0, 350.0, 220.0], PositionFrame::Sheet);
    assert_eq!(gate, MathGate::NoGlyphs);
    let result = extract_math_latex_in_regions_mem(
        &bytes,
        &[(5, vec![[0.0, 0.0, 10.0, 10.0]])],
        PositionFrame::Sheet,
    )
    .expect("read");
    assert_eq!(result[0].page, 5);
    assert_eq!(result[0].regions[0].gate, MathGate::NoGlyphs);
}
