//! A kashida glyph that a ToUnicode CMap reads as U+FFFD is dropped from the
//! text, and no other U+FFFD entry is.

use super::*;
use lopdf::{dictionary, Document, Object, Stream};

const FFFD: char = '\u{FFFD}';

/// Eight Arabic letters, on codes 0x10..=0x17: what makes a CMap an Arabic
/// font's.
const ARABIC_ENTRIES: &str = "<0010><0627>\n<0011><0628>\n<0012><062A>\n<0013><062B>\n\
<0014><062C>\n<0015><062D>\n<0016><062E>\n<0017><062F>\n";
const LATIN_ENTRIES: &str = "<0010><0061>\n<0011><0062>\n<0012><0063>\n<0013><0064>\n\
<0014><0065>\n<0015><0066>\n<0016><0067>\n<0017><0068>\n";

/// Glyph 1 is a letter, 2 a kashida (it spans its advance, low and flat),
/// 3 a hyphen (flat, but at mid height), 4 a low stroke inside its advance,
/// 5 a tall bar that spans its advance. Each is `(x_min, y_min, x_max,
/// y_max, advance)` in 1000 units per em.
const GLYPHS: &[(i16, i16, i16, i16, u16)] = &[
    (50, 0, 450, 600, 500),
    (-10, 0, 260, 70, 250),
    (0, 250, 250, 320, 250),
    (60, 0, 190, 70, 250),
    (-10, 0, 260, 600, 250),
];

/// A TrueType font of rectangular-box glyphs (`.notdef` first, then `glyphs`):
/// a triangle whose bounding box is the box, which is all that is read.
fn boxed_truetype(glyphs: &[(i16, i16, i16, i16, u16)]) -> Vec<u8> {
    let num_glyphs = glyphs.len() as u16 + 1;
    let mut glyf = Vec::new();
    let mut loca: Vec<u8> = vec![0; 4];
    loca.extend(0u32.to_be_bytes()); // .notdef has no outline
    for &(x_min, y_min, x_max, y_max, _) in glyphs {
        let mut g = Vec::new();
        g.extend(1i16.to_be_bytes());
        for v in [x_min, y_min, x_max, y_max] {
            g.extend(v.to_be_bytes());
        }
        g.extend(2u16.to_be_bytes()); // endPtsOfContours
        g.extend(0u16.to_be_bytes()); // no instructions
        g.extend([1u8, 1, 1]); // on-curve, i16 deltas
        for v in [x_min, x_max - x_min, 0] {
            g.extend(v.to_be_bytes());
        }
        for v in [y_min, 0, y_max - y_min] {
            g.extend(v.to_be_bytes());
        }
        while g.len() % 4 != 0 {
            g.push(0);
        }
        glyf.extend(g);
        loca.extend((glyf.len() as u32).to_be_bytes());
    }
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&1i16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut maxp = vec![0u8; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut hmtx = Vec::new();
    hmtx.extend(600u16.to_be_bytes());
    hmtx.extend(0i16.to_be_bytes());
    for &(.., advance) in glyphs {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
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
    font.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        font.extend(*tag);
        font.extend(0u32.to_be_bytes());
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

/// The CMap text: `entries` plus codes 1..=5 (the glyphs above) read as
/// U+0628 for the letter and U+FFFD for the rest.
fn cmap_text(entries: &str) -> String {
    format!(
        "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
13 beginbfchar\n<0001><0628>\n<0002><FFFD>\n<0003><FFFD>\n<0004><FFFD>\n<0005><FFFD>\n{entries}endbfchar"
    )
}

/// A one-page document with an Identity-H CIDFontType2 on the boxed font,
/// whose ToUnicode is `tounicode`, showing each of the hex strings `shows` as a TJ array
/// (a `> n <` inside one is a gap between two strings), one line each.
fn type0_document(tounicode: &str, shows: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let font_file = doc.add_object(Stream::new(dictionary! {}, boxed_truetype(GLYPHS)));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "ABCDEF+Synthetic", "Flags" => 4,
        "FontFile2" => font_file,
    });
    let cid_font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "ABCDEF+Synthetic",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"), "Supplement" => 0,
        },
        "CIDToGIDMap" => "Identity", "FontDescriptor" => descriptor, "DW" => 500,
    });
    let to_unicode = doc.add_object(Stream::new(dictionary! {}, tounicode.as_bytes().to_vec()));
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "ABCDEF+Synthetic",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(cid_font)],
        "ToUnicode" => to_unicode,
    });
    let content = doc.add_object(Stream::new(
        dictionary! {},
        format!(
            "BT /F1 12 Tf 14 TL 1 0 0 1 40 700 Tm {} ET",
            shows
                .iter()
                .map(|hex| format!("[<{hex}>] TJ T*"))
                .collect::<String>()
        )
        .into_bytes(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    let pages = doc.add_object(
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page)] },
    );
    doc.get_object_mut(page)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Parent", pages);
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn text_of(bytes: &[u8]) -> String {
    crate::extract_text_with_positions_mem(bytes)
        .unwrap()
        .into_iter()
        .map(|item| item.text)
        .collect::<Vec<_>>()
        .join("|")
}

