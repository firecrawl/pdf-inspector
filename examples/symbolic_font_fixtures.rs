//! Generates the symbolic-font, base-encoding and ligature-name fixtures
//! under `tests/fixtures/`.
//!
//! Every fixture is synthetic. The pages use the non-embedded standard
//! fonts (Symbol, ZapfDingbats, Courier, Helvetica, Times) and four tiny
//! TrueType programs built here from scratch — a handful of square glyphs
//! with the tables a reader needs — so nothing in them comes from any
//! existing font or document.
//!
//! Regenerate with `cargo run --example symbolic_font_fixtures`. The output
//! is deterministic (no timestamps, no compression, no document ID).

use std::path::Path;

use lopdf::xref::XrefType;
use lopdf::{dictionary, Document, Object, Stream, StringFormat};

const PAGE_WIDTH: f32 = 612.0;
const PAGE_HEIGHT: f32 = 792.0;

// ---------------------------------------------------------------------------
// A minimal TrueType builder
// ---------------------------------------------------------------------------

/// A glyph of the synthetic font: its advance and, unless it is blank, a
/// square outline; its `post` name when the font names its glyphs; and the
/// code its cmap maps to it, if any.
struct Glyph {
    name: Option<&'static str>,
    advance: u16,
    outlined: bool,
    code: Option<u32>,
}

/// The cmap subtable a synthetic font carries: Windows Unicode (3,1) or
/// Windows Symbol (3,0).
#[derive(Clone, Copy)]
enum CmapKind {
    Unicode,
    Symbol,
}

struct Be(Vec<u8>);

