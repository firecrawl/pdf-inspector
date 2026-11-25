//! Formula structure from glyphs: script levels from baselines and sizes,
//! limits of large operators, accents and stacked delimiter pieces, then
//! LaTeX for one line of mathematics.

use super::glyph::{FontClass, G, MAPSTO_STEM};

const ATOM: &str = "\u{1}";

fn greek(c: char) -> Option<&'static str> {
    Some(match c {
        'α' => "alpha",
        'β' => "beta",
        'γ' => "gamma",
        'δ' => "delta",
        'ϵ' => "epsilon",
        'ε' => "varepsilon",
        'ζ' => "zeta",
        'η' => "eta",
        'θ' => "theta",
        'ϑ' => "vartheta",
        'ι' => "iota",
        'κ' => "kappa",
        'λ' => "lambda",
        'μ' => "mu",
        'ν' => "nu",
        'ξ' => "xi",
        'π' => "pi",
        'ϖ' => "varpi",
        'ρ' => "rho",
        'ϱ' => "varrho",
        'σ' => "sigma",
        'ς' => "varsigma",
        'τ' => "tau",
        'υ' => "upsilon",
        'ϕ' => "phi",
        'φ' => "varphi",
        'ψ' => "psi",
        'χ' => "chi",
        'ω' => "omega",
        'Γ' => "Gamma",
        'Δ' => "Delta",
        'Θ' => "Theta",
        'Λ' => "Lambda",
        'Ξ' => "Xi",
        'Π' => "Pi",
        'Σ' => "Sigma",
        'Υ' => "Upsilon",
        'Φ' => "Phi",
        'Ψ' => "Psi",
        'Ω' => "Omega",
        _ => return None,
    })
}

fn symbol(c: char) -> Option<&'static str> {
    Some(match c {
        'µ' => "\\mu",
        '∆' => "\\Delta",
        'Ω' => "\\Omega",
        'ℵ' => "\\aleph",
        '⊊' => "\\subsetneq",
        '≲' => "\\lesssim",
        '≳' => "\\gtrsim",
        '≍' => "\\asymp",
        '⊑' => "\\sqsubseteq",
        '⊔' => "\\sqcup",
        '⊎' => "\\uplus",
        '⋀' => "\\bigwedge",
        '⋁' => "\\bigvee",
        '∐' => "\\coprod",
        '⇑' => "\\Uparrow",
        '⇓' => "\\Downarrow",
        '≤' => "\\leq",
        '≥' => "\\geq",
        '≠' => "\\neq",
        '≈' => "\\approx",
        '≡' => "\\equiv",
        '∼' => "\\sim",
        '≃' => "\\simeq",
        '≅' => "\\cong",
        '∝' => "\\propto",
        '×' => "\\times",
        '·' | '⋅' => "\\cdot",
        '∗' => "*",
        '∘' | '◦' => "\\circ",
        '±' => "\\pm",
        '∓' => "\\mp",
        '÷' => "\\div",
        '∈' => "\\in",
        '∉' => "\\notin",
        '∋' => "\\ni",
        '⊂' => "\\subset",
        '⊆' => "\\subseteq",
        '⊃' => "\\supset",
        '⊇' => "\\supseteq",
        '∪' => "\\cup",
        '∩' => "\\cap",
        '∖' => "\\setminus",
        '∅' => "\\emptyset",
        '∀' => "\\forall",
        '∃' => "\\exists",
        '¬' => "\\neg",
        '∧' => "\\wedge",
        '∨' => "\\vee",
        '→' => "\\rightarrow",
        '←' => "\\leftarrow",
        '↔' => "\\leftrightarrow",
        '⇒' => "\\Rightarrow",
        '⇐' => "\\Leftarrow",
        '⇔' => "\\Leftrightarrow",
        '↦' => "\\mapsto",
        '⟶' => "\\longrightarrow",
        '⟹' => "\\Longrightarrow",
        '↪' => "\\hookrightarrow",
        '↑' => "\\uparrow",
        '↓' => "\\downarrow",
        '∞' => "\\infty",
        '∂' => "\\partial",
        '∇' => "\\nabla",
        '′' => "'",
        'ℓ' => "\\ell",
        'ℏ' => "\\hbar",
        'ℜ' => "\\Re",
        'ℑ' => "\\Im",
        '℘' => "\\wp",
        'ı' => "\\imath",
        'ȷ' => "\\jmath",
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∫' => "\\int",
        '∮' => "\\oint",
        '⋃' => "\\bigcup",
        '⋂' => "\\bigcap",
        '⨁' => "\\bigoplus",
        '⨂' => "\\bigotimes",
        '⊕' => "\\oplus",
        '⊗' => "\\otimes",
        '⊙' => "\\odot",
        '†' => "\\dagger",
        '⋆' => "\\star",
        '⟨' | '〈' => "\\langle",
        '⟩' | '〉' => "\\rangle",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        '∥' | '‖' => "\\|",
        '{' => "\\{",
        '}' => "\\}",
        '−' | '–' => "-",
        '…' => "\\ldots",
        '⋯' => "\\cdots",
        '⋮' => "\\vdots",
        '⋱' => "\\ddots",
        '≪' => "\\ll",
        '≫' => "\\gg",
        '≺' => "\\prec",
        '≻' => "\\succ",
        '⪯' => "\\preceq",
        '⊥' => "\\perp",
        '∠' => "\\angle",
        '△' => "\\triangle",
        '□' => "\\square",
        '∎' => "\\blacksquare",
        '≔' => ":=",
        '⊤' => "\\top",
        '⊢' => "\\vdash",
        '⊨' => "\\models",
        '√' => "\\sqrt",
        '#' => "\\#",
        '%' => "\\%",
        '&' => "\\&",
        '_' => "\\_",
        '^' => "\\^{}",
        '~' => "\\sim",
        _ => return None,
    })
}

