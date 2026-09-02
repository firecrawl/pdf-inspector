//! Superscript / subscript detection.
//!
//! Footnote and affiliation markers, exponents and chemistry indices are
//! rendered as separate text items with a much smaller font size and a
//! baseline offset from the word they annotate. This module finds those glyph
//! runs after `merge_text_items`, fuses digit-only runs into their word as
//! Unicode super/subscript characters, and materializes every other run as a
//! single item flagged with [`TextItem::baseline_shift`]. Line grouping and
//! rendering (`TextItem::line_y`, `TextLine::text`) consume those results.

use crate::types::{ItemType, TextItem};
use std::collections::HashMap;

/// Merge subscript/superscript glyph runs into their parent items, or flag
/// them with a baseline offset when they cannot be fused.
///
/// Scripts — footnote and affiliation markers, exponents, chemistry indices —
/// are rendered as separate text items with a much smaller font size and a
/// baseline offset from the word they annotate. This pass finds runs of such
/// glyphs tightly attached to a larger neighbor (the anchor) and:
///
/// - absorbs a digit-only run into the anchor word as Unicode sub/superscript
///   characters ("H"+"2" → "H₂", "word"+"2" → "word²", "1"+"Hong Kong" →
///   "¹Hong Kong"), so downstream table detection and line grouping see one
///   token;
/// - coalesces any other run ("1,2,3", "*", "†", "th", "max") into ONE item
///   carrying `baseline_shift` = offset from the anchor's baseline, so line
///   grouping keeps it on the anchor's line (`TextItem::line_y`) and
///   `TextLine::text` renders it as `<sup>…</sup>` / `<sub>…</sub>`.
///
/// Item order is unchanged from the fusion-only version of this pass: items
/// are bucketed into 5pt rough lines and x-sorted within each, and the result
/// is that order minus the fused glyphs.
pub(crate) fn merge_subscript_items(items: Vec<TextItem>) -> Vec<TextItem> {
    if items.len() < 2 {
        return items;
    }

    // Rough (page, y) grouping with a 5pt window, x-sorted per group. This
    // fixes the OUTPUT ORDER only — stream-order line assembly downstream
    // depends on it — while script detection below is purely geometric and
    // so also reaches markers raised further than 5pt on large type.
    let y_tolerance = 5.0;
    let mut line_groups: Vec<(u32, f32, Vec<TextItem>)> = Vec::new();

    for item in items {
        let found = line_groups
            .iter_mut()
            .find(|(pg, y, _)| *pg == item.page && (item.y - *y).abs() < y_tolerance);
        if let Some((_, _, group)) = found {
            group.push(item);
        } else {
            let page = item.page;
            let y = item.y;
            line_groups.push((page, y, vec![item]));
        }
    }

    let mut ordered: Vec<TextItem> =
        Vec::with_capacity(line_groups.iter().map(|(_, _, g)| g.len()).sum());
    for (_, _, mut group) in line_groups {
        group.sort_by(|a, b| a.x.total_cmp(&b.x));
        ordered.extend(group);
    }

    let runs = detect_script_runs(&ordered);
    apply_script_runs(ordered, &runs)
}