impl Be {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// A simple glyph: one square contour from (50, 0) to (550, 700), as four
/// on-curve points with 16-bit deltas.
fn square_glyph() -> Vec<u8> {
    let mut g = Be::new();
    g.i16(1); // contours
    g.i16(50);
    g.i16(0);
    g.i16(550);
    g.i16(700); // bounding box
    g.u16(3); // end point of the contour
    g.u16(0); // no instructions
    for _ in 0..4 {
        g.u8(0x01); // on curve, x and y as i16
    }
    for dx in [50i16, 0, 500, 0] {
        g.i16(dx);
    }
    for dy in [0i16, 700, 0, -700] {
        g.i16(dy);
    }
    g.0
}

/// Format 4 cmap subtable mapping each `(code, glyph)` pair with a segment
/// of its own.
fn cmap_format4(mappings: &[(u32, u16)]) -> Vec<u8> {
    let mut mappings: Vec<(u32, u16)> = mappings.to_vec();
    mappings.sort_by_key(|m| m.0);
    let seg_count = (mappings.len() + 1) as u16;
    let entry_selector = (seg_count as f32).log2().floor() as u16;
    let search_range = 2 * (1u16 << entry_selector);
    let range_shift = 2 * seg_count - search_range;
    let mut t = Be::new();
    t.u16(4);
    t.u16(16 + 8 * seg_count); // length
    t.u16(0); // language
    t.u16(seg_count * 2);
    t.u16(search_range);
    t.u16(entry_selector);
    t.u16(range_shift);
    for &(code, _) in &mappings {
        t.u16(code as u16);
    }
    t.u16(0xFFFF);
    t.u16(0); // reserved pad
    for &(code, _) in &mappings {
        t.u16(code as u16);
    }
    t.u16(0xFFFF);
    for &(code, gid) in &mappings {
        t.u16(gid.wrapping_sub(code as u16)); // idDelta
    }
    t.u16(1);
    for _ in 0..seg_count {
        t.u16(0); // idRangeOffset
    }
    t.0
}

/// Build a TrueType font program from `glyphs` (glyph 0 must be `.notdef`).
fn build_truetype(glyphs: &[Glyph], cmap: CmapKind) -> Vec<u8> {
    let num_glyphs = glyphs.len() as u16;

    let mut head = Be::new();
    head.u16(1);
    head.u16(0); // version 1.0
    head.u32(0x0001_0000); // font revision
    head.u32(0); // checksum adjustment (filled in below)
    head.u32(0x5F0F_3CF5); // magic
    head.u16(0x000B); // flags
    head.u16(1000); // units per em
    head.i64(0);
    head.i64(0); // created, modified
    head.i16(0);
    head.i16(0);
    head.i16(550);
    head.i16(700); // bounding box
    head.u16(0); // mac style
    head.u16(8); // lowest rec ppem
    head.i16(2); // direction hint
    head.i16(1); // long loca offsets
    head.i16(0); // glyph data format

    let mut hhea = Be::new();
    hhea.u32(0x0001_0000);
    hhea.i16(800);
    hhea.i16(-200);
    hhea.i16(0); // ascender, descender, line gap
    hhea.u16(glyphs.iter().map(|g| g.advance).max().unwrap_or(0));
    hhea.i16(0);
    hhea.i16(0);
    hhea.i16(550); // min lsb, min rsb, x max extent
    hhea.i16(1);
    hhea.i16(0);
    hhea.i16(0); // caret slope rise, run, offset
    for _ in 0..4 {
        hhea.i16(0); // reserved
    }
    hhea.i16(0); // metric data format
    hhea.u16(num_glyphs);

    let mut maxp = Be::new();
    maxp.u32(0x0001_0000);
    maxp.u16(num_glyphs);
    for v in [4u16, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0] {
        maxp.u16(v);
    }

    let mut hmtx = Be::new();
    for g in glyphs {
        hmtx.u16(g.advance);
        hmtx.i16(if g.outlined { 50 } else { 0 });
    }

    let mut glyf: Vec<u8> = Vec::new();
    let mut loca = Be::new();
    for g in glyphs {
        loca.u32(glyf.len() as u32);
        if g.outlined {
            glyf.extend(square_glyph());
            pad4(&mut glyf);
        }
    }
    loca.u32(glyf.len() as u32);

    let mappings: Vec<(u32, u16)> = glyphs
        .iter()
        .enumerate()
        .filter_map(|(gid, g)| g.code.map(|code| (code, gid as u16)))
        .collect();
    let subtable = cmap_format4(&mappings);
    let mut cmap_table = Be::new();
    cmap_table.u16(0);
    cmap_table.u16(1);
    cmap_table.u16(3); // Windows
    cmap_table.u16(match cmap {
        CmapKind::Unicode => 1,
        CmapKind::Symbol => 0,
    });
    cmap_table.u32(12);
    cmap_table.0.extend(subtable);

    // post: format 2 with the glyphs' names when any glyph has one,
    // format 3 (no names) otherwise.
    let mut post = Be::new();
    if glyphs.iter().any(|g| g.name.is_some()) {
        post.u32(0x0002_0000);
    } else {
        post.u32(0x0003_0000);
    }
    post.u32(0); // italic angle
    post.i16(-100);
    post.i16(50); // underline position, thickness
    post.u32(0); // proportional
    for _ in 0..4 {
        post.u32(0); // memory hints
    }
    if glyphs.iter().any(|g| g.name.is_some()) {
        post.u16(num_glyphs);
        let mut custom: Vec<&str> = Vec::new();
        for (gid, g) in glyphs.iter().enumerate() {
            match (gid, g.name) {
                (0, _) => post.u16(0), // .notdef
                (_, Some("space")) => post.u16(3),
                (_, Some(name)) => {
                    post.u16(258 + custom.len() as u16);
                    custom.push(name);
                }
                (_, None) => post.u16(0),
            }
        }
        for name in custom {
            post.u8(name.len() as u8);
            post.0.extend_from_slice(name.as_bytes());
        }
    }

    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap_table.0),
        (b"glyf", glyf),
        (b"head", head.0),
        (b"hhea", hhea.0),
        (b"hmtx", hmtx.0),
        (b"loca", loca.0),
        (b"maxp", maxp.0),
        (b"post", post.0),
    ];
    tables.sort_by_key(|(tag, _)| *tag);

    let num_tables = tables.len() as u16;
    let entry_selector = (num_tables as f32).log2().floor() as u16;
    let search_range = (1u16 << entry_selector) * 16;
    let mut font = Be::new();
    font.u32(0x0001_0000);
    font.u16(num_tables);
    font.u16(search_range);
    font.u16(entry_selector);
    font.u16(num_tables * 16 - search_range);
    let mut offset = 12 + 16 * tables.len();
    let mut body: Vec<u8> = Vec::new();
    let mut head_offset = 0;
    for (tag, data) in &tables {
        font.0.extend_from_slice(*tag);
        font.u32(checksum(data));
        font.u32(offset as u32);
        font.u32(data.len() as u32);
        if *tag == b"head" {
            head_offset = offset;
        }
        let mut padded = data.clone();
        pad4(&mut padded);
        offset += padded.len();
        body.extend(padded);
    }
    let mut out = font.0;
    out.extend(body);
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    out[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    out
}