fn count(text: &str, ch: char) -> usize {
    text.chars().filter(|&c| c == ch).count()
}

#[test]
fn synthetic_font_has_the_intended_glyph_boxes() {
    let data = boxed_truetype(GLYPHS);
    let face = ttf_parser::Face::parse(&data, 0).expect("valid synthetic TrueType");
    assert_eq!(face.number_of_glyphs(), 6);
    let glyphs = ProgramGlyphs::parse(&data).unwrap();
    let kashidas: Vec<u16> = (0..6).filter(|&gid| glyphs.is_kashida(gid)).collect();
    assert_eq!(kashidas, vec![2]);
}

#[test]
fn replacement_codes_lists_entries_and_range_starts_that_read_u_fffd() {
    let cmap = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
1 beginbfchar\n<0002><FFFD>\nendbfchar\n1 beginbfrange\n<0009><0009><FFFD>\nendbfrange\n\
1 beginbfchar\n<0003><0041FFFD>\nendbfchar",
    )
    .unwrap();
    assert_eq!(cmap.replacement_codes(), vec![2, 9]);
}

#[test]
fn only_a_cmap_of_arabic_letters_is_an_arabic_script_cmap() {
    let arabic = ToUnicodeCMap::parse(cmap_text(ARABIC_ENTRIES).as_bytes()).unwrap();
    let latin = ToUnicodeCMap::parse(cmap_text(LATIN_ENTRIES).as_bytes()).unwrap();
    assert!(arabic.is_arabic_script());
    assert!(!latin.is_arabic_script());
}

#[test]
fn an_emptied_entry_reads_as_nothing_and_adds_no_code() {
    let mut cmap = ToUnicodeCMap::parse(cmap_text(ARABIC_ENTRIES).as_bytes()).unwrap();
    cmap.char_map.insert(2, String::new());
    let decoded = cmap.decode_cids_with(&[0, 1, 0, 2, 0, 1], |out, label| out.push_str(label));
    assert_eq!(decoded.text, "\u{628}\u{628}");
    // The kashida adds no text, so it is no code of the string's text.
    assert_eq!(decoded.contributing, 2);
    assert!(cmap.reads_as_nothing(&[0, 2, 0, 2]));
    assert!(!cmap.reads_as_nothing(&[0, 2, 0, 1]));
    assert!(!cmap.reads_as_nothing(&[0, 2, 0]));
    assert!(!cmap.reads_as_nothing(&[]));
}

#[test]
fn a_kashida_read_as_u_fffd_is_dropped_from_the_text() {
    // letter, kashida, letter, kashida, letter
    let bytes = type0_document(&cmap_text(ARABIC_ENTRIES), &["00010002000100020001"]);
    let text = text_of(&bytes);
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
    assert_eq!(count(&text, '\u{628}'), 3, "{text:?}");
}

#[test]
fn a_string_of_nothing_but_kashidas_is_empty_not_a_failed_decode() {
    let bytes = type0_document(&cmap_text(ARABIC_ENTRIES), &["00020002> -20 <0001"]);
    let text = text_of(&bytes);
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
    assert_eq!(count(&text, '\u{628}'), 1, "{text:?}");
}

#[test]
fn other_u_fffd_entries_of_an_arabic_font_stay_failures() {
    // letter, hyphen-like glyph, letter, a stroke that does not reach its
    // advance, letter, a tall bar, letter, kashida: three failures remain.
    let bytes = type0_document(
        &cmap_text(ARABIC_ENTRIES),
        &["00010003000100040001000500010002"],
    );
    let text = text_of(&bytes);
    assert_eq!(count(&text, FFFD), 3, "{text:?}");
    assert_eq!(count(&text, '\u{628}'), 4, "{text:?}");
}

#[test]
fn a_kashida_shaped_glyph_of_a_font_that_is_not_arabic_is_left_alone() {
    let bytes = type0_document(&cmap_text(LATIN_ENTRIES), &["00010002000100020001"]);
    let text = text_of(&bytes);
    assert_eq!(count(&text, FFFD), 2, "{text:?}");
}
