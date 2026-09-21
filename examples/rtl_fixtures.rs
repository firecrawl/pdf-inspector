//! Generates the right-to-left text fixtures under `tests/fixtures/`.
//!
//! Every fixture is synthetic: the sentences were written for these tests,
//! and the Hebrew and Arabic glyphs come from subsets of Noto Sans Hebrew
//! and Noto Sans Arabic (SIL Open Font License 1.1 — the licence texts sit
//! next to the subsets in `tests/fixtures/fonts/`), embedded as CID-keyed
//! TrueType fonts addressed by glyph index. Latin letters and digits use
//! the non-embedded Helvetica.
//!
//! The pages are laid out the way producers of visible right-to-left text
//! lay them out: each line is run through the Unicode Bidirectional
//! Algorithm and its glyphs are painted in display order, left to right, so
//! the content stream holds every right-to-left word spelled backwards.
//! One fixture positions every glyph on its own; one shapes Arabic into its
//! presentation forms and maps the glyphs to those code points, as fonts
//! subset by glyph do; one shows its word runs in reading order — right to
//! left across the line, one text object each — the way word processors
//! hand their runs to a PDF context; and one is an invisible text layer
//! holding its words in logical order, the convention of OCR layers.
//!
//! Regenerate with `cargo run --example rtl_fixtures`. The output is
//! deterministic (no timestamps, no compression, no document ID).
//!
//! The subsets were cut from the upstream release files with fontTools:
//!
//! ```text
//! pyftsubset NotoSansHebrew-Regular.ttf \
//!   --unicodes="U+0020,U+05B0-05C7,U+05D0-05EA,U+05F0-05F4" \
//!   --layout-features='' --drop-tables+=GSUB,GPOS,GDEF --no-hinting \
//!   --name-IDs='*' --glyph-names
//! pyftsubset NotoSansArabic-Regular.ttf \
//!   --unicodes="U+0020,U+060C,U+061B,U+061F,U+0621-063A,U+0640-0652,U+0660-066C,U+FE70-FEFC" \
//!   --layout-features='' --drop-tables+=GSUB,GPOS,GDEF --no-hinting \
//!   --name-IDs='*' --glyph-names
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use lopdf::xref::XrefType;
use lopdf::{dictionary, Document, Object, Stream, StringFormat};
use unicode_bidi::{BidiInfo, Level};

const PAGE_WIDTH: f32 = 612.0;
const PAGE_HEIGHT: f32 = 792.0;
const LEFT_MARGIN: f32 = 72.0;
const RIGHT_MARGIN: f32 = 540.0;
const BODY_SIZE: f32 = 12.0;
const HEADING_SIZE: f32 = 16.0;
const LEADING: f32 = 20.0;

/// An embedded TrueType subset: glyph lookup and metrics.
struct EmbeddedFont {
    resource: &'static str,
    base_font: &'static str,
    data: Vec<u8>,
    units_per_em: f32,
    /// Character → glyph index, from the font's own cmap.
    glyphs: BTreeMap<char, u16>,
    advances: BTreeMap<u16, u16>,
    space_width: f32,
}

impl EmbeddedFont {
    fn load(resource: &'static str, base_font: &'static str, path: &Path) -> Self {
        let data =
            std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let face = ttf_parser::Face::parse(&data, 0).expect("font parses");
        let mut glyphs = BTreeMap::new();
        let mut advances = BTreeMap::new();
        for subtable in face.tables().cmap.expect("cmap").subtables {
            if !subtable.is_unicode() {
                continue;
            }
            subtable.codepoints(|cp| {
                if let (Some(ch), Some(gid)) = (char::from_u32(cp), subtable.glyph_index(cp)) {
                    glyphs.entry(ch).or_insert(gid.0);
                    if let Some(advance) = face.glyph_hor_advance(gid) {
                        advances.insert(gid.0, advance);
                    }
                }
            });
        }
        let units_per_em = f32::from(face.units_per_em());
        let space_width = glyphs
            .get(&' ')
            .and_then(|gid| advances.get(gid))
            .map_or(0.25, |&w| f32::from(w) / units_per_em);
        Self {
            resource,
            base_font,
            data,
            units_per_em,
            glyphs,
            advances,
            space_width,
        }
    }

    fn covers(&self, c: char) -> bool {
        self.glyphs.contains_key(&c)
    }

    fn glyph(&self, c: char) -> u16 {
        *self
            .glyphs
            .get(&c)
            .unwrap_or_else(|| panic!("{} has no glyph for U+{:04X}", self.base_font, c as u32))
    }

    /// Advance of `c`, in text space units of 1/1000 em.
    fn width_1000(&self, c: char) -> f32 {
        let gid = self.glyph(c);
        f32::from(self.advances.get(&gid).copied().unwrap_or(0)) * 1000.0 / self.units_per_em
    }
}