// ---------------------------------------------------------------------------
// Fixture documents
// ---------------------------------------------------------------------------

fn plain_stream(dict: lopdf::Dictionary, content: Vec<u8>) -> Object {
    Object::Stream(Stream::new(dict, content).with_compression(false))
}

fn literal(bytes: &[u8]) -> Object {
    Object::String(bytes.to_vec(), StringFormat::Literal)
}

/// Add a page with the given font resources and content stream to a
/// document the fonts' indirect objects are already in, and save it to
/// `path` as a one-page document with the given title and subject.
fn finish_document(
    mut doc: Document,
    path: &Path,
    title: &str,
    subject: &[u8],
    fonts: lopdf::Dictionary,
    content: String,
) {
    let content_id = doc.add_object(plain_stream(dictionary! {}, content.into_bytes()));
    let pages_id = doc.new_object_id();
    let page = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()]),
        "Resources" => dictionary! { "Font" => fonts },
        "Contents" => Object::Reference(content_id),
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
        "Title" => literal(title.as_bytes()),
        "Subject" => literal(subject),
        "Producer" => literal(b"pdf-inspector examples/symbolic_font_fixtures.rs"),
    });
    doc.trailer.set("Root", Object::Reference(catalog));
    doc.trailer.set("Info", Object::Reference(info));
    doc.save(path)
        .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    println!("{}", path.display());
}

fn simple_font(subtype: &str, base_font: &str, encoding: Option<Object>) -> lopdf::Dictionary {
    let mut font = dictionary! {
        "Type" => "Font",
        "Subtype" => Object::Name(subtype.as_bytes().to_vec()),
        "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
    };
    if let Some(encoding) = encoding {
        font.set("Encoding", encoding);
    }
    font
}

/// Non-embedded Symbol and ZapfDingbats, read through their built-in
/// encodings; a Symbol font whose `/Differences` override one code; a
/// Symbol font whose `/Encoding` names a Latin encoding through an
/// indirect name object, which replaces the built-in one; and a Symbol
/// font that names its own built-in encoding outright.
fn symbol_builtin_encoding(dir: &Path) {
    let mut doc = Document::new();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    let win_ansi = doc.add_object(Object::Name(b"WinAnsiEncoding".to_vec()));
    let fonts = dictionary! {
        "F1" => simple_font("Type1", "Symbol", None),
        "F2" => simple_font("Type1", "ZapfDingbats", None),
        "F3" => simple_font("Type1", "Symbol", Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "Differences" => Object::Array(vec![Object::Integer(0x61), Object::Name(b"omega".to_vec())]),
        }))),
        "F4" => simple_font("Type1", "Symbol", Some(Object::Reference(win_ansi))),
        "F5" => simple_font("Type1", "Symbol", Some(Object::Name(b"SymbolEncoding".to_vec()))),
    };
    // Symbol: "abgd" are alpha, beta, gamma, delta; 0xE1/0xF1 the angle
    // brackets; 0xAE the right arrow. ZapfDingbats: "34" are two check
    // marks. F3 reads "a" through its Differences (omega) and "b" through
    // the built-in encoding (beta). F4 reads "abgd" as the Latin letters
    // its named encoding places at those codes; F5 reads them as Greek
    // through the built-in encoding it names.
    let content = "BT /F1 14 Tf 72 700 Td (abgd \\341\\361 \\256) Tj ET\n\
                   BT /F2 14 Tf 72 670 Td (34) Tj ET\n\
                   BT /F3 14 Tf 72 640 Td (ab) Tj ET\n\
                   BT /F4 14 Tf 72 610 Td (abgd) Tj ET\n\
                   BT /F5 14 Tf 72 580 Td (abgd) Tj ET\n"
        .to_string();
    finish_document(
        doc,
        &dir.join("symbol_builtin_encoding.pdf"),
        "Symbol and ZapfDingbats through their built-in encodings",
        b"Synthetic test fixture: standard fonts only.",
        fonts,
        content,
    );
}