/// Largest script-to-anchor font-size ratio. Real sub/superscripts run
/// 0.5–0.7 of the body size; 0.75 leaves room for producers that shrink less
/// while keeping small caps (≈0.8) and ordinary size changes out.
const SCRIPT_MAX_RATIO: f32 = 0.75;
/// Smallest ratio: body text beside a drop cap or a display figure is far
/// smaller than this, and never a script of it.
const SCRIPT_MIN_RATIO: f32 = 0.4;
/// Minimum |baseline offset| as a fraction of the anchor size. Level runs —
/// small caps, a smaller label on the same baseline — are not scripts.
const SCRIPT_MIN_SHIFT: f32 = 0.1;
/// Largest raise (superscript) and drop (subscript), as fractions of the
/// anchor size. Superscripts sit around 0.33–0.45 em up, subscripts
/// 0.15–0.3 em down; ruby/annotation text above a line starts near 0.9 em.
const SCRIPT_MAX_RAISE: f32 = 0.6;
const SCRIPT_MAX_DROP: f32 = 0.45;
/// Horizontal attachment window between a run and its anchor, as fractions
/// of the anchor size: kerned markers overlap slightly (negative gap), a
/// word space (≥ 0.25 em) means a separate word.
const SCRIPT_ATTACH_OVERLAP: f32 = 0.5;
const SCRIPT_ATTACH_GAP: f32 = 0.25;
/// Glyphs of one run touch: gap between consecutive glyphs, as a fraction of
/// the glyph size ("1" "," "2" of an affiliation marker).
const SCRIPT_CHAIN_GAP: f32 = 0.35;
/// Glyphs of one run share a baseline (points).
const SCRIPT_RUN_BASELINE_TOL: f32 = 0.75;
/// Markers and indices are short. A whole small-font line beside a large
/// glyph (drop cap, display number) is not a script.
const SCRIPT_MAX_RUN_CHARS: usize = 12;
const SCRIPT_MAX_GLYPH_CHARS: usize = 8;
const SCRIPT_MAX_GLYPH_LETTERS: usize = 4;
/// Digit-only runs up to this many digits fuse into the anchor as Unicode
/// super/subscript characters.
const SCRIPT_MAX_FUSED_DIGITS: usize = 4;

/// A glyph run detected as the super/subscript of `anchor`.
#[derive(Debug)]
struct ScriptRun {
    /// Item indices of the run's glyphs, in x order.
    glyphs: Vec<usize>,
    /// Item index of the normal-sized text the run is attached to.
    anchor: usize,
    /// The anchor precedes the run ("word²") rather than following it
    /// ("¹Hong Kong").
    anchor_on_left: bool,
}

/// Items that can take part in script detection, as glyph or anchor.
fn is_script_participant(item: &TextItem) -> bool {
    matches!(item.item_type, ItemType::Text | ItemType::Link(_))
        && item.font_size > 0.0
        && !item.text.trim().is_empty()
}

/// Punctuation and symbols that appear inside footnote/affiliation markers
/// and math indices, besides letters and digits.
fn is_script_marker_symbol(c: char) -> bool {
    matches!(
        c,
        '*' | '†'
            | '‡'
            | '§'
            | '¶'
            | '‖'
            | '#'
            | '∗'
            | '⁎'
            | '⋆'
            | '®'
            | '™'
            | '©'
            | ','
            | ';'
            | ':'
            | '.'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '+'
            | '-'
            | '−'
            | '–'
            | '/'
            | '\''
            | '′'
            | '″'
    )
}

/// Whether an item's text has the shape of a script glyph run: short, no
/// whitespace, letters/digits/marker symbols only. Bullets, arrows, box
/// glyphs and words are excluded here regardless of geometry.
fn is_script_glyph_text(text: &str) -> bool {
    if text.trim().len() != text.len() {
        // Edge whitespace means a word boundary was typeset around it.
        return false;
    }
    let mut chars = 0usize;
    let mut letters = 0usize;
    for c in text.chars() {
        chars += 1;
        if c.is_whitespace() {
            return false;
        }
        if c.is_alphabetic() {
            letters += 1;
        } else if !(c.is_numeric() || is_script_marker_symbol(c)) {
            return false;
        }
    }
    chars > 0 && chars <= SCRIPT_MAX_GLYPH_CHARS && letters <= SCRIPT_MAX_GLYPH_LETTERS
}

fn item_right(item: &TextItem) -> f32 {
    item.x + crate::text_utils::effective_width(item)
}