/// Helvetica advance widths (Adobe font metrics), for the characters the
/// fixtures use.
fn helvetica_width(c: char) -> u16 {
    match c {
        ' ' | '.' | ',' | ':' | ';' => 278,
        '-' | '(' | ')' | 'r' => 333,
        '0'..='9' => 556,
        '%' => 889,
        'i' | 'j' | 'l' => 222,
        'f' | 't' | 'I' => 278,
        'c' | 'k' | 's' | 'v' | 'x' | 'y' | 'z' | 'J' => 500,
        'm' => 833,
        'w' => 722,
        'A' | 'B' | 'E' | 'K' | 'P' | 'V' | 'X' | 'Y' => 667,
        'C' | 'D' | 'H' | 'N' | 'R' | 'U' => 722,
        'F' | 'T' | 'Z' => 611,
        'G' | 'O' | 'Q' => 778,
        'L' => 556,
        'M' => 833,
        'S' => 667,
        'W' => 944,
        _ => 556,
    }
}

/// Which font paints a character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FontUse {
    Hebrew,
    Arabic,
    Latin,
}

fn font_for(c: char, hebrew: &EmbeddedFont, arabic: &EmbeddedFont) -> FontUse {
    if matches!(c, '\u{0590}'..='\u{05FF}') && hebrew.covers(c) {
        FontUse::Hebrew
    } else if (matches!(c, '\u{0600}'..='\u{06FF}' | '\u{FE70}'..='\u{FEFF}')) && arabic.covers(c) {
        FontUse::Arabic
    } else {
        FontUse::Latin
    }
}

/// Mirror glyphs of the paired brackets the fixtures use (rule L4).
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        _ => c,
    }
}

fn is_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
}

