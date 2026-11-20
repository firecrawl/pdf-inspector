//! Glyphs as the formula reader sees them: decoded character, font class,
//! and a box that is tight around the ink row of the glyph's font size.

use crate::extractor::math_glyphs::MathGlyph;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontClass {
    Roman,
    Italic,
    Bold,
    Symbol,
    Blackboard,
    Fraktur,
    Script,
    Extension,
}

/// A glyph or a finished atom (`tex` set). `y` grows downwards.
#[derive(Debug, Clone)]
pub(crate) struct G {
    pub(crate) c: String,
    pub(crate) x0: f32,
    pub(crate) y0: f32,
    pub(crate) x1: f32,
    pub(crate) y1: f32,
    pub(crate) base: f32,
    pub(crate) size: f32,
    pub(crate) class: FontClass,
    pub(crate) tex: Option<String>,
}

impl G {
    pub(crate) fn xc(&self) -> f32 {
        (self.x0 + self.x1) / 2.0
    }

    pub(crate) fn yc(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

/// The font name without its subset tag, lower-cased.
pub(crate) fn font_key(font: &str) -> String {
    font.rsplit('+').next().unwrap_or(font).to_ascii_lowercase()
}

/// TeX's extension fonts: large operators and delimiters whose glyphs hang
/// below the baseline, read by their raw codes.
pub(crate) fn is_extension(key: &str) -> bool {
    [
        "mathextension",
        "cmex",
        "lmex",
        "mathex",
        "txex",
        "pxex",
        "esint",
    ]
    .iter()
    .any(|n| key.contains(n))
}

fn is_computer_modern(key: &str) -> bool {
    ["cm", "lm", "ec", "msam", "msbm", "eufm", "rsfs"]
        .iter()
        .any(|p| key.starts_with(p))
        || key.contains("latinmodern")
}

/// Fonts that set mathematics: the TeX families and the math faces of the
/// common OpenType and Type 1 math sets. A text layer that an OCR engine
/// wrote, or a page set in one text face, has few of these.
pub(crate) fn is_math_font(key: &str) -> bool {
    const PREFIXES: [&str; 24] = [
        "cm", "lm", "ec", "msam", "msbm", "eufm", "eusm", "eurm", "eufb", "rsfs", "stix", "txmi",
        "txsy", "txex", "pxmi", "pxsy", "pxex", "ntx", "rtx", "rpx", "zx", "mt", "bbold",
        "mnsymbol",
    ];
    const PARTS: [&str; 15] = [
        "math",
        "latinmodern",
        "cmmi",
        "cmsy",
        "cmex",
        "cmr",
        "symbol",
        "euler",
        "esint",
        "newtx",
        "newpx",
        "dsrom",
        "xits",
        "asana",
        "kp--m",
    ];
    PREFIXES.iter().any(|p| key.starts_with(p)) || PARTS.iter().any(|p| key.contains(p))
}

fn font_class(key: &str) -> FontClass {
    if is_extension(key) {
        return FontClass::Extension;
    }
    let has = |parts: &[&str]| parts.iter().any(|p| key.contains(p));
    if has(&["msbm", "bbold", "dsrom"]) {
        FontClass::Blackboard
    } else if has(&["eufm", "frak"]) {
        FontClass::Fraktur
    } else if has(&["rsfs", "script"]) {
        FontClass::Script
    } else if key.starts_with("cmsy") || has(&["cmsy", "mathsymbols", "lmsy"]) {
        FontClass::Symbol
    } else if has(&["cmbx", "cmmib", "bold", "cmbsy"]) {
        FontClass::Bold
    } else if has(&["cmmi", "ital", "cmti"]) {
        FontClass::Italic
    } else {
        FontClass::Roman
    }
}

/// The character each code of TeX's extension font (cmex) draws.
fn cmex(code: u32) -> Option<char> {
    const ROWS: [(u32, &str); 6] = [
        (0x00, "()[]⌊⌋⌈⌉{}⟨⟩|‖/\\"),
        (0x10, "()()[]⌊⌋⌈⌉{}⟨⟩/\\"),
        (0x20, "()[]⌊⌋⌈⌉{}⟨⟩/\\/\\"),
        (0x30, "()[][][]{}{}{}{|"),
        (0x40, "()()⟨⟩⊔⊔∮∮⊙⊙⊕⊕⊗⊗"),
        (0x50, "∑∏∫⋃⋂⊎⋀⋁∑∏∫⋃⋂⊎⋀⋁"),
    ];
    if let Some((start, row)) = ROWS
        .iter()
        .find(|(start, _)| (*start..*start + 16).contains(&code))
    {
        return row.chars().nth((code - start) as usize);
    }
    Some(match code {
        0x60 | 0x61 => '∐',
        0x62..=0x64 => '^',
        0x65..=0x67 => '~',
        0x68 => '[',
        0x69 => ']',
        0x6A => '⌊',
        0x6B => '⌋',
        0x6C => '⌈',
        0x6D => '⌉',
        0x6E => '{',
        0x6F => '}',
        0x70..=0x77 => '√',
        0x78 => '↑',
        0x79 => '↓',
        0x7E => '⇑',
        0x7F => '⇓',
        _ => return None,
    })
}

/// How far an extension glyph reaches above and below its origin, in ems:
/// the origin sits at the top of most of them.
fn cmex_extent(code: u32) -> (f32, f32) {
    let depth = match code {
        0x00..=0x0F => 1.2,
        0x10 | 0x11 | 0x44 | 0x45 => 1.8,
        0x12..=0x1F | 0x68..=0x6F | 0x72 => 2.4,
        0x20..=0x2F | 0x73 => 3.0,
        0x30 | 0x31 | 0x3C | 0x3D | 0x40..=0x43 | 0x71 | 0x74 => 1.8,
        0x38..=0x3B => 0.9,
        0x3E => 0.3,
        0x48 | 0x52 => 1.11,
        0x49 | 0x5A => 2.22,
        0x46..=0x4F | 0x50..=0x57 | 0x60 => {
            if code >= 0x50 || code.is_multiple_of(2) {
                1.0
            } else {
                1.4
            }
        }
        0x58..=0x5F | 0x61 => 1.4,
        0x62..=0x67 => return (0.75, 0.0),
        0x70 => 1.2,
        _ => 0.6,
    };
    (0.04, depth - 0.04)
}

/// The character a code of TeX's math italic (OML), math symbol (OMS) or
/// AMS symbol encoding draws, for fonts whose subset leaves the glyph
/// unnamed.
fn tex_encoding(key: &str, code: u32) -> Option<char> {
    const OML: &str = "ΓΔΘΛΞΠΣΥΦΨΩαβγδεζηθικλμνξπρστυφχψωϵϑϖϱςϕ↼↽⇀⇁";
    const OMS: &str =
        "−·×∗÷⋄±∓⊕⊖⊗⊘⊙◯∘•≍≡⊆⊇≤≥⪯⪰∼≈⊂⊃≪≫≺≻←→↑↓↔↗↘≃⇐⇒⇑⇓⇔↖↙∝′∞∈∋△▽\u{338}\u{338}∀∃¬∅ℜℑ⊤⊥";
    const MSAM: &str = "⊡⊞⊠□■⋅◊";
    let has = |names: &[&str]| names.iter().any(|n| key.contains(n));
    let table = if has(&["cmmi", "lmmi", "txmi", "pxmi", "rmtmi", "mathitalic"]) {
        OML
    } else if has(&["cmsy", "lmsy", "txsy", "pxsy", "mtsy", "mathsymbols"]) {
        OMS
    } else if key.contains("msam") {
        MSAM
    } else {
        return None;
    };
    table.chars().nth(code as usize)
}

fn is_unmapped(c: char) -> bool {
    c == '\u{FFFD}' || ('\u{E000}'..='\u{F8FF}').contains(&c) || c.is_control()
}

/// The reader's view of a painted glyph, or `None` when it paints only
/// space. The second value says the glyph decoded to nothing usable.
pub(crate) fn from_math_glyph(m: &MathGlyph) -> (Option<G>, bool) {
    let key = font_key(&m.font);
    let class = font_class(&key);
    let ex = class == FontClass::Extension;
    let mut text = m.text.clone();
    if text.is_empty() || text.chars().any(is_unmapped) {
        if let Some(c) = tex_encoding(&key, m.code) {
            text = c.to_string();
        }
    }
    if ex && m.code < 0x80 {
        if let Some(c) = cmex(m.code) {
            text = c.to_string();
        }
    } else if !is_computer_modern(&key) {
        text = text
            .chars()
            .map(|c| match c {
                'φ' => 'ϕ',
                'ϕ' => 'φ',
                'ε' => 'ϵ',
                'ϵ' => 'ε',
                c => c,
            })
            .collect();
    }
    if text.trim().is_empty() {
        return (None, text.is_empty());
    }
    if text.chars().any(is_unmapped) {
        return (None, true);
    }
    let (up, down) = if ex && m.code < 0x80 {
        cmex_extent(m.code)
    } else {
        (0.72, 0.22)
    };
    (
        Some(G {
            c: text,
            x0: m.x0,
            y0: m.base - up * m.size,
            x1: m.x1,
            y1: m.base + down * m.size,
            base: m.base,
            size: m.size,
            class,
            tex: None,
        }),
        false,
    )
}
