//! Adobe Glyph List mapping from glyph names to Unicode
//! This is a subset of the most common glyph names

use std::collections::HashMap;
use std::sync::LazyLock;

/// Maps Adobe glyph names to Unicode code points
pub static GLYPH_TO_UNICODE: LazyLock<HashMap<&'static str, char>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // Basic Latin
    m.insert("space", ' ');
    m.insert("exclam", '!');
    m.insert("quotedbl", '"');
    m.insert("numbersign", '#');
    m.insert("dollar", '$');
    m.insert("percent", '%');
    m.insert("ampersand", '&');
    m.insert("quotesingle", '\'');
    m.insert("quoteright", '\u{2019}');
    m.insert("parenleft", '(');
    m.insert("parenright", ')');
    m.insert("asterisk", '*');
    m.insert("plus", '+');
    m.insert("comma", ',');
    m.insert("hyphen", '-');
    m.insert("period", '.');
    m.insert("slash", '/');
    m.insert("zero", '0');
    m.insert("one", '1');
    m.insert("two", '2');
    m.insert("three", '3');
    m.insert("four", '4');
    m.insert("five", '5');
    m.insert("six", '6');
    m.insert("seven", '7');
    m.insert("eight", '8');
    m.insert("nine", '9');
    m.insert("colon", ':');
    m.insert("semicolon", ';');
    m.insert("less", '<');
    m.insert("equal", '=');
    m.insert("greater", '>');
    m.insert("question", '?');
    m.insert("at", '@');

    // Uppercase letters
    m.insert("A", 'A');
    m.insert("B", 'B');
    m.insert("C", 'C');
    m.insert("D", 'D');
    m.insert("E", 'E');
    m.insert("F", 'F');
    m.insert("G", 'G');
    m.insert("H", 'H');
    m.insert("I", 'I');
    m.insert("J", 'J');
    m.insert("K", 'K');
    m.insert("L", 'L');
    m.insert("M", 'M');
    m.insert("N", 'N');
    m.insert("O", 'O');
    m.insert("P", 'P');
    m.insert("Q", 'Q');
    m.insert("R", 'R');
    m.insert("S", 'S');
    m.insert("T", 'T');
    m.insert("U", 'U');
    m.insert("V", 'V');
    m.insert("W", 'W');
    m.insert("X", 'X');
    m.insert("Y", 'Y');
    m.insert("Z", 'Z');

    m.insert("bracketleft", '[');
    m.insert("backslash", '\\');
    m.insert("bracketright", ']');
    m.insert("asciicircum", '^');
    m.insert("underscore", '_');
    m.insert("grave", '`');
    m.insert("quoteleft", '\u{2018}');

    // Lowercase letters
    m.insert("a", 'a');
    m.insert("b", 'b');
    m.insert("c", 'c');
    m.insert("d", 'd');
    m.insert("e", 'e');
    m.insert("f", 'f');
    m.insert("g", 'g');
    m.insert("h", 'h');
    m.insert("i", 'i');
    m.insert("j", 'j');
    m.insert("k", 'k');
    m.insert("l", 'l');
    m.insert("m", 'm');
    m.insert("n", 'n');
    m.insert("o", 'o');
    m.insert("p", 'p');
    m.insert("q", 'q');
    m.insert("r", 'r');
    m.insert("s", 's');
    m.insert("t", 't');
    m.insert("u", 'u');
    m.insert("v", 'v');
    m.insert("w", 'w');
    m.insert("x", 'x');
    m.insert("y", 'y');
    m.insert("z", 'z');

    m.insert("braceleft", '{');
    m.insert("bar", '|');
    m.insert("braceright", '}');
    m.insert("asciitilde", '~');

    // Extended Latin and punctuation
    m.insert("exclamdown", '¡');
    m.insert("cent", '¢');
    m.insert("sterling", '£');
    m.insert("currency", '¤');
    m.insert("yen", '¥');
    m.insert("brokenbar", '¦');
    m.insert("section", '§');
    m.insert("dieresis", '¨');
    m.insert("copyright", '©');
    m.insert("ordfeminine", 'ª');
    m.insert("guillemotleft", '«');
    m.insert("logicalnot", '¬');
    m.insert("registered", '®');
    m.insert("macron", '¯');
    m.insert("degree", '°');
    m.insert("plusminus", '±');
    m.insert("twosuperior", '²');
    m.insert("threesuperior", '³');
    m.insert("acute", '´');
    m.insert("mu", 'µ');
    m.insert("paragraph", '¶');
    m.insert("periodcentered", '·');
    m.insert("cedilla", '¸');
    m.insert("onesuperior", '¹');
    m.insert("ordmasculine", 'º');
    m.insert("guillemotright", '»');
    m.insert("onequarter", '¼');
    m.insert("onehalf", '½');
    m.insert("threequarters", '¾');
    m.insert("questiondown", '¿');

    // Accented capitals
    m.insert("Agrave", 'À');
    m.insert("Aacute", 'Á');
    m.insert("Acircumflex", 'Â');
    m.insert("Atilde", 'Ã');
    m.insert("Adieresis", 'Ä');
    m.insert("Aring", 'Å');
    m.insert("AE", 'Æ');
    m.insert("Ccedilla", 'Ç');
    m.insert("Egrave", 'È');
    m.insert("Eacute", 'É');
    m.insert("Ecircumflex", 'Ê');
    m.insert("Edieresis", 'Ë');
    m.insert("Igrave", 'Ì');
    m.insert("Iacute", 'Í');
    m.insert("Icircumflex", 'Î');
    m.insert("Idieresis", 'Ï');
    m.insert("Eth", 'Ð');
    m.insert("Ntilde", 'Ñ');
    m.insert("Ograve", 'Ò');
    m.insert("Oacute", 'Ó');
    m.insert("Ocircumflex", 'Ô');
    m.insert("Otilde", 'Õ');
    m.insert("Odieresis", 'Ö');
    m.insert("multiply", '×');
    m.insert("Oslash", 'Ø');
    m.insert("Ugrave", 'Ù');
    m.insert("Uacute", 'Ú');
    m.insert("Ucircumflex", 'Û');
    m.insert("Udieresis", 'Ü');
    m.insert("Yacute", 'Ý');
    m.insert("Thorn", 'Þ');
    m.insert("germandbls", 'ß');

    // Accented lowercase
    m.insert("agrave", 'à');
    m.insert("aacute", 'á');
    m.insert("acircumflex", 'â');
    m.insert("atilde", 'ã');
    m.insert("adieresis", 'ä');
    m.insert("aring", 'å');
    m.insert("ae", 'æ');
    m.insert("ccedilla", 'ç');
    m.insert("egrave", 'è');
    m.insert("eacute", 'é');
    m.insert("ecircumflex", 'ê');
    m.insert("edieresis", 'ë');
    m.insert("igrave", 'ì');
    m.insert("iacute", 'í');
    m.insert("icircumflex", 'î');
    m.insert("idieresis", 'ï');
    m.insert("eth", 'ð');
    m.insert("ntilde", 'ñ');
    m.insert("ograve", 'ò');
    m.insert("oacute", 'ó');
    m.insert("ocircumflex", 'ô');
    m.insert("otilde", 'õ');
    m.insert("odieresis", 'ö');
    m.insert("divide", '÷');
    m.insert("oslash", 'ø');
    m.insert("ugrave", 'ù');
    m.insert("uacute", 'ú');
    m.insert("ucircumflex", 'û');
    m.insert("udieresis", 'ü');
    m.insert("yacute", 'ý');
    m.insert("thorn", 'þ');
    m.insert("ydieresis", 'ÿ');

    // Ligatures and special (Unicode ligature characters)
    m.insert("fi", '\u{FB01}'); // ﬁ
    m.insert("fl", '\u{FB02}'); // ﬂ
    m.insert("ff", '\u{FB00}'); // ﬀ
    m.insert("ffi", '\u{FB03}'); // ﬃ
    m.insert("ffl", '\u{FB04}'); // ﬄ
                                 // Alternative naming with underscores (used by some PDF producers)
    m.insert("f_i", '\u{FB01}'); // ﬁ
    m.insert("f_l", '\u{FB02}'); // ﬂ
    m.insert("f_f", '\u{FB00}'); // ﬀ
    m.insert("f_f_i", '\u{FB03}'); // ﬃ
    m.insert("f_f_l", '\u{FB04}'); // ﬄ

    // Quotes and dashes
    m.insert("endash", '–');
    m.insert("emdash", '—');
    m.insert("quotedblleft", '"');
    m.insert("quotedblright", '"');
    m.insert("quoteleft", '\u{2018}');
    m.insert("quoteright", '\u{2019}');
    m.insert("quotesinglbase", '‚');
    m.insert("quotedblbase", '„');
    m.insert("dagger", '†');
    m.insert("daggerdbl", '‡');
    m.insert("bullet", '•');
    m.insert("ellipsis", '…');
    m.insert("perthousand", '‰');
    m.insert("guilsinglleft", '‹');
    m.insert("guilsinglright", '›');
    m.insert("fraction", '⁄');
    m.insert("trademark", '™');
    m.insert("minus", '−');

    // Math symbols
    m.insert("infinity", '∞');
    m.insert("notequal", '≠');
    m.insert("lessequal", '≤');
    m.insert("greaterequal", '≥');
    m.insert("partialdiff", '∂');
    m.insert("summation", '∑');
    m.insert("product", '∏');
    m.insert("radical", '√');
    m.insert("approxequal", '≈');
    m.insert("Delta", 'Δ');
    m.insert("lozenge", '◊');

    // Greek letters (common ones)
    m.insert("Alpha", 'Α');
    m.insert("Beta", 'Β');
    m.insert("Gamma", 'Γ');
    m.insert("Epsilon", 'Ε');
    m.insert("Zeta", 'Ζ');
    m.insert("Eta", 'Η');
    m.insert("Theta", 'Θ');
    m.insert("Iota", 'Ι');
    m.insert("Kappa", 'Κ');
    m.insert("Lambda", 'Λ');
    m.insert("Mu", 'Μ');
    m.insert("Nu", 'Ν');
    m.insert("Xi", 'Ξ');
    m.insert("Omicron", 'Ο');
    m.insert("Pi", 'Π');
    m.insert("Rho", 'Ρ');
    m.insert("Sigma", 'Σ');
    m.insert("Tau", 'Τ');
    m.insert("Upsilon", 'Υ');
    m.insert("Phi", 'Φ');
    m.insert("Chi", 'Χ');
    m.insert("Psi", 'Ψ');
    m.insert("Omega", 'Ω');
    m.insert("alpha", 'α');
    m.insert("beta", 'β');
    m.insert("gamma", 'γ');
    m.insert("delta", 'δ');
    m.insert("epsilon", 'ε');
    m.insert("zeta", 'ζ');
    m.insert("eta", 'η');
    m.insert("theta", 'θ');
    m.insert("iota", 'ι');
    m.insert("kappa", 'κ');
    m.insert("lambda", 'λ');
    m.insert("nu", 'ν');
    m.insert("xi", 'ξ');
    m.insert("omicron", 'ο');
    m.insert("pi", 'π');
    m.insert("rho", 'ρ');
    m.insert("sigma", 'σ');
    m.insert("tau", 'τ');
    m.insert("upsilon", 'υ');
    m.insert("phi", 'φ');
    m.insert("chi", 'χ');
    m.insert("psi", 'ψ');
    m.insert("omega", 'ω');

    m
});

/// Convert a glyph name to its Unicode character
pub fn glyph_to_char(name: &str) -> Option<char> {
    // First check our mapping
    if let Some(&c) = GLYPH_TO_UNICODE.get(name) {
        return Some(c);
    }

    // Try to parse uniXXXX format
    if name.starts_with("uni") && name.len() >= 7 {
        if let Ok(code) = u32::from_str_radix(&name[3..7], 16) {
            return char::from_u32(code);
        }
    }

    // Try to parse uXXXX or uXXXXX format
    if name.starts_with('u') && name.len() >= 5 {
        if let Ok(code) = u32::from_str_radix(&name[1..], 16) {
            return char::from_u32(code);
        }
    }

    None
}