/// Standard-14 text fonts whose encoding dictionary names a
/// `/BaseEncoding` and carries no `/Differences`: inline, as an indirect
/// object, MacRoman, and with `/Differences` on top.
fn base_encoding_without_differences(dir: &Path) {
    // The shared encoding dictionary is an indirect object of the document
    // the fonts are built for, so the fonts are built around it.
    let mut doc = Document::new();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    let shared_encoding = doc.add_object(dictionary! {
        "Type" => "Encoding",
        "BaseEncoding" => "WinAnsiEncoding",
    });
    let fonts = dictionary! {
        "F1" => simple_font("Type1", "Courier", Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
        }))),
        "F2" => simple_font("Type1", "Helvetica", Some(Object::Reference(shared_encoding))),
        "F3" => simple_font("Type1", "Times-Roman", Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "MacRomanEncoding",
        }))),
        "F4" => simple_font("Type1", "Courier", Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
            "Differences" => Object::Array(vec![Object::Integer(0x41), Object::Name(b"Alpha".to_vec())]),
        }))),
    };
    // "Año café señal über façade" in Windows-1252, and "Año café" in Mac
    // Roman (0x8E é, 0x96 ñ).
    let cp1252 = b"A\xf1o caf\xe9 se\xf1al \xfcber fa\xe7ade";
    let mac_roman = b"A\x96o caf\x8e";
    let escape = |bytes: &[u8]| -> String {
        bytes
            .iter()
            .map(|&b| {
                if b.is_ascii_alphanumeric() || b == b' ' {
                    (b as char).to_string()
                } else {
                    format!("\\{b:03o}")
                }
            })
            .collect()
    };
    let content = format!(
        "BT /F1 12 Tf 72 700 Td ({}) Tj ET\nBT /F2 12 Tf 72 680 Td ({}) Tj ET\n\
         BT /F3 12 Tf 72 660 Td ({}) Tj ET\nBT /F4 12 Tf 72 640 Td ({}) Tj ET\n",
        escape(cp1252),
        escape(cp1252),
        escape(mac_roman),
        escape(cp1252)
    );
    finish_document(
        doc,
        &dir.join("base_encoding_without_differences.pdf"),
        "Encoding dictionaries with a BaseEncoding and no Differences",
        b"Synthetic test fixture: standard fonts only.",
        fonts,
        content,
    );
}