fn accent(c: &str) -> Option<&'static str> {
    Some(match c {
        "˜" | "~" => "tilde",
        "¯" | "ˉ" => "bar",
        "ˆ" | "^" => "hat",
        "˘" => "breve",
        "˙" => "dot",
        "¨" => "ddot",
        "ˇ" => "check",
        "\u{20D7}" => "vec",
        "´" => "acute",
        "\u{60}" => "grave",
        _ => return None,
    })
}

/// A letter of the Mathematical Alphanumeric Symbols block with the style
/// its alphabet names, as LaTeX.
fn math_alphanumeric(c: char) -> Option<String> {
    let o = c as u32;
    if (0x1D6A8..=0x1D7C9).contains(&o) {
        const GREEK: &str = "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡϴΣΤΥΦΧΨΩ∇αβγδεζηθικλμνξοπρςστυφχψω∂ϵϑϰϕϱϖ";
        return GREEK
            .chars()
            .nth(((o - 0x1D6A8) % 58) as usize)
            .map(|g| tok_char(g, FontClass::Roman));
    }
    if !(0x1D400..=0x1D6A3).contains(&o) {
        if (0x1D7CE..=0x1D7FF).contains(&o) {
            let offset = o - 0x1D7CE;
            let digit = char::from(b'0' + (offset % 10) as u8);
            return Some(match offset / 10 {
                0 => format!("\\mathbf{{{digit}}}"),
                1 => format!("\\mathbb{{{digit}}}"),
                _ => digit.to_string(),
            });
        }
        return None;
    }
    let offset = o - 0x1D400;
    let index = offset % 52;
    let letter = if index < 26 {
        char::from(b'A' + index as u8)
    } else {
        char::from(b'a' + (index - 26) as u8)
    };
    Some(match offset / 52 {
        0 | 2 => format!("\\mathbf{{{letter}}}"),
        3 | 4 => format!("\\mathcal{{{}}}", letter.to_ascii_uppercase()),
        5 | 7 => format!("\\mathfrak{{{letter}}}"),
        6 => format!("\\mathbb{{{}}}", letter.to_ascii_uppercase()),
        _ => letter.to_string(),
    })
}

