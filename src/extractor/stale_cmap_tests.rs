use super::*;
use lopdf::{dictionary, Stream};

const GLYPHS: &[&str] = &["scaron", "ccaron", "rcaron", "uni00A0", "Aacute", "Eacute"];
const DIFFERENCES: &[(u8, &str)] = &[
    (33, "scaron"),
    (34, "ccaron"),
    (35, "rcaron"),
    (48, "uni00A0"),
    (64, "Aacute"),
    (65, "Eacute"),
];
const STALE_MAP: &str = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
1 beginbfrange\n<20><7e><0020>\nendbfrange\n\
6 beginbfchar\n<90><0161>\n<91><010d>\n<92><0159>\n<93><00a0>\n<40><006600660069>\n<41><03a9>\nendbfchar";

fn cff_index(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (objects.len() as u16).to_be_bytes().to_vec();
    if objects.is_empty() {
        return bytes;
    }
    bytes.push(2); // two-byte offsets
    let mut offset = 1u16;
    bytes.extend(offset.to_be_bytes());
    for object in objects {
        offset += object.len() as u16;
        bytes.extend(offset.to_be_bytes());
    }
    for object in objects {
        bytes.extend(object);
    }
    bytes
}

// A tiny, invented CFF program with named glyphs. All charstrings draw a box;
// their Unicode names and the PDF's encoding exercise decoding, not OCR.
fn named_cff(glyphs: &[&str]) -> Vec<u8> {
    let names = cff_index(&[b"SyntheticFace".to_vec()]);
    let strings = cff_index(
        &glyphs
            .iter()
            .map(|g| g.as_bytes().to_vec())
            .collect::<Vec<_>>(),
    );
    let top_len = cff_index(&[vec![0; 12]]).len();
    let charset_offset = 4 + names.len() + top_len + strings.len() + 2;
    let charstrings_offset = charset_offset + 1 + glyphs.len() * 2;
    let mut top = vec![29];
    top.extend((charset_offset as u32).to_be_bytes());
    top.extend([15, 29]);
    top.extend((charstrings_offset as u32).to_be_bytes());
    top.push(17);
    let mut bytes = vec![1, 0, 4, 4];
    bytes.extend(names);
    bytes.extend(cff_index(&[top]));
    bytes.extend(strings);
    bytes.extend([0, 0]); // no global subroutines
    bytes.push(0); // charset format 0
    for (i, glyph) in glyphs.iter().enumerate() {
        let sid = match *glyph {
            "Aacute" => 171u16,
            "Eacute" => 178,
            "scaron" => 221,
            "Scaron" => 192,
            _ => 391 + i as u16,
        };
        bytes.extend(sid.to_be_bytes());
    }
    let outline = vec![139, 139, 21, 189, 139, 5, 139, 189, 5, 89, 139, 5, 14];
    bytes.extend(cff_index(&vec![outline; glyphs.len() + 1]));
    bytes
}

const SUBSET_CONTENT: &[u8] = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <212223304041> Tj ET\nBT /F2 12 Tf 1 0 0 1 40 670 Tm <212223> Tj ET";

fn subset_doc(glyphs: &[&str], differences: &[(u8, &str)], cmap: &str) -> (Document, ObjectId) {
    font_doc(glyphs, differences, cmap, (0, 255), SUBSET_CONTENT)
}

/// A one-page document showing `content` with `/F1`, a Type1 font over the
/// synthetic CFF of `glyphs`, encoded by `differences`, covering the codes
/// `first_last` with its width table, under the ToUnicode CMap `cmap`; and
/// `/F2`, a font sharing the CMap but keeping its original encoding.
fn font_doc(
    glyphs: &[&str],
    differences: &[(u8, &str)],
    cmap: &str,
    first_last: (u8, u8),
    content: &[u8],
) -> (Document, ObjectId) {
    let mut doc = Document::with_version("1.4");
    let cmap_id = doc.add_object(Stream::new(dictionary! {}, cmap.as_bytes().to_vec()));
    let font_file = doc.add_object(Stream::new(
        dictionary! { "Subtype" => "Type1C" },
        named_cff(glyphs),
    ));
    let encoding: Vec<Object> = differences
        .iter()
        .flat_map(|(code, name)| {
            [
                Object::Integer(*code as i64),
                Object::Name(name.as_bytes().to_vec()),
            ]
        })
        .collect();
    let (first, last) = first_last;
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "ABCDEF+SyntheticFace",
        "FirstChar" => i64::from(first), "LastChar" => i64::from(last),
        "Widths" => vec![Object::Integer(500); usize::from(last - first) + 1],
        "Encoding" => dictionary! { "Differences" => encoding },
        "FontDescriptor" => dictionary! { "FontFile3" => font_file },
        "ToUnicode" => cmap_id,
    };
    let font_id = doc.add_object(font.clone());
    // A second font shares the same CMap but retains its original encoding.
    let mut unchanged_font = font;
    unchanged_font.set("Encoding", "StandardEncoding");
    let unchanged_id = doc.add_object(unchanged_font);
    let content = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id, "F2" => unchanged_id } },
        "Contents" => content,
    });
    let pages_id = doc.add_object(
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page_id)] },
    );
    doc.get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Parent", pages_id);
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    (doc, font_id)
}