/// `text` in display order, left to right, for a paragraph of the given
/// base direction: the Unicode Bidirectional Algorithm's rule L2 with
/// combining marks kept after their base and brackets mirrored at odd
/// levels.
fn display_order(text: &str, rtl_base: bool) -> Vec<char> {
    let level = if rtl_base { Level::rtl() } else { Level::ltr() };
    let info = BidiInfo::new(text, Some(level));
    let para = &info.paragraphs[0];
    let levels: Vec<u8> = info
        .reordered_levels_per_char(para, para.range.clone())
        .iter()
        .map(|l| l.number())
        .collect();
    let chars: Vec<char> = text.chars().collect();
    let mut order: Vec<usize> = (0..chars.len()).collect();
    let max_level = levels.iter().copied().max().unwrap_or(0);
    for min in (1..=max_level).rev() {
        let mut start = 0;
        while start < order.len() {
            if levels[order[start]] < min {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < order.len() && levels[order[end]] >= min {
                end += 1;
            }
            // Reverse the run by clusters: a base character keeps its marks.
            let mut clusters: Vec<Vec<usize>> = Vec::new();
            for &p in &order[start..end] {
                if is_mark(chars[p]) && !clusters.is_empty() {
                    clusters.last_mut().unwrap().push(p);
                } else {
                    clusters.push(vec![p]);
                }
            }
            let reversed: Vec<usize> = clusters.into_iter().rev().flatten().collect();
            order[start..end].copy_from_slice(&reversed);
            start = end;
        }
    }
    order
        .into_iter()
        .map(|p| {
            if levels[p] % 2 == 1 {
                mirror(chars[p])
            } else {
                chars[p]
            }
        })
        .collect()
}

/// Arabic letters shaped into their Presentation Forms-B code points.
///
/// Each joining letter has an isolated and a final form; the dual-joining
/// letters also have initial and medial forms. Lam followed by alef becomes
/// the lam-alef ligature. Everything else passes through.
fn shape_arabic(text: &str) -> String {
    // (letter, isolated, final, initial, medial); a letter without initial
    // and medial forms (`None`) joins only to the letter before it.
    type Forms = (char, u32, u32, Option<(u32, u32)>);
    const FORMS: &[Forms] = &[
        ('\u{0622}', 0xFE81, 0xFE82, None),
        ('\u{0623}', 0xFE83, 0xFE84, None),
        ('\u{0624}', 0xFE85, 0xFE86, None),
        ('\u{0625}', 0xFE87, 0xFE88, None),
        ('\u{0626}', 0xFE89, 0xFE8A, Some((0xFE8B, 0xFE8C))),
        ('\u{0627}', 0xFE8D, 0xFE8E, None),
        ('\u{0628}', 0xFE8F, 0xFE90, Some((0xFE91, 0xFE92))),
        ('\u{0629}', 0xFE93, 0xFE94, None),
        ('\u{062A}', 0xFE95, 0xFE96, Some((0xFE97, 0xFE98))),
        ('\u{062B}', 0xFE99, 0xFE9A, Some((0xFE9B, 0xFE9C))),
        ('\u{062C}', 0xFE9D, 0xFE9E, Some((0xFE9F, 0xFEA0))),
        ('\u{062D}', 0xFEA1, 0xFEA2, Some((0xFEA3, 0xFEA4))),
        ('\u{062E}', 0xFEA5, 0xFEA6, Some((0xFEA7, 0xFEA8))),
        ('\u{062F}', 0xFEA9, 0xFEAA, None),
        ('\u{0630}', 0xFEAB, 0xFEAC, None),
        ('\u{0631}', 0xFEAD, 0xFEAE, None),
        ('\u{0632}', 0xFEAF, 0xFEB0, None),
        ('\u{0633}', 0xFEB1, 0xFEB2, Some((0xFEB3, 0xFEB4))),
        ('\u{0634}', 0xFEB5, 0xFEB6, Some((0xFEB7, 0xFEB8))),
        ('\u{0635}', 0xFEB9, 0xFEBA, Some((0xFEBB, 0xFEBC))),
        ('\u{0636}', 0xFEBD, 0xFEBE, Some((0xFEBF, 0xFEC0))),
        ('\u{0637}', 0xFEC1, 0xFEC2, Some((0xFEC3, 0xFEC4))),
        ('\u{0638}', 0xFEC5, 0xFEC6, Some((0xFEC7, 0xFEC8))),
        ('\u{0639}', 0xFEC9, 0xFECA, Some((0xFECB, 0xFECC))),
        ('\u{063A}', 0xFECD, 0xFECE, Some((0xFECF, 0xFED0))),
        ('\u{0641}', 0xFED1, 0xFED2, Some((0xFED3, 0xFED4))),
        ('\u{0642}', 0xFED5, 0xFED6, Some((0xFED7, 0xFED8))),
        ('\u{0643}', 0xFED9, 0xFEDA, Some((0xFEDB, 0xFEDC))),
        ('\u{0644}', 0xFEDD, 0xFEDE, Some((0xFEDF, 0xFEE0))),
        ('\u{0645}', 0xFEE1, 0xFEE2, Some((0xFEE3, 0xFEE4))),
        ('\u{0646}', 0xFEE5, 0xFEE6, Some((0xFEE7, 0xFEE8))),
        ('\u{0647}', 0xFEE9, 0xFEEA, Some((0xFEEB, 0xFEEC))),
        ('\u{0648}', 0xFEED, 0xFEEE, None),
        ('\u{0649}', 0xFEEF, 0xFEF0, None),
        ('\u{064A}', 0xFEF1, 0xFEF2, Some((0xFEF3, 0xFEF4))),
    ];
    let form_of = |c: char| FORMS.iter().find(|f| f.0 == c);
    let joins_forward = |c: char| form_of(c).is_some_and(|f| f.3.is_some());
    let joins = |c: char| form_of(c).is_some();
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() * 2);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let Some(&(_, isolated, final_form, medial)) = form_of(c) else {
            out.push(c);
            i += 1;
            continue;
        };
        let prev_connects = i > 0 && joins_forward(chars[i - 1]);
        // Lam followed by alef: the ligature, final when the lam connects.
        if c == '\u{0644}' && i + 1 < chars.len() && chars[i + 1] == '\u{0627}' {
            let ligature = if prev_connects { 0xFEFC } else { 0xFEFB };
            out.push(char::from_u32(ligature).unwrap());
            i += 2;
            continue;
        }
        let next_connects = medial.is_some() && i + 1 < chars.len() && joins(chars[i + 1]);
        let code = match (prev_connects, next_connects) {
            (false, false) => isolated,
            (true, false) => final_form,
            (false, true) => medial.unwrap().0,
            (true, true) => medial.unwrap().1,
        };
        out.push(char::from_u32(code).unwrap());
        i += 1;
    }
    out
}

/// One line of a fixture page.
struct Line {
    /// Logical text as written.
    text: String,
    size: f32,
    rtl_base: bool,
    /// Right-aligned at the right margin (RTL lines) or left-aligned.
    right_aligned: bool,
}

impl Line {
    fn rtl(text: &str) -> Self {
        Self {
            text: text.to_string(),
            size: BODY_SIZE,
            rtl_base: true,
            right_aligned: true,
        }
    }
    fn heading(text: &str) -> Self {
        Self {
            size: HEADING_SIZE,
            ..Self::rtl(text)
        }
    }
    fn ltr(text: &str) -> Self {
        Self {
            text: text.to_string(),
            size: BODY_SIZE,
            rtl_base: false,
            right_aligned: false,
        }
    }
}

/// How the glyphs of a line are shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Painting {
    /// One show operator per run of glyphs from one font, positioned with
    /// `Tm`; word spaces are pen movements, not glyphs.
    WordRuns,
    /// Every glyph positioned by its own `Tm`, with the small rounding
    /// jitter producers leave in their coordinates.
    GlyphByGlyph,
    /// The runs of `WordRuns`, each shown by its own text object, in reading
    /// order: right to left across a right-to-left line, the way word
    /// processors hand their runs to a PDF context. Each run's glyphs stay
    /// in visual order with forward advances.
    WordRunsInReadingOrder,
    /// One show operator per word holding the word's glyphs in logical
    /// (reading) order with forward advances, positioned at the word's place
    /// on the line, in reading order, as invisible text (render mode 3): the
    /// convention of OCR text layers, whose glyphs are never displayed.
    InvisibleLogicalWords,
}