fn tok_char(c: char, class: FontClass) -> String {
    if let Some(name) = greek(c) {
        return format!("\\{name}");
    }
    if let Some(tex) = symbol(c) {
        return tex.to_string();
    }
    if c.is_ascii_alphabetic() {
        match class {
            FontClass::Blackboard => return format!("\\mathbb{{{c}}}"),
            FontClass::Fraktur => return format!("\\mathfrak{{{c}}}"),
            FontClass::Script => return format!("\\mathscr{{{c}}}"),
            FontClass::Symbol if c.is_ascii_uppercase() => return format!("\\mathcal{{{c}}}"),
            FontClass::Bold => return format!("\\mathbf{{{c}}}"),
            _ => {}
        }
    }
    if let Some(tex) = math_alphanumeric(c) {
        return tex;
    }
    match c {
        'ℝ' => "\\mathbb{R}".to_string(),
        'ℕ' => "\\mathbb{N}".to_string(),
        'ℤ' => "\\mathbb{Z}".to_string(),
        'ℚ' => "\\mathbb{Q}".to_string(),
        'ℂ' => "\\mathbb{C}".to_string(),
        '$' => "\\$".to_string(),
        '\\' => "\\backslash".to_string(),
        _ => c.to_string(),
    }
}

pub(crate) fn tok(text: &str, class: FontClass) -> String {
    join(text.chars().map(|c| tok_char(c, class)))
}

fn glyph_tex(g: &G) -> String {
    g.tex.clone().unwrap_or_else(|| tok(&g.c, g.class))
}

fn ends_with_command(s: &str) -> bool {
    let trimmed = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    trimmed.len() < s.len() && trimmed.ends_with('\\')
}

fn join<I: IntoIterator<Item = String>>(toks: I) -> String {
    let mut s = String::new();
    for t in toks {
        if t.starts_with(|c: char| c.is_ascii_alphabetic()) && ends_with_command(&s) {
            s.push(' ');
        }
        s.push_str(&t);
    }
    s
}

fn group(s: &str) -> String {
    if s.chars().count() == 1 {
        s.to_string()
    } else {
        format!("{{{s}}}")
    }
}

const FUNCS: [&str; 34] = [
    "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "sin", "cos", "tan", "cot", "sec", "csc",
    "log", "ln", "exp", "lim", "sup", "inf", "max", "min", "det", "dim", "ker", "deg", "arg",
    "gcd", "Pr", "tr", "Tr", "sgn", "diag", "rank", "span", "mod",
];

/// Function names LaTeX has no command for.
const OPERATOR_NAMES: [&str; 6] = ["tr", "Tr", "sgn", "diag", "rank", "span"];

