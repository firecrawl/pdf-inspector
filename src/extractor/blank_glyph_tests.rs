use lopdf::{dictionary, Document, Object, ObjectId, Stream};

/// A minimal TrueType font: `head`, `hhea`, `maxp`, `hmtx`, `loca`, `glyf`
/// and a (1,0) format 0 `cmap`. Glyph 0 is `.notdef`; each entry in
/// `glyphs` is `(outlined, advance)`, mapped from `codes[i]`.
fn synthetic_truetype(glyphs: &[(bool, u16)], codes: &[u8]) -> Vec<u8> {
    let num_glyphs = glyphs.len() as u16 + 1;
    let square: Vec<u8> = {
        let mut g = Vec::new();
        g.extend(1i16.to_be_bytes()); // one contour
        for v in [0i16, 0, 500, 700] {
            g.extend(v.to_be_bytes()); // bbox
        }
        g.extend(2u16.to_be_bytes()); // endPtsOfContours
        g.extend(0u16.to_be_bytes()); // no instructions
        g.extend([1u8, 1, 1]); // on-curve, i16 coordinates
        for v in [0i16, 500, 0, 0, 0, 700] {
            g.extend(v.to_be_bytes());
        }
        g
    };
    let mut glyf = Vec::new();
    let mut loca: Vec<u8> = Vec::new();
    loca.extend(0u32.to_be_bytes());
    glyf.extend(&square); // .notdef has an outline
    loca.extend((glyf.len() as u32).to_be_bytes());
    for &(outlined, _) in glyphs {
        if outlined {
            glyf.extend(&square);
        }
        loca.extend((glyf.len() as u32).to_be_bytes());
    }
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    head[50..52].copy_from_slice(&1i16.to_be_bytes()); // long loca offsets
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut maxp = vec![0u8; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut hmtx = Vec::new();
    hmtx.extend(600u16.to_be_bytes());
    hmtx.extend(0i16.to_be_bytes());
    for &(_, advance) in glyphs {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut cmap = Vec::new();
    cmap.extend(0u16.to_be_bytes()); // version
    cmap.extend(1u16.to_be_bytes()); // one subtable
    cmap.extend(1u16.to_be_bytes()); // platform Macintosh
    cmap.extend(0u16.to_be_bytes()); // encoding Roman
    cmap.extend(12u32.to_be_bytes()); // offset
    cmap.extend(0u16.to_be_bytes()); // format 0
    cmap.extend(262u16.to_be_bytes());
    cmap.extend(0u16.to_be_bytes()); // language
    let mut glyph_ids = [0u8; 256];
    for (i, &code) in codes.iter().enumerate() {
        glyph_ids[code as usize] = i as u8 + 1;
    }
    cmap.extend(glyph_ids);
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    tables.sort_by_key(|(tag, _)| **tag);
    let mut font = Vec::new();
    font.extend(0x0001_0000u32.to_be_bytes());
    font.extend((tables.len() as u16).to_be_bytes());
    font.extend([0u8; 6]); // searchRange, entrySelector, rangeShift
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        font.extend(*tag);
        font.extend(0u32.to_be_bytes()); // checksum, unchecked
        font.extend((offset as u32).to_be_bytes());
        font.extend((data.len() as u32).to_be_bytes());
        let padded = data.len().div_ceil(4) * 4;
        body.extend(data);
        body.extend(std::iter::repeat_n(0u8, padded - data.len()));
        offset += padded;
    }
    font.extend(body);
    font
}

/// Space glyph on code `$` (0x24, no outline, advance 226) between two
/// letters, as Word 2011 for Mac writes it. `tounicode` may label `$` as
/// itself; `encoding` optionally adds an `/Encoding` name.
fn doc_with_font(
    glyphs: &[(bool, u16)],
    codes: &[u8],
    tounicode: Option<&str>,
    encoding: Option<&str>,
    content: &[u8],
) -> (Document, ObjectId) {
    let mut doc = Document::with_version("1.4");
    let font_file = doc.add_object(Stream::new(
        dictionary! {},
        synthetic_truetype(glyphs, codes),
    ));
    let mut widths = vec![Object::Integer(0); 256];
    for (i, &code) in codes.iter().enumerate() {
        widths[code as usize] = Object::Integer(i64::from(glyphs[i].1));
    }
    let mut font = dictionary! {
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "ABCDEF+Synthetic-Bold",
        "FirstChar" => 0, "LastChar" => 255, "Widths" => widths,
        "FontDescriptor" => dictionary! { "Flags" => 4, "FontFile2" => font_file },
    };
    if let Some(cmap) = tounicode {
        let cmap_id = doc.add_object(Stream::new(dictionary! {}, cmap.as_bytes().to_vec()));
        font.set("ToUnicode", cmap_id);
    }
    if let Some(name) = encoding {
        font.set("Encoding", Object::Name(name.as_bytes().to_vec()));
    }
    let font_id = doc.add_object(font);
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        "Contents" => content_id,
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

fn text_of(doc: &mut Document) -> String {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    crate::extract_text_with_positions_mem(&bytes)
        .unwrap()
        .into_iter()
        .map(|item| item.text)
        .collect::<Vec<_>>()
        .join("|")
}

/// Codes `!` `"` `$` map to glyphs 1 (`3`), 2 (`r`) and 3 (the blank space).
const WORD_FOR_MAC: &[(bool, u16)] = &[(true, 500), (true, 400), (false, 226)];
const CODES: &[u8] = &[0x21, 0x22, 0x24];
const STALE_TOUNICODE: &str = "1 begincodespacerange\n<00><FF>\nendcodespacerange\n\
3 beginbfchar\n<21><0033>\n<22><0072>\n<24><0024>\nendbfchar";
const CONTENT: &[u8] = b"BT /F1 12 Tf 1 0 0 1 40 700 Tm (!\"$\"!) Tj ET";

#[test]
fn synthetic_truetype_parses_with_the_intended_outlines() {
    let data = synthetic_truetype(WORD_FOR_MAC, CODES);
    let face = ttf_parser::Face::parse(&data, 0).expect("valid synthetic TrueType");
    assert_eq!(face.number_of_glyphs(), 4);
    let cmap = face.tables().cmap.unwrap();
    let subtable = cmap.subtables.get(0).unwrap();
    let gid = |code: u8| subtable.glyph_index(u32::from(code)).unwrap();
    assert!(face.glyph_bounding_box(gid(0x21)).is_some());
    assert!(face.glyph_bounding_box(gid(0x24)).is_none());
    assert_eq!(face.glyph_hor_advance(gid(0x24)), Some(226));
}

#[test]
fn blank_glyph_reads_as_space_despite_stale_tounicode() {
    let (mut doc, _) = doc_with_font(WORD_FOR_MAC, CODES, Some(STALE_TOUNICODE), None, CONTENT);
    assert_eq!(text_of(&mut doc), "3r r3");
}

#[test]
fn blank_glyph_reads_as_space_without_tounicode() {
    let (mut doc, _) = doc_with_font(WORD_FOR_MAC, CODES, None, None, CONTENT);
    assert_eq!(text_of(&mut doc), "!\" \"!");
}

#[test]
fn fonts_with_an_encoding_keep_their_tounicode() {
    // With `/Encoding` the codes no longer route through the font's own
    // cmap, so the outline evidence does not apply.
    let (mut doc, _) = doc_with_font(
        WORD_FOR_MAC,
        CODES,
        Some(STALE_TOUNICODE),
        Some("WinAnsiEncoding"),
        CONTENT,
    );
    assert_eq!(text_of(&mut doc), "3r$r3");
}

#[test]
fn all_blank_fonts_are_an_invisible_layer_and_stay_untouched() {
    let (mut doc, _) = doc_with_font(
        &[(false, 500), (false, 400), (false, 226)],
        CODES,
        Some(STALE_TOUNICODE),
        None,
        CONTENT,
    );
    assert_eq!(text_of(&mut doc), "3r$r3");
}

#[test]
fn blank_glyph_without_advance_is_not_a_space() {
    let (mut doc, _) = doc_with_font(
        &[(true, 500), (true, 400), (false, 0)],
        CODES,
        Some(STALE_TOUNICODE),
        None,
        CONTENT,
    );
    assert_eq!(text_of(&mut doc), "3r$r3");
}

#[test]
fn blank_glyph_labelled_as_invisible_formatting_is_not_a_space() {
    // A soft hyphen or a zero-width space renders as nothing: a blank glyph
    // is what the label already says, so it is not evidence of a stale
    // ToUnicode. Downstream cleanup may drop the invisible character, but
    // no word space is invented.
    for label in ["00AD", "200B"] {
        let cmap = STALE_TOUNICODE.replace("<24><0024>", &format!("<24><{label}>"));
        let (mut doc, _) = doc_with_font(WORD_FOR_MAC, CODES, Some(&cmap), None, CONTENT);
        let text = text_of(&mut doc);
        assert!(!text.contains(' '), "label {label}: {text:?}");
        assert_eq!(
            text.replace(['\u{00AD}', '\u{200B}'], ""),
            "3rr3",
            "label {label}"
        );
    }
}

#[test]
fn blank_glyph_labelled_as_a_tab_reads_as_a_word_space() {
    // Word for Mac labels the blank tab glyph U+0009; in prose it is a word
    // gap, and a literal tab has no use in Markdown.
    let cmap = STALE_TOUNICODE.replace("<24><0024>", "<24><0009>");
    let (mut doc, _) = doc_with_font(WORD_FOR_MAC, CODES, Some(&cmap), None, CONTENT);
    assert_eq!(text_of(&mut doc), "3r r3");
}

#[test]
fn non_symbolic_fonts_are_left_alone() {
    let (mut doc, font_id) =
        doc_with_font(WORD_FOR_MAC, CODES, Some(STALE_TOUNICODE), None, CONTENT);
    let descriptor = doc
        .get_object(font_id)
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"FontDescriptor")
        .unwrap()
        .clone();
    let mut descriptor = descriptor.as_dict().unwrap().clone();
    descriptor.set("Flags", 32); // Nonsymbolic
    doc.get_object_mut(font_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("FontDescriptor", descriptor);
    assert_eq!(text_of(&mut doc), "3r$r3");
}