/// Three embedded TrueType programs without ToUnicode: a symbolic one whose
/// (3,0) cmap maps the codes to glyphs named `uniXXXX` and by Adobe Glyph
/// List names, and one addressed by glyph index through `/Differences`
/// names of the `gNN` form, resolved through its (3,1) cmap.
fn glyph_names_in_embedded_fonts(dir: &Path) {
    let mut doc = Document::new();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;

    // F1: symbolic, codes 0x41..=0x44 through the (3,0) cmap at F041..F044,
    // glyphs named uni03B1 uni03B2 uni03B3 omega (α β γ ω).
    let symbolic = build_truetype(
        &[
            Glyph {
                name: Some(".notdef"),
                advance: 500,
                outlined: false,
                code: None,
            },
            Glyph {
                name: Some("uni03B1"),
                advance: 600,
                outlined: true,
                code: Some(0xF041),
            },
            Glyph {
                name: Some("uni03B2"),
                advance: 600,
                outlined: true,
                code: Some(0xF042),
            },
            Glyph {
                name: Some("uni03B3"),
                advance: 600,
                outlined: true,
                code: Some(0xF043),
            },
            Glyph {
                name: Some("omega"),
                advance: 600,
                outlined: true,
                code: Some(0xF044),
            },
            Glyph {
                name: Some("space"),
                advance: 300,
                outlined: false,
                code: Some(0xF020),
            },
        ],
        CmapKind::Symbol,
    );
    // F2: glyphs 1..=3 are δ ε ζ by the (3,1) cmap, unnamed (post format 3);
    // the font's /Differences name them by index.
    let indexed = build_truetype(
        &[
            Glyph {
                name: None,
                advance: 500,
                outlined: false,
                code: None,
            },
            Glyph {
                name: None,
                advance: 600,
                outlined: true,
                code: Some(0x03B4),
            },
            Glyph {
                name: None,
                advance: 600,
                outlined: true,
                code: Some(0x03B5),
            },
            Glyph {
                name: None,
                advance: 600,
                outlined: true,
                code: Some(0x03B6),
            },
        ],
        CmapKind::Unicode,
    );

    let mut embed = |program: Vec<u8>, name: &str, flags: i64, encoding: Option<Object>| {
        let file = doc.add_object(plain_stream(
            dictionary! { "Length1" => Object::Integer(program.len() as i64) },
            program,
        ));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => Object::Name(name.as_bytes().to_vec()),
            "Flags" => Object::Integer(flags),
            "FontBBox" => Object::Array(vec![0.into(), 0.into(), 550.into(), 700.into()]),
            "ItalicAngle" => Object::Integer(0),
            "Ascent" => Object::Integer(800),
            "Descent" => Object::Integer(-200),
            "CapHeight" => Object::Integer(700),
            "StemV" => Object::Integer(80),
            "FontFile2" => Object::Reference(file),
        });
        let mut font = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => Object::Name(name.as_bytes().to_vec()),
            "FirstChar" => Object::Integer(0x20),
            "LastChar" => Object::Integer(0x44),
            "Widths" => Object::Array(
                (0x20..=0x44).map(|c| Object::Integer(if c == 0x20 { 300 } else { 600 })).collect()
            ),
            "FontDescriptor" => Object::Reference(descriptor),
        };
        if let Some(encoding) = encoding {
            font.set("Encoding", encoding);
        }
        Object::Reference(doc.add_object(font))
    };
    let f1 = embed(symbolic, "SyntheticSymbolic", 4, None);
    let f2 = embed(
        indexed,
        "SyntheticIndexed",
        32,
        Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "Differences" => Object::Array(vec![
                Object::Integer(0x41),
                Object::Name(b"g1".to_vec()),
                Object::Name(b"g2".to_vec()),
                Object::Name(b"glyph3".to_vec()),
            ]),
        })),
    );
    // F3: glyphs 1..=3 are δ ε ζ by the (3,1) cmap, and the program names
    // them — glyph 3 is named "g1", the way a subsetter names glyphs with
    // no regard to their index. The font's /Differences say "g1" and "g2":
    // the program's own name wins for the first (ζ), the second reads as
    // an index (ε).
    let named_by_program = build_truetype(
        &[
            Glyph {
                name: Some(".notdef"),
                advance: 500,
                outlined: false,
                code: None,
            },
            Glyph {
                name: Some("uni03B4"),
                advance: 600,
                outlined: true,
                code: Some(0x03B4),
            },
            Glyph {
                name: Some("uni03B5"),
                advance: 600,
                outlined: true,
                code: Some(0x03B5),
            },
            Glyph {
                name: Some("g1"),
                advance: 600,
                outlined: true,
                code: Some(0x03B6),
            },
        ],
        CmapKind::Unicode,
    );
    let f3 = embed(
        named_by_program,
        "SyntheticNamedByProgram",
        32,
        Some(Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "Differences" => Object::Array(vec![
                Object::Integer(0x41),
                Object::Name(b"g1".to_vec()),
                Object::Name(b"g2".to_vec()),
            ]),
        })),
    );
    let fonts = dictionary! { "F1" => f1, "F2" => f2, "F3" => f3 };
    let content = "BT /F1 14 Tf 72 700 Td (ABCD) Tj ET\nBT /F2 14 Tf 72 670 Td (ABC) Tj ET\n\
                   BT /F3 14 Tf 72 640 Td (AB) Tj ET\n"
        .to_string();
    finish_document(
        doc,
        &dir.join("glyph_names_in_embedded_fonts.pdf"),
        "Embedded fonts decoded through their glyph names",
        b"Synthetic test fixture: TrueType programs built for it, no ToUnicode.",
        fonts,
        content,
    );
}

