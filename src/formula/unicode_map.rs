//! Unicode to LaTeX character mapping.
//!
//! Maps Unicode math symbols, Greek letters, operators, and relations to their
//! LaTeX command equivalents. Only includes characters that commonly appear in
//! PDF text extraction of mathematical formulas.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Returns a reference to the global Unicode → LaTeX mapping table.
pub fn unicode_to_latex_map() -> &'static HashMap<char, &'static str> {
    static MAP: OnceLock<HashMap<char, &'static str>> = OnceLock::new();
    MAP.get_or_init(build_map)
}

fn build_map() -> HashMap<char, &'static str> {
    let entries: &[(char, &str)] = &[
        // ── Greek lowercase ─────────────────────────────────────────
        ('\u{03B1}', r"\alpha"),
        ('\u{03B2}', r"\beta"),
        ('\u{03B3}', r"\gamma"),
        ('\u{03B4}', r"\delta"),
        ('\u{03B5}', r"\varepsilon"),
        ('\u{03F5}', r"\epsilon"),
        ('\u{03B6}', r"\zeta"),
        ('\u{03B7}', r"\eta"),
        ('\u{03B8}', r"\theta"),
        ('\u{03D1}', r"\vartheta"),
        ('\u{03B9}', r"\iota"),
        ('\u{03BA}', r"\kappa"),
        ('\u{03BB}', r"\lambda"),
        ('\u{03BC}', r"\mu"),
        ('\u{03BD}', r"\nu"),
        ('\u{03BE}', r"\xi"),
        ('\u{03C0}', r"\pi"),
        ('\u{03D6}', r"\varpi"),
        ('\u{03C1}', r"\rho"),
        ('\u{03C2}', r"\varsigma"),
        ('\u{03C3}', r"\sigma"),
        ('\u{03C4}', r"\tau"),
        ('\u{03C5}', r"\upsilon"),
        ('\u{03C6}', r"\varphi"),
        ('\u{03D5}', r"\phi"),
        ('\u{03C7}', r"\chi"),
        ('\u{03C8}', r"\psi"),
        ('\u{03C9}', r"\omega"),
        // ── Greek uppercase ─────────────────────────────────────────
        ('\u{0393}', r"\Gamma"),
        ('\u{0394}', r"\Delta"),
        ('\u{0398}', r"\Theta"),
        ('\u{039B}', r"\Lambda"),
        ('\u{039E}', r"\Xi"),
        ('\u{03A0}', r"\Pi"),
        ('\u{03A3}', r"\Sigma"),
        ('\u{03A5}', r"\Upsilon"),
        ('\u{03A6}', r"\Phi"),
        ('\u{03A8}', r"\Psi"),
        ('\u{03A9}', r"\Omega"),
        // ── Large operators ─────────────────────────────────────────
        ('\u{222B}', r"\int"),
        ('\u{222C}', r"\iint"),
        ('\u{222D}', r"\iiint"),
        ('\u{222E}', r"\oint"),
        ('\u{2211}', r"\sum"),
        ('\u{220F}', r"\prod"),
        ('\u{2210}', r"\coprod"),
        // ── Roots / radicals ────────────────────────────────────────
        ('\u{221A}', r"\sqrt"),
        // ── Calculus / differential ─────────────────────────────────
        ('\u{2202}', r"\partial"),
        ('\u{2207}', r"\nabla"),
        // ── Binary operators ────────────────────────────────────────
        ('\u{00B1}', r"\pm"),
        ('\u{2213}', r"\mp"),
        ('\u{00D7}', r"\times"),
        ('\u{00F7}', r"\div"),
        ('\u{2217}', r"\ast"),
        ('\u{22C6}', r"\star"),
        ('\u{00B7}', r"\cdot"),
        ('\u{2219}', r"\bullet"),
        ('\u{2218}', r"\circ"),
        ('\u{2020}', r"\dagger"),
        ('\u{2021}', r"\ddagger"),
        ('\u{2295}', r"\oplus"),
        ('\u{2297}', r"\otimes"),
        ('\u{2227}', r"\wedge"),
        ('\u{2228}', r"\vee"),
        ('\u{2229}', r"\cap"),
        ('\u{222A}', r"\cup"),
        // ── Relations ───────────────────────────────────────────────
        ('\u{2264}', r"\leq"),
        ('\u{2265}', r"\geq"),
        ('\u{2260}', r"\neq"),
        ('\u{2248}', r"\approx"),
        ('\u{223C}', r"\sim"),
        ('\u{2243}', r"\simeq"),
        ('\u{2261}', r"\equiv"),
        ('\u{226A}', r"\ll"),
        ('\u{226B}', r"\gg"),
        ('\u{221D}', r"\propto"),
        ('\u{2208}', r"\in"),
        ('\u{2209}', r"\notin"),
        ('\u{220B}', r"\ni"),
        ('\u{2282}', r"\subset"),
        ('\u{2283}', r"\supset"),
        ('\u{2286}', r"\subseteq"),
        ('\u{2287}', r"\supseteq"),
        ('\u{22A2}', r"\vdash"),
        ('\u{22A3}', r"\dashv"),
        ('\u{22A4}', r"\top"),
        ('\u{22A5}', r"\bot"),
        ('\u{2225}', r"\parallel"),
        ('\u{22A5}', r"\perp"),
        // ── Arrows ──────────────────────────────────────────────────
        ('\u{2190}', r"\leftarrow"),
        ('\u{2192}', r"\to"),
        ('\u{2191}', r"\uparrow"),
        ('\u{2193}', r"\downarrow"),
        ('\u{2194}', r"\leftrightarrow"),
        ('\u{21D0}', r"\Leftarrow"),
        ('\u{21D2}', r"\Rightarrow"),
        ('\u{21D4}', r"\Leftrightarrow"),
        ('\u{21A6}', r"\mapsto"),
        ('\u{2197}', r"\nearrow"),
        ('\u{2198}', r"\searrow"),
        // ── Miscellaneous symbols ───────────────────────────────────
        ('\u{221E}', r"\infty"),
        ('\u{2200}', r"\forall"),
        ('\u{2203}', r"\exists"),
        ('\u{2204}', r"\nexists"),
        ('\u{2205}', r"\emptyset"),
        ('\u{00AC}', r"\neg"),
        ('\u{00B0}', r"^\circ"),
        ('\u{2032}', r"'"),  // prime (common in physics: x')
        ('\u{2033}', r"''"), // double prime
        ('\u{210F}', r"\hbar"),
        ('\u{2113}', r"\ell"),
        ('\u{211C}', r"\Re"),
        ('\u{2111}', r"\Im"),
        ('\u{2118}', r"\wp"),
        ('\u{2135}', r"\aleph"),
        // ── Dots ────────────────────────────────────────────────────
        ('\u{22EF}', r"\cdots"),
        ('\u{22EE}', r"\vdots"),
        ('\u{22F1}', r"\ddots"),
        ('\u{2026}', r"\ldots"),
        // ── Delimiters / brackets ───────────────────────────────────
        ('\u{27E8}', r"\langle"),
        ('\u{27E9}', r"\rangle"),
        ('\u{2308}', r"\lceil"),
        ('\u{2309}', r"\rceil"),
        ('\u{230A}', r"\lfloor"),
        ('\u{230B}', r"\rfloor"),
        ('\u{2016}', r"\|"),
        // ── Accents / decorations (as standalone chars) ─────────────
        ('\u{0302}', r"\hat{}"),
        ('\u{0303}', r"\tilde{}"),
        ('\u{0304}', r"\bar{}"),
        ('\u{0307}', r"\dot{}"),
        ('\u{0308}', r"\ddot{}"),
        ('\u{20D7}', r"\vec{}"),
        // Hat/tilde as standalone characters (sometimes extracted separately)
        ('\u{02C6}', r"\hat{}"),
        ('\u{02DC}', r"\tilde{}"),
        // ── Subscript/superscript digits (Unicode) ──────────────────
        ('\u{2070}', "^{0}"),
        ('\u{00B9}', "^{1}"),
        ('\u{00B2}', "^{2}"),
        ('\u{00B3}', "^{3}"),
        ('\u{2074}', "^{4}"),
        ('\u{2075}', "^{5}"),
        ('\u{2076}', "^{6}"),
        ('\u{2077}', "^{7}"),
        ('\u{2078}', "^{8}"),
        ('\u{2079}', "^{9}"),
        ('\u{207A}', "^{+}"),
        ('\u{207B}', "^{-}"),
        ('\u{2080}', "_{0}"),
        ('\u{2081}', "_{1}"),
        ('\u{2082}', "_{2}"),
        ('\u{2083}', "_{3}"),
        ('\u{2084}', "_{4}"),
        ('\u{2085}', "_{5}"),
        ('\u{2086}', "_{6}"),
        ('\u{2087}', "_{7}"),
        ('\u{2088}', "_{8}"),
        ('\u{2089}', "_{9}"),
        ('\u{208A}', "_{+}"),
        ('\u{208B}', "_{-}"),
        // ── Math italic letters (sometimes used in PDF fonts) ───────
        // These map back to plain ASCII in LaTeX (math mode handles italics)
        // ── Codepoint variants of glyphs above ──────────────────────
        // TeX/legacy fonts frequently decode to the "wrong" sibling
        // codepoint: MICRO SIGN for mu, GREEK LUNATE EPSILON for epsilon,
        // RING OPERATOR vs MASCULINE ORDINAL. Map every sibling.
        ('\u{00B5}', r"\mu"), // MICRO SIGN
        ('\u{2212}', "-"),    // MINUS SIGN → ASCII hyphen-minus
        ('\u{2044}', "/"),    // FRACTION SLASH
        ('\u{2215}', "/"),    // DIVISION SLASH
        // ── Blackboard bold (double-struck) letters ─────────────────
        ('\u{2102}', r"\mathbb{C}"),
        ('\u{210D}', r"\mathbb{H}"),
        ('\u{2115}', r"\mathbb{N}"),
        ('\u{2119}', r"\mathbb{P}"),
        ('\u{211A}', r"\mathbb{Q}"),
        ('\u{211D}', r"\mathbb{R}"),
        ('\u{2124}', r"\mathbb{Z}"),
        ('\u{1D53C}', r"\mathbb{E}"),
        ('\u{1D53D}', r"\mathbb{F}"),
        // ── Script (calligraphic) letters with dedicated codepoints ──
        ('\u{2110}', r"\mathcal{I}"),
        ('\u{2112}', r"\mathcal{L}"),
        ('\u{2130}', r"\mathcal{E}"),
        ('\u{2131}', r"\mathcal{F}"),
        ('\u{2133}', r"\mathcal{M}"),
        ('\u{212C}', r"\mathcal{B}"),
        ('\u{210B}', r"\mathcal{H}"),
        ('\u{211B}', r"\mathcal{R}"),
        // ── Additional relations seen in TeX corpora ─────────────────
        ('\u{2245}', r"\cong"),
        ('\u{224A}', r"\approxeq"),
        ('\u{227A}', r"\prec"),
        ('\u{227B}', r"\succ"),
        ('\u{2AAF}', r"\preceq"),
        ('\u{2AB0}', r"\succeq"),
        ('\u{22C0}', r"\bigwedge"),
        ('\u{22C1}', r"\bigvee"),
        ('\u{22C2}', r"\bigcap"),
        ('\u{22C3}', r"\bigcup"),
        ('\u{2294}', r"\sqcup"),
        ('\u{2293}', r"\sqcap"),
        ('\u{2296}', r"\ominus"),
        ('\u{2298}', r"\oslash"),
        ('\u{2299}', r"\odot"),
        ('\u{22C4}', r"\diamond"),
        ('\u{25CB}', r"\bigcirc"),
        ('\u{2216}', r"\setminus"),
        ('\u{2234}', r"\therefore"),
        ('\u{2235}', r"\because"),
        ('\u{221F}', r"\angle"),
        ('\u{2220}', r"\angle"),
        ('\u{22C8}', r"\bowtie"),
        ('\u{228E}', r"\uplus"),
        ('\u{2201}', r"\complement"),
        ('\u{22D8}', r"\lll"),
        ('\u{22D9}', r"\ggg"),
        ('\u{227E}', r"\precsim"),
        ('\u{2272}', r"\lesssim"),
        ('\u{2273}', r"\gtrsim"),
        ('\u{2266}', r"\leqq"),
        ('\u{2267}', r"\geqq"),
        ('\u{2268}', r"\lneq"),
        ('\u{2269}', r"\gneq"),
        ('\u{228A}', r"\subsetneq"),
        ('\u{228B}', r"\supsetneq"),
        ('\u{21C0}', r"\rightharpoonup"),
        ('\u{21CC}', r"\rightleftharpoons"),
        ('\u{27F6}', r"\longrightarrow"),
        ('\u{27F5}', r"\longleftarrow"),
        ('\u{27F7}', r"\longleftrightarrow"),
        ('\u{27F9}', r"\Longrightarrow"),
        ('\u{21AA}', r"\hookrightarrow"),
        ('\u{21A9}', r"\hookleftarrow"),
        ('\u{21D1}', r"\Uparrow"),
        ('\u{21D3}', r"\Downarrow"),
        ('\u{22A8}', r"\models"),
        ('\u{22A9}', r"\Vdash"),
        ('\u{225C}', r"\triangleq"),
        ('\u{2250}', r"\doteq"),
        ('\u{226E}', r"\nless"),
        ('\u{226F}', r"\ngtr"),
        ('\u{2270}', r"\nleq"),
        ('\u{2271}', r"\ngeq"),
        ('\u{2288}', r"\nsubseteq"),
        ('\u{2289}', r"\nsupseteq"),
        ('\u{22E6}', r"\lnsim"),
        ('\u{25B3}', r"\triangle"),
        ('\u{25A1}', r"\square"),
        ('\u{25A0}', r"\blacksquare"),
        ('\u{2662}', r"\diamondsuit"),
        ('\u{22D5}', r"\#"),
    ];

    let mut map = HashMap::with_capacity(entries.len());
    for &(ch, latex) in entries {
        map.insert(ch, latex);
    }
    map
}