/// Function names written as plain letters become their operator commands.
fn funcify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let eligible = !prev.is_some_and(|p| p == '\\' || p.is_ascii_alphabetic())
            && !out.ends_with("\\operatorname{");
        let hit = eligible
            .then(|| {
                FUNCS.iter().find(|f| {
                    rest.starts_with(**f)
                        && !rest[f.len()..].starts_with(|c: char| c.is_ascii_alphabetic())
                })
            })
            .flatten();
        if let Some(f) = hit {
            if OPERATOR_NAMES.contains(f) {
                out.push_str("\\operatorname{");
                out.push_str(f);
                out.push('}');
            } else {
                out.push('\\');
                out.push_str(f);
                out.push(' ');
            }
            i += f.len();
            prev = Some(' ');
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

fn postfix(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '.' {
            let mut j = i + 1;
            let mut dots = 1;
            let mut end = i + 1;
            while j < chars.len() && dots < 3 {
                if chars[j] == '.' {
                    dots += 1;
                    end = j + 1;
                } else if !chars[j].is_whitespace() {
                    break;
                }
                j += 1;
            }
            if dots == 3 {
                out.push_str("\\ldots ");
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out.replace("\\cdot\\cdot\\cdot", "\\cdots ")
        .replace(&format!("{MAPSTO_STEM}\\rightarrow"), "\\mapsto ")
        .replace(MAPSTO_STEM, "\\mapsto ")
        .replace("\u{338}=", "\\neq ")
        .replace("\u{338}\\in", "\\notin")
        .replace('\u{338}', "\\not ")
}

fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

const STACKABLE: &str = "|‖()[]{}⌊⌋⌈⌉√";

/// Stack extensible delimiter pieces (same character, same column,
/// touching) into one tall glyph, and put accents on the glyph under them.
pub(crate) fn merge_pieces(mut gs: Vec<G>) -> Vec<G> {
    gs.sort_by(|a, b| {
        a.x0.round_ties_even()
            .total_cmp(&b.x0.round_ties_even())
            .then(a.y0.total_cmp(&b.y0))
    });
    let mut out: Vec<G> = Vec::with_capacity(gs.len());
    for g in gs {
        if let Some(p) = out.last_mut() {
            if g.class == FontClass::Extension
                && p.class == FontClass::Extension
                && g.c == p.c
                && g.c.chars().count() == 1
                && STACKABLE.contains(g.c.as_str())
                && (g.x0 - p.x0).abs() < 1.5
                && g.y0 <= p.y1 + 1.5
            {
                p.y1 = p.y1.max(g.y1);
                p.x1 = p.x1.max(g.x1);
                continue;
            }
        }
        out.push(g);
    }
    let (accents, mut rest): (Vec<G>, Vec<G>) = out
        .into_iter()
        .partition(|g| g.tex.is_none() && accent(&g.c).is_some());
    for a in accents {
        let under = rest
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                g.tex.is_none()
                    && accent(&g.c).is_none()
                    && g.x0 - 1.0 <= a.xc()
                    && a.xc() <= g.x1 + 1.0
                    && g.y0 >= a.y0 - 1.0
                    && g.y0 - a.y1 < g.size * 0.5
            })
            .fold(None::<(usize, f32)>, |best, (i, g)| {
                let gap = g.y0 - a.y1;
                match best {
                    Some((_, b)) if b <= gap => best,
                    _ => Some((i, gap)),
                }
            });
        match under {
            Some((i, _)) => {
                let b = &mut rest[i];
                let name = accent(&a.c).unwrap_or("hat");
                b.tex = Some(format!("\\{name}{{{}}}", tok(&b.c, b.class)));
            }
            None if a.c == "~" || a.c == "^" => rest.push(a),
            None => {}
        }
    }
    rest
}

fn limit_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "lim" => "\\lim",
        "max" | "maximize" => "\\max",
        "min" | "minimize" => "\\min",
        "sup" => "\\sup",
        "inf" => "\\inf",
        "liminf" => "\\liminf",
        "limsup" => "\\limsup",
        "argmax" => "\\arg\\max",
        "argmin" => "\\arg\\min",
        "det" => "\\det",
        "Pr" => "\\Pr",
        _ => return None,
    })
}