/// A painted glyph: its font, code, advance and the pen position it was
/// shown at.
struct PaintedGlyph {
    font: FontUse,
    ch: char,
    x: f32,
    advance: f32,
}

struct Fixture {
    file: &'static str,
    title: &'static str,
    lines: Vec<Line>,
    painting: Painting,
    /// Shape Arabic into presentation forms and map the glyphs to them.
    presentation_forms: bool,
    /// Factor applied to the declared glyph widths (`/W`) of the embedded
    /// fonts, while the glyphs are positioned by their true advances: the
    /// width mismatch of producers whose metrics do not match the subset
    /// they embed.
    declared_width_scale: f32,
}

struct Layout {
    content: String,
    /// Per line: the logical text and the painted extent (left, right).
    extents: Vec<(String, f32, f32)>,
    used_hebrew: BTreeMap<u16, char>,
    used_arabic: BTreeMap<u16, char>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn lay_out(fixture: &Fixture, hebrew: &EmbeddedFont, arabic: &EmbeddedFont) -> Layout {
    let mut content = String::new();
    let mut extents = Vec::new();
    let mut used_hebrew = BTreeMap::new();
    let mut used_arabic = BTreeMap::new();
    let mut y = PAGE_HEIGHT - 100.0;
    for line in &fixture.lines {
        let shaped = if fixture.presentation_forms {
            shape_arabic(&line.text)
        } else {
            line.text.clone()
        };
        let display = display_order(&shaped, line.rtl_base);
        let space = if line.rtl_base {
            if display
                .iter()
                .any(|&c| font_for(c, hebrew, arabic) == FontUse::Arabic)
            {
                arabic.space_width
            } else {
                hebrew.space_width
            }
        } else {
            f32::from(helvetica_width(' ')) / 1000.0
        } * line.size;

        // Pen positions of every glyph, left to right.
        let mut glyphs: Vec<PaintedGlyph> = Vec::new();
        let mut pen = 0.0f32;
        for &c in &display {
            if c == ' ' {
                pen += space;
                continue;
            }
            let font = font_for(c, hebrew, arabic);
            let advance = match font {
                FontUse::Hebrew => hebrew.width_1000(c),
                FontUse::Arabic => arabic.width_1000(c),
                FontUse::Latin => f32::from(helvetica_width(c)),
            } / 1000.0
                * line.size;
            glyphs.push(PaintedGlyph {
                font,
                ch: c,
                x: pen,
                advance,
            });
            pen += advance;
        }
        let x0 = if line.right_aligned {
            RIGHT_MARGIN - pen
        } else {
            LEFT_MARGIN
        };
        let (mut left, mut right) = (f32::MAX, f32::MIN);
        for glyph in &glyphs {
            left = left.min(x0 + glyph.x);
            right = right.max(x0 + glyph.x + glyph.advance);
        }
        extents.push((line.text.clone(), left, right));

        let code_of = |glyph: &PaintedGlyph| -> Vec<u8> {
            match glyph.font {
                FontUse::Hebrew => hebrew.glyph(glyph.ch).to_be_bytes().to_vec(),
                FontUse::Arabic => arabic.glyph(glyph.ch).to_be_bytes().to_vec(),
                FontUse::Latin => vec![glyph.ch as u8],
            }
        };
        for glyph in &glyphs {
            match glyph.font {
                FontUse::Hebrew => {
                    used_hebrew.insert(hebrew.glyph(glyph.ch), glyph.ch);
                }
                FontUse::Arabic => {
                    used_arabic.insert(arabic.glyph(glyph.ch), glyph.ch);
                }
                FontUse::Latin => {}
            }
        }
        let resource = |font: FontUse| match font {
            FontUse::Hebrew => hebrew.resource,
            FontUse::Arabic => arabic.resource,
            FontUse::Latin => "FL",
        };

        // A run: consecutive glyphs of one font with no word space between
        // them — a word, or a Latin phrase.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < glyphs.len() {
            let font = glyphs[i].font;
            let mut j = i + 1;
            while j < glyphs.len()
                && glyphs[j].font == font
                && (glyphs[j].x - (glyphs[j - 1].x + glyphs[j - 1].advance)).abs() < 0.01
            {
                j += 1;
            }
            runs.push((i, j));
            i = j;
        }
        // The runs in reading order: right to left across a right-to-left
        // line.
        let mut reading_order = runs.clone();
        if line.rtl_base {
            reading_order.reverse();
        }
        let show_run = |content: &mut String, first: &PaintedGlyph, codes: &[u8]| {
            writeln!(
                content,
                "/{} {} Tf 1 0 0 1 {:.2} {:.2} Tm <{}> Tj",
                resource(first.font),
                line.size,
                x0 + first.x,
                y,
                hex(codes)
            )
            .unwrap();
        };
        match fixture.painting {
            Painting::WordRuns => {
                content.push_str("BT\n");
                for &(i, j) in &runs {
                    let codes: Vec<u8> = glyphs[i..j].iter().flat_map(code_of).collect();
                    show_run(&mut content, &glyphs[i], &codes);
                }
                content.push_str("ET\n");
            }
            Painting::WordRunsInReadingOrder => {
                for &(i, j) in &reading_order {
                    let codes: Vec<u8> = glyphs[i..j].iter().flat_map(code_of).collect();
                    content.push_str("BT\n");
                    show_run(&mut content, &glyphs[i], &codes);
                    content.push_str("ET\n");
                }
            }
            Painting::InvisibleLogicalWords => {
                // A right-to-left run's glyphs in reading order are its
                // display order turned round; a Latin run is already read
                // left to right.
                content.push_str("BT\n3 Tr\n");
                for &(i, j) in &reading_order {
                    let run = &glyphs[i..j];
                    let codes: Vec<u8> = if run[0].font == FontUse::Latin {
                        run.iter().flat_map(code_of).collect()
                    } else {
                        run.iter().rev().flat_map(code_of).collect()
                    };
                    show_run(&mut content, &glyphs[i], &codes);
                }
                content.push_str("ET\n");
            }
            Painting::GlyphByGlyph => {
                content.push_str("BT\n");
                for (k, glyph) in glyphs.iter().enumerate() {
                    let jitter = [0.0, 0.02, -0.02][k % 3];
                    writeln!(
                        content,
                        "/{} {} Tf 1 0 0 1 {:.2} {:.2} Tm <{}> Tj",
                        resource(glyph.font),
                        line.size,
                        x0 + glyph.x + jitter,
                        y,
                        hex(&code_of(glyph))
                    )
                    .unwrap();
                }
                content.push_str("ET\n");
            }
        }
        y -= LEADING + (line.size - BODY_SIZE);
    }
    Layout {
        content,
        extents,
        used_hebrew,
        used_arabic,
    }
}

