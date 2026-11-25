//! A code of a simple Arabic font whose ToUnicode entry is U+FFFD alone is
//! no text; in a font without Arabic letters, and for every other entry,
//! the reading is the one it always was.

use super::super::content_stream::extract_page_text_items;
use super::super::FormWalkBudget;
use super::tests::{decode_simple_font_string, simple_font_doc};
use super::FontStyleCache;
use crate::tounicode::FontCMaps;
use lopdf::{dictionary, Object, Stream};

/// Eight letters on codes 0x21..=0x28, U+FFFD on 0x29, 0x2A and 0x2B, and
/// a Latin `A` on 0x2C.
fn bfchar(letters: &str) -> String {
    let letters: String = letters
        .chars()
        .enumerate()
        .map(|(i, ch)| format!("<{:02X}> <{:04X}>\n", 0x21 + i, u32::from(ch)))
        .collect();
    format!("{letters}<29> <FFFD>\n<2A> <FFFD>\n<2B> <FFFD>\n<2C> <0041>\n")
}

const ARABIC: &str = "الخصتجحد";
const LATIN: &str = "abcdefgh";

/// WinAnsi with `/ucircumflex` on 0x29 and 0x2A: the names a producer gives
/// the first pieces of split letters.
fn encoding() -> Option<Object> {
    Some(Object::Dictionary(dictionary! {
        "Type" => "Encoding",
        "BaseEncoding" => "WinAnsiEncoding",
        "Differences" => vec![
            0x29.into(),
            Object::Name(b"ucircumflex".to_vec()),
            Object::Name(b"udieresis".to_vec()),
        ],
    }))
}

fn decode(letters: &str, bytes: &[u8]) -> String {
    decode_simple_font_string(&bfchar(letters), None, encoding(), bytes)
}

#[test]
fn a_u_fffd_code_of_a_simple_arabic_font_is_dropped_from_the_text() {
    // ا, the dropped code, خ, ل, the other dropped code, ص.
    let text = decode(ARABIC, &[0x21, 0x29, 0x23, 0x22, 0x2A, 0x24]);
    assert_eq!(text, "اخلص");
}

#[test]
fn a_u_fffd_code_of_a_simple_latin_font_keeps_its_reading() {
    let text = decode(LATIN, &[0x21, 0x29, 0x2A, 0x22]);
    assert_eq!(text, "a\u{FB}\u{FC}b");
}

#[test]
fn a_code_the_cmap_of_a_simple_arabic_font_reads_as_a_letter_is_untouched() {
    let text = decode(ARABIC, &[0x21, 0x22, 0x23]);
    assert_eq!(text, "الخ");
}

#[test]
fn a_u_fffd_code_with_no_other_name_of_a_simple_arabic_font_is_dropped_too() {
    let text = decode_simple_font_string(&bfchar(ARABIC), None, None, &[0x21, 0x2B, 0x22]);
    assert_eq!(text, "ال");
}

#[test]
fn a_run_of_nothing_but_dropped_codes_is_no_item_and_no_space() {
    let (mut doc, _, page) = simple_font_doc(&bfchar(ARABIC), None, None, encoding(), false);
    let content = b"BT /F1 12 Tf 100 700 Td <2C> Tj <292A> Tj <2C> Tj ET".to_vec();
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
    let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["A", "A"], "{items:?}");
}