/// Upright words that take limits (lim, max, sup, ...) become one atom.
fn word_atoms(gs: Vec<G>, main: f32) -> Vec<G> {
    let mut cand: Vec<usize> = (0..gs.len())
        .filter(|&i| {
            let g = &gs[i];
            g.tex.is_none()
                && g.c.chars().count() == 1
                && g.c.chars().all(|c| c.is_ascii_alphabetic())
                && g.size >= main * 0.85
                && matches!(g.class, FontClass::Roman | FontClass::Bold)
        })
        .collect();
    cand.sort_by(|&a, &b| gs[a].x0.total_cmp(&gs[b].x0));
    let mut drop = vec![false; gs.len()];
    let mut atoms = Vec::new();
    let mut i = 0;
    while i < cand.len() {
        let g = &gs[cand[i]];
        let mut word = vec![cand[i]];
        let mut j = i;
        while j + 1 < cand.len()
            && (gs[cand[j + 1]].base - g.base).abs() < 0.5
            && gs[cand[j + 1]].x0 - gs[*word.last().unwrap_or(&cand[i])].x1 < 0.15 * g.size
        {
            j += 1;
            word.push(cand[j]);
        }
        let w: String = word.iter().map(|&k| gs[k].c.as_str()).collect();
        if let Some(tex) = limit_word(&w) {
            let first = &gs[word[0]];
            let last = &gs[word[word.len() - 1]];
            atoms.push(G {
                c: ATOM.to_string(),
                x0: first.x0,
                y0: word.iter().map(|&k| gs[k].y0).fold(f32::INFINITY, f32::min),
                x1: last.x1,
                y1: word
                    .iter()
                    .map(|&k| gs[k].y1)
                    .fold(f32::NEG_INFINITY, f32::max),
                base: g.base,
                size: g.size,
                class: FontClass::Roman,
                tex: Some(tex.to_string()),
            });
            for &k in &word {
                drop[k] = true;
            }
        }
        i = j + 1;
    }
    let mut out: Vec<G> = gs
        .into_iter()
        .zip(drop)
        .filter_map(|(g, d)| (!d).then_some(g))
        .collect();
    out.extend(atoms);
    out
}

const BIG_OPS: [&str; 11] = [
    "\\coprod",
    "\\bigwedge",
    "\\bigvee",
    "\\sum",
    "\\prod",
    "\\int",
    "\\oint",
    "\\bigcup",
    "\\bigcap",
    "\\bigoplus",
    "\\bigotimes",
];

/// The glyphs of `cands` that sit centred on operator `g`: those nearest
/// to it among `ops`, grouped into horizontal runs, keeping the runs
/// centred within its width.
fn limit_runs(gs: &[G], cands: Vec<usize>, g: usize, ops: &[usize]) -> Vec<usize> {
    let op = &gs[g];
    let w = op.x1 - op.x0;
    let mut cands: Vec<usize> = cands
        .into_iter()
        .filter(|&h| {
            ops.is_empty()
                || ops
                    .iter()
                    .fold(None::<(usize, f32)>, |best, &o| {
                        let d = (gs[o].xc() - gs[h].xc()).abs();
                        match best {
                            Some((_, b)) if b <= d => best,
                            _ => Some((o, d)),
                        }
                    })
                    .is_some_and(|(o, _)| o == g)
        })
        .filter(|&h| op.x0 - 1.2 * w <= gs[h].xc() && gs[h].xc() <= op.x1 + 1.2 * w)
        .collect();
    cands.sort_by(|&a, &b| gs[a].x0.total_cmp(&gs[b].x0));
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for h in cands {
        match runs.last_mut() {
            Some(run) if gs[h].x0 - gs[run[run.len() - 1]].x1 < gs[h].size * 0.35 => run.push(h),
            _ => runs.push(vec![h]),
        }
    }
    runs.into_iter()
        .filter(|run| {
            let right = run
                .iter()
                .map(|&h| gs[h].x1)
                .fold(f32::NEG_INFINITY, f32::max);
            let c = (gs[run[0]].x0 + right) / 2.0;
            op.x0 - 1.0 <= c && c <= op.x1 + 1.0
        })
        .flatten()
        .collect()
}

#[derive(PartialEq)]
enum Level {
    Main,
    Sup,
    Sub,
}

fn classify(g: &G, base: f32, main: f32) -> Level {
    if g.class == FontClass::Extension
        || g.size >= main * 0.85
        || (g.tex.is_some() && !g.c.is_empty())
    {
        Level::Main
    } else if g.base < base - main * 0.12 {
        Level::Sup
    } else if g.base > base + main * 0.08 {
        Level::Sub
    } else {
        Level::Main
    }
}

fn pick(gs: &[G], idx: &[usize]) -> Vec<G> {
    idx.iter().map(|&i| gs[i].clone()).collect()
}