/// Whether `body` anchors a glyph run spanning `first..=last` (x order) at
/// `run_fs`. Returns the attachment gap (clamped at 0) and whether the body
/// sits to the left of the run.
fn script_anchor_gap(
    first: &TextItem,
    last: &TextItem,
    run_fs: f32,
    body: &TextItem,
) -> Option<(f32, bool)> {
    let ratio = run_fs / body.font_size;
    if !(SCRIPT_MIN_RATIO..=SCRIPT_MAX_RATIO).contains(&ratio) {
        return None;
    }
    let dy = first.y - body.y;
    if dy.abs() < body.font_size * SCRIPT_MIN_SHIFT
        || dy > body.font_size * SCRIPT_MAX_RAISE
        || dy < -body.font_size * SCRIPT_MAX_DROP
    {
        return None;
    }
    let window = (-body.font_size * SCRIPT_ATTACH_OVERLAP)..=(body.font_size * SCRIPT_ATTACH_GAP);
    let body_center = (body.x + item_right(body)) / 2.0;
    let run_center = (first.x + item_right(last)) / 2.0;
    if body_center <= run_center {
        // Body precedes the run: its LAST character faces the run. A
        // trailing space means the "marker" is a separate word (a table
        // credit "Health 1"), not a script.
        let gap = first.x - item_right(body);
        let edge_ok = body.text.chars().last().is_some_and(|c| !c.is_whitespace());
        (window.contains(&gap) && edge_ok).then_some((gap.max(0.0), true))
    } else {
        let gap = body.x - item_right(last);
        let edge_ok = body.text.chars().next().is_some_and(|c| !c.is_whitespace());
        (window.contains(&gap) && edge_ok).then_some((gap.max(0.0), false))
    }
}

/// Find every super/subscript glyph run and its anchor.
fn detect_script_runs(items: &[TextItem]) -> Vec<ScriptRun> {
    // Per page: every participant sorted by y (anchor lookup window) and the
    // glyph-shaped subset sorted by x (run assembly).
    let mut by_page: HashMap<u32, Vec<usize>> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if is_script_participant(item) {
            by_page.entry(item.page).or_default().push(idx);
        }
    }
    let mut pages: Vec<(u32, Vec<usize>)> = by_page.into_iter().collect();
    pages.sort_by_key(|(page, _)| *page);

    let mut runs = Vec::new();
    for (_, mut by_y) in pages {
        by_y.sort_by(|&a, &b| items[a].y.total_cmp(&items[b].y));
        let max_fs = by_y
            .iter()
            .map(|&i| items[i].font_size)
            .fold(0.0_f32, f32::max);
        let window = max_fs * SCRIPT_MAX_RAISE.max(SCRIPT_MAX_DROP);

        // Assemble candidate runs: glyph-shaped items on one baseline, of
        // similar size, touching in x.
        let mut glyphs: Vec<usize> = by_y
            .iter()
            .copied()
            .filter(|&i| is_script_glyph_text(&items[i].text))
            .collect();
        glyphs.sort_by(|&a, &b| {
            items[a]
                .x
                .total_cmp(&items[b].x)
                .then(items[a].y.total_cmp(&items[b].y))
        });
        let mut chains: Vec<Vec<usize>> = Vec::new();
        for g in glyphs {
            let glyph = &items[g];
            let joined = chains.iter_mut().rev().find(|chain| {
                let last = &items[*chain.last().unwrap()];
                let fs = last.font_size.max(glyph.font_size);
                let gap = glyph.x - item_right(last);
                (last.y - glyph.y).abs() <= SCRIPT_RUN_BASELINE_TOL
                    && (last.font_size - glyph.font_size).abs() <= fs * 0.2
                    && gap <= fs * SCRIPT_CHAIN_GAP
                    && gap >= -fs
            });
            match joined {
                Some(chain) => chain.push(g),
                None => chains.push(vec![g]),
            }
        }

        for chain in chains {
            let chars: usize = chain.iter().map(|&i| items[i].text.chars().count()).sum();
            if chars > SCRIPT_MAX_RUN_CHARS {
                continue;
            }
            let first = &items[chain[0]];
            let last = &items[*chain.last().unwrap()];
            let run_fs = chain
                .iter()
                .map(|&i| items[i].font_size)
                .fold(0.0_f32, f32::max);

            // Nearest anchor wins; on a tie the preceding word does — a
            // footnote reference belongs to the word before it, not to the
            // comma after it.
            let lo = by_y.partition_point(|&i| items[i].y < first.y - window);
            let mut best: Option<(f32, usize, bool)> = None;
            for &n in &by_y[lo..] {
                let body = &items[n];
                if body.y > first.y + window {
                    break;
                }
                if chain.contains(&n) {
                    continue;
                }
                let Some((gap, on_left)) = script_anchor_gap(first, last, run_fs, body) else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some((best_gap, _, best_left)) => {
                        gap < best_gap - 0.01
                            || ((gap - best_gap).abs() <= 0.01 && on_left && !best_left)
                    }
                };
                if better {
                    best = Some((gap, n, on_left));
                }
            }
            if let Some((_, anchor, anchor_on_left)) = best {
                runs.push(ScriptRun {
                    glyphs: chain,
                    anchor,
                    anchor_on_left,
                });
            }
        }
    }
    runs
}

