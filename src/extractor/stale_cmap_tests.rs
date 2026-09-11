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

fn subset_doc(glyphs: &[&str], differences: &[(u8, &str)], cmap: &str) -> (Document, ObjectId) {
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
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "ABCDEF+SyntheticFace",
        "FirstChar" => 0, "LastChar" => 255, "Widths" => vec![Object::Integer(500); 256],
        "Encoding" => dictionary! { "Differences" => encoding },
        "FontDescriptor" => dictionary! { "FontFile3" => font_file },
        "ToUnicode" => cmap_id,
    };
    let font_id = doc.add_object(font.clone());
    // A second font shares the same CMap but retains its original encoding.
    let mut unchanged_font = font;
    unchanged_font.set("Encoding", "StandardEncoding");
    let unchanged_id = doc.add_object(unchanged_font);
    let content = doc.add_object(Stream::new(dictionary! {},
        b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <212223304041> Tj ET\nBT /F2 12 Tf 1 0 0 1 40 670 Tm <212223> Tj ET".to_vec()));
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
fn stale_cmap_case_counterparts_need_exact_anchors_and_leave_unrelated_letters_alone() {
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
    // The lowercase counterpart corroborates Scaron, while neither case of
    // Nacute appears in the old CMap. Three exact anchors are still required.
    assert_eq!(first_text(&mut doc), "ščřŠ%\u{00a0}ffiΩ");
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
