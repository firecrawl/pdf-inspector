//! A kashida glyph that a Type0 font's ToUnicode CMap reads as U+FFFD is no
//! text, and no other U+FFFD entry is dropped.

use super::blank_glyph_tests::synthetic_truetype;
use super::tests::{cid_font_doc, decode_page_font_string_as};
use super::{add_kashida_codes, build_font_widths, FontStyleCache};
use crate::tounicode::{kashida_codes, FontCMaps, ToUnicodeCMap, MAX_KASHIDA_CANDIDATES};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

const FFFD: char = '\u{FFFD}';

/// A glyph box: `(x_min, y_min, x_max, y_max, advance)` in 1000 units per em.
type GlyphBox = (i16, i16, i16, i16, u16);

/// Glyph 1 is a letter, 2 a kashida (flat, low, and it spans its advance),
/// 3 a hyphen (flat, but at mid height), 4 a low stroke inside its advance,
/// 5 a tall bar that spans its advance, 6 a blob that is higher than wide.
const GLYPHS: [GlyphBox; 6] = [
    (50, 0, 450, 600, 500),
    (-10, 0, 260, 70, 250),
    (0, 250, 250, 320, 250),
    (60, 0, 190, 70, 250),
    (-10, 0, 260, 600, 250),
    (0, 0, 150, 200, 140),
];

/// Eight Arabic letters, on codes 0x10..=0x17: what makes a CMap an Arabic
/// font's.
const ARABIC: &str = "<0010> <0627>\n<0011> <0628>\n<0012> <062A>\n<0013> <062B>\n\
<0014> <062C>\n<0015> <062D>\n<0016> <062E>\n<0017> <062F>\n";
const LATIN: &str = "<0010> <0061>\n<0011> <0062>\n<0012> <0063>\n<0013> <0064>\n\
<0014> <0065>\n<0015> <0066>\n<0016> <0067>\n<0017> <0068>\n";

/// A TrueType font with a glyph of each box, after `.notdef`. It is the
/// font of the blank glyph tests, each glyph an outline record of 29 bytes
/// of three points; the points are moved to the corners and the middle of the
/// box, from which the bounding box is read (a spread of up to 65,534 fits
/// the steps between the points).
fn program(boxes: &[GlyphBox]) -> Vec<u8> {
    let glyphs: Vec<(bool, u16)> = boxes.iter().map(|b| (true, b.4)).collect();
    let mut font = synthetic_truetype(&glyphs, &[]);
    let glyf = ttf_parser::RawFace::parse(&font, 0)
        .unwrap()
        .table(ttf_parser::Tag::from_bytes(b"glyf"))
        .unwrap();
    let start = glyf.as_ptr() as usize - font.as_ptr() as usize;
    for (i, &(x_min, y_min, x_max, y_max, _)) in boxes.iter().enumerate() {
        let steps = |min: i16, max: i16| {
            let half = (i32::from(max) - i32::from(min)) / 2;
            [
                min,
                half as i16,
                (i32::from(max) - i32::from(min) - half) as i16,
            ]
        };
        let deltas = [steps(x_min, x_max), steps(y_min, y_max)].concat();
        let at = start + 29 * (i + 1) + 17;
        for (k, delta) in deltas.into_iter().enumerate() {
            font[at + 2 * k..at + 2 * k + 2].copy_from_slice(&delta.to_be_bytes());
        }
    }
    font
}

/// The `bfchar` lines of a CMap that reads code 1 as a letter, each of
/// `replaced` as U+FFFD, and the given letters.
fn bfchar(replaced: &[u16], letters: &str) -> String {
    let replaced: String = replaced
        .iter()
        .map(|code| format!("<{code:04X}> <FFFD>\n"))
        .collect();
    format!("<0001> <0628>\n{replaced}{letters}")
}

/// A document of the one Type0 font `F1` on `program`, read by `bfchar`.
fn document(
    bfchar: &str,
    program: Vec<u8>,
    cid_to_gid: Option<&[u16]>,
) -> (Document, u32, ObjectId) {
    cid_font_doc(bfchar, program, None, false, cid_to_gid)
}

