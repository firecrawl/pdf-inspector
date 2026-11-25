//! A kashida glyph that a ToUnicode CMap reads as U+FFFD is dropped from the
//! text, and no other U+FFFD entry is.

use super::{ProgramGlyphs, ToUnicodeCMap};
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
const GLYPHS: &[Glyph] = &[
    (50, 0, 450, 600, 500),
    (-10, 0, 260, 70, 250),
    (0, 250, 250, 320, 250),
    (60, 0, 190, 70, 250),
    (-10, 0, 260, 600, 250),
];

/// A glyph: `(x_min, y_min, x_max, y_max, advance)`.
type Glyph = (i16, i16, i16, i16, u16);

/// A table of `len` zero bytes with `fields` written at their offsets.
fn table(len: usize, fields: &[(usize, &[u8])]) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    for &(at, field) in fields {
        bytes[at..at + field.len()].copy_from_slice(field);
    }
    bytes
}

/// The outline of a box: a path along its bottom edge, then up its right,
/// in steps of a third of a side so that no step overflows an i16 whatever
/// the box, then `repeats` more points on the last one. That path spans the
/// box, which is what the glyph's bounding box is measured from.
fn box_outline(&(x_min, y_min, x_max, y_max, _): &Glyph, repeats: usize) -> Vec<u8> {
    let at =
        |from: i16, to: i16, k: i32| i32::from(from) + (i32::from(to) - i32::from(from)) * k / 3;
    let mut points: Vec<(i32, i32)> = (0..=3)
        .map(|k| (at(x_min, x_max, k), i32::from(y_min)))
        .collect();
    points.extend((1..=3).map(|k| (i32::from(x_max), at(y_min, y_max, k))));
    points.extend(std::iter::repeat_n(points[points.len() - 1], repeats));
    let deltas = |axis: fn(&(i32, i32)) -> i32| -> Vec<u8> {
        let mut last = 0;
        points
            .iter()
            .flat_map(|p| {
                let step = i16::try_from(axis(p) - last).expect("a step fits an i16");
                last = axis(p);
                step.to_be_bytes()
            })
            .collect()
    };
    let mut glyph: Vec<u8> = [1i16, x_min, y_min, x_max, y_max]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect();
    glyph.extend((points.len() as u16 - 1).to_be_bytes()); // endPtsOfContours
    glyph.extend(0u16.to_be_bytes()); // no instructions
    glyph.extend(vec![1u8; points.len()]); // on-curve, i16 deltas
    glyph.extend(deltas(|p| p.0));
    glyph.extend(deltas(|p| p.1));
    glyph.resize(glyph.len().next_multiple_of(4), 0);
    glyph
}

/// A TrueType font of box-shaped glyphs (`.notdef` first, then `glyphs`),
/// each outline of `repeats` points more than its box needs.
fn boxed_truetype_of(glyphs: &[Glyph], repeats: usize) -> Vec<u8> {
    let count = (glyphs.len() as u16 + 1).to_be_bytes();
    let (mut glyf, mut loca) = (Vec::new(), vec![0u8; 8]);
    for glyph in glyphs {
        glyf.extend(box_outline(glyph, repeats));
        loca.extend((glyf.len() as u32).to_be_bytes());
    }
    let advances: Vec<u8> = [600]
        .into_iter()
        .chain(glyphs.iter().map(|g| g.4))
        .flat_map(|advance: u16| [advance.to_be_bytes(), [0, 0]].concat())
        .collect();
    let tables = [
        (*b"glyf", glyf),
        (
            *b"head",
            table(
                54,
                &[
                    (0, &[0, 1, 0, 0]),
                    (12, &[0x5F, 0x0F, 0x3C, 0xF5]),
                    (18, &[3, 0xE8]),
                    (51, &[1]),
                ],
            ),
        ),
        (*b"hhea", table(36, &[(0, &[0, 1, 0, 0]), (34, &count)])),
        (*b"hmtx", advances),
        (*b"loca", loca),
        (*b"maxp", table(32, &[(0, &[0, 1, 0, 0]), (4, &count)])),
    ];
    let mut font = [
        &[0u8, 1, 0, 0][..],
        &(tables.len() as u16).to_be_bytes(),
        &[0; 6],
    ]
    .concat();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        let offset = (12 + 16 * tables.len() + body.len()) as u32;
        font.extend(
            [
                &tag[..],
                &[0; 4],
                &offset.to_be_bytes(),
                &(data.len() as u32).to_be_bytes(),
            ]
            .concat(),
        );
        body.extend(data);
        body.resize(body.len().next_multiple_of(4), 0);
    }
    font.extend(body);
    font
}