fn first_text(doc: &mut Document) -> String {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    crate::extract_text_with_positions_mem(&bytes)
        .unwrap()
        .into_iter()
        .find(|item| item.text.contains("ffi"))
        .unwrap()
        .text
}

/// The text of every item of the page, top line first.
fn line_texts(doc: &mut Document) -> Vec<String> {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let mut items = crate::extract_text_with_positions_mem(&bytes).unwrap();
    items.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    items.into_iter().map(|item| item.text).collect()
}

/// The repairs the stale-CMap check makes for `/F1` of `doc`.
fn overrides(doc: &Document, font_id: ObjectId) -> HashMap<u8, String> {
    let fonts = FontCMaps::from_doc(doc);
    let font = doc.get_dictionary(font_id).unwrap();
    let encoding = parse_font_encoding(doc, font).unwrap();
    stale_identity_cmap_overrides(doc, font, &fonts, &encoding)
}

#[test]
fn stale_cmap_repairs_only_font_backed_identity_entries_through_extraction() {
    let raw_cff = named_cff(GLYPHS);
    let parsed = ttf_parser::cff::Table::parse(&raw_cff).expect("valid synthetic CFF");
    for name in GLYPHS {
        assert!(
            parsed.glyph_index_by_name(name).is_some(),
            "missing glyph {name}"
        );
    }
    let (mut doc, _) = subset_doc(GLYPHS, DIFFERENCES, STALE_MAP);
    assert_eq!(first_text(&mut doc), "ščř\u{00a0}ffiΩ");
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let items = crate::extract_text_with_positions_mem(&bytes).unwrap();
    assert!(
        items.iter().any(|item| item.text == "!\"#"),
        "another font sharing the CMap must remain unchanged"
    );
}

#[test]
fn stale_cmap_keeps_valid_nonidentity_mappings_and_ligatures() {
    let valid =
        format!("{STALE_MAP}\n3 beginbfchar\n<21><017e>\n<22><0107>\n<23><00660069>\nendbfchar");
    let (mut doc, _) = subset_doc(GLYPHS, DIFFERENCES, &valid);
    assert_eq!(first_text(&mut doc), "žćfi0ffiΩ");
}

#[test]
fn stale_cmap_requires_multiple_distinct_letter_disagreements() {
    for differences in [
        &DIFFERENCES[..1],
        &[(33, "scaron"), (34, "scaron"), (35, "scaron")],
    ] {
        let (mut doc, _) = subset_doc(GLYPHS, differences, STALE_MAP);
        assert_eq!(first_text(&mut doc), "!\"#0ffiΩ");
    }
}

#[test]
fn stale_cmap_requires_the_letter_mapping_elsewhere_in_the_cmap() {
    let cmap = STALE_MAP.replace("<90><0161>", "<90><0041>");
    let (mut doc, _) = subset_doc(GLYPHS, DIFFERENCES, &cmap);
    assert_eq!(first_text(&mut doc), "!\"#0ffiΩ");
}