/// The anchor edge a digit-only run may fuse onto: a footnote reference
/// follows a word or its closing punctuation ("word²", "sentence.²"), never
/// another digit ("33" + "1" of "33 1/3") — and a leading marker precedes a
/// word ("¹Hong Kong").
fn fusable_anchor_edge(anchor: &TextItem, anchor_on_left: bool) -> bool {
    if anchor_on_left {
        anchor.text.chars().last().is_some_and(|c| {
            c.is_alphabetic()
                || matches!(
                    c,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\'' | '”' | '’'
                )
        })
    } else {
        anchor
            .text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic())
    }
}

/// Fuse digit-only runs into their anchors; flag every other run.
fn apply_script_runs(mut items: Vec<TextItem>, runs: &[ScriptRun]) -> Vec<TextItem> {
    // A glyph can itself anchor a smaller run (nested scripts: "x", "2",
    // "n"). Shifts are measured from the BODY baseline, so anchor chains are
    // followed to the first item that is not a script glyph — otherwise a
    // suffix attached to a fused (removed) digit would keep a shift relative
    // to the digit's raised baseline and land on the wrong line. Fusion only
    // ever targets body text for the same reason.
    let run_of_glyph: HashMap<usize, usize> = runs
        .iter()
        .enumerate()
        .flat_map(|(ri, run)| run.glyphs.iter().map(move |&g| (g, ri)))
        .collect();
    let body_anchor_y: Vec<f32> = runs
        .iter()
        .map(|run| {
            let mut idx = run.anchor;
            for _ in 0..8 {
                match run_of_glyph.get(&idx) {
                    Some(&ri) => idx = runs[ri].anchor,
                    None => break,
                }
            }
            items[idx].y
        })
        .collect();

    let mut remove = vec![false; items.len()];
    for (ri, run) in runs.iter().enumerate() {
        let anchor = &items[run.anchor];
        let digits: usize = run
            .glyphs
            .iter()
            .map(|&g| items[g].text.chars().count())
            .sum();
        let digit_only = run
            .glyphs
            .iter()
            .all(|&g| items[g].text.chars().all(|c| c.is_ascii_digit()));
        // Strikeout boundaries block fusion (a struck word must not extend
        // its strike over a live footnote digit, and a struck digit must not
        // lose its own mark). An underlined anchor with an unmarked digit
        // DOES fuse: the drawn rule easily misses the tiny digit's overlap
        // window, and refusing costs the whole token. Visually the rule
        // spans both.
        let marks_ok = run.glyphs.iter().all(|&g| {
            let glyph = &items[g];
            anchor.is_strikeout == glyph.is_strikeout
                && (anchor.is_underline == glyph.is_underline
                    || (anchor.is_underline && !glyph.is_underline))
        });
        let fuse = digit_only
            && digits <= SCRIPT_MAX_FUSED_DIGITS
            && marks_ok
            && fusable_anchor_edge(anchor, run.anchor_on_left)
            && !run_of_glyph.contains_key(&run.anchor);

        if fuse {
            // Direction from the baseline offset (y-up): raised → superscript
            // digits (footnote refs), lowered → subscript (chemistry). NFKC
            // folds both back to plain digits, so downstream text matching
            // is unaffected.
            let raised = items[run.glyphs[0]].y > anchor.y + anchor.font_size * 0.1;
            let mapped: String = run
                .glyphs
                .iter()
                .map(|&g| map_script_digits(&items[g].text, raised))
                .collect();
            let run_left = items[run.glyphs[0]].x;
            let run_right = item_right(&items[*run.glyphs.last().unwrap()]);
            let anchor = &mut items[run.anchor];
            // The fused item spans the union of word and marker: a kerned
            // marker may end inside the word's advance or start before it.
            let anchor_right = item_right(anchor);
            if run.anchor_on_left {
                anchor.text.push_str(&mapped);
                anchor.width = anchor_right.max(run_right) - anchor.x;
            } else {
                anchor.text = mapped + &anchor.text;
                let left = anchor.x.min(run_left);
                anchor.width = anchor_right.max(run_right) - left;
                anchor.x = left;
            }
            for &g in &run.glyphs {
                remove[g] = true;
            }
        } else {
            // Materialize the run as ONE item, so line assembly,
            // `TextLine::text` and the bindings consume the detector's
            // decision instead of re-deriving run membership from geometry:
            // the glyph texts concatenate in x order (a run's glyphs touch,
            // there is no space inside "1,2,3"), the item spans the run, and
            // `baseline_shift` records the offset from the anchor baseline.
            // The head glyph keeps its font, style flags and mcid.
            let anchor_y = body_anchor_y[ri];
            let run_right = item_right(&items[*run.glyphs.last().unwrap()]);
            let text: String = run.glyphs.iter().map(|&g| items[g].text.as_str()).collect();
            let head = &mut items[run.glyphs[0]];
            head.text = text;
            head.width = run_right - head.x;
            head.baseline_shift = head.y - anchor_y;
            for &g in &run.glyphs[1..] {
                remove[g] = true;
            }
        }
    }

    items
        .into_iter()
        .zip(remove)
        .filter_map(|(item, removed)| (!removed).then_some(item))
        .collect()
}