fn boxed_truetype(glyphs: &[Glyph]) -> Vec<u8> {
    boxed_truetype_of(glyphs, 0)
}

/// The CMap text: `entries` plus codes 1..=5 (the glyphs above) read as
/// U+0628 for the letter and U+FFFD for the rest.
fn cmap_text(entries: &str) -> String {
    format!(
        "1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
13 beginbfchar\n<0001><0628>\n<0002><FFFD>\n<0003><FFFD>\n<0004><FFFD>\n<0005><FFFD>\n{entries}endbfchar"
    )
}

/// What a document of Type0 fonts is built from.
struct Fonts<'a> {
    /// The TrueType program of each font, `/F1`, `/F2` and so on.
    programs: &'a [Vec<u8>],
    /// The ToUnicode CMap all of them share.
    tounicode: &'a str,
    /// The CIDToGIDMap stream of each of them in turn; a font without one
    /// (the list is shorter, or its entry `None`) has the identity.
    cid_to_gid: Vec<Option<Vec<u8>>>,
    /// Whether each font has a page of its own, with the strings it shows.
    page_per_font: bool,
}

/// A one-page document of Identity-H CIDFontType2 fonts, each on its boxed
/// program with the one ToUnicode stream, showing each `(font, hex string)`
/// of `shows` as a TJ array (a `> n <` inside one is a gap between two
/// strings), one line each.
fn type0_document_of(fonts: &Fonts, shows: &[(usize, &str)]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let to_unicode = doc.add_object(Stream::new(
        dictionary! {},
        fonts.tounicode.as_bytes().to_vec(),
    ));
    let mut resources = Vec::new();
    // Equal programs are the one stream, as fonts that share a program have it.
    let mut files = std::collections::HashMap::new();
    for (i, program) in fonts.programs.iter().enumerate() {
        let map = fonts
            .cid_to_gid
            .get(i)
            .cloned()
            .flatten()
            .map_or(Object::Name(b"Identity".to_vec()), |bytes| {
                Object::Reference(doc.add_object(Stream::new(dictionary! {}, bytes)))
            });
        let font_file = *files
            .entry(program)
            .or_insert_with(|| doc.add_object(Stream::new(dictionary! {}, program.clone())));
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
            "CIDToGIDMap" => map.clone(), "FontDescriptor" => descriptor, "DW" => 500,
        });
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "ABCDEF+Synthetic",
            "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(cid_font)],
            "ToUnicode" => to_unicode,
        });
        resources.push((format!("F{}", i + 1), font));
    }
    // The fonts and the strings of each page: all on one, or one font each.
    let groups: Vec<Vec<usize>> = if fonts.page_per_font {
        (1..=resources.len()).map(|font| vec![font]).collect()
    } else {
        vec![(1..=resources.len()).collect()]
    };
    let mut kids = Vec::new();
    for group in &groups {
        let mut page_fonts = dictionary! {};
        for &font in group {
            page_fonts.set(resources[font - 1].0.clone(), resources[font - 1].1);
        }
        let shown: String = shows
            .iter()
            .filter(|(font, _)| group.contains(font))
            .map(|(font, hex)| format!("/F{font} 12 Tf [<{hex}>] TJ T*\n"))
            .collect();
        let content = doc.add_object(Stream::new(
            dictionary! {},
            format!("BT 14 TL 1 0 0 1 40 700 Tm {shown} ET").into_bytes(),
        ));
        kids.push(doc.add_object(dictionary! {
            "Type" => "Page", "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
            "Resources" => dictionary! { "Font" => page_fonts },
            "Contents" => content,
        }));
    }
    let pages = doc.add_object(dictionary! {
        "Type" => "Pages", "Count" => kids.len() as i64,
        "Kids" => kids.iter().map(|&page| Object::Reference(page)).collect::<Vec<_>>(),
    });
    for &page in &kids {
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Parent", pages);
    }
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

