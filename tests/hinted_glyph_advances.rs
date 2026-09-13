//! Browser print-to-PDF output places every glyph with its own `Td`/`Tj` at
//! the whole-pixel advance a hinting rasterizer laid it out with, while the
//! font's `/Widths` keep the unhinted metrics. The word spaces are painted as
//! space glyphs. These fixtures rebuild that layout from synthetic text.

use lopdf::{dictionary, Document, Object, Stream};
use pdf_inspector::{extract_text_with_positions_mem, process_pdf_mem};

/// Times Roman and Times Bold AFM advances, per 1000 em.
const REGULAR_WIDTHS: &str = "  250 ( 333 ) 333 , 250 - 333 . 250 / 278 : 278 \
    0 500 1 500 2 500 3 500 4 500 5 500 6 500 7 500 8 500 9 500 \
    A 722 B 667 C 667 D 722 E 611 F 556 G 722 H 722 I 333 L 611 M 889 N 722 O 722 P 556 \
    Q 722 R 667 S 556 T 611 U 722 Y 722 \
    a 444 b 500 c 444 d 500 e 444 f 333 g 500 h 500 i 278 k 500 l 278 m 778 n 500 o 500 \
    p 500 q 500 r 333 s 389 t 278 u 500 v 500 y 500";
const BOLD_WIDTHS: &str = "  250 ( 333 ) 333 , 250 - 333 . 250 / 278 : 333 \
    0 500 1 500 2 500 3 500 4 500 5 500 6 500 7 500 8 500 9 500 \
    A 722 B 667 C 722 D 722 E 667 F 611 G 778 H 778 I 389 L 667 M 944 N 722 O 778 P 611 \
    Q 778 R 722 S 556 T 667 U 722 Y 722 \
    a 500 b 556 c 444 d 556 e 444 f 333 g 500 h 556 i 278 k 556 l 278 m 833 n 556 o 500 \
    p 556 q 556 r 444 s 389 t 333 u 556 v 500 y 500";

/// Whole-pixel advances of the same glyphs laid out at 8px. They stray from
/// the declared widths by up to 1.8px (bold Q: 6.22 declared, 8 laid out).
const REGULAR_ADVANCES: &str = "  2 ( 3 ) 3 , 2 - 3 . 2 / 2 : 2 \
    0 4 1 4 2 4 3 4 4 4 5 4 6 4 7 4 8 4 9 4 \
    A 6 B 5 C 5 D 6 E 5 F 4 G 6 H 6 I 3 L 5 M 7 N 6 O 6 P 5 Q 6 R 5 S 4 T 5 U 6 Y 6 \
    a 4 b 4 c 4 d 4 e 4 f 3 g 4 h 4 i 2 k 3 l 2 m 7 n 4 o 4 p 4 q 4 r 3 s 3 t 2 u 4 v 4 y 5";
const BOLD_ADVANCES: &str = "  2 ( 3 ) 3 , 2 - 3 . 2 / 2 : 3 \
    0 4 1 4 2 4 3 4 4 4 5 4 6 4 7 4 8 4 9 4 \
    A 6 B 6 C 6 D 6 E 5 F 5 G 7 H 6 I 3 L 5 M 8 N 6 O 6 P 5 Q 8 R 6 S 4 T 5 U 6 Y 6 \
    a 4 b 4 c 4 d 4 e 4 f 3 g 4 h 4 i 2 k 3 l 2 m 6 n 4 o 4 p 4 q 4 r 4 s 3 t 3 u 4 v 4 y 4";

fn table(entries: &str) -> Vec<(u8, i64)> {
    let mut out = Vec::new();
    // A leading "  " entry stands for the space glyph.
    let (space, rest) = entries.split_at(2);
    assert_eq!(space, "  ");
    let mut fields = rest.split_whitespace();
    out.push((b' ', fields.next().unwrap().parse().unwrap()));
    while let (Some(glyph), Some(value)) = (fields.next(), fields.next()) {
        out.push((glyph.as_bytes()[0], value.parse().unwrap()));
    }
    out
}

fn lookup(entries: &[(u8, i64)], glyph: u8) -> i64 {
    entries
        .iter()
        .find(|(g, _)| *g == glyph)
        .unwrap_or_else(|| panic!("no metric for {:?}", glyph as char))
        .1
}

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Regular,
    Bold,
    /// Monospaced, 600 per 1000 em declared and 5px laid out.
    Mono,
}

/// One string drawn glyph by glyph from its baseline, in CSS pixels from the
/// top-left corner of the page.
struct Run<'a> {
    x: f32,
    y: f32,
    face: Face,
    text: &'a str,
}

fn run(x: f32, y: f32, bold: bool, text: &str) -> Run<'_> {
    let face = if bold { Face::Bold } else { Face::Regular };
    Run { x, y, face, text }
}

