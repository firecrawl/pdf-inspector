//! Standard Macintosh glyph order as a last-resort CID→Unicode mapping.
//!
//! A TrueType subset embedded for an Identity-H CIDFont sometimes keeps
//! neither a `cmap` table nor glyph names, and the PDF carries no
//! ToUnicode. The glyph IDs are then the only handle on the text. The core
//! Windows and Macintosh fonts (Arial, Times New Roman, Helvetica, ...) place
//! the 258 glyphs of the standard Macintosh ordering first, so for those
//! fonts the glyph ID alone still names the character. The order is only
//! assumed when the font's own metrics corroborate it.

use crate::glyph_names::glyph_to_char;
use crate::tounicode::ToUnicodeCMap;
use lopdf::{Document, Object};

/// The 258 glyph names of the standard Macintosh ordering (`post` format 1;
/// TrueType Reference Manual, "The 'post' table"). The core Windows and
/// Macintosh fonts (Arial, Times New Roman, Helvetica, ...) place these
/// glyphs first in this order, so a subset that dropped its `cmap` and its
/// glyph names still exposes them through the glyph ID alone.
const MAC_GLYPH_NAMES: [&str; 258] = [
    ".notdef",
    ".null",
    "nonmarkingreturn",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "Adieresis",
    "Aring",
    "Ccedilla",
    "Eacute",
    "Ntilde",
    "Odieresis",
    "Udieresis",
    "aacute",
    "agrave",
    "acircumflex",
    "adieresis",
    "atilde",
    "aring",
    "ccedilla",
    "eacute",
    "egrave",
    "ecircumflex",
    "edieresis",
    "iacute",
    "igrave",
    "icircumflex",
    "idieresis",
    "ntilde",
    "oacute",
    "ograve",
    "ocircumflex",
    "odieresis",
    "otilde",
    "uacute",
    "ugrave",
    "ucircumflex",
    "udieresis",
    "dagger",
    "degree",
    "cent",
    "sterling",
    "section",
    "bullet",
    "paragraph",
    "germandbls",
    "registered",
    "copyright",
    "trademark",
    "acute",
    "dieresis",
    "notequal",
    "AE",
    "Oslash",
    "infinity",
    "plusminus",
    "lessequal",
    "greaterequal",
    "yen",
    "mu",
    "partialdiff",
    "summation",
    "product",
    "pi",
    "integral",
    "ordfeminine",
    "ordmasculine",
    "Omega",
    "ae",
    "oslash",
    "questiondown",
    "exclamdown",
    "logicalnot",
    "radical",
    "florin",
    "approxequal",
    "Delta",
    "guillemotleft",
    "guillemotright",
    "ellipsis",
    "nonbreakingspace",
    "Agrave",
    "Atilde",
    "Otilde",
    "OE",
    "oe",
    "endash",
    "emdash",
    "quotedblleft",
    "quotedblright",
    "quoteleft",
    "quoteright",
    "divide",
    "lozenge",
    "ydieresis",
    "Ydieresis",
    "fraction",
    "currency",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "daggerdbl",
    "periodcentered",
    "quotesinglbase",
    "quotedblbase",
    "perthousand",
    "Acircumflex",
    "Ecircumflex",
    "Aacute",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Oacute",
    "Ocircumflex",
    "apple",
    "Ograve",
    "Uacute",
    "Ucircumflex",
    "Ugrave",
    "dotlessi",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "Lslash",
    "lslash",
    "Scaron",
    "scaron",
    "Zcaron",
    "zcaron",
    "brokenbar",
    "Eth",
    "eth",
    "Yacute",
    "yacute",
    "Thorn",
    "thorn",
    "minus",
    "multiply",
    "onesuperior",
    "twosuperior",
    "threesuperior",
    "onehalf",
    "onequarter",
    "threequarters",
    "franc",
    "Gbreve",
    "gbreve",
    "Idotaccent",
    "Scedilla",
    "scedilla",
    "Cacute",
    "cacute",
    "Ccaron",
    "ccaron",
    "dcroat",
];