/// A document of the one font on [`GLYPHS`], showing each hex string.
fn type0_document(tounicode: &str, shows: &[&str]) -> Vec<u8> {
    let shows: Vec<(usize, &str)> = shows.iter().map(|hex| (1, *hex)).collect();
    type0_document_of(
        &Fonts {
            programs: &[boxed_truetype(GLYPHS)],
            tounicode,
            cid_to_gid: Vec::new(),
            page_per_font: false,
        },
        &shows,
    )
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
fn a_range_start_overridden_by_a_direct_entry_is_no_replacement_code() {
    let cmap = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
1 beginbfchar\n<0002><0640>\nendbfchar\n1 beginbfrange\n<0002><0002><FFFD>\nendbfrange",
    )
    .unwrap();
    assert_eq!(cmap.replacement_codes(), Vec::<u16>::new());
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

/// The CMap text of [`cmap_text`], its eight letters given as the one range
/// of eight codes from `<0010>` that starts at `first`.
fn ranged_cmap_text(first: &str) -> String {
    cmap_text("").replace(
        "endbfchar",
        &format!("endbfchar\n1 beginbfrange\n<0010><0017><{first}>\nendbfrange"),
    )
}

#[test]
fn letters_given_as_a_range_make_an_arabic_script_cmap() {
    let arabic = ToUnicodeCMap::parse(ranged_cmap_text("0627").as_bytes()).unwrap();
    let latin = ToUnicodeCMap::parse(ranged_cmap_text("0061").as_bytes()).unwrap();
    assert!(
        arabic.char_map.keys().all(|&code| code < 6),
        "{:?}",
        arabic.char_map
    );
    assert!(arabic.is_arabic_script());
    assert!(!latin.is_arabic_script());
    // Seven letters (six and the one in the entries) are too few, whatever their encoding.
    let short = ToUnicodeCMap::parse(
        cmap_text("")
            .replace(
                "endbfchar",
                "endbfchar\n1 beginbfrange\n<0010><0015><0627>\nendbfrange",
            )
            .as_bytes(),
    )
    .unwrap();
    assert!(!short.is_arabic_script());
}

#[test]
fn a_range_over_the_whole_code_space_is_judged_by_its_share_of_letters() {
    let cmap = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
1 beginbfrange\n<0000><FFFF><0600>\nendbfrange",
    )
    .unwrap();
    // Only 0x100 + 0x80 of the 65,536 code points are Arabic: a third is not.
    assert!(!cmap.is_arabic_script());
    let cmap = ToUnicodeCMap::parse(
        b"1 begincodespacerange\n<0000><FFFF>\nendcodespacerange\n\
1 beginbfrange\n<0000><00FF><0600>\nendbfrange",
    )
    .unwrap();
    assert!(cmap.is_arabic_script());
}

#[test]
fn direct_entries_that_override_a_range_are_what_is_counted() {
    // Eight Arabic letters by range, every one overridden by a Latin letter.
    let overridden = ToUnicodeCMap::parse(
        cmap_text(LATIN_ENTRIES)
            .replace(
                "endbfchar",
                "endbfchar\n1 beginbfrange\n<0010><0017><0627>\nendbfrange",
            )
            .as_bytes(),
    )
    .unwrap();
    assert!(!overridden.is_arabic_script());
}

#[test]
fn a_range_that_spans_u_fffd_does_not_count_it() {
    // Nine letters, and a range of 19 codes with U+FFFD among them: 27 other
    // destinations is as many as the letters can be a third of.
    let cmap = ToUnicodeCMap::parse(
        cmap_text(ARABIC_ENTRIES)
            .replace(
                "endbfchar",
                "endbfchar\n1 beginbfrange\n<0100><0112><FFF0>\nendbfrange",
            )
            .as_bytes(),
    )
    .unwrap();
    assert!(cmap.is_arabic_script());
}

#[test]
fn a_kashida_of_an_arabic_font_given_as_ranges_is_dropped() {
    let bytes = type0_document(&ranged_cmap_text("0627"), &["00100002001100020012"]);
    let text = text_of(&bytes);
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
    assert_eq!(count(&text, '\u{627}'), 1, "{text:?}");
}

/// The same four letters on a font whose glyph 2 is a hyphen and glyph 3
/// the kashida: the one shared CMap reads code 2 as U+FFFD for both.
fn hyphen_first_glyphs() -> Vec<Glyph> {
    let mut glyphs = GLYPHS.to_vec();
    glyphs.swap(1, 2);
    glyphs
}