/// One line of mathematics: glyphs off the main baseline become sub- and
/// superscripts of what precedes them, glyphs centred over or under a large
/// operator its limits.
pub(crate) fn render_line(gs: Vec<G>) -> String {
    if gs.is_empty() {
        return String::new();
    }
    let sizes: Vec<f32> = gs
        .iter()
        .filter(|g| g.tex.is_none() && g.class != FontClass::Extension)
        .map(|g| g.size)
        .collect();
    let main = if sizes.is_empty() {
        gs.iter().map(|g| g.size).fold(f32::NEG_INFINITY, f32::max)
    } else {
        sizes.into_iter().fold(f32::NEG_INFINITY, f32::max)
    };
    let mains: Vec<f32> = gs
        .iter()
        .filter(|g| g.size >= main * 0.9)
        .map(|g| g.base)
        .collect();
    let base = if mains.is_empty() {
        median(gs.iter().map(|g| g.base).collect())
    } else {
        median(mains)
    };
    let mut gs = word_atoms(gs, main);
    gs.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let n = gs.len();
    let mut used = vec![false; n];
    let ops: Vec<usize> = (0..n)
        .filter(|&i| {
            let g = &gs[i];
            (g.tex.is_none()
                && BIG_OPS.contains(&tok(&g.c, g.class).as_str())
                && (g.y1 - g.y0) > main * 0.9)
                || g.c == ATOM
        })
        .collect();
    let mut optex: Vec<Option<String>> = vec![None; n];
    for &g in &ops {
        let op = &gs[g];
        let ex = op.class == FontClass::Extension;
        let mut t = if op.c == ATOM {
            op.tex.clone().unwrap_or_default()
        } else {
            tok(&op.c, op.class)
        };
        let above_c: Vec<usize> = (0..n)
            .filter(|&h| {
                h != g
                    && !used[h]
                    && gs[h].size < main
                    && if ex {
                        gs[h].base <= op.base + 1.0
                    } else {
                        gs[h].y1 <= op.y0 + 1.5
                    }
            })
            .collect();
        let above = limit_runs(&gs, above_c, g, &ops);
        let below_c: Vec<usize> = (0..n)
            .filter(|&h| {
                h != g
                    && !used[h]
                    && gs[h].size < main
                    && if ex {
                        gs[h].y0 >= op.base + 0.8 * op.size
                    } else {
                        gs[h].y0 >= op.y1 - 1.5
                    }
            })
            .collect();
        let below = limit_runs(&gs, below_c, g, &ops);
        for &h in above.iter().chain(below.iter()) {
            used[h] = true;
        }
        if !below.is_empty() {
            t.push('_');
            t.push_str(&group(&render_line(pick(&gs, &below))));
        }
        if !above.is_empty() {
            t.push('^');
            t.push_str(&group(&render_line(pick(&gs, &above))));
        }
        optex[g] = Some(t);
    }
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut i = 0;
    while i < n {
        if used[i] {
            i += 1;
            continue;
        }
        if let Some(t) = optex[i].take() {
            flush_word(&mut out, &mut word);
            used[i] = true;
            out.push(t);
            i += 1;
            continue;
        }
        if classify(&gs[i], base, main) == Level::Main {
            used[i] = true;
            let g = &gs[i];
            if g.tex.is_none()
                && g.class == FontClass::Roman
                && g.c.chars().all(|c| c.is_ascii_alphabetic())
            {
                word.push_str(&g.c);
            } else {
                flush_word(&mut out, &mut word);
                out.push(glyph_tex(g));
            }
            i += 1;
            continue;
        }
        flush_word(&mut out, &mut word);
        let (mut sub, mut sup) = (Vec::new(), Vec::new());
        while i < n {
            if used[i] {
                i += 1;
                continue;
            }
            match classify(&gs[i], base, main) {
                Level::Main => break,
                Level::Sub => sub.push(i),
                Level::Sup => sup.push(i),
            }
            used[i] = true;
            i += 1;
        }
        let mut s = String::new();
        if !sub.is_empty() {
            s.push('_');
            s.push_str(&group(&render_line(pick(&gs, &sub))));
        }
        if !sup.is_empty() {
            let st = render_line(pick(&gs, &sup));
            let st = if st.chars().all(|c| c == '\'') {
                st.replace('\'', "\\prime")
            } else {
                st
            };
            s.push('^');
            s.push_str(&group(&st));
        }
        out.push(s);
    }
    flush_word(&mut out, &mut word);
    postfix(&join(out))
}