/// Whether the CIDFont maps CIDs to glyph IDs without a table, so the CIDs
/// in the content stream are the embedded font's glyph IDs.
pub(crate) fn cid_to_gid_is_identity(cid_font_dict: &lopdf::Dictionary, doc: &Document) -> bool {
    match cid_font_dict.get(b"CIDToGIDMap") {
        Err(_) => true,
        Ok(Object::Name(name)) => name == b"Identity",
        Ok(Object::Reference(r)) => {
            matches!(doc.get_object(*r), Ok(Object::Name(name)) if name == b"Identity")
        }
        Ok(_) => false,
    }
}

/// Build a GID→Unicode CMap for a TrueType font that has neither a `cmap`
/// table nor glyph names, assuming the standard Macintosh glyph order.
///
/// The assumption is only accepted when the font's own metrics corroborate
/// it: at least three glyphs at the digit positions (19–28) exist and share
/// one advance, as tabular figures do; the glyph at the space position (3)
/// advances without an outline; `i` and `l` are narrower than `m` and `w`;
/// and capitals average wider than lowercase. Each check only applies to
/// glyphs the subset kept. Everything else keeps today's behaviour.
pub(crate) fn build_cmap_from_mac_glyph_order(font_data: &[u8]) -> Option<ToUnicodeCMap> {
    use ttf_parser::GlyphId;

    let face = ttf_parser::Face::parse(font_data, 0).ok()?;
    if face.tables().cmap.is_some() || face.tables().glyf.is_none() {
        return None;
    }
    if (0..face.number_of_glyphs()).any(|gid| face.glyph_name(GlyphId(gid)).is_some()) {
        return None;
    }
    let present = |gid: u16| face.glyph_bounding_box(GlyphId(gid)).is_some();
    let advance = |gid: u16| {
        present(gid)
            .then(|| face.glyph_hor_advance(GlyphId(gid)))
            .flatten()
    };
    // Tabular figures: the digits present share one advance.
    let digits: Vec<u16> = (19u16..=28).filter_map(advance).collect();
    if digits.len() < 3 {
        return None;
    }
    let (min, max) = digits
        .iter()
        .fold((u16::MAX, 0u16), |(lo, hi), &w| (lo.min(w), hi.max(w)));
    if min == 0 || u32::from(max - min) * 100 > u32::from(max) * 2 {
        return None;
    }
    // The space advances without an outline.
    if face.number_of_glyphs() > 3
        && (present(3) || face.glyph_hor_advance(GlyphId(3)).is_none_or(|w| w == 0))
    {
        return None;
    }
    // Letter proportions: `i` and `l` are narrower than `m` and `w`, and
    // capitals are wider than lowercase on average. A random glyph order
    // fails these as often as it passes them.
    // A subset too sparse to run any of them is left alone.
    let mut letter_checks = 0usize;
    let narrow = [advance(76), advance(79)]; // i l
    let wide = [advance(80), advance(90)]; // m w
    for (n, w) in narrow.iter().zip(wide) {
        if let (Some(n), Some(w)) = (n, w) {
            letter_checks += 1;
            if *n >= w {
                return None;
            }
        }
    }
    let mean = |range: std::ops::RangeInclusive<u16>| {
        let widths: Vec<u32> = range.filter_map(advance).map(u32::from).collect();
        (widths.len() >= 3).then(|| widths.iter().sum::<u32>() / widths.len() as u32)
    };
    if let (Some(upper), Some(lower)) = (mean(36..=61), mean(68..=93)) {
        letter_checks += 1;
        if upper <= lower {
            return None;
        }
    }
    if letter_checks == 0 {
        return None;
    }

    // Only slots the subset kept: an outline, or a blank glyph that still
    // advances (space, no-break space).
    let mut cmap = ToUnicodeCMap::new();
    let count = usize::from(face.number_of_glyphs()).min(MAC_GLYPH_NAMES.len());
    for (gid, name) in MAC_GLYPH_NAMES.iter().enumerate().take(count) {
        let gid = gid as u16;
        let kept = present(gid) || face.glyph_hor_advance(GlyphId(gid)).is_some_and(|w| w > 0);
        if !kept {
            continue;
        }
        if let Some(ch) = glyph_to_char(name) {
            cmap.char_map.insert(gid, ch.to_string());
        }
    }
    if cmap.char_map.is_empty() {
        return None;
    }
    cmap.code_byte_length = 2;
    Some(cmap)
}

#[cfg(test)]
#[path = "tounicode_mac_order_tests.rs"]
mod tests;