/// The codes shown through `font`, as the page decodes them.
fn decode(doc: &(Document, u32, ObjectId), font: &str, codes: &[u16]) -> String {
    let bytes: Vec<u8> = codes.iter().flat_map(|code| code.to_be_bytes()).collect();
    decode_page_font_string_as(&doc.0, font, doc.1, doc.2, true, &bytes)
}

fn count(text: &str, ch: char) -> usize {
    text.chars().filter(|&c| c == ch).count()
}

/// The kashida codes of `F1` of `doc`, read through its own CMap.
fn font_kashida_codes(doc: &(Document, u32, ObjectId)) -> Vec<u16> {
    let cmaps = FontCMaps::from_doc(&doc.0);
    let fonts = doc.0.get_page_fonts(doc.2).unwrap();
    kashida_codes(
        &cmaps.get_by_obj(doc.1).unwrap().primary,
        fonts[&b"F1"[..]],
        &doc.0,
    )
}

/// The font resources of `page`.
fn page_fonts_mut(doc: &mut Document, page: ObjectId) -> &mut lopdf::Dictionary {
    let page = doc.get_object_mut(page).unwrap().as_dict_mut().unwrap();
    let resources = page.get_mut(b"Resources").unwrap().as_dict_mut().unwrap();
    resources.get_mut(b"Font").unwrap().as_dict_mut().unwrap()
}

/// Add `F2` to the page of `doc`: the font `F1`, with a program of its own
/// when `program` is given and a CIDToGIDMap of its own when `cid_to_gid` is.
/// It reads the same ToUnicode stream.
fn add_second_font(
    doc: &mut Document,
    page: ObjectId,
    program: Option<Vec<u8>>,
    cid_to_gid: Option<&[u16]>,
) {
    let f1 = page_fonts_mut(doc, page).get(b"F1").unwrap().as_reference();
    let mut font = doc.get_dictionary(f1.unwrap()).unwrap().clone();
    let cid_ref = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0].as_reference();
    let mut cid_font = doc.get_dictionary(cid_ref.unwrap()).unwrap().clone();
    if let Some(program) = program {
        let descriptor = cid_font.get(b"FontDescriptor").unwrap().as_reference();
        let mut descriptor = doc.get_dictionary(descriptor.unwrap()).unwrap().clone();
        let file = doc.add_object(Stream::new(dictionary! {}, program));
        descriptor.set("FontFile2", file);
        cid_font.set("FontDescriptor", doc.add_object(descriptor));
    }
    if let Some(map) = cid_to_gid {
        let bytes: Vec<u8> = map.iter().flat_map(|gid| gid.to_be_bytes()).collect();
        let map = doc.add_object(Stream::new(dictionary! {}, bytes));
        cid_font.set("CIDToGIDMap", map);
    }
    let cid_font = doc.add_object(cid_font);
    font.set("DescendantFonts", vec![Object::Reference(cid_font)]);
    let font = doc.add_object(font);
    page_fonts_mut(doc, page).set("F2", font);
}

#[test]
fn a_kashida_read_as_u_fffd_is_dropped_from_the_text() {
    let doc = document(&bfchar(&[2], ARABIC), program(&GLYPHS), None);
    let text = decode(&doc, "F1", &[1, 2, 1, 2, 1]);
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
    assert_eq!(count(&text, '\u{628}'), 3, "{text:?}");
}

#[test]
fn a_string_of_nothing_but_kashidas_is_empty_not_a_failed_decode() {
    let doc = document(&bfchar(&[2], ARABIC), program(&GLYPHS), None);
    assert_eq!(decode(&doc, "F1", &[2, 2]), "");
}

#[test]
fn other_u_fffd_entries_of_an_arabic_font_stay_failures() {
    // letter, hyphen, letter, a stroke short of its advance, letter, a tall
    // bar, letter, a blob, letter, kashida: the four failures remain.
    let doc = document(&bfchar(&[2, 3, 4, 5, 6], ARABIC), program(&GLYPHS), None);
    let text = decode(&doc, "F1", &[1, 3, 1, 4, 1, 5, 1, 6, 1, 2]);
    assert_eq!(count(&text, FFFD), 4, "{text:?}");
    assert_eq!(count(&text, '\u{628}'), 5, "{text:?}");
}