/// A run of upright letters, with the function names in it written as
/// their operators: italic letters are variables, however they spell.
fn flush_word(out: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        out.push(funcify(word));
        word.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(c: &str, x0: f32, base: f32, size: f32, class: FontClass) -> G {
        G {
            c: c.to_string(),
            x0,
            y0: base - 0.72 * size,
            x1: x0 + 0.5 * size,
            y1: base + 0.22 * size,
            base,
            size,
            class,
            tex: None,
        }
    }

    #[test]
    fn function_names_become_operators_once() {
        assert_eq!(funcify("sin x+arcsin y"), "\\sin  x+\\arcsin  y");
        assert_eq!(funcify("sinx"), "sinx");
        assert_eq!(funcify("\\sin x"), "\\sin x");
        assert_eq!(funcify("trA"), "trA");
        assert_eq!(funcify("tr A"), "\\operatorname{tr} A");
        assert_eq!(funcify(&funcify("tr A")), "\\operatorname{tr} A");
        assert_eq!(funcify("using"), "using");
    }

    #[test]
    fn dots_and_negations_are_rewritten() {
        assert_eq!(postfix("a. . .b"), "a\\ldots b");
        assert_eq!(postfix("\\cdot\\cdot\\cdot"), "\\cdots ");
        assert_eq!(postfix("a\u{338}=b"), "a\\neq b");
        assert_eq!(postfix("a\u{338}\\in B"), "a\\notin B");
        assert_eq!(postfix("x\u{F0000}\\rightarrow y"), "x\\mapsto  y");
        assert_eq!(
            postfix("x\\mapsto\\rightarrow y"),
            "x\\mapsto\\rightarrow y"
        );
    }

    #[test]
    fn letters_take_their_font_alphabet() {
        assert_eq!(tok("R", FontClass::Blackboard), "\\mathbb{R}");
        assert_eq!(tok("A", FontClass::Symbol), "\\mathcal{A}");
        assert_eq!(tok("a", FontClass::Symbol), "a");
        assert_eq!(tok("\u{1D400}", FontClass::Roman), "\\mathbf{A}");
        assert_eq!(tok("\u{1D53D}", FontClass::Roman), "\\mathbb{F}");
        assert_eq!(tok("α≤", FontClass::Italic), "\\alpha\\leq");
    }

    #[test]
    fn an_accent_sits_on_the_glyph_under_it() {
        let x = g("x", 100.0, 100.0, 10.0, FontClass::Italic);
        let mut hat = g("ˆ", 101.0, 98.0, 10.0, FontClass::Roman);
        hat.x1 = 104.0;
        let merged = merge_pieces(vec![x, hat]);
        assert_eq!(merged.len(), 1);
        assert_eq!(render_line(merged), "\\hat{x}");
    }

    #[test]
    fn stacked_delimiter_pieces_merge() {
        let mut top = g("(", 100.0, 90.0, 10.0, FontClass::Extension);
        top.y0 = 90.0;
        top.y1 = 108.0;
        let mut bottom = top.clone();
        bottom.y0 = 108.0;
        bottom.y1 = 126.0;
        let merged = merge_pieces(vec![top, bottom]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].y1, 126.0);
    }

    #[test]
    fn primes_in_a_superscript_are_written_as_prime() {
        let line = vec![
            g("f", 100.0, 100.0, 10.0, FontClass::Italic),
            g("′", 105.0, 96.0, 7.0, FontClass::Symbol),
        ];
        assert_eq!(render_line(line), "f^{\\prime}");
    }
}