fn font(doc: &mut Document, name: &str, widths: &[(u8, i64)]) -> Object {
    let row: Vec<Object> = (32u8..=122)
        .map(|code| {
            widths
                .iter()
                .find(|(g, _)| *g == code)
                .map_or(0, |(_, w)| *w)
                .into()
        })
        .collect();
    doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => name,
        "FirstChar" => 32, "LastChar" => 122, "Widths" => row,
        "Encoding" => "WinAnsiEncoding",
    })
    .into()
}

/// Draws `runs`; with `paint_spaces` false a space only moves the next glyph
/// along, as producers that skip blank glyphs do.
fn pdf(runs: &[Run], paint_spaces: bool) -> Vec<u8> {
    pdf_with_space_width(runs, paint_spaces, 250)
}

/// Draws `runs` with the space glyph of the proportional faces declared
/// `space_width` per 1000 em wide.
fn pdf_with_space_width(runs: &[Run], paint_spaces: bool, space_width: i64) -> Vec<u8> {
    const HEIGHT: f32 = 842.0;
    let (regular, bold) = (table(REGULAR_ADVANCES), table(BOLD_ADVANCES));
    let mono: Vec<(u8, i64)> = (32u8..=122).map(|code| (code, 5)).collect();
    let mut content = format!("1 0 0 -1 0 {HEIGHT} cm\nq\n0.75 0 0 0.75 0 0 cm\n");
    for r in runs {
        let (name, advances) = match r.face {
            Face::Regular => ("F1", &regular),
            Face::Bold => ("F2", &bold),
            Face::Mono => ("F3", &mono),
        };
        content.push_str(&format!("BT\n/{name} 8 Tf 1 0 0 -1 0 0 Tm\n"));
        let mut pen = None;
        let mut travel = 0;
        for glyph in r.text.bytes() {
            if glyph != b' ' || paint_spaces {
                let glyph_hex = format!("<{glyph:02x}>");
                match pen {
                    None => content.push_str(&format!("{} {} Td {glyph_hex} Tj\n", r.x, -r.y)),
                    Some(_) => content.push_str(&format!("{travel} 0 Td {glyph_hex} Tj\n")),
                }
                pen = Some(());
                travel = 0;
            }
            travel += lookup(advances, glyph);
        }
        content.push_str("ET\n");
    }
    content.push_str("Q\n");

    let mut doc = Document::with_version("1.4");
    let pages_id = doc.new_object_id();
    let with_space = |widths: &str| {
        let mut widths = table(widths);
        widths[0].1 = space_width;
        widths
    };
    let f1 = font(&mut doc, "SyntheticSerif", &with_space(REGULAR_WIDTHS));
    let f2 = font(&mut doc, "SyntheticSerif-Bold", &with_space(BOLD_WIDTHS));
    let mono_widths: Vec<(u8, i64)> = (32u8..=122).map(|code| (code, 600)).collect();
    let f3 = font(&mut doc, "SyntheticMono", &mono_widths);
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), HEIGHT.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => f1, "F2" => f2, "F3" => f3 } },
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page_id.into()] }.into(),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn markdown(runs: &[Run]) -> String {
    process_pdf_mem(&pdf(runs, true)).unwrap().markdown.unwrap()
}

fn item_texts(runs: &[Run], paint_spaces: bool) -> Vec<String> {
    extract_text_with_positions_mem(&pdf(runs, paint_spaces))
        .unwrap()
        .into_iter()
        .map(|item| item.text.trim().to_string())
        .collect()
}

#[test]
fn hinted_advances_do_not_split_words() {
    let texts = item_texts(
        &[
            run(
                35.0,
                51.0,
                true,
                "GENERAL INFORMATION ABOUT FINANCIAL STATEMENTS",
            ),
            run(35.0, 62.0, true, "LIABILITIES"),
            run(35.0, 73.0, true, "EQUITY"),
            run(35.0, 84.0, true, "STATEMENT OF CHANGES IN EQUITY"),
            run(45.0, 95.0, false, "Product code (Symbol)"),
            run(45.0, 106.0, false, "Name of reporting entity"),
            run(45.0, 117.0, false, "Investment in associates"),
        ],
        true,
    );
    for expected in [
        "GENERAL INFORMATION ABOUT FINANCIAL STATEMENTS",
        "LIABILITIES",
        "EQUITY",
        "STATEMENT OF CHANGES IN EQUITY",
        "Product code (Symbol)",
        "Name of reporting entity",
        "Investment in associates",
    ] {
        assert!(
            texts.iter().any(|t| t == expected),
            "missing {expected:?} in {texts:?}"
        );
    }
}