#[test]
fn a_kashida_shaped_glyph_of_a_font_without_arabic_letters_is_left_alone() {
    let doc = document(&bfchar(&[2], LATIN), program(&GLYPHS), None);
    assert!(font_kashida_codes(&doc).is_empty());
    let text = decode(&doc, "F1", &[1, 2, 1, 2, 1]);
    assert_eq!(count(&text, FFFD), 2, "{text:?}");
}

/// The kashida codes of `F1` and `F2` of `doc`, found with one cache.
fn kashida_codes_of_both(doc: &(Document, u32, ObjectId)) -> (Vec<u16>, Vec<u16>) {
    let fonts = doc.0.get_page_fonts(doc.2).unwrap();
    let mut cache = FontStyleCache::new();
    let mut widths = build_font_widths(&doc.0, &fonts, &mut cache);
    add_kashida_codes(
        &mut widths,
        &doc.0,
        &fonts,
        &FontCMaps::from_doc(&doc.0),
        &mut cache,
    );
    (
        widths["F1"].kashida_codes.clone(),
        widths["F2"].kashida_codes.clone(),
    )
}

#[test]
fn fonts_that_share_a_cmap_are_read_each_by_its_own_glyphs() {
    // The second font's program has the hyphen as glyph 2 and the kashida as
    // glyph 3: the one CMap's U+FFFD at code 2 is a kashida only in the first.
    let mut swapped = GLYPHS;
    swapped.swap(1, 2);
    let mut doc = document(&bfchar(&[2], ARABIC), program(&GLYPHS), None);
    add_second_font(&mut doc.0, doc.2, Some(program(&swapped)), None);
    assert_eq!(kashida_codes_of_both(&doc), (vec![2], vec![]));
    assert_eq!(count(&decode(&doc, "F1", &[1, 2, 1]), FFFD), 0);
    assert_eq!(count(&decode(&doc, "F2", &[1, 2, 1]), FFFD), 1);
}

#[test]
fn fonts_that_share_a_cmap_and_a_program_are_read_each_by_its_own_cid_to_gid_map() {
    // The second font has the first's program and sends CID 2 to glyph 3, the
    // hyphen.
    let mut doc = document(&bfchar(&[2], ARABIC), program(&GLYPHS), None);
    add_second_font(&mut doc.0, doc.2, None, Some(&[0, 1, 3, 3, 4, 5]));
    assert_eq!(kashida_codes_of_both(&doc), (vec![2], vec![]));
    assert_eq!(count(&decode(&doc, "F1", &[1, 2, 1]), FFFD), 0);
    assert_eq!(count(&decode(&doc, "F2", &[1, 2, 1]), FFFD), 1);
}

#[test]
fn the_fast_collection_measures_no_font() {
    let doc = document(&bfchar(&[2], ARABIC), program(&GLYPHS), None);
    let codes_of = |cmaps: FontCMaps| {
        let fonts = doc.0.get_page_fonts(doc.2).unwrap();
        let mut cache = FontStyleCache::new();
        let mut widths = build_font_widths(&doc.0, &fonts, &mut cache);
        add_kashida_codes(&mut widths, &doc.0, &fonts, &cmaps, &mut cache);
        widths["F1"].kashida_codes.clone()
    };
    assert_eq!(codes_of(FontCMaps::from_doc(&doc.0)), vec![2]);
    assert!(codes_of(FontCMaps::from_doc_pages_fast(&doc.0, None)).is_empty());
}

#[test]
fn a_glyph_spanning_the_whole_coordinate_range_is_measured_without_overflow() {
    // The spread of these i16 coordinates is 65,534: more than an i16 holds.
    let extreme = [
        (i16::MIN, 0, i16::MAX - 1, 70, 250),
        (-10, i16::MIN, 260, i16::MAX - 1, 250),
        (i16::MIN, i16::MIN, i16::MAX - 1, i16::MAX - 1, 250),
    ];
    let doc = document(&bfchar(&[1, 2, 3], ARABIC), program(&extreme), None);
    assert_eq!(font_kashida_codes(&doc), vec![1]);
}