/// An embedded TrueType program whose `/Differences` name ligature glyphs
/// the way the Adobe Glyph List Specification allows: by their components
/// joined with underscores (`f_t`, `f_f_i`, `T_h`, `t_z`), with a suffix
/// (`a.sc`, `f_i.liga`) and as a `uni` sequence (`uni00660069`). The
/// glyphs are squares; only the names carry meaning.
fn ligature_glyph_names(dir: &Path) {
    let mut doc = Document::new();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    let names = [
        "f_t",
        "f_f_i",
        "T_h",
        "a.sc",
        "uni00660069",
        "f_i.liga",
        "t_z",
    ];
    let mut glyphs = vec![Glyph {
        name: Some(".notdef"),
        advance: 500,
        outlined: false,
        code: None,
    }];
    glyphs.push(Glyph {
        name: Some("space"),
        advance: 300,
        outlined: false,
        code: Some(0x20),
    });
    for name in names {
        glyphs.push(Glyph {
            name: Some(name),
            advance: 900,
            outlined: true,
            code: None,
        });
    }
    let program = build_truetype(&glyphs, CmapKind::Unicode);
    let file = doc.add_object(plain_stream(
        dictionary! { "Length1" => Object::Integer(program.len() as i64) },
        program,
    ));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::Name(b"SyntheticLigatures".to_vec()),
        "Flags" => Object::Integer(32),
        "FontBBox" => Object::Array(vec![0.into(), 0.into(), 550.into(), 700.into()]),
        "ItalicAngle" => Object::Integer(0),
        "Ascent" => Object::Integer(800),
        "Descent" => Object::Integer(-200),
        "CapHeight" => Object::Integer(700),
        "StemV" => Object::Integer(80),
        "FontFile2" => Object::Reference(file),
    });
    // Codes 0x41.. name the ligature glyphs; 0x20 is the space.
    let mut differences = vec![Object::Integer(0x41)];
    differences.extend(
        names
            .iter()
            .map(|name| Object::Name(name.as_bytes().to_vec())),
    );
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => Object::Name(b"SyntheticLigatures".to_vec()),
        "FirstChar" => Object::Integer(0x20),
        "LastChar" => Object::Integer(0x47),
        "Widths" => Object::Array(
            (0x20..=0x47).map(|c| Object::Integer(if c == 0x20 { 300 } else if c >= 0x41 { 900 } else { 0 })).collect()
        ),
        "FontDescriptor" => Object::Reference(descriptor),
        "Encoding" => Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "Differences" => Object::Array(differences),
        }),
    });
    let fonts = dictionary! { "F1" => Object::Reference(font) };
    let content = "BT /F1 14 Tf 72 700 Td (A B C D E F G) Tj ET\n".to_string();
    finish_document(
        doc,
        &dir.join("ligature_glyph_names.pdf"),
        "Ligature glyphs named by their components",
        b"Synthetic test fixture: one TrueType program built for it, no ToUnicode.",
        fonts,
        content,
    );
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    symbol_builtin_encoding(&dir);
    base_encoding_without_differences(&dir);
    glyph_names_in_embedded_fonts(&dir);
    ligature_glyph_names(&dir);
}