#[test]
fn a_cmap_shared_with_a_font_of_the_same_program_is_still_repaired() {
    let fonts = Fonts {
        programs: &[boxed_truetype(GLYPHS), boxed_truetype(GLYPHS)],
        tounicode: &cmap_text(ARABIC_ENTRIES),
        cid_to_gid: Vec::new(),
        page_per_font: false,
    };
    let text = text_of(&type0_document_of(
        &fonts,
        &[(2, "000100020001"), (1, "000100020001")],
    ));
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
}

#[test]
fn a_cmap_shared_by_fonts_on_different_pages_is_repaired_for_each_font_alone() {
    let fonts = Fonts {
        programs: &[
            boxed_truetype(GLYPHS),
            boxed_truetype(&hyphen_first_glyphs()),
        ],
        tounicode: &cmap_text(ARABIC_ENTRIES),
        cid_to_gid: Vec::new(),
        page_per_font: true,
    };
    // Page 1's kashida goes; page 2's hyphen at the same code stays.
    let text = text_of(&type0_document_of(
        &fonts,
        &[(1, "000100020001"), (2, "000100020001")],
    ));
    assert_eq!(count(&text, FFFD), 1, "{text:?}");
}

#[test]
fn a_cmap_shared_by_fonts_with_different_cid_to_gid_maps_is_repaired_for_each_alone() {
    // Both fonts have the same program. The second sends CID 2 to glyph 3
    // (the hyphen) where the first reads the kashida, glyph 2.
    let map: Vec<u8> = (0..=u16::MAX)
        .flat_map(|cid| [0, if cid == 2 { 3 } else { cid as u8 }])
        .collect();
    let fonts = Fonts {
        programs: &[boxed_truetype(GLYPHS), boxed_truetype(GLYPHS)],
        tounicode: &cmap_text(ARABIC_ENTRIES),
        cid_to_gid: vec![None, Some(map)],
        page_per_font: false,
    };
    let text = text_of(&type0_document_of(
        &fonts,
        &[(2, "000100020001"), (1, "000100020001")],
    ));
    assert_eq!(count(&text, FFFD), 1, "{text:?}");
}

#[test]
fn a_glyph_spanning_the_whole_coordinate_range_is_measured_without_overflow() {
    // The spread of these i16 coordinates is 65,535: more than an i16 holds.
    let extreme: &[Glyph] = &[
        (i16::MIN, 0, i16::MAX, 70, 250),
        (-10, i16::MIN, 260, i16::MAX, 250),
        (i16::MIN, i16::MIN, i16::MAX, i16::MAX, 250),
    ];
    let data = boxed_truetype(extreme);
    let glyphs = ProgramGlyphs::parse(&data).unwrap();
    let verdicts: Vec<bool> = (1..=3).map(|gid| glyphs.is_kashida(gid)).collect();
    assert_eq!(verdicts, vec![true, false, false]);
}

#[test]
fn many_codes_sent_to_one_large_glyph_measure_it_once() {
    // 65,000 replacement codes, all read through a CIDToGIDMap as glyph 2,
    // a kashida of 20,000 points: measured once, not 65,000 times.
    let mut tounicode = cmap_text(ARABIC_ENTRIES).replace("endbfchar", "");
    tounicode.push_str("endbfchar\n");
    for chunk in (0x100u32..0x100 + 65_000).collect::<Vec<_>>().chunks(100) {
        tounicode.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for code in chunk {
            tounicode.push_str(&format!("<{code:04X}><FFFD>\n"));
        }
        tounicode.push_str("endbfchar\n");
    }
    let map: Vec<u8> = (0..=u16::MAX)
        .flat_map(|cid| [0, if cid < 0x10 { 1 } else { 2 }])
        .collect();
    let fonts = Fonts {
        programs: &[boxed_truetype_of(GLYPHS, 20_000)],
        tounicode: &tounicode,
        cid_to_gid: vec![Some(map)],
        page_per_font: false,
    };
    let bytes = type0_document_of(&fonts, &[(1, "0010010001000100")]);
    let started = std::time::Instant::now();
    let text = text_of(&bytes);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(count(&text, FFFD), 0, "{text:?}");
}