/// Map ASCII digits to their Unicode superscript (`raised`) or subscript
/// forms. Callers guarantee digit-only input (see `merge_subscript_items`);
/// anything else passes through unchanged.
fn map_script_digits(text: &str, raised: bool) -> String {
    const SUP: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    const SUB: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
    text.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) if raised => SUP[d as usize],
            Some(d) => SUB[d as usize],
            None => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item_fs(text: &str, x: f32, y: f32, width: f32, font_size: f32) -> TextItem {
        TextItem {
            rotation: 0.0,
            advance_known: true,
            text: text.into(),
            x,
            y,
            width,
            height: font_size,
            font: "F1".into(),
            font_tag: String::new(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    fn make_merge_item(text: &str, x: f32, width: f32) -> TextItem {
        make_item_fs(text, x, 700.0, width, 12.0)
    }

    fn texts(items: &[TextItem]) -> Vec<&str> {
        items.iter().map(|i| i.text.as_str()).collect()
    }

    // ---- fusion behaviour carried over from the fusion-only pass ----

    #[test]
    fn test_merge_subscript_items_chemical_formula() {
        // NH₃: "NH" at fs=8 followed by subscript "3" at fs=4.7
        let items = vec![
            make_item_fs("NH", 78.0, 499.0, 12.0, 8.0),
            make_item_fs("3", 90.0, 496.0, 2.3, 4.7),
            make_item_fs("Cl", 100.0, 499.0, 7.0, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        // Lowered baseline → Unicode subscript form (NFKC folds back to "NH3")
        assert_eq!(merged[0].text, "NH₃");
        assert_eq!(merged[1].text, "Cl");
    }

    #[test]
    fn test_merge_subscript_items_h2o() {
        // H₂O: "H" then subscript "2" then "O"
        let items = vec![
            make_item_fs("H", 250.0, 499.0, 5.0, 8.0),
            make_item_fs("2", 255.0, 496.0, 2.3, 4.7),
            make_item_fs("O", 257.5, 499.0, 6.0, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "H₂");
        assert_eq!(merged[1].text, "O");
    }

    #[test]
    fn test_merge_subscript_items_raised_marker_becomes_superscript() {
        // Footnote reference: "word" followed by a RAISED small "2" → word²
        let mut marker = make_item_fs("2", 90.0, 502.5, 2.3, 4.7);
        marker.y = 502.5; // raised above the 499.0 parent baseline
        let items = vec![make_item_fs("word", 78.0, 499.0, 12.0, 8.0), marker];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "word²");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_far_gap() {
        // Subscript-sized item that's far from the parent should NOT merge
        let items = vec![
            make_item_fs("Text", 78.0, 499.0, 20.0, 8.0),
            make_item_fs("▶", 120.0, 498.0, 3.0, 3.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "Text");
        assert_eq!(merged[1].text, "▶");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_long_text() {
        // Long subscript-sized text should NOT merge (not a true subscript)
        let items = vec![
            make_item_fs("Title", 78.0, 499.0, 30.0, 8.0),
            make_item_fs("footnote", 108.0, 496.0, 20.0, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_subscript_items_no_merge_same_font_size() {
        // Same font size items should NOT be treated as subscripts
        let items = vec![
            make_item_fs("NH", 78.0, 499.0, 12.0, 8.0),
            make_item_fs("3", 90.0, 496.0, 2.3, 8.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_subscript_items_no_merge_non_numeric() {
        // Non-numeric subscript text (e.g. "sol", "º", "vf") should NOT merge
        let items = vec![
            make_item_fs("∆", 200.0, 639.0, 5.5, 8.0),
            make_item_fs("sol", 205.8, 636.9, 5.7, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "∆");
        assert_eq!(merged[1].text, "sol");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_parent_ends_with_digit() {
        // "33" + "1" in "33 1/3%" — parent ends with digit, should NOT merge
        let items = vec![
            make_item_fs("33", 78.0, 499.0, 10.0, 8.0),
            make_item_fs("1", 88.0, 496.0, 2.3, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "33");
        assert_eq!(merged[1].text, "1");
    }

    #[test]
    fn test_merge_subscript_items_no_merge_parent_ends_with_space() {
        // "Health " + "1" — parent ends with space (table credit), should NOT merge
        let items = vec![
            make_item_fs("Health ", 78.0, 499.0, 30.0, 8.0),
            make_item_fs("1", 108.0, 496.0, 2.3, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn subscript_digit_with_different_marks_is_not_absorbed() {
        // A struck-out word followed by an unmarked footnote digit: merging
        // would widen the parent's strikeout claim over the digit (and the
        // reverse would drop the digit's own mark). Style boundaries break
        // the merge, as in merge_text_items.
        let mut word = make_merge_item("word", 100.0, 24.0);
        word.font_size = 10.0;
        word.is_strikeout = true;
        let mut digit = make_merge_item("2", 124.5, 4.0);
        digit.font_size = 6.0;
        digit.y = word.y + 3.0;

        let merged = merge_subscript_items(vec![word.clone(), digit.clone()]);
        assert_eq!(merged.len(), 2);

        // Same marks still merge (footnote ref inside the strike).
        digit.is_strikeout = true;
        let merged = merge_subscript_items(vec![word, digit]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].text.starts_with("word"));
    }

    // ---- multi-glyph runs, leading markers, anchors ----

    #[test]
    fn multi_glyph_marker_run_is_coalesced_and_flagged() {
        // Author block: "Yibo Yan" at 11.96pt, markers at 7.97pt raised 4.3pt,
        // the commas as separate glyphs; then the body comma and next name.
        let items = vec![
            make_item_fs("Yibo Yan", 72.0, 700.0, 48.5, 11.96),
            make_item_fs("1", 120.5, 704.3, 4.4, 7.97),
            make_item_fs(",", 124.9, 704.3, 2.0, 7.97),
            make_item_fs("2", 126.9, 704.3, 4.4, 7.97),
            make_item_fs(",", 131.3, 700.0, 3.3, 11.96),
            make_item_fs("Jiahao Huo", 137.9, 700.0, 60.0, 11.96),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["Yibo Yan", "1,2", ",", "Jiahao Huo"]);
        let run = &merged[1];
        assert!((run.baseline_shift - 4.3).abs() < 1e-3, "{run:?}");
        assert!((run.line_y() - 700.0).abs() < 1e-3);
        assert!((run.x - 120.5).abs() < 1e-3);
        assert!(
            (run.x + run.width - 131.3).abs() < 1e-3,
            "run spans all glyphs"
        );
        assert!(merged.iter().filter(|i| i.is_script()).count() == 1);
    }

    #[test]
    fn leading_digit_marker_fuses_into_following_word() {
        let items = vec![
            make_item_fs("1", 72.0, 653.5, 3.9, 6.97),
            make_item_fs("Hong Kong University", 75.9, 650.0, 100.0, 9.96),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["¹Hong Kong University"]);
        assert!(
            (merged[0].x - 72.0).abs() < 1e-3,
            "fused item starts at the marker"
        );
        assert!(!merged[0].is_script());
    }

    #[test]
    fn leading_mixed_run_is_flagged_not_fused() {
        let items = vec![
            make_item_fs("3", 72.0, 640.5, 3.9, 6.97),
            make_item_fs(",", 75.9, 640.5, 1.7, 6.97),
            make_item_fs("4", 77.6, 640.5, 3.9, 6.97),
            make_item_fs("Some Institute", 81.5, 637.0, 70.0, 9.96),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["3,4", "Some Institute"]);
        assert!((merged[0].baseline_shift - 3.5).abs() < 1e-3);
    }

    #[test]
    fn marker_after_closing_punctuation_fuses() {
        let items = vec![
            make_item_fs("sentence.", 72.0, 500.0, 45.0, 10.0),
            make_item_fs("2", 117.0, 503.5, 3.6, 6.5),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["sentence.²"]);
    }

    #[test]
    fn marker_after_digits_is_flagged_not_fused() {
        // "$1,234" + raised "1": gluing would corrupt the number, so the
        // marker stays a separate flagged item ("$1,234<sup>1</sup>").
        let items = vec![
            make_item_fs("$1,234", 72.0, 500.0, 33.0, 10.0),
            make_item_fs("1", 105.0, 503.5, 3.6, 6.5),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["$1,234", "1"]);
        assert!(merged[1].is_script() && merged[1].baseline_shift > 0.0);
    }

    #[test]
    fn symbol_marker_beyond_the_rough_line_window_still_attaches() {
        // 16pt title, 10.6pt asterisk raised 6.5pt — past the 5pt rough
        // grouping window, so detection must be geometric.
        let items = vec![
            make_item_fs("A Fixture Title", 72.0, 730.0, 98.7, 16.0),
            make_item_fs("*", 170.7, 736.5, 4.1, 10.6),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["A Fixture Title", "*"]);
        assert!((merged[1].baseline_shift - 6.5).abs() < 1e-3);
    }

    #[test]
    fn level_small_run_is_not_a_script() {
        // Small caps: shrunken capitals on the SAME baseline are not a script.
        let items = vec![
            make_item_fs("R", 100.0, 500.0, 7.0, 9.98),
            make_item_fs("OLAN", 107.0, 500.0, 18.0, 6.74),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["R", "OLAN"]);
        assert!(merged.iter().all(|i| !i.is_script()));
    }

    #[test]
    fn bullet_glyph_is_not_a_script() {
        let items = vec![
            make_item_fs("•", 72.0, 501.5, 3.5, 7.0),
            make_item_fs("Item text", 76.0, 500.0, 40.0, 10.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["•", "Item text"]);
        assert!(merged.iter().all(|i| !i.is_script()));
    }

    #[test]
    fn drop_cap_body_text_is_not_a_script() {
        // Three-line drop cap: the body words beside it are far smaller than
        // the ratio window allows and too long to be markers.
        let items = vec![
            make_item_fs("T", 72.0, 500.0, 20.0, 30.0),
            make_item_fs("he", 92.0, 512.0, 10.0, 10.0),
            make_item_fs("court held", 104.0, 512.0, 45.0, 10.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(merged.len(), 3);
        assert!(merged.iter().all(|i| !i.is_script()));
    }

    #[test]
    fn separate_word_after_space_is_not_a_script() {
        // A small item a word space away is a separate word, not a marker.
        let items = vec![
            make_item_fs("Total", 72.0, 500.0, 24.0, 10.0),
            make_item_fs("2", 79.5 + 24.0, 503.0, 3.6, 6.5),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["Total", "2"]);
        assert!(!merged[1].is_script());
    }

    #[test]
    fn nested_script_shift_is_measured_from_the_body_baseline() {
        // "x" + superscript "2" (fused into the word) + a smaller "n" riding
        // on the "2": the suffix is flagged, never fused into the removed
        // digit, and its shift is relative to the body baseline so it stays
        // on the body line.
        let items = vec![
            make_item_fs("x", 100.0, 500.0, 5.0, 10.0),
            make_item_fs("2", 105.0, 503.5, 3.6, 6.5),
            make_item_fs("n", 108.6, 506.0, 2.4, 4.5),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["x²", "n"]);
        assert!(
            (merged[1].baseline_shift - 6.0).abs() < 1e-3,
            "{:?}",
            merged[1]
        );
        assert!((merged[1].line_y() - 500.0).abs() < 1e-3);
    }

    #[test]
    fn fusion_spans_the_union_of_word_and_marker() {
        // A kerned marker ending inside the word's advance must not shrink
        // the fused item.
        let items = vec![
            make_item_fs("word", 78.0, 499.0, 12.0, 8.0),
            make_item_fs("2", 87.0, 502.5, 2.0, 4.7),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["word²"]);
        assert!((merged[0].width - 12.0).abs() < 1e-3, "{}", merged[0].width);
        // Leading marker starting inside the word's box keeps the word's x.
        let items = vec![
            make_item_fs("1", 76.5, 653.5, 3.9, 6.97),
            make_item_fs("Hong Kong", 75.9, 650.0, 50.0, 9.96),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["¹Hong Kong"]);
        assert!((merged[0].x - 75.9).abs() < 1e-3);
        assert!((merged[0].width - 50.0).abs() < 1e-3);
    }

    #[test]
    fn output_order_follows_rough_lines_sorted_by_x() {
        // Ordering contract: 5pt rough groups in discovery order, x-sorted
        // within — unchanged from the fusion-only pass.
        let items = vec![
            make_item_fs("b", 200.0, 500.0, 5.0, 10.0),
            make_item_fs("a", 100.0, 500.0, 5.0, 10.0),
            make_item_fs("second", 100.0, 480.0, 30.0, 10.0),
        ];
        let merged = merge_subscript_items(items);
        assert_eq!(texts(&merged), vec!["a", "b", "second"]);
    }
}