/// Convert a single character to its LaTeX representation.
///
/// Returns `Some(latex_str)` if the character has a known mapping,
/// or `None` if it should be kept as-is.
pub fn char_to_latex(ch: char) -> Option<&'static str> {
    unicode_to_latex_map().get(&ch).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greek_lowercase() {
        assert_eq!(char_to_latex('\u{03B1}'), Some(r"\alpha"));
        assert_eq!(char_to_latex('\u{03B2}'), Some(r"\beta"));
        assert_eq!(char_to_latex('\u{03B3}'), Some(r"\gamma"));
        assert_eq!(char_to_latex('\u{03B4}'), Some(r"\delta"));
        assert_eq!(char_to_latex('\u{03B5}'), Some(r"\varepsilon"));
        assert_eq!(char_to_latex('\u{03B6}'), Some(r"\zeta"));
        assert_eq!(char_to_latex('\u{03B7}'), Some(r"\eta"));
        assert_eq!(char_to_latex('\u{03B8}'), Some(r"\theta"));
        assert_eq!(char_to_latex('\u{03D1}'), Some(r"\vartheta"));
        assert_eq!(char_to_latex('\u{03B9}'), Some(r"\iota"));
        assert_eq!(char_to_latex('\u{03BA}'), Some(r"\kappa"));
        assert_eq!(char_to_latex('\u{03BB}'), Some(r"\lambda"));
        assert_eq!(char_to_latex('\u{03BC}'), Some(r"\mu"));
        assert_eq!(char_to_latex('\u{03BD}'), Some(r"\nu"));
        assert_eq!(char_to_latex('\u{03BE}'), Some(r"\xi"));
        assert_eq!(char_to_latex('\u{03C0}'), Some(r"\pi"));
        assert_eq!(char_to_latex('\u{03C1}'), Some(r"\rho"));
        assert_eq!(char_to_latex('\u{03C3}'), Some(r"\sigma"));
        assert_eq!(char_to_latex('\u{03C4}'), Some(r"\tau"));
        assert_eq!(char_to_latex('\u{03C9}'), Some(r"\omega"));
    }

    #[test]
    fn test_greek_uppercase() {
        assert_eq!(char_to_latex('\u{0393}'), Some(r"\Gamma"));
        assert_eq!(char_to_latex('\u{0394}'), Some(r"\Delta"));
        assert_eq!(char_to_latex('\u{0398}'), Some(r"\Theta"));
        assert_eq!(char_to_latex('\u{039B}'), Some(r"\Lambda"));
        assert_eq!(char_to_latex('\u{03A3}'), Some(r"\Sigma"));
        assert_eq!(char_to_latex('\u{03A6}'), Some(r"\Phi"));
        assert_eq!(char_to_latex('\u{03A9}'), Some(r"\Omega"));
    }

    #[test]
    fn test_operators() {
        assert_eq!(char_to_latex('\u{222B}'), Some(r"\int"));
        assert_eq!(char_to_latex('\u{2211}'), Some(r"\sum"));
        assert_eq!(char_to_latex('\u{220F}'), Some(r"\prod"));
        assert_eq!(char_to_latex('\u{221A}'), Some(r"\sqrt"));
        assert_eq!(char_to_latex('\u{2202}'), Some(r"\partial"));
        assert_eq!(char_to_latex('\u{2207}'), Some(r"\nabla"));
    }

    #[test]
    fn test_relations() {
        assert_eq!(char_to_latex('\u{2264}'), Some(r"\leq"));
        assert_eq!(char_to_latex('\u{2265}'), Some(r"\geq"));
        assert_eq!(char_to_latex('\u{2260}'), Some(r"\neq"));
        assert_eq!(char_to_latex('\u{2248}'), Some(r"\approx"));
        assert_eq!(char_to_latex('\u{223C}'), Some(r"\sim"));
        assert_eq!(char_to_latex('\u{226A}'), Some(r"\ll"));
        assert_eq!(char_to_latex('\u{226B}'), Some(r"\gg"));
        assert_eq!(char_to_latex('\u{221E}'), Some(r"\infty"));
        assert_eq!(char_to_latex('\u{2208}'), Some(r"\in"));
        assert_eq!(char_to_latex('\u{2209}'), Some(r"\notin"));
        assert_eq!(char_to_latex('\u{2282}'), Some(r"\subset"));
    }

    #[test]
    fn test_misc_symbols() {
        assert_eq!(char_to_latex('\u{00B1}'), Some(r"\pm"));
        assert_eq!(char_to_latex('\u{00D7}'), Some(r"\times"));
        assert_eq!(char_to_latex('\u{00B7}'), Some(r"\cdot"));
        assert_eq!(char_to_latex('\u{00B0}'), Some(r"^\circ"));
        assert_eq!(char_to_latex('\u{2192}'), Some(r"\to"));
        assert_eq!(char_to_latex('\u{21D2}'), Some(r"\Rightarrow"));
        assert_eq!(char_to_latex('\u{210F}'), Some(r"\hbar"));
    }

    #[test]
    fn test_unicode_super_sub_digits() {
        assert_eq!(char_to_latex('\u{00B2}'), Some("^{2}"));
        assert_eq!(char_to_latex('\u{00B3}'), Some("^{3}"));
        assert_eq!(char_to_latex('\u{2082}'), Some("_{2}"));
        assert_eq!(char_to_latex('\u{2083}'), Some("_{3}"));
    }

    #[test]
    fn test_codepoint_variants_map_to_same_commands() {
        // MICRO SIGN and GREEK SMALL MU both mean \mu in extracted PDFs.
        assert_eq!(char_to_latex('\u{00B5}'), Some(r"\mu"));
        assert_eq!(char_to_latex('\u{03BC}'), Some(r"\mu"));
        assert_eq!(char_to_latex('\u{2212}'), Some("-"));
        assert_eq!(char_to_latex('\u{211D}'), Some(r"\mathbb{R}"));
        assert_eq!(char_to_latex('\u{211A}'), Some(r"\mathbb{Q}"));
    }
}
