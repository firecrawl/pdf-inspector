//! Model-free inline-formula recovery: rewrites native math glyph runs into
//! `$...$` LaTeX using font evidence and item geometry.
//!
//! Digital TeX/word-processor PDFs draw formulas as native text in
//! self-identifying math fonts (CMMI, CMSY, MSBM, OpenType `*Math*`), with
//! sub/superscripts encoded purely by baseline offset and font-size drop.
//! That is enough evidence to reconstruct structurally flat LaTeX — symbols,
//! identifiers, and sub/superscripts — without any recognition model. The
//! font name even disambiguates alphabets an image model has to guess at:
//! `MSBM` is `\mathbb`, `CMSY` capitals are `\mathcal`, `EUFM` is
//! `\mathfrak`.
//!
//! Reconstruction is confidence-gated and deliberately conservative:
//! anything with structure this pass cannot represent (fractions, matrices,
//! bounded big operators, unmapped glyphs) keeps its original Unicode text.
//! A wrong `$...$` is worse than none.

mod unicode_map;

use crate::types::{TextItem, TextLine};

pub(crate) use unicode_map::char_to_latex;

/// Emit LaTeX only at or above this reconstruction confidence.
const MINIMUM_CONFIDENCE: f32 = 0.7;

/// Superscripts sit at least this fraction of the base font size above the
/// dominant baseline (PDF y grows upward).
const SUPERSCRIPT_RISE: f32 = 0.25;

/// Subscripts sit at least this fraction of the base font size below the
/// dominant baseline.
const SUBSCRIPT_DROP: f32 = 0.12;

/// Sub/superscript glyphs are rendered smaller than the base text.
const SCRIPT_SIZE_RATIO: f32 = 0.85;

/// Upright function names TeX sets with `\name` in math mode.
const FUNCTION_NAMES: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "log", "ln", "lg", "exp", "lim", "max", "min", "sup", "inf", "det", "dim", "ker", "deg", "arg",
    "gcd", "mod",
];

/// How strongly one item signals mathematics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MathEvidence {
    /// Math font or mapped math symbol: can anchor a run.
    Strong,
    /// Digits, brackets, relations, or short italic identifiers: may join a
    /// run that a strong item anchors, but cannot start one.
    Connective,
    /// Ordinary prose: terminates a run.
    None,
}

/// Which LaTeX alphabet a math font's letters belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MathAlphabet {
    /// Plain math letters (or a non-math font).
    Plain,
    /// `\mathbb` (double-struck).
    Blackboard,
    /// `\mathcal` — capitals only.
    Calligraphic,
    /// `\mathfrak` (Fraktur).
    Fraktur,
}

/// A font's role in mathematics: whether it exists to set math at all, and
/// which alphabet its letters render in. The single owner of font-name
/// normalization and family tables, so run anchoring and letter rendering
/// can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FontProfile {
    is_math: bool,
    alphabet: MathAlphabet,
}

fn font_profile(font: &str) -> FontProfile {
    // Subset tags ("ABCDEF+CMMI10") and foundry prefixes are common; match
    // on the family fragment, case-insensitively.
    let name = font.rsplit('+').next().unwrap_or(font).to_ascii_lowercase();

    // (family fragment, alphabet its letters render in)
    const FAMILIES: &[(&str, MathAlphabet)] = &[
        ("cmmi", MathAlphabet::Plain),
        ("cmmib", MathAlphabet::Plain),
        ("cmsy", MathAlphabet::Calligraphic),
        ("cmbsy", MathAlphabet::Calligraphic),
        ("cmex", MathAlphabet::Plain),
        ("msam", MathAlphabet::Plain),
        ("msbm", MathAlphabet::Blackboard),
        ("bbold", MathAlphabet::Blackboard),
        ("dsrom", MathAlphabet::Blackboard),
        ("bbm", MathAlphabet::Blackboard),
        ("eufm", MathAlphabet::Fraktur),
        ("eufb", MathAlphabet::Fraktur),
        ("eusm", MathAlphabet::Calligraphic),
        ("eusb", MathAlphabet::Calligraphic),
        ("rsfs", MathAlphabet::Calligraphic),
        ("stmary", MathAlphabet::Plain),
        ("mathitalic", MathAlphabet::Plain),
        ("math-italic", MathAlphabet::Plain),
        ("mathsymbols", MathAlphabet::Plain),
        ("mathextension", MathAlphabet::Plain),
        ("mathoperators", MathAlphabet::Plain),
        ("wasy", MathAlphabet::Plain),
        ("lasy", MathAlphabet::Plain),
    ];
    for (family, alphabet) in FAMILIES {
        if name.contains(family) {
            return FontProfile {
                is_math: true,
                alphabet: *alphabet,
            };
        }
    }

    // OpenType math families: "XITSMath", "CambriaMath", "STIXTwoMath",
    // "LatinModernMath" — but not e.g. "Mathias"; require the fragment to
    // end the family word or be followed by a non-letter.
    if let Some(position) = name.find("math") {
        let after = name[position + 4..].chars().next();
        if !matches!(after, Some(c) if c.is_ascii_lowercase() && !name[position + 4..].starts_with("ital"))
        {
            return FontProfile {
                is_math: true,
                alphabet: MathAlphabet::Plain,
            };
        }
    }
    FontProfile {
        is_math: false,
        alphabet: MathAlphabet::Plain,
    }
}