#[test]
fn stale_cmap_proven_by_its_anchors_repairs_the_letters_it_never_mapped_too() {
    let glyphs = [GLYPHS, &["Scaron", "Nacute"]].concat();
    let differences = [DIFFERENCES, &[(36, "Scaron"), (37, "Nacute")]].concat();
    let (mut doc, font_id) = subset_doc(&glyphs, &differences, STALE_MAP);
    let page = *doc.get_pages().values().next().unwrap();
    let contents = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_object_mut(contents)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_plain_content(b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <2122232425304041> Tj ET".to_vec());
    // Three letters the old CMap still maps at other codes prove it stale;
    // then a letter it never mapped at all (Nacute) is a glyph that moved
    // like the others, and its slot reads by its name too.
    assert_eq!(first_text(&mut doc), "ščřŠŃ\u{00a0}ffiΩ");
    let cmap_id = doc
        .get_dictionary(font_id)
        .unwrap()
        .get(b"ToUnicode")
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_object_mut(cmap_id)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_plain_content(STALE_MAP.replace("<91><010d>", "<91><0041>").into_bytes());
    assert_eq!(first_text(&mut doc), "!\"#$%0ffiΩ");
}

#[test]
fn stale_cmap_requires_the_named_glyphs_in_the_embedded_font() {
    let (mut doc, _) = subset_doc(&["alpha", "beta", "gamma"], DIFFERENCES, STALE_MAP);
    assert_eq!(first_text(&mut doc), "!\"#0ffiΩ");
}

#[test]
fn stale_cmap_requires_a_simple_embedded_cff_font() {
    for subtype in ["Type0", "TrueType", "Type3"] {
        let (mut doc, font_id) = subset_doc(GLYPHS, DIFFERENCES, STALE_MAP);
        doc.get_object_mut(font_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Subtype", subtype);
        let fonts = FontCMaps::from_doc(&doc);
        let font = doc.get_dictionary(font_id).unwrap();
        let encoding = parse_font_encoding(&doc, font).unwrap();
        assert!(stale_identity_cmap_overrides(&doc, font, &fonts, &encoding).is_empty());
    }
    let (mut doc, font_id) = subset_doc(GLYPHS, DIFFERENCES, STALE_MAP);
    doc.get_object_mut(font_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .remove(b"FontDescriptor");
    assert_eq!(first_text(&mut doc), "!\"#0ffiΩ");
}

// A re-encoded font of a right-to-left script: its glyphs, named by their
// code points and positional forms, sit at codes from 33 upwards, under the
// original font's CMap — laid out in the original's slots (0x01-0x1F,
// 0x7F-0xF4) with the bracket, punctuation and letter slots of ASCII
// describing those slots' old occupants. Only one code in five of the CMap
// lies within the font's own width table.
const RTL_GLYPHS: &[&str] = &[
    "uni0647",
    "uni0645.m",
    "uni064A.m",
    "uni0628.i",
    "uni0622",
    "uni06440627.f",
    "arHamzaAboveCCMP",
];
const RTL_DIFFERENCES: &[(u8, &str)] = &[
    (0x21, "uni0647"),
    (0x26, "uni0645.m"),
    (0x28, "uni064A.m"),
    (0x29, "uni0628.i"),
    (0x2E, "uni0622"),
    (0x3C, "uni06440627.f"),
    (0x69, "arHamzaAboveCCMP"),
];
const RTL_STALE_MAP: &str = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
2 beginbfrange\n<01><1F><0660>\n<7F><F4><FB50>\nendbfrange\n\
5 beginbfchar\n<28><0029>\n<29><0028>\n<2E><002E>\n<3C><003E>\n<69><0069>\nendbfchar";
// Lines painted in display order (left to right), one word each: the four
// letters heh, meem, yeh, beh; a lam-alef glyph then heh; heh then the
// mark glyph; heh then alef with madda.
const RTL_CONTENT: &[u8] = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <21262829> Tj ET\n\
BT /F1 12 Tf 1 0 0 1 40 670 Tm <3C21> Tj ET\n\
BT /F1 12 Tf 1 0 0 1 40 640 Tm <2169> Tj ET\n\
BT /F1 12 Tf 1 0 0 1 40 610 Tm <212E> Tj ET";

#[test]
fn re_encoded_right_to_left_font_reads_by_its_differences_under_its_stale_cmap() {
    let (mut doc, font_id) = font_doc(
        RTL_GLYPHS,
        RTL_DIFFERENCES,
        RTL_STALE_MAP,
        (0x21, 0x69),
        RTL_CONTENT,
    );
    // No letter the Differences name appears elsewhere in the CMap: the
    // proof is the CMap's own range, most of it outside the font's codes.
    let repairs = overrides(&doc, font_id);
    assert_eq!(repairs.get(&0x28).map(String::as_str), Some("\u{064A}"));
    assert_eq!(repairs.get(&0x29).map(String::as_str), Some("\u{0628}"));
    assert_eq!(repairs.get(&0x2E).map(String::as_str), Some("\u{0622}"));
    assert_eq!(
        repairs.get(&0x3C).map(String::as_str),
        Some("\u{0644}\u{0627}")
    );
    assert_eq!(repairs.get(&0x69).map(String::as_str), Some(""));
    assert_eq!(repairs.len(), 5);
    // Read back into logical order: the mirrored bracket slots are the
    // letters yeh and beh, the period slot is the alef, the ligature keeps
    // the order of its letters, and the mark glyph adds no stray `i`.
    assert_eq!(
        line_texts(&mut doc),
        [
            "\u{0628}\u{064A}\u{0645}\u{0647}",
            "\u{0647}\u{0644}\u{0627}",
            "\u{0647}",
            "\u{0622}\u{0647}",
        ]
    );
}

#[test]
fn stale_cmap_proven_by_its_code_range_repairs_a_digit_slot() {
    // Tabular digits at codes 33 upwards under a CMap copied from the
    // parent subset: the letter slots it maps are all outside the font's
    // seven codes, and its apostrophe slot — `quoteright` at 0x27, as
    // StandardEncoding has it — is where the five now sits.
    let glyphs = [
        "one.tnum",
        "zero.tnum",
        "two.tnum",
        "three.tnum",
        "four.tnum",
        "five.tnum",
        "period",
    ];
    let differences = [
        (0x21, "one.tnum"),
        (0x22, "zero.tnum"),
        (0x23, "two.tnum"),
        (0x24, "three.tnum"),
        (0x25, "four.tnum"),
        (0x27, "five.tnum"),
        (0x2E, "period"),
    ];
    let stale = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
3 beginbfrange\n<27><27><2019>\n<41><5A><0041>\n<61><7A><0061>\nendbfrange\n\
1 beginbfchar\n<2E><002E>\nendbfchar";
    let content = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <272E23> Tj ET";
    let (mut doc, font_id) = font_doc(&glyphs, &differences, stale, (0x21, 0x2E), content);
    let repairs = overrides(&doc, font_id);
    assert_eq!(repairs.get(&0x27).map(String::as_str), Some("5"));
    // The period agrees with its slot and is no repair.
    assert_eq!(repairs.len(), 1);
    assert_eq!(line_texts(&mut doc), ["5.2"]);
    // The same font under a CMap that maps the five as a five needs none.
    let agreeing = stale.replace("<27><27><2019>", "<27><27><0035>");
    let (mut doc, font_id) = font_doc(&glyphs, &differences, &agreeing, (0x21, 0x2E), content);
    assert!(overrides(&doc, font_id).is_empty());
    assert_eq!(line_texts(&mut doc), ["5.2"]);
}

#[test]
fn font_whose_cmap_agrees_with_its_differences_is_left_alone() {
    // A CMap mostly outside the font's codes proves nothing by itself: the
    // slots agree with the names (a small capital differing only in
    // capitalization, a quote differing only in convention), so nothing is
    // repaired and the string reads through the CMap as before.
    let glyphs = ["A", "A.sc", "quotesingle", "zero.tnum"];
    let differences = [
        (0x41, "A"),
        (0x61, "A.sc"),
        (0x27, "quotesingle"),
        (0x30, "zero.tnum"),
    ];
    let cmap = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
1 beginbfrange\n<80><FF><0400>\nendbfrange\n\
4 beginbfchar\n<41><0041>\n<61><0061>\n<27><2019>\n<30><0030>\nendbfchar";
    let content = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <41612730> Tj ET";
    let (mut doc, font_id) = font_doc(&glyphs, &differences, cmap, (0x27, 0x61), content);
    assert!(overrides(&doc, font_id).is_empty());
    assert_eq!(line_texts(&mut doc), ["Aa\u{2019}0"]);
}

#[test]
fn stale_cmap_anchors_accept_mirrored_bracket_slots() {
    // Three letters at the bracket slots, whose entries are the mirror
    // images of the brackets, corroborated by the same letters at the old
    // codes; the font's width table covers every code, so only the anchors
    // can prove the CMap stale.
    let glyphs = ["uni05D0", "uni05D1", "uni05D2"];
    let differences = [(0x28, "uni05D0"), (0x29, "uni05D1"), (0x3C, "uni05D2")];
    let cmap = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
6 beginbfchar\n<28><0029>\n<29><0028>\n<3C><003E>\n<90><05D0>\n<91><05D1>\n<92><05D2>\nendbfchar";
    let content = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <28293C> Tj ET";
    let (mut doc, font_id) = font_doc(&glyphs, &differences, cmap, (0, 255), content);
    assert_eq!(overrides(&doc, font_id).len(), 3);
    assert_eq!(line_texts(&mut doc), ["\u{05D2}\u{05D1}\u{05D0}"]);
    // One anchor short, nothing is repaired: the brackets stand.
    let (mut doc, font_id) = font_doc(
        &glyphs,
        &differences,
        &cmap.replace("<92><05D2>", "<92><0041>"),
        (0, 255),
        content,
    );
    assert!(overrides(&doc, font_id).is_empty());
    assert_eq!(line_texts(&mut doc), [")(>"]);
}

#[test]
fn a_nameless_glyph_reads_as_nothing_only_under_a_proven_stale_cmap() {
    let glyphs = ["uni05D0", "arHamzaAboveCCMP"];
    let differences = [(0x28, "uni05D0"), (0x69, "arHamzaAboveCCMP")];
    let cmap = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
3 beginbfchar\n<28><0029>\n<69><0069>\n<90><05D0>\nendbfchar";
    let content = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <2869> Tj ET";
    let (mut doc, font_id) = font_doc(&glyphs, &differences, cmap, (0, 255), content);
    assert!(overrides(&doc, font_id).is_empty());
    assert_eq!(line_texts(&mut doc), [")i"]);
}