#[test]
fn hinted_statement_keeps_words_and_cells_apart() {
    let cols = [293.0, 345.0];
    let mut runs = vec![
        run(33.0, 40.0, true, "Statement of financial position"),
        run(cols[0], 40.0, true, "31/12/2025"),
        run(cols[1], 40.0, true, "31/12/2024"),
    ];
    let rows: [(f32, bool, &str, [&str; 2]); 6] = [
        (51.0, true, "LIABILITIES", ["", ""]),
        (62.0, false, "Trade payables", ["1,120", "1,015"]),
        (73.0, true, "EQUITY", ["", ""]),
        (84.0, false, "Share capital", ["2,000", "2,000"]),
        (95.0, true, "Total equity", ["3,394", "2,848"]),
        (
            106.0,
            true,
            "TOTAL EQUITY AND LIABILITIES",
            ["4,514", "3,863"],
        ),
    ];
    for (y, bold, label, cells) in rows {
        runs.push(run(35.0, y, bold, label));
        for (col, cell) in cols.iter().zip(cells) {
            if !cell.is_empty() {
                runs.push(run(col + 40.0 - 4.0 * cell.len() as f32, y, bold, cell));
            }
        }
    }
    let md = markdown(&runs);
    for expected in [
        "|LIABILITIES|",
        "|EQUITY|",
        "|Trade payables|1,120|1,015|",
        "|Total equity|3,394|2,848|",
        "|TOTAL EQUITY AND LIABILITIES|4,514|3,863|",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
}

/// Without painted spaces the gaps are the only word-boundary evidence, so
/// the word spaces they show are kept.
#[test]
fn unpainted_word_spaces_still_come_from_gaps() {
    let texts = item_texts(
        &[
            run(35.0, 51.0, true, "CASH AND TOTAL ASSETS"),
            run(45.0, 62.0, false, "Name of reporting entity"),
        ],
        false,
    );
    for expected in ["CASH AND TOTAL ASSETS", "Name of reporting entity"] {
        assert!(
            texts.iter().any(|t| t == expected),
            "missing {expected:?} in {texts:?}"
        );
    }
}

/// A face whose space glyph is declared just under 0.2 em paints its spaces
/// as squeezed runs. The space after "," or "." is such a run too, and the
/// only word boundary there: the gap it leaves is no wider than the hinted
/// gaps inside a word.
#[test]
fn painted_spaces_after_punctuation_break_words_in_a_narrow_space_face() {
    let texts: Vec<String> = extract_text_with_positions_mem(&pdf_with_space_width(
        &[
            run(35.0, 51.0, false, "Date: May 12, 2026 at 11:23 PM"),
            run(35.0, 62.0, false, "ACME INC. 800-625 av. du Parc"),
            run(35.0, 73.0, true, "TOTAL. INCLUDING FEES"),
            run(35.0, 84.0, true, "LIABILITIES AND EQUITY"),
        ],
        true,
        198,
    ))
    .unwrap()
    .into_iter()
    .map(|item| item.text.trim().to_string())
    .collect();
    for expected in [
        "Date: May 12, 2026 at 11:23 PM",
        "ACME INC. 800-625 av. du Parc",
        "TOTAL. INCLUDING FEES",
        "LIABILITIES AND EQUITY",
    ] {
        assert!(
            texts.iter().any(|t| t == expected),
            "missing {expected:?} in {texts:?}"
        );
    }
}

/// The hinted-gap allowance of a face that paints its spaces holds between
/// two of its own glyphs: a code word in another face keeps the gap to it.
#[test]
fn a_glyph_of_another_face_keeps_its_gap_to_a_painted_space_face() {
    let code = "PENDING";
    let texts = item_texts(
        &[
            Run {
                x: 35.0,
                y: 51.0,
                face: Face::Mono,
                text: code,
            },
            run(
                35.0 + 5.0 * code.len() as f32 + 1.8,
                51.0,
                false,
                ": held for sign off by the author",
            ),
        ],
        true,
    );
    let line = texts.join(" ");
    assert!(
        line.contains("PENDING : held for sign off by the author"),
        "{texts:?}"
    );
}

/// A face that paints its word spaces can also set a grid of single digits
/// on the same page. Its cells stand 0.15 and 0.25 em apart, uneven gaps no
/// tracked run leaves, and a digit's hinted advance never strays that far.
#[test]
fn a_digit_grid_in_a_painted_space_face_keeps_its_cells_apart() {
    let mut runs = vec![
        run(35.0, 51.0, false, "Name of reporting entity"),
        run(35.0, 62.0, false, "Investment in associates"),
        run(35.0, 73.0, false, "Trade and other payables"),
    ];
    for (row, digits) in ["101", "010"].iter().enumerate() {
        let y = 90.0 + 11.0 * row as f32;
        for (col, x) in [35.0, 40.2, 46.2].into_iter().enumerate() {
            runs.push(run(x, y, false, &digits[col..=col]));
        }
    }
    let texts = item_texts(&runs, true);
    for expected in ["1 0 1", "0 1 0", "Name of reporting entity"] {
        assert!(
            texts.iter().any(|t| t == expected),
            "missing {expected:?} in {texts:?}"
        );
    }
}