/// Mapped symbols that also live in ordinary prose — ellipses in dot
/// leaders, footnote daggers, bullets, degree signs, primes in quoted
/// coordinates. They may join a run another glyph anchors, but must never
/// anchor one themselves.
fn is_prose_punctuation(c: char) -> bool {
    matches!(c, '…' | '†' | '‡' | '•' | '·' | '°' | '′' | '″' | '±' | '×')
}

fn is_math_symbol_char(c: char) -> bool {
    !c.is_ascii() && char_to_latex(c).is_some()
}

/// Symbols strong enough to anchor a formula run.
fn is_anchor_symbol_char(c: char) -> bool {
    is_math_symbol_char(c) && !is_prose_punctuation(c)
}

fn classify_item(item: &TextItem) -> MathEvidence {
    let text = item.text.trim();
    if text.is_empty() {
        return MathEvidence::None;
    }
    if font_profile(&item.font).is_math {
        return MathEvidence::Strong;
    }
    let chars: Vec<char> = text.chars().collect();
    let symbolic = chars.iter().filter(|&&c| is_anchor_symbol_char(c)).count();
    if symbolic * 2 >= chars.len() {
        return MathEvidence::Strong;
    }
    // Digits, operators, brackets, and 1-2 letter italic identifiers can
    // extend a run anchored elsewhere.
    let connective = chars.iter().all(|&c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '+' | '-'
                    | '='
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '|'
                    | '/'
                    | ','
                    | '.'
                    | ':'
                    | ';'
                    | '*'
                    | '\''
            )
            || is_math_symbol_char(c)
    });
    if connective {
        return MathEvidence::Connective;
    }
    if item.is_italic && chars.len() <= 2 && chars.iter().all(|c| c.is_ascii_alphabetic()) {
        return MathEvidence::Connective;
    }
    if FUNCTION_NAMES.contains(&text) {
        return MathEvidence::Connective;
    }
    // Mixed identifier-and-punctuation items like "F(q,r)=" or "f:X" — the
    // left-hand sides equations lose when runs anchor late. Letters may only
    // appear in one- or two-character bursts (identifiers, never words).
    let mut alpha_run = 0usize;
    let mut longest_alpha_run = 0usize;
    let mut mixed = true;
    for &c in &chars {
        if c.is_ascii_alphabetic() {
            alpha_run += 1;
            longest_alpha_run = longest_alpha_run.max(alpha_run);
        } else {
            alpha_run = 0;
            if !c.is_ascii_digit()
                && !matches!(
                    c,
                    '+' | '-'
                        | '='
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '|'
                        | '/'
                        | ','
                        | '.'
                        | ':'
                        | ';'
                        | '*'
                        | '\''
                )
                && !is_math_symbol_char(c)
            {
                mixed = false;
                break;
            }
        }
    }
    if mixed && longest_alpha_run <= 2 && chars.len() >= 2 {
        return MathEvidence::Connective;
    }
    MathEvidence::None
}

/// Baseline role of one item inside a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptRole {
    Base,
    Superscript,
    Subscript,
}

#[derive(Debug, Default)]
struct Penalties {
    fired: Vec<&'static str>,
    total: f32,
}

impl Penalties {
    fn add(&mut self, name: &'static str, value: f32) {
        self.fired.push(name);
        self.total += value;
    }
}

/// One reconstructed run.
#[derive(Debug)]
struct Reconstruction {
    latex: String,
    confidence: f32,
}