#[test]
fn a_cmap_of_65_000_replacement_codes_is_measured_for_the_lowest_few() {
    // Every code from 0x100 on is sent to glyph 2, a kashida, and the CMap
    // reads each as U+FFFD: only the lowest codes are looked at, each glyph once.
    let replaced: Vec<u16> = (0x100..0x100 + 65_000).collect();
    let map: Vec<u16> = (0..=u16::MAX)
        .map(|cid| if cid < 0x100 { 1 } else { 2 })
        .collect();
    let doc = document(&bfchar(&replaced, ARABIC), program(&GLYPHS), Some(&map));
    let started = std::time::Instant::now();
    let codes = font_kashida_codes(&doc);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let lowest: Vec<u16> = (0x100..0x100 + MAX_KASHIDA_CANDIDATES as u16).collect();
    assert_eq!(codes, lowest);
}

#[test]
fn replacement_codes_are_found_in_entries_and_ranges_only() {
    let cmap = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
2 beginbfchar\n<0002><FFFD>\n<0003><0041FFFD>\nendbfchar\n\
2 beginbfrange\n<0009><0009><FFFD>\n<0010><0020><FFF0>\nendbfrange\n\
1 beginbfchar\n<001D><0640>\nendbfchar",
    )
    .unwrap();
    // Code 2 is an entry, 9 a range of one, and 0x1D would be the range's
    // U+FFFD, were it not overridden by an entry of its own.
    assert_eq!(cmap.replacement_codes(10), vec![2, 9]);
    assert_eq!(cmap.replacement_codes(1), vec![2]);
    let spanning = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
1 beginbfrange\n<0010><0020><FFF0>\nendbfrange",
    )
    .unwrap();
    assert_eq!(spanning.replacement_codes(10), vec![0x1D]);
}

#[test]
fn a_code_another_range_maps_to_text_is_not_a_kashida() {
    // The range <0001>-<0004> reaches U+FFFD at code 2, but <0002> is read
    // as U+0640 by a range of its own: the character is text, not a gap.
    let cmap = "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
3 beginbfrange\n<0001><0004><FFFC>\n<0002><0002><0640>\n<0010><0017><0627>\nendbfrange\n";
    let parsed = ToUnicodeCMap::parse(cmap.as_bytes()).unwrap();
    assert_eq!(parsed.lookup(2).as_deref(), Some("\u{640}"));
    assert!(parsed.replacement_codes(10).is_empty());
    let doc = document(cmap, program(&GLYPHS), None);
    assert!(font_kashida_codes(&doc).is_empty());
    let text = decode(&doc, "F1", &[0x10, 2, 0x11]);
    assert_eq!(text, "\u{627}\u{640}\u{628}");
}

#[test]
fn a_run_of_nothing_but_a_kashida_is_no_space_between_letters() {
    use super::super::content_stream::extract_page_text_items;
    use super::super::FormWalkBudget;
    // Latin letters on either side of the kashida: only a run after a
    // letter that can end a word is noted as a possible word space.
    let cmap = format!("<0001> <0041>\n<0002> <FFFD>\n{ARABIC}");
    let (mut doc, _, page) = document(&cmap, program(&GLYPHS), None);
    // Narrow glyphs: the kashida's run is a squeezed gap, as a space is.
    let font = page_fonts_mut(&mut doc, page)
        .get(b"F1")
        .unwrap()
        .as_reference();
    let font = doc.get_dictionary(font.unwrap()).unwrap();
    let cid = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0].as_reference();
    doc.get_object_mut(cid.unwrap())
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("DW", 20);
    let content = b"BT /F1 12 Tf 100 700 Td <0001> Tj <0002> Tj <0001> Tj ET".to_vec();
    let contents = doc.add_object(Stream::new(dictionary! {}, content));
    let page_dict = doc.get_object_mut(page).unwrap().as_dict_mut().unwrap();
    page_dict.set("Contents", contents);
    let cmaps = FontCMaps::from_doc(&doc);
    let ((items, _, _), _, _, _) = extract_page_text_items(
        &doc,
        page,
        1,
        &cmaps,
        false,
        &mut FontStyleCache::new(),
        &mut FormWalkBudget::new(),
    )
    .unwrap();

    let text: String = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(text, "AA", "{items:?}");
    // The dropped kashida still advances the position: one item that spans
    // all three glyphs, each 20 units of 12 point text wide.
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        (items[0].width - 3.0 * 20.0 * 12.0 / 1000.0).abs() < 1e-3,
        "{items:?}"
    );
}