/// A ToUnicode CMap mapping each used glyph index to the character it
/// stands for.
fn to_unicode_cmap(used: &BTreeMap<u16, char>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in used.iter().collect::<Vec<_>>().chunks(100) {
        writeln!(cmap, "{} beginbfchar", chunk.len()).unwrap();
        for (gid, ch) in chunk {
            let mut units = [0u16; 2];
            let encoded = ch.encode_utf16(&mut units);
            let dst: String = encoded.iter().map(|u| format!("{u:04X}")).collect();
            writeln!(cmap, "<{gid:04X}> <{dst}>").unwrap();
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn plain_stream(dict: lopdf::Dictionary, content: Vec<u8>) -> Object {
    Object::Stream(Stream::new(dict, content).with_compression(false))
}

/// A CID-keyed TrueType font dictionary for an embedded subset, with the
/// widths of the used glyphs and a ToUnicode CMap.
fn add_cid_font(
    doc: &mut Document,
    font: &EmbeddedFont,
    used: &BTreeMap<u16, char>,
    declared_width_scale: f32,
) -> Object {
    let font_file = doc.add_object(plain_stream(
        dictionary! { "Length1" => Object::Integer(font.data.len() as i64) },
        font.data.clone(),
    ));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::Name(font.base_font.as_bytes().to_vec()),
        "Flags" => Object::Integer(4),
        "FontBBox" => Object::Array(vec![(-500).into(), (-300).into(), 1500.into(), 1100.into()]),
        "ItalicAngle" => Object::Integer(0),
        "Ascent" => Object::Integer(1069),
        "Descent" => Object::Integer(-293),
        "CapHeight" => Object::Integer(714),
        "StemV" => Object::Integer(80),
        "FontFile2" => Object::Reference(font_file),
    });
    let mut widths: Vec<Object> = Vec::new();
    for &gid in used.keys() {
        let w = f32::from(font.advances.get(&gid).copied().unwrap_or(0)) * 1000.0
            / font.units_per_em
            * declared_width_scale;
        widths.push(Object::Integer(i64::from(gid)));
        widths.push(Object::Array(vec![Object::Integer(w.round() as i64)]));
    }
    let cid_font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => Object::Name(font.base_font.as_bytes().to_vec()),
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::String(b"Adobe".to_vec(), StringFormat::Literal),
            "Ordering" => Object::String(b"Identity".to_vec(), StringFormat::Literal),
            "Supplement" => Object::Integer(0),
        },
        "FontDescriptor" => Object::Reference(descriptor),
        "DW" => Object::Integer(500),
        "W" => Object::Array(widths),
        "CIDToGIDMap" => "Identity",
    });
    let to_unicode = doc.add_object(plain_stream(dictionary! {}, to_unicode_cmap(used)));
    let type0 = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => Object::Name(font.base_font.as_bytes().to_vec()),
        "Encoding" => "Identity-H",
        "DescendantFonts" => Object::Array(vec![Object::Reference(cid_font)]),
        "ToUnicode" => Object::Reference(to_unicode),
    });
    Object::Reference(type0)
}