/// True when (), [], and {} all nest correctly.
fn delimiters_balanced(latex: &str) -> bool {
    let mut stack: Vec<char> = Vec::new();
    for c in latex.chars() {
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' => {
                if stack.pop() != Some('(') {
                    return false;
                }
            }
            ']' => {
                if stack.pop() != Some('[') {
                    return false;
                }
            }
            '}' => {
                if stack.pop() != Some('{') {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty()
}

fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

const SCRIPT_DIGIT_PREFIX_SUP: &str = "^{";
const SCRIPT_DIGIT_PREFIX_SUB: &str = "_{";

/// Extends a trailing `^{...}` / `_{...}` group instead of opening an
/// invalid adjacent one, so "x₁₂" becomes `x_{12}`.
fn append_script_digit(out: &mut String, kind: char, mapped: &str) {
    let digits = mapped.trim_end_matches('}');
    if out.ends_with('}') {
        let opener = format!("{kind}{{");
        if let Some(start) = out.rfind(&opener) {
            if !out[start + 2..out.len() - 1].contains('}') {
                out.truncate(out.len() - 1);
                out.push_str(digits);
                out.push('}');
                return;
            }
        }
    }
    out.push(kind);
    out.push('{');
    out.push_str(digits);
    out.push('}');
}

/// Removes and returns the last logical glyph: a trailing `\command{X}`
/// group, a `\command`, or a single character.
fn pop_last_glyph(out: &mut String) -> Option<String> {
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    if out.is_empty() {
        return None;
    }
    if out.ends_with('}') {
        // Take the whole trailing \command{...} group.
        if let Some(start) = out.rfind('\\') {
            let glyph = out[start..].to_string();
            out.truncate(start);
            return Some(glyph);
        }
        return None;
    }
    let last = out.chars().last()?;
    if last.is_ascii_alphanumeric() {
        // The trailing alphanumeric may terminate a \command run — walk to
        // the start of the run and pop the whole command if so.
        let run_start = out
            .rfind(|c: char| !c.is_ascii_alphanumeric())
            .map(|i| i + 1)
            .unwrap_or(0);
        if out[..run_start].ends_with('\\') {
            let glyph = out[run_start - 1..].to_string();
            out.truncate(run_start - 1);
            return Some(glyph);
        }
        let boundary = out.len() - last.len_utf8();
        out.truncate(boundary);
        return Some(last.to_string());
    }
    None
}

/// Maps one item's text to LaTeX tokens, honoring alphabet fonts.
fn item_to_latex(item: &TextItem, penalties: &mut Penalties) -> String {
    let text = item.text.trim();
    if FUNCTION_NAMES.contains(&text) && !item.is_italic {
        return format!("\\{text} ");
    }
    let alphabet = font_profile(&item.font).alphabet;
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphabetic() && alphabet == MathAlphabet::Blackboard {
            out.push_str(&format!("\\mathbb{{{c}}}"));
        } else if c.is_ascii_uppercase() && alphabet == MathAlphabet::Calligraphic {
            out.push_str(&format!("\\mathcal{{{c}}}"));
        } else if c.is_ascii_alphabetic() && alphabet == MathAlphabet::Fraktur {
            out.push_str(&format!("\\mathfrak{{{c}}}"));
        } else if c.is_ascii() {
            match c {
                // LaTeX-active ASCII that never appears as itself in the
                // flat formulas this pass emits.
                '#' | '$' | '%' | '&' | '~' => {
                    penalties.add("latex_active_ascii", 0.4);
                    out.push(c);
                }
                '{' => out.push_str("\\{"),
                '}' => out.push_str("\\}"),
                _ => out.push(c),
            }
        } else if let Some(latex) = char_to_latex(c) {
            if let Some(argument) = latex.strip_suffix("{}") {
                // Accents extracted as standalone combining characters wrap
                // the glyph they follow: "Q" + \hat{} → \hat{Q}. Without a
                // preceding glyph the accent has no argument and the run is
                // not representable.
                if let Some(previous) = pop_last_glyph(&mut out) {
                    out.push_str(&format!("{argument}{{{previous}}}"));
                } else {
                    penalties.add("dangling_accent", 1.0);
                }
            } else if let Some(rest) = latex.strip_prefix(SCRIPT_DIGIT_PREFIX_SUP) {
                // Coalesce runs of Unicode super/subscript digits into one
                // group: "₁₂" is _{12}, never the invalid _{1}_{2}.
                append_script_digit(&mut out, '^', rest);
            } else if let Some(rest) = latex.strip_prefix(SCRIPT_DIGIT_PREFIX_SUB) {
                append_script_digit(&mut out, '_', rest);
            } else {
                out.push_str(latex);
                // Commands eat following letters: "\leq k" not "\leqk".
                if latex
                    .chars()
                    .last()
                    .is_some_and(|last| last.is_ascii_alphabetic())
                {
                    out.push(' ');
                }
            }
        } else if c.is_alphabetic() {
            // Non-ASCII letters without a mapping (accented identifiers)
            // pass through; KaTeX accepts them in text-ish positions but
            // they are a mild risk.
            penalties.add("unmapped_letter", 0.15);
            out.push(c);
        } else {
            penalties.add("unmapped_symbol", 0.4);
            out.push(c);
        }
    }
    out
}

/// Reconstructs LaTeX for a run of items already sorted by x.
fn reconstruct(items: &[&TextItem]) -> Reconstruction {
    let mut penalties = Penalties::default();

    let mut sizes: Vec<f32> = items.iter().map(|item| item.font_size).collect();
    let base_size = median(&mut sizes).max(1.0);
    let mut baselines: Vec<f32> = items
        .iter()
        .filter(|item| item.font_size >= base_size * SCRIPT_SIZE_RATIO)
        .map(|item| item.y)
        .collect();
    let mut all_baselines: Vec<f32> = items.iter().map(|item| item.y).collect();
    let base_y = if baselines.is_empty() {
        median(&mut all_baselines)
    } else {
        median(&mut baselines)
    };

    // Distinct y-bands beyond base/sub/super suggest stacked structure
    // (fractions, matrices) that flat reconstruction cannot represent.
    let mut distinct_bands: Vec<f32> = Vec::new();
    for item in items {
        let offset = item.y - base_y;
        if !distinct_bands
            .iter()
            .any(|band| (band - offset).abs() < base_size * 0.2)
        {
            distinct_bands.push(offset);
        }
    }
    if distinct_bands.len() > 3 {
        penalties.add("multi_band", 0.5);
    }

    if items.len() > 15 {
        penalties.add("many_items", 0.2);
    }

    // Bounded big operators (∑ with limits above/below) need structure this
    // pass does not build.
    let has_huge_operator = items.iter().any(|item| {
        item.font_size > base_size * 1.3
            && item
                .text
                .chars()
                .any(|c| matches!(c, '∑' | '∏' | '∫' | '√' | '∮' | '⋃' | '⋂'))
    });
    if has_huge_operator {
        penalties.add("huge_operator", 0.5);
    }

    let mut latex = String::new();
    let mut previous_role = ScriptRole::Base;
    let mut open_group = false;
    let mut previous_end_x: Option<f32> = None;

    for item in items {
        let rise = item.y - base_y;
        let small = item.font_size < base_size * SCRIPT_SIZE_RATIO;
        let role = if small && rise > base_size * SUPERSCRIPT_RISE {
            ScriptRole::Superscript
        } else if small && rise < -(base_size * SUBSCRIPT_DROP) {
            ScriptRole::Subscript
        } else if rise.abs() > base_size * 0.45 {
            // Base-size text far off the baseline: stacked structure.
            penalties.add("offset_base_text", 0.35);
            ScriptRole::Base
        } else {
            ScriptRole::Base
        };

        if role != previous_role {
            if open_group {
                // Command-guard spaces are only needed before letters, never
                // before the closing brace.
                while latex.ends_with(' ') {
                    latex.pop();
                }
                latex.push('}');
                open_group = false;
            }
            match role {
                ScriptRole::Superscript => {
                    open_script_group(&mut latex, '^');
                    open_group = true;
                }
                ScriptRole::Subscript => {
                    open_script_group(&mut latex, '_');
                    open_group = true;
                }
                ScriptRole::Base => {}
            }
        }

        // Preserve word gaps between base items so identifiers don't fuse.
        if role == ScriptRole::Base && previous_role == ScriptRole::Base {
            if let Some(end_x) = previous_end_x {
                if item.x - end_x > item.font_size * 0.2 && !latex.is_empty() {
                    latex.push(' ');
                }
            }
        }

        latex.push_str(&item_to_latex(item, &mut penalties));
        previous_role = role;
        previous_end_x = Some(item.x + item.width);
    }
    if open_group {
        while latex.ends_with(' ') {
            latex.pop();
        }
        latex.push('}');
    }

    // A run that reconstructs to bare ASCII prose earned no math evidence in
    // its glyphs; require at least one command, script, or symbol.
    if !latex.contains('\\') && !latex.contains('^') && !latex.contains('_') {
        penalties.add("no_math_content", 1.0);
    }

    // Unbalanced delimiters mean the run boundary cut through the middle of
    // an expression (the rest classified as prose); a truncated formula is
    // worse than the original text.
    if !delimiters_balanced(&latex) {
        penalties.add("unbalanced_delimiters", 1.0);
    }

    let confidence = (1.0 - penalties.total).clamp(0.0, 1.0);
    Reconstruction {
        latex: latex.trim().to_string(),
        confidence,
    }
}

/// Opens a `^{`/`_{` group, reopening a same-kind group that immediately
/// precedes it instead of emitting the invalid adjacent form: an in-text
/// Unicode subscript ("E₄" → `E_{4}`) followed by a geometric subscript item
/// must extend the existing group (`E_{4x4}`), never produce `E_{4}_{x4}`.
fn open_script_group(latex: &mut String, kind: char) {
    if latex.ends_with('}') {
        // Find the matching opening brace of the trailing group.
        let bytes = latex.as_bytes();
        let mut depth = 0usize;
        for index in (0..bytes.len()).rev() {
            match bytes[index] {
                b'}' => depth += 1,
                b'{' => {
                    depth -= 1;
                    if depth == 0 {
                        if index > 0 && bytes[index - 1] == kind as u8 {
                            latex.pop();
                            return;
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    latex.push(kind);
    latex.push('{');
}

/// True when `fragment` is a line of detached sub/superscript glyphs
/// belonging to `base`: the upstream line grouper's fixed baseline tolerance
/// (3pt) is smaller than TeX's superscript rise at common text sizes, so
/// scripts frequently arrive as their own one- or two-item lines.
fn is_script_fragment_of(fragment: &TextLine, base: &TextLine) -> bool {
    if fragment.page != base.page || fragment.items.is_empty() || base.items.is_empty() {
        return false;
    }
    let mut base_sizes: Vec<f32> = base.items.iter().map(|item| item.font_size).collect();
    let base_size = median(&mut base_sizes).max(1.0);

    // Scripts are smaller and within one line-height of the base baseline.
    let all_script_sized = fragment.items.iter().all(|item| {
        item.font_size < base_size * SCRIPT_SIZE_RATIO
            && (item.y - base.y).abs() < base_size * 0.9
            && item.text.trim().chars().count() <= 6
            && classify_item(item) != MathEvidence::None
    });
    if !all_script_sized {
        return false;
    }

    // The base must carry math evidence and horizontally contain the
    // fragment (with one em of slack for trailing exponents).
    if !base
        .items
        .iter()
        .any(|item| classify_item(item) == MathEvidence::Strong)
    {
        return false;
    }
    let base_start = base.items.iter().map(|i| i.x).fold(f32::MAX, f32::min);
    let base_end = base
        .items
        .iter()
        .map(|i| i.x + i.width)
        .fold(f32::MIN, f32::max);
    fragment
        .items
        .iter()
        .all(|item| item.x + item.width >= base_start - base_size && item.x <= base_end + base_size)
}

/// Reattaches detached script-fragment lines to their base line. Superscript
/// fragments sort before their base in reading order (higher y), subscript
/// fragments after — merge in both directions, restoring x order.
fn stitch_script_lines(lines: Vec<TextLine>) -> Vec<TextLine> {
    let mut result: Vec<TextLine> = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(previous) = result.last_mut() {
            // Subscript-style: the fragment follows its base.
            if is_script_fragment_of(&line, previous) {
                previous.items.extend(line.items);
                previous.items.sort_by(|a, b| a.x.total_cmp(&b.x));
                continue;
            }
            // Superscript-style: the fragment preceded its base.
            if is_script_fragment_of(previous, &line) {
                let fragment = result.pop().expect("checked non-empty");
                let mut base = line;
                base.items.extend(fragment.items);
                base.items.sort_by(|a, b| a.x.total_cmp(&b.x));
                result.push(base);
                continue;
            }
        }
        result.push(line);
    }
    result
}

/// Rewrites confident math runs inside each line into single `$...$` items.
///
/// Runs are maximal sequences of math-evidence items anchored by at least
/// two strong items (one lone symbol is not a formula). Reconstruction
/// below the confidence gate keeps the original items untouched.
pub(crate) fn rewrite_math_runs(lines: Vec<TextLine>) -> Vec<TextLine> {
    stitch_script_lines(lines)
        .into_iter()
        .map(|mut line| {
            let evidence: Vec<MathEvidence> = line.items.iter().map(classify_item).collect();
            if !evidence.contains(&MathEvidence::Strong) {
                return line;
            }

            let mut rewritten: Vec<TextItem> = Vec::with_capacity(line.items.len());
            let mut index = 0;
            while index < line.items.len() {
                if evidence[index] == MathEvidence::None {
                    rewritten.push(line.items[index].clone());
                    index += 1;
                    continue;
                }
                let mut end = index;
                while end < line.items.len() && evidence[end] != MathEvidence::None {
                    end += 1;
                }
                let run: Vec<&TextItem> = line.items[index..end].iter().collect();
                let strong = evidence[index..end]
                    .iter()
                    .filter(|e| **e == MathEvidence::Strong)
                    .count();
                // Trailing/leading connectives (punctuation) stay prose.
                let replaced = if strong >= 2 || (strong == 1 && run.len() >= 2) {
                    let reconstruction = reconstruct(&run);
                    if reconstruction.confidence >= MINIMUM_CONFIDENCE
                        && !reconstruction.latex.is_empty()
                    {
                        let first = run[0];
                        let last = run[run.len() - 1];
                        let mut item = first.clone();
                        // Sentence punctuation that trails the run belongs to
                        // the prose around the formula, not inside it.
                        let mut latex = reconstruction.latex.as_str();
                        let mut trailer = "";
                        if let Some(stripped) =
                            latex.strip_suffix(['.', ',', ';', ':']).map(str::trim_end)
                        {
                            trailer = &reconstruction.latex[stripped.len()..];
                            latex = stripped;
                        }
                        if latex.is_empty() {
                            rewritten.extend(line.items[index..end].iter().cloned());
                            index = end;
                            continue;
                        }
                        item.text = format!("${latex}${}", trailer.trim_start());
                        item.width = (last.x + last.width - first.x).max(first.width);
                        // LaTeX math carries its own styling; markdown
                        // emphasis markers would wrap or split the $...$
                        // delimiters.
                        item.is_italic = false;
                        item.is_bold = false;
                        item.is_underline = false;
                        item.is_strikeout = false;
                        log::debug!(
                            "page {} formula: {} (confidence {:.2})",
                            item.page,
                            item.text,
                            reconstruction.confidence
                        );
                        rewritten.push(item);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !replaced {
                    rewritten.extend(line.items[index..end].iter().cloned());
                }
                index = end;
            }
            line.items = rewritten;
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, x: f32, y: f32, size: f32, font: &str, italic: bool) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * size * 0.5,
            height: size,
            font: font.to_string(),
            font_size: size,
            page: 1,
            is_bold: false,
            is_italic: italic,
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        }
    }

    fn line(items: Vec<TextItem>) -> TextLine {
        TextLine {
            y: items.first().map(|i| i.y).unwrap_or(0.0),
            page: 1,
            items,
            adaptive_threshold: 0.1,
        }
    }

    #[test]
    fn math_fonts_are_recognized_through_subset_tags() {
        assert!(font_profile("ABCDEF+CMMI10").is_math);
        assert!(font_profile("CMSY7").is_math);
        assert!(font_profile("MSBM10").is_math);
        assert!(font_profile("XITSMath").is_math);
        assert!(font_profile("LatinModernMath-Regular").is_math);
        assert!(font_profile("QILUSS+LMMathSymbols10-Regular").is_math);
        assert!(font_profile("TDHQPX+LMMathItalic10-Regular").is_math);
        assert!(!font_profile("Times-Roman").is_math);
        assert!(!font_profile("ABCDEF+Calibri").is_math);
        assert!(!font_profile("MathiasHandwriting").is_math);
    }

    #[test]
    fn flat_relation_reconstructs_with_symbols_mapped() {
        // "0 ≤ k ≤ 2" in a math-font run.
        let run = vec![
            item("0", 10.0, 100.0, 10.0, "CMR10", false),
            item("≤", 18.0, 100.0, 10.0, "CMSY10", false),
            item("k", 26.0, 100.0, 10.0, "CMMI10", true),
            item("≤", 34.0, 100.0, 10.0, "CMSY10", false),
            item("2", 42.0, 100.0, 10.0, "CMR10", false),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, r"0 \leq k \leq 2");
    }

    #[test]
    fn superscript_from_baseline_and_size() {
        // "2^{N}" — raised, smaller N.
        let run = vec![
            item("2", 10.0, 100.0, 10.0, "CMR10", false),
            item("N", 16.0, 104.5, 7.0, "CMMI7", true),
            item("−", 22.0, 100.0, 10.0, "CMSY10", false),
            item("1", 30.0, 100.0, 10.0, "CMR10", false),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, "2^{N}-1");
    }

    #[test]
    fn subscript_groups_consecutive_script_items() {
        // "k_{ε}" style: base then dropped smaller epsilon.
        let run = vec![
            item("k", 10.0, 100.0, 10.0, "CMMI10", true),
            item("ε", 16.0, 98.0, 7.0, "CMMI7", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, r"k_{\varepsilon}");
    }

    #[test]
    fn blackboard_font_maps_letters_to_mathbb() {
        let run = vec![
            item("R", 10.0, 100.0, 10.0, "MSBM10", false),
            item("→", 18.0, 100.0, 10.0, "CMSY10", false),
            item("C", 28.0, 100.0, 10.0, "MSBM10", false),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE);
        assert_eq!(result.latex, r"\mathbb{R} \to \mathbb{C}");
    }

    #[test]
    fn stacked_structure_is_rejected() {
        // Two base-size rows (a fraction) must not emit flat LaTeX.
        let run = vec![
            item("d", 10.0, 106.0, 10.0, "CMMI10", true),
            item("W", 16.0, 106.0, 10.0, "CMMI10", true),
            item("d", 10.0, 94.0, 10.0, "CMMI10", true),
            item("ω", 16.0, 94.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence < MINIMUM_CONFIDENCE, "{result:?}");
    }

    #[test]
    fn bounded_big_operator_is_rejected() {
        let run = vec![
            item("∑", 10.0, 100.0, 14.0, "CMEX10", false),
            item("i", 12.0, 92.0, 7.0, "CMMI7", true),
            item("x", 22.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence < MINIMUM_CONFIDENCE, "{result:?}");
    }

    #[test]
    fn prose_line_is_untouched() {
        let lines = vec![line(vec![
            item("The", 10.0, 100.0, 10.0, "Times-Roman", false),
            item("quick", 40.0, 100.0, 10.0, "Times-Roman", false),
            item("fox", 80.0, 100.0, 10.0, "Times-Roman", false),
        ])];
        let out = rewrite_math_runs(lines);
        assert_eq!(out[0].items.len(), 3);
        assert!(!out[0].items[0].text.contains('$'));
    }

    #[test]
    fn mixed_line_rewrites_only_the_math_run() {
        let lines = vec![line(vec![
            item("where", 10.0, 100.0, 10.0, "Times-Roman", false),
            item("k", 50.0, 100.0, 10.0, "CMMI10", true),
            item("≥", 58.0, 100.0, 10.0, "CMSY10", false),
            item("0", 66.0, 100.0, 10.0, "CMR10", false),
            item("holds.", 80.0, 100.0, 10.0, "Times-Roman", false),
        ])];
        let out = rewrite_math_runs(lines);
        let texts: Vec<&str> = out[0].items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["where", r"$k \geq 0$", "holds."]);
    }

    #[test]
    fn mixed_identifier_items_join_the_run() {
        // "F(q,r)=" is the left-hand side of an equation: letters only in
        // short bursts, so it must extend a run anchored by the math font
        // items that follow — equations should not lose their left side.
        assert_eq!(
            classify_item(&item("F(q,r)=", 10.0, 100.0, 10.0, "Times-Italic", true)),
            MathEvidence::Connective
        );
        // Words never qualify, even with punctuation attached.
        assert_eq!(
            classify_item(&item("where,", 10.0, 100.0, 10.0, "Times-Roman", false)),
            MathEvidence::None
        );
    }

    #[test]
    fn trailing_sentence_punctuation_stays_outside_the_span() {
        let lines = vec![line(vec![
            item("k", 10.0, 100.0, 10.0, "CMMI10", true),
            item("≥", 18.0, 100.0, 10.0, "CMSY10", false),
            item("0,", 26.0, 100.0, 10.0, "CMR10", false),
        ])];
        let out = rewrite_math_runs(lines);
        assert_eq!(out[0].items[0].text, r"$k \geq 0$,");
    }

    #[test]
    fn geometric_script_extends_in_text_unicode_script_group() {
        // "E₄" carries an in-text Unicode subscript; a stitched geometric
        // subscript item "x4" must extend the same group: E_{4x4}.
        let run = vec![
            item("E₄", 10.0, 100.0, 10.0, "CMMI10", true),
            item("x4", 19.0, 97.5, 7.0, "CMMI7", true),
            item("=", 28.0, 100.0, 10.0, "CMR10", false),
            item("e", 36.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, "E_{4x4}= e");
    }

    #[test]
    fn detached_superscript_line_is_stitched_and_reconstructed() {
        // The upstream line grouper's 3pt baseline tolerance splits TeX
        // superscripts into their own line, which sorts before the base
        // line in reading order. "2^{N}" arriving as two lines.
        let script_line = line(vec![item("N", 24.0, 104.5, 7.0, "CMMI7", true)]);
        let base_line = line(vec![
            item("k", 10.0, 100.0, 10.0, "CMMI10", true),
            item("≤", 16.0, 100.0, 10.0, "CMSY10", false),
            item("2", 23.0, 100.0, 10.0, "CMR10", false),
        ]);
        let out = rewrite_math_runs(vec![script_line, base_line]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].items[0].text, r"$k\leq 2^{N}$");
    }

    #[test]
    fn detached_subscript_line_is_stitched() {
        let base_line = line(vec![
            item("x", 10.0, 100.0, 10.0, "CMMI10", true),
            item("∈", 18.0, 100.0, 10.0, "CMSY10", false),
            item("A", 26.0, 100.0, 10.0, "CMMI10", true),
        ]);
        let script_line = line(vec![item("i", 14.0, 97.5, 7.0, "CMMI7", true)]);
        let out = rewrite_math_runs(vec![base_line, script_line]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].items[0].text, r"$x_{i}\in A$");
    }

    #[test]
    fn prose_footnote_marker_line_is_not_stitched() {
        // A superscript footnote marker near a prose line (no math
        // evidence) must stay its own line.
        let marker = line(vec![item("1", 80.0, 104.0, 6.0, "Times-Roman", false)]);
        let prose = line(vec![
            item("See", 10.0, 100.0, 10.0, "Times-Roman", false),
            item("appendix", 40.0, 100.0, 10.0, "Times-Roman", false),
        ]);
        let out = rewrite_math_runs(vec![marker, prose]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn unicode_script_digits_coalesce_into_one_group() {
        // "x₁₂" must become x_{12}, never the invalid x_{1}_{2}.
        let run = vec![
            item("x₁₂", 10.0, 100.0, 10.0, "CMMI10", true),
            item("≤", 20.0, 100.0, 10.0, "CMSY10", false),
            item("y", 28.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, r"x_{12}\leq y");
    }

    #[test]
    fn combining_accent_wraps_previous_glyph() {
        // "Q" followed by a standalone combining hat is \hat{Q}.
        let run = vec![
            item("Q\u{0302}", 10.0, 100.0, 10.0, "CMMI10", true),
            item("=", 20.0, 100.0, 10.0, "CMR10", false),
            item("γ", 28.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, r"\hat{Q}= \gamma");
    }

    #[test]
    fn accent_wraps_whole_trailing_command() {
        // "α" + combining hat is \hat{\alpha}, not \alph + \hat{a}.
        let run = vec![
            item("α\u{0302}", 10.0, 100.0, 10.0, "CMMI10", true),
            item("∈", 20.0, 100.0, 10.0, "CMSY10", false),
            item("A", 28.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence >= MINIMUM_CONFIDENCE, "{result:?}");
        assert_eq!(result.latex, r"\hat{\alpha}\in A");
    }

    #[test]
    fn dangling_accent_rejects_the_run() {
        let run = vec![
            item("\u{0302}", 10.0, 100.0, 10.0, "CMSY10", false),
            item("x", 16.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence < MINIMUM_CONFIDENCE, "{result:?}");
    }

    #[test]
    fn truncated_expression_with_unbalanced_parens_is_rejected() {
        // "¬(O" where the closing paren fell outside the run.
        let run = vec![
            item("¬", 10.0, 100.0, 10.0, "CMSY10", false),
            item("(O", 16.0, 100.0, 10.0, "CMMI10", true),
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence < MINIMUM_CONFIDENCE, "{result:?}");
    }

    #[test]
    fn rewritten_item_drops_emphasis_flags() {
        let mut math_item = item("k", 50.0, 100.0, 10.0, "CMMI10", true);
        math_item.is_bold = true;
        let lines = vec![line(vec![
            math_item,
            item("≥", 58.0, 100.0, 10.0, "CMSY10", false),
            item("0", 66.0, 100.0, 10.0, "CMR10", false),
        ])];
        let out = rewrite_math_runs(lines);
        assert!(out[0].items[0].text.starts_with('$'));
        assert!(!out[0].items[0].is_italic);
        assert!(!out[0].items[0].is_bold);
    }

    #[test]
    fn dot_leaders_do_not_anchor_a_formula() {
        // ToC / index lines: "1.331-1T…………… 1545-2019". The ellipses map to
        // \ldots but are prose punctuation and must not anchor a run.
        let lines = vec![line(vec![
            item("1.331-1T", 10.0, 100.0, 10.0, "Times-Roman", false),
            item("……………………", 60.0, 100.0, 10.0, "Times-Roman", false),
            item("1545-2019", 160.0, 100.0, 10.0, "Times-Roman", false),
        ])];
        let out = rewrite_math_runs(lines);
        assert_eq!(out[0].items.len(), 3);
        assert!(!out[0].items.iter().any(|i| i.text.contains('$')));
    }

    #[test]
    fn single_lone_symbol_is_not_a_formula() {
        // An isolated dagger or bullet in prose must not become math.
        let lines = vec![line(vec![
            item("†", 10.0, 100.0, 10.0, "CMSY10", false),
            item("Author", 20.0, 100.0, 10.0, "Times-Roman", false),
        ])];
        let out = rewrite_math_runs(lines);
        assert_eq!(out[0].items[0].text, "†");
    }

    #[test]
    fn unmapped_symbols_reject_the_run() {
        let run = vec![
            item("k", 10.0, 100.0, 10.0, "CMMI10", true),
            item("\u{E123}", 16.0, 100.0, 10.0, "CMSY10", false), // PUA glyph
        ];
        let refs: Vec<&TextItem> = run.iter().collect();
        let result = reconstruct(&refs);
        assert!(result.confidence < MINIMUM_CONFIDENCE, "{result:?}");
    }
}