/// A CMap reading each of `chars` from its own code.
fn cmap_of(chars: &[u32]) -> ToUnicodeCMap {
    let entries: String = chars
        .iter()
        .enumerate()
        .map(|(code, &ch)| {
            let mut units = [0u16; 2];
            let units = char::from_u32(ch).unwrap().encode_utf16(&mut units);
            let hex: String = units.iter().map(|unit| format!("{unit:04X}")).collect();
            format!("<{:04X}> <{hex}>\n", code + 1)
        })
        .collect();
    let cmap = format!(
        "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
{} beginbfchar\n{entries}endbfchar",
        chars.len()
    );
    ToUnicodeCMap::parse(cmap.as_bytes()).unwrap()
}

#[test]
fn arabic_indic_digits_and_punctuation_are_not_arabic_letters() {
    // Ten digits and the Arabic comma, semicolon, question mark and percent.
    let symbols: Vec<u32> = (0x660..=0x669)
        .chain([0x60C, 0x61B, 0x61F, 0x66A])
        .collect();
    assert!(!cmap_of(&symbols).reads_arabic_letters());
    assert!(cmap_of(&(0x627..=0x62E).collect::<Vec<u32>>()).reads_arabic_letters());
}

#[test]
fn letters_of_the_other_arabic_blocks_make_an_arabic_cmap() {
    // Presentation Forms-B, Presentation Forms-A, Extended-A and Extended-B:
    // eight letters each.
    for first in [0xFE8F, 0xFB56, 0x8A0, 0x870] {
        let letters: Vec<u32> = (first..first + 8).collect();
        assert!(cmap_of(&letters).reads_arabic_letters(), "{first:X}");
    }
    // Mathematical Alphabetic Symbols, skipping the unassigned U+1EE04.
    let math = [
        0x1EE00, 0x1EE01, 0x1EE02, 0x1EE03, 0x1EE05, 0x1EE06, 0x1EE07, 0x1EE08,
    ];
    assert!(cmap_of(&math).reads_arabic_letters());
}

#[test]
fn an_odd_length_string_is_read_by_the_byte_not_by_pairs_of_a_kashida() {
    // Code 0x0100 is a kashida (every code from 0x100 on is sent to glyph 2).
    // A string of three bytes is read a byte at a time: its first two are
    // not the code 0x0100, and the letter at code 1 is shown twice.
    let map: Vec<u16> = (0..=u16::MAX)
        .map(|cid| if cid < 0x100 { 1 } else { 2 })
        .collect();
    let doc = document(&bfchar(&[0x100], ARABIC), program(&GLYPHS), Some(&map));
    assert_eq!(font_kashida_codes(&doc), vec![0x100]);
    let text = |bytes: &[u8]| decode_page_font_string_as(&doc.0, "F1", doc.1, doc.2, true, bytes);
    assert_eq!(text(&[1, 0, 1]), "\u{628}\u{628}");
    assert_eq!(text(&[0, 1, 1, 0, 0, 1]), "\u{628}\u{628}");
}

#[test]
fn a_code_a_direct_entry_overrides_is_not_counted_with_its_range() {
    // A range of eight Arabic letters whose members are each read as Latin
    // by an entry of its own: no Arabic letter is read.
    let overridden: String = (1..=8)
        .map(|code| format!("<{code:04X}> <0061>\n"))
        .collect();
    let cmap = format!(
        "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
8 beginbfchar\n{overridden}endbfchar\n\
1 beginbfrange\n<0001><0008><0627>\nendbfrange"
    );
    let cmap = ToUnicodeCMap::parse(cmap.as_bytes()).unwrap();
    assert_eq!(cmap.lookup(1).as_deref(), Some("a"));
    assert!(!cmap.reads_arabic_letters());
    // Two members left to the range, and six letters of an entry each.
    let some = "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
8 beginbfchar\n<0001> <0061>\n<0002> <0062>\n<0021> <0627>\n<0022> <0628>\n<0023> <062A>\n<0024> <062B>\n<0025> <062C>\n<0026> <062D>\nendbfchar\n\
1 beginbfrange\n<0001><0004><0627>\nendbfrange";
    let some = ToUnicodeCMap::parse(some.as_bytes()).unwrap();
    assert!(some.reads_arabic_letters());
}