fn add_helvetica(doc: &mut Document) -> Object {
    let widths: Vec<Object> = (32u8..=126)
        .map(|b| Object::Integer(i64::from(helvetica_width(b as char))))
        .collect();
    Object::Reference(doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
        "FirstChar" => Object::Integer(32),
        "LastChar" => Object::Integer(126),
        "Widths" => Object::Array(widths),
    }))
}

fn write_fixture(fixture: &Fixture, hebrew: &EmbeddedFont, arabic: &EmbeddedFont, dir: &Path) {
    let layout = lay_out(fixture, hebrew, arabic);
    let mut doc = Document::new();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;

    let mut fonts = dictionary! { "FL" => add_helvetica(&mut doc) };
    if !layout.used_hebrew.is_empty() {
        fonts.set(
            hebrew.resource,
            add_cid_font(
                &mut doc,
                hebrew,
                &layout.used_hebrew,
                fixture.declared_width_scale,
            ),
        );
    }
    if !layout.used_arabic.is_empty() {
        fonts.set(
            arabic.resource,
            add_cid_font(
                &mut doc,
                arabic,
                &layout.used_arabic,
                fixture.declared_width_scale,
            ),
        );
    }
    let content = doc.add_object(plain_stream(dictionary! {}, layout.content.into_bytes()));
    let pages_id = doc.new_object_id();
    let page = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()]),
        "Resources" => dictionary! { "Font" => fonts },
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page)]),
            "Count" => Object::Integer(1),
        }),
    );
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    let info = doc.add_object(dictionary! {
        "Title" => Object::String(fixture.title.as_bytes().to_vec(), StringFormat::Literal),
        "Subject" => Object::String(
            b"Synthetic test fixture. Embedded glyphs: Noto Sans Hebrew / Noto Sans Arabic subsets, Copyright 2022 The Noto Project Authors, SIL Open Font License 1.1.".to_vec(),
            StringFormat::Literal,
        ),
        "Producer" => Object::String(b"pdf-inspector examples/rtl_fixtures.rs".to_vec(), StringFormat::Literal),
    });
    doc.trailer.set("Root", Object::Reference(catalog));
    doc.trailer.set("Info", Object::Reference(info));

    let path = dir.join(fixture.file);
    doc.save(&path)
        .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    println!("{}", path.display());
    for (text, left, right) in &layout.extents {
        println!("  {left:7.2} {right:7.2}  {text}");
    }
}

/// The lines of the Hebrew report page, shared by the fixtures that store
/// them in visual order.
fn hebrew_report_lines() -> Vec<Line> {
    vec![
                // דוח שנתי 2024
                Line::heading("\u{05D3}\u{05D5}\u{05D7} \u{05E9}\u{05E0}\u{05EA}\u{05D9} 2024"),
                // מספר העובדים גדל ב-12% לעומת השנה הקודמת.
                Line::rtl(
                    "\u{05DE}\u{05E1}\u{05E4}\u{05E8} \u{05D4}\u{05E2}\u{05D5}\u{05D1}\u{05D3}\u{05D9}\u{05DD} \
                     \u{05D2}\u{05D3}\u{05DC} \u{05D1}-12% \u{05DC}\u{05E2}\u{05D5}\u{05DE}\u{05EA} \
                     \u{05D4}\u{05E9}\u{05E0}\u{05D4} \u{05D4}\u{05E7}\u{05D5}\u{05D3}\u{05DE}\u{05EA}.",
                ),
                // התקן (IFRS 16) אומץ בשנת 2019.
                Line::rtl(
                    "\u{05D4}\u{05EA}\u{05E7}\u{05DF} (IFRS 16) \u{05D0}\u{05D5}\u{05DE}\u{05E5} \
                     \u{05D1}\u{05E9}\u{05E0}\u{05EA} 2019.",
                ),
                // A right-aligned paragraph of three lines:
                // הוועדה בחנה את הנתונים במהלך הרבעון השלישי.
                Line::rtl(
                    "\u{05D4}\u{05D5}\u{05D5}\u{05E2}\u{05D3}\u{05D4} \u{05D1}\u{05D7}\u{05E0}\u{05D4} \
                     \u{05D0}\u{05EA} \u{05D4}\u{05E0}\u{05EA}\u{05D5}\u{05E0}\u{05D9}\u{05DD} \
                     \u{05D1}\u{05DE}\u{05D4}\u{05DC}\u{05DA} \u{05D4}\u{05E8}\u{05D1}\u{05E2}\u{05D5}\u{05DF} \
                     \u{05D4}\u{05E9}\u{05DC}\u{05D9}\u{05E9}\u{05D9}.",
                ),
                // הממצאים הוצגו להנהלה, ולאחר דיון אושרו ההמלצות.
                Line::rtl(
                    "\u{05D4}\u{05DE}\u{05DE}\u{05E6}\u{05D0}\u{05D9}\u{05DD} \u{05D4}\u{05D5}\u{05E6}\u{05D2}\u{05D5} \
                     \u{05DC}\u{05D4}\u{05E0}\u{05D4}\u{05DC}\u{05D4}, \u{05D5}\u{05DC}\u{05D0}\u{05D7}\u{05E8} \
                     \u{05D3}\u{05D9}\u{05D5}\u{05DF} \u{05D0}\u{05D5}\u{05E9}\u{05E8}\u{05D5} \
                     \u{05D4}\u{05D4}\u{05DE}\u{05DC}\u{05E6}\u{05D5}\u{05EA}.",
                ),
                // היישום יחל בתחילת השנה הבאה.
                Line::rtl(
                    "\u{05D4}\u{05D9}\u{05D9}\u{05E9}\u{05D5}\u{05DD} \u{05D9}\u{05D7}\u{05DC} \
                     \u{05D1}\u{05EA}\u{05D7}\u{05D9}\u{05DC}\u{05EA} \u{05D4}\u{05E9}\u{05E0}\u{05D4} \
                     \u{05D4}\u{05D1}\u{05D0}\u{05D4}.",
                ),
    ]
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fonts = root.join("tests/fixtures/fonts");
    let hebrew = EmbeddedFont::load(
        "FH",
        "NotoSansHebrew-Regular",
        &fonts.join("NotoSansHebrew-Regular.subset.ttf"),
    );
    let arabic = EmbeddedFont::load(
        "FA",
        "NotoSansArabic-Regular",
        &fonts.join("NotoSansArabic-Regular.subset.ttf"),
    );

    let fixtures = [
        Fixture {
            file: "rtl_hebrew_visual_words.pdf",
            title: "Hebrew text stored in visual order, one run per word",
            painting: Painting::WordRuns,
            presentation_forms: false,
            declared_width_scale: 1.0,
            lines: hebrew_report_lines(),
        },
        Fixture {
            file: "rtl_hebrew_visual_words_in_reading_order.pdf",
            title: "Hebrew text stored in visual order, one text object per word, shown in reading order",
            painting: Painting::WordRunsInReadingOrder,
            presentation_forms: false,
            declared_width_scale: 1.0,
            lines: hebrew_report_lines(),
        },
        Fixture {
            file: "rtl_hebrew_invisible_logical_words.pdf",
            title: "Hebrew text in logical order shown as an invisible text layer, one word per show operator",
            painting: Painting::InvisibleLogicalWords,
            presentation_forms: false,
            declared_width_scale: 1.0,
            lines: vec![
                // הספרייה פתוחה בכל ימות השבוע
                Line::rtl(
                    "\u{05D4}\u{05E1}\u{05E4}\u{05E8}\u{05D9}\u{05D9}\u{05D4} \u{05E4}\u{05EA}\u{05D5}\u{05D7}\u{05D4} \
                     \u{05D1}\u{05DB}\u{05DC} \u{05D9}\u{05DE}\u{05D5}\u{05EA} \u{05D4}\u{05E9}\u{05D1}\u{05D5}\u{05E2}",
                ),
                // הקוראים מוזמנים להשאיל ספרים
                Line::rtl(
                    "\u{05D4}\u{05E7}\u{05D5}\u{05E8}\u{05D0}\u{05D9}\u{05DD} \u{05DE}\u{05D5}\u{05D6}\u{05DE}\u{05E0}\u{05D9}\u{05DD} \
                     \u{05DC}\u{05D4}\u{05E9}\u{05D0}\u{05D9}\u{05DC} \u{05E1}\u{05E4}\u{05E8}\u{05D9}\u{05DD}",
                ),
                // ההרשמה נעשית בדלפק הכניסה
                Line::rtl(
                    "\u{05D4}\u{05D4}\u{05E8}\u{05E9}\u{05DE}\u{05D4} \u{05E0}\u{05E2}\u{05E9}\u{05D9}\u{05EA} \
                     \u{05D1}\u{05D3}\u{05DC}\u{05E4}\u{05E7} \u{05D4}\u{05DB}\u{05E0}\u{05D9}\u{05E1}\u{05D4}",
                ),
            ],
        },
        Fixture {
            file: "rtl_hebrew_glyph_by_glyph.pdf",
            title: "Hebrew text with every glyph positioned on its own",
            painting: Painting::GlyphByGlyph,
            presentation_forms: false,
            // Declared widths 15% narrower than the painted advances: every
            // letter box ends short of the next letter, by more for the
            // wider letters.
            declared_width_scale: 0.85,
            lines: vec![
                // שוק העבודה השתנה בעשור האחרון
                Line::rtl(
                    "\u{05E9}\u{05D5}\u{05E7} \u{05D4}\u{05E2}\u{05D1}\u{05D5}\u{05D3}\u{05D4} \
                     \u{05D4}\u{05E9}\u{05EA}\u{05E0}\u{05D4} \u{05D1}\u{05E2}\u{05E9}\u{05D5}\u{05E8} \
                     \u{05D4}\u{05D0}\u{05D7}\u{05E8}\u{05D5}\u{05DF}",
                ),
                // בשנת 2023 נוספו 1,250 משרות חדשות
                Line::rtl(
                    "\u{05D1}\u{05E9}\u{05E0}\u{05EA} 2023 \u{05E0}\u{05D5}\u{05E1}\u{05E4}\u{05D5} 1,250 \
                     \u{05DE}\u{05E9}\u{05E8}\u{05D5}\u{05EA} \u{05D7}\u{05D3}\u{05E9}\u{05D5}\u{05EA}",
                ),
                // הדוח המלא זמין באתר (PDF)
                Line::rtl(
                    "\u{05D4}\u{05D3}\u{05D5}\u{05D7} \u{05D4}\u{05DE}\u{05DC}\u{05D0} \u{05D6}\u{05DE}\u{05D9}\u{05DF} \
                     \u{05D1}\u{05D0}\u{05EA}\u{05E8} (PDF)",
                ),
            ],
        },
        Fixture {
            file: "rtl_mixed_direction.pdf",
            title: "Hebrew, Arabic and Latin runs on one page",
            painting: Painting::WordRuns,
            presentation_forms: false,
            declared_width_scale: 1.0,
            lines: vec![
                // המסמך נכתב על ידי Open Data Team בשנת 2025
                Line::rtl(
                    "\u{05D4}\u{05DE}\u{05E1}\u{05DE}\u{05DA} \u{05E0}\u{05DB}\u{05EA}\u{05D1} \u{05E2}\u{05DC} \
                     \u{05D9}\u{05D3}\u{05D9} Open Data Team \u{05D1}\u{05E9}\u{05E0}\u{05EA} 2025",
                ),
                // التقرير السنوي 2024
                Line::rtl(
                    "\u{0627}\u{0644}\u{062A}\u{0642}\u{0631}\u{064A}\u{0631} \
                     \u{0627}\u{0644}\u{0633}\u{0646}\u{0648}\u{064A} 2024",
                ),
                // بلغ عدد الطلاب ١٢٥٠ في عام ٢٠٢٤
                Line::rtl(
                    "\u{0628}\u{0644}\u{063A} \u{0639}\u{062F}\u{062F} \u{0627}\u{0644}\u{0637}\u{0644}\u{0627}\u{0628} \
                     \u{0661}\u{0662}\u{0665}\u{0660} \u{0641}\u{064A} \u{0639}\u{0627}\u{0645} \
                     \u{0662}\u{0660}\u{0662}\u{0664}",
                ),
                // The word שלום means peace
                Line::ltr("The word \u{05E9}\u{05DC}\u{05D5}\u{05DD} means peace"),
            ],
        },
        Fixture {
            file: "rtl_arabic_presentation_forms.pdf",
            title: "Arabic shaped into presentation forms, stored in visual order",
            painting: Painting::WordRuns,
            presentation_forms: true,
            declared_width_scale: 1.0,
            lines: vec![
                // كتاب جديد للطلاب
                Line::rtl(
                    "\u{0643}\u{062A}\u{0627}\u{0628} \u{062C}\u{062F}\u{064A}\u{062F} \
                     \u{0644}\u{0644}\u{0637}\u{0644}\u{0627}\u{0628}",
                ),
                // النسبة ٢٥٪ من المجموع
                Line::rtl(
                    "\u{0627}\u{0644}\u{0646}\u{0633}\u{0628}\u{0629} \u{0662}\u{0665}\u{066A} \
                     \u{0645}\u{0646} \u{0627}\u{0644}\u{0645}\u{062C}\u{0645}\u{0648}\u{0639}",
                ),
                // الاجتماع (Zoom) في الساعة 10:30
                Line::rtl(
                    "\u{0627}\u{0644}\u{0627}\u{062C}\u{062A}\u{0645}\u{0627}\u{0639} (Zoom) \
                     \u{0641}\u{064A} \u{0627}\u{0644}\u{0633}\u{0627}\u{0639}\u{0629} 10:30",
                ),
            ],
        },
    ];
    let out = root.join("tests/fixtures");
    for fixture in &fixtures {
        write_fixture(fixture, &hebrew, &arabic, &out);
    }
}
