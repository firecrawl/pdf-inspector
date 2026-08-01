//! Geometric underline detection.
//!
//! PDFs have no underline font flag — underlines are drawn as separate
//! graphics: stroked horizontal lines (`l`/`S` operators) or thin filled
//! rectangles (`re`/`f`). This pass correlates those graphics with text
//! items after extraction: an item is underlined when a horizontal
//! line/thin rect sits just below its baseline and covers most of its
//! horizontal extent.
//!
//! Repeated same-span rules are treated as table/form rulings rather than
//! underlines, which avoids marking every cell in ruled tables.

use std::collections::HashSet;

use crate::types::{ItemType, PdfRect, TextItem};

/// Max thickness (pt) for a stroked line / filled rect to count as an
/// underline rule rather than a border or decorative band.
const MAX_RULE_THICKNESS: f32 = 2.0;

/// Fraction of the item's width that the rule must cover horizontally.
const MIN_X_OVERLAP: f32 = 0.6;

/// Same-span rules repeated at this many y-levels are usually table/form
/// rulings, not semantic underlines.
const MIN_REPEATED_RULE_LEVELS: usize = 3;

/// Vertical tolerance for considering two rules to be on the same row edge.
const RULE_Y_DEDUP_EPS: f32 = 2.0;

/// Horizontal span similarity required when clustering repeated rulings.
const RULE_SPAN_OVERLAP_RATIO: f32 = 0.8;
const RULE_SPAN_WIDTH_RATIO: f32 = 1.5;

/// Multiple separated rule segments on one row are usually per-column table
/// header/body separators.
const MIN_SEGMENTED_ROW_RULES: usize = 3;
const MIN_SEGMENTED_ROW_GAPS: usize = 2;
const SEGMENTED_ROW_GAP_MIN: f32 = 12.0;

/// A single rule under several widely separated items is usually a table
/// header/body separator, not a sentence underline.
const MIN_TABULAR_RULE_ITEMS: usize = 3;
const MIN_TABULAR_RULE_GAPS: usize = 2;
const TABULAR_RULE_GAP_EM: f32 = 2.0;

#[derive(Clone)]
pub(crate) struct UnderlineLine {
    pub(crate) x1: f32,
    pub(crate) y1: f32,
    pub(crate) x2: f32,
    pub(crate) y2: f32,
    pub(crate) stroke_width: f32,
    pub(crate) page: u32,
}

/// A horizontal rule candidate in page coordinates (PDF y-up).
#[derive(Clone)]
struct Rule {
    x1: f32,
    x2: f32,
    y: f32,
}

impl Rule {
    fn width(&self) -> f32 {
        self.x2 - self.x1
    }
}

fn rules_from_graphics(rects: &[PdfRect], lines: &[UnderlineLine], page: u32) -> Vec<Rule> {
    let mut rules: Vec<Rule> = Vec::new();
    for l in lines {
        if l.page != page {
            continue;
        }
        // Horizontal stroked line (tolerate slight skew).
        if l.stroke_width <= MAX_RULE_THICKNESS && (l.y1 - l.y2).abs() <= MAX_RULE_THICKNESS {
            let (x1, x2) = if l.x1 <= l.x2 {
                (l.x1, l.x2)
            } else {
                (l.x2, l.x1)
            };
            if x2 - x1 > 1.0 {
                rules.push(Rule {
                    x1,
                    x2,
                    y: (l.y1 + l.y2) / 2.0,
                });
            }
        }
    }
    for r in rects {
        if r.page != page {
            continue;
        }
        // Thin filled rect used as an underline rule. Extents are
        // normalized first: `re` operands pass through the CTM, so
        // width/height can be negative (flipped axes / negative scale) —
        // without normalization negative-width rules are missed and
        // negative-height bands sneak past the thickness check.
        let (x1, x2) = if r.width >= 0.0 {
            (r.x, r.x + r.width)
        } else {
            (r.x + r.width, r.x)
        };
        if r.height.abs() <= MAX_RULE_THICKNESS && x2 - x1 > 1.0 {
            rules.push(Rule {
                x1,
                x2,
                y: r.y + r.height / 2.0,
            });
        }
    }
    rules
}

fn discard_repeated_ruling_rules(
    rules: Vec<Rule>,
    items: &[TextItem],
    rects: &[PdfRect],
    lines: &[UnderlineLine],
    page: u32,
) -> Vec<Rule> {
    if rules.len() < MIN_REPEATED_RULE_LEVELS {
        return rules;
    }

    rules
        .iter()
        .filter(|rule| {
            // A rule snugly owned by one text line is an underline even when
            // span-similar rules repeat down the page — documents that
            // underline many full-width lines (dense CJK business docs) look
            // exactly like table rulings to the repetition check, which used
            // to discard every one of them. Table rulings fail snugness:
            // row separators extend past their cells' text (or have no text
            // on the baseline above), and multi-column matches are still
            // culled by the tabular filter afterwards.
            // Same-row segmented rules (column-header separators) are
            // always rulings — each segment snugly owns its column label,
            // so snugness must not override that check.
            !is_segmented_row_ruling_rule(rule, &rules)
                && ((has_snug_text_owner(rule, items)
                    && !has_flanking_verticals(rule, rects, lines, page))
                    || !is_repeated_ruling_rule(rule, &rules))
        })
        .cloned()
        .collect()
}

/// True when a single text item both matches the rule vertically (baseline
/// window) and horizontally contains it: the rule may not extend past the
/// item's span by more than ~0.75em on either side. Underlines are drawn to
/// the width of the text they decorate; table/form rulings span cells or
/// full table width and overshoot any single item.
/// A rule flanked by vertical strokes at its ends is a table/box border
/// row edge, not an underline — underlined text lines have no vertical
/// rules rising from their ends. Checked against raw stroked lines: a
/// near-vertical segment whose x sits at either end of the rule and whose
/// y-range covers the rule's row.
fn has_flanking_verticals(
    rule: &Rule,
    rects: &[PdfRect],
    lines: &[UnderlineLine],
    page: u32,
) -> bool {
    // A drawn rect that CONTAINS the rule vetoes rescue only with GRID
    // EVIDENCE: another drawn rect abutting it vertically (cell rows tile).
    // Height alone can't separate a table cell from a decorative callout
    // panel — genuine underlines live inside isolated filled panels, and
    // multiline table cells can be arbitrarily tall.
    let norm = |r: &PdfRect| {
        let (x_lo, x_hi) = if r.width >= 0.0 {
            (r.x, r.x + r.width)
        } else {
            (r.x + r.width, r.x)
        };
        let (y_lo, y_hi) = if r.height >= 0.0 {
            (r.y, r.y + r.height)
        } else {
            (r.y + r.height, r.y)
        };
        (x_lo, x_hi, y_lo, y_hi)
    };
    let page_rects: Vec<(f32, f32, f32, f32)> = rects
        .iter()
        .filter(|r| r.page == page && r.height.abs() > 6.0)
        .map(norm)
        .collect();
    let rect_flank = page_rects.iter().any(|&(x_lo, x_hi, y_lo, y_hi)| {
        let contains = x_lo <= rule.x1 + 2.0
            && x_hi >= rule.x2 - 2.0
            && y_lo <= rule.y + 2.0
            && y_hi >= rule.y - 2.0;
        if !contains {
            return false;
        }
        // Grid evidence: a vertically abutting neighbor box with x-overlap.
        page_rects.iter().any(|&(nx_lo, nx_hi, ny_lo, ny_hi)| {
            let x_overlap = nx_hi.min(x_hi) - nx_lo.max(x_lo);
            if x_overlap <= 10.0 {
                return false;
            }
            (ny_lo - y_hi).abs() <= 3.0 || (y_lo - ny_hi).abs() <= 3.0
        })
    });
    if rect_flank {
        return true;
    }
    lines.iter().any(|l| {
        if l.page != page || (l.x1 - l.x2).abs() > 2.0 {
            return false;
        }
        let x = (l.x1 + l.x2) / 2.0;
        let near_end = (x - rule.x1).abs() <= 6.0 || (x - rule.x2).abs() <= 6.0;
        if !near_end {
            return false;
        }
        let (y_lo, y_hi) = if l.y1 <= l.y2 {
            (l.y1, l.y2)
        } else {
            (l.y2, l.y1)
        };
        y_lo <= rule.y + 2.0 && y_hi >= rule.y - 2.0
    })
}

fn has_snug_text_owner(rule: &Rule, items: &[TextItem]) -> bool {
    // Underlines are drawn to the width of the text they decorate, but the
    // text may be split into several runs (CJK lines mix scripts and font
    // switches) — so ownership is judged against the UNION of the runs on
    // the rule's baseline row. Table/form rulings overshoot their row's
    // text (row separators span cell padding and empty columns), so they
    // fail either containment or coverage.
    let matched: Vec<&TextItem> = items
        .iter()
        .filter(|item| is_underline_candidate(item) && rule_matches_item(rule, item))
        .collect();
    if matched.is_empty() {
        return false;
    }
    let x1 = matched.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let x2 = matched
        .iter()
        .map(|i| i.x + i.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_fs = matched.iter().map(|i| i.font_size).fold(0.0, f32::max);
    let pad = (max_fs * 0.75).max(4.0);
    if rule.x1 < x1 - pad || rule.x2 > x2 + pad {
        return false;
    }
    let covered: f32 = matched.iter().map(|i| i.width).sum();
    if covered < rule.width() * 0.6 {
        return false;
    }
    // A table row also unions to the rule's span — but its cells sit apart.
    // An underlined text line is contiguous runs with word-sized gaps; any
    // column-sized hole between matched runs means this is a row ruling.
    let mut sorted = matched;
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
    sorted.windows(2).all(|pair| {
        let gap = pair[1].x - (pair[0].x + pair[0].width);
        gap <= (max_fs * 2.0).max(12.0)
    })
}

fn is_repeated_ruling_rule(rule: &Rule, rules: &[Rule]) -> bool {
    let mut y_levels: Vec<f32> = rules
        .iter()
        .filter(|other| has_similar_span(rule, other))
        .map(|other| other.y)
        .collect();

    y_levels.sort_by(|a, b| a.total_cmp(b));
    y_levels.dedup_by(|a, b| (*a - *b).abs() <= RULE_Y_DEDUP_EPS);
    y_levels.len() >= MIN_REPEATED_RULE_LEVELS
}

fn is_segmented_row_ruling_rule(rule: &Rule, rules: &[Rule]) -> bool {
    let mut row_rules: Vec<&Rule> = rules
        .iter()
        .filter(|other| (other.y - rule.y).abs() <= RULE_Y_DEDUP_EPS)
        .collect();

    if row_rules.len() < MIN_SEGMENTED_ROW_RULES {
        return false;
    }

    row_rules.sort_by(|a, b| a.x1.total_cmp(&b.x1));
    let large_gaps = row_rules
        .windows(2)
        .filter(|pair| pair[1].x1 - pair[0].x2 > SEGMENTED_ROW_GAP_MIN)
        .count();

    large_gaps >= MIN_SEGMENTED_ROW_GAPS
}

fn has_similar_span(a: &Rule, b: &Rule) -> bool {
    let a_width = a.width();
    let b_width = b.width();
    if a_width <= 1.0 || b_width <= 1.0 {
        return false;
    }

    let width_ratio = a_width.max(b_width) / a_width.min(b_width);
    if width_ratio > RULE_SPAN_WIDTH_RATIO {
        return false;
    }

    let overlap = a.x2.min(b.x2) - a.x1.max(b.x1);
    overlap >= a_width.min(b_width) * RULE_SPAN_OVERLAP_RATIO
}

fn tabular_row_separator_rule_indices(rules: &[Rule], items: &[TextItem]) -> HashSet<usize> {
    let mut tabular_rules = HashSet::new();

    for (rule_idx, rule) in rules.iter().enumerate() {
        let mut matched_items: Vec<&TextItem> = items
            .iter()
            .filter(|item| is_underline_candidate(item) && rule_matches_item(rule, item))
            .collect();

        if matched_items.len() < MIN_TABULAR_RULE_ITEMS {
            continue;
        }

        matched_items.sort_by(|a, b| a.x.total_cmp(&b.x));
        let large_gaps = matched_items
            .windows(2)
            .filter(|pair| {
                let left = pair[0];
                let right = pair[1];
                let gap = right.x - (left.x + left.width);
                let font_size = left.font_size.max(right.font_size).max(1.0);
                gap > font_size * TABULAR_RULE_GAP_EM
            })
            .count();

        if large_gaps >= MIN_TABULAR_RULE_GAPS {
            tabular_rules.insert(rule_idx);
        }
    }

    tabular_rules
}

fn is_underline_candidate(item: &TextItem) -> bool {
    matches!(item.item_type, ItemType::Text) && !item.text.trim().is_empty() && item.width > 0.0
}

fn rule_matches_item(rule: &Rule, item: &TextItem) -> bool {
    // Vertical window: underlines sit at or slightly below the baseline.
    // Latin fonts draw them at roughly 5-15% of the em below; CJK layouts
    // put them under the full em box, measured up to ~0.67em below the
    // baseline (text_dense__underline). Allow 0.72em (min 3pt) below and
    // 1pt above for rounding.
    let below = (item.font_size * 0.72).max(3.0);
    let y_min = item.y - below;
    let y_max = item.y + 1.0;
    if rule.y < y_min || rule.y > y_max {
        return false;
    }

    let ix1 = item.x;
    let ix2 = item.x + item.width;
    let min_overlap = item.width * MIN_X_OVERLAP;
    let overlap = rule.x2.min(ix2) - rule.x1.max(ix1);
    overlap >= min_overlap
}

/// Strikeout window: a rule crossing the glyphs. Strikethroughs sit at
/// roughly 20-35% of the em above the baseline (about half the x-height);
/// accept a band well inside the glyph body so baseline underlines and
/// overlines never qualify.
fn rule_strikes_item(rule: &Rule, item: &TextItem) -> bool {
    let y_min = item.y + item.font_size * 0.12;
    let y_max = item.y + item.font_size * 0.55;
    if rule.y < y_min || rule.y > y_max {
        return false;
    }

    let ix1 = item.x;
    let ix2 = item.x + item.width;
    let min_overlap = item.width * MIN_X_OVERLAP;
    let overlap = rule.x2.min(ix2) - rule.x1.max(ix1);
    overlap >= min_overlap
}

/// Mark `is_underline` on text items that have a horizontal rule just
/// below their baseline, and `is_strikeout` on items whose glyphs a rule
/// crosses at mid x-height. `items`, `rects`, and `lines` are a single
/// page's extraction output (all in PDF coordinates, y-up, where
/// `TextItem::y` is the text baseline).
pub(crate) fn mark_underlined_items(
    items: &mut [TextItem],
    rects: &[PdfRect],
    lines: &[UnderlineLine],
    page: u32,
) {
    let rules = discard_repeated_ruling_rules(
        rules_from_graphics(rects, lines, page),
        items,
        rects,
        lines,
        page,
    );
    if rules.is_empty() {
        return;
    }
    let tabular_rules = tabular_row_separator_rule_indices(&rules, items);

    // Math fraction bars are short horizontal lines with the numerator just
    // above AND the denominator just below — underline geometry from above,
    // but no underline has text hanging directly beneath it at fraction
    // distance. Only narrow rules qualify: real underlines under short
    // labels have their next text line a full line-pitch away.
    let fraction_rules: HashSet<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule.width() <= 60.0
                && items.iter().any(|item| {
                    if !is_underline_candidate(item) {
                        return false;
                    }
                    // A denominator HUGS the bar (fraction typesetting
                    // leaves ~0.1-0.2em) and is bar-sized. Both bounds
                    // matter: a short last-line of a paragraph at normal
                    // leading sits further below, and a full next text
                    // line is far wider than the rule.
                    let dy = rule.y - (item.y + item.height);
                    let overlap = rule.x2.min(item.x + item.width) - rule.x1.max(item.x);
                    dy > 0.0
                        && dy <= item.font_size * 0.3
                        && overlap > rule.width() * 0.5
                        && item.width <= rule.width() * 1.5
                })
        })
        .map(|(i, _)| i)
        .collect();

    for item in items.iter_mut() {
        if !is_underline_candidate(item) {
            continue;
        }

        for (rule_idx, rule) in rules.iter().enumerate() {
            if tabular_rules.contains(&rule_idx) {
                continue;
            }
            // The fraction guard only gates UNDERLINE marking — a rule that
            // reads as a fraction bar from below can still legitimately
            // strike through a line above it.
            if !fraction_rules.contains(&rule_idx) && rule_matches_item(rule, item) {
                item.is_underline = true;
            }
            if rule_strikes_item(rule, item) {
                item.is_strikeout = true;
            }
            if item.is_underline && item.is_strikeout {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn item(text: &str, x: f32, y: f32, width: f32, font_size: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width,
            height: font_size,
            font: "F1".to_string(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn hline(x1: f32, x2: f32, y: f32) -> UnderlineLine {
        UnderlineLine {
            x1,
            y1: y,
            x2,
            y2: y,
            stroke_width: 1.0,
            page: 1,
        }
    }

    fn cell_rect(x: f32, y: f32, width: f32, height: f32) -> PdfRect {
        PdfRect {
            x,
            y,
            width,
            height,
            page: 1,
        }
    }

    fn thin_rect(x: f32, y: f32, width: f32) -> PdfRect {
        PdfRect {
            x,
            y,
            width,
            height: 0.8,
            page: 1,
        }
    }

    #[test]
    fn stroked_line_under_baseline_marks_underline() {
        let mut items = vec![item("underlined", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(99.0, 161.0, 498.5)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(items[0].is_underline);
    }

    #[test]
    fn thin_filled_rect_under_baseline_marks_underline() {
        let mut items = vec![item("underlined", 100.0, 500.0, 60.0, 10.0)];
        let rects = vec![thin_rect(100.0, 497.8, 60.0)];
        mark_underlined_items(&mut items, &rects, &[], 1);
        assert!(items[0].is_underline);
    }

    #[test]
    fn long_rule_under_multiple_items_marks_each() {
        // One underline drawn under a whole sentence: every overlapped
        // item gets the flag.
        let mut items = vec![
            item("first", 100.0, 500.0, 40.0, 10.0),
            item("second", 145.0, 500.0, 50.0, 10.0),
        ];
        let lines = vec![hline(98.0, 200.0, 498.0)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(items[0].is_underline);
        assert!(items[1].is_underline);
    }

    #[test]
    fn line_far_below_baseline_is_not_an_underline() {
        // A horizontal rule 30pt below (section divider) must not mark.
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(90.0, 300.0, 470.0)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn thick_stroked_line_is_not_an_underline() {
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let mut line = hline(99.0, 161.0, 498.5);
        line.stroke_width = 4.0;

        mark_underlined_items(&mut items, &[], &[line], 1);

        assert!(!items[0].is_underline);
    }

    #[test]
    fn mid_glyph_rule_marks_strikeout_not_underline() {
        // Rule at ~30% of the em above the baseline crosses the glyphs.
        let mut items = vec![item("struck out", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(99.0, 161.0, 503.0)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(items[0].is_strikeout);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn baseline_rule_marks_underline_not_strikeout() {
        let mut items = vec![item("underlined", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(99.0, 161.0, 498.5)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(items[0].is_underline);
        assert!(!items[0].is_strikeout);
    }

    #[test]
    fn overline_is_neither_underline_nor_strikeout() {
        // Rule just above the cap height (overline / next line's rule).
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(99.0, 161.0, 507.0)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(!items[0].is_underline);
        assert!(!items[0].is_strikeout);
    }

    #[test]
    fn thin_filled_rect_at_mid_glyph_marks_strikeout() {
        let mut items = vec![item("struck out", 100.0, 500.0, 60.0, 10.0)];
        let rects = vec![thin_rect(100.0, 502.6, 60.0)];
        mark_underlined_items(&mut items, &rects, &[], 1);
        assert!(items[0].is_strikeout);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn line_above_baseline_is_not_an_underline() {
        // Strikethrough / overline geometry must not mark.
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![hline(90.0, 300.0, 505.0)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn insufficient_horizontal_overlap_is_not_an_underline() {
        // Rule under only a quarter of the item (e.g. neighboring cell
        // border) must not mark.
        let mut items = vec![item("wide text item", 100.0, 500.0, 100.0, 10.0)];
        let lines = vec![hline(100.0, 125.0, 498.5)];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn negative_width_rect_is_normalized_and_marks_underline() {
        // A CTM with negative x-scale (or negative `re` operands) produces
        // rects whose width is negative; the rule extents must normalize.
        let mut items = vec![item("underlined", 100.0, 500.0, 60.0, 10.0)];
        let rects = vec![PdfRect {
            x: 160.0,
            y: 497.8,
            width: -60.0,
            height: 0.8,
            page: 1,
        }];
        mark_underlined_items(&mut items, &rects, &[], 1);
        assert!(items[0].is_underline);
    }

    #[test]
    fn negative_height_band_is_not_an_underline() {
        // A 14pt band expressed with negative height must not pass the
        // thickness check via sign trickery.
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let rects = vec![PdfRect {
            x: 95.0,
            y: 509.0,
            width: 80.0,
            height: -14.0,
            page: 1,
        }];
        mark_underlined_items(&mut items, &rects, &[], 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn thick_band_is_not_an_underline() {
        // A highlight bar / filled cell background (tall rect) must not mark.
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let rects = vec![PdfRect {
            x: 95.0,
            y: 495.0,
            width: 80.0,
            height: 14.0,
            page: 1,
        }];
        mark_underlined_items(&mut items, &rects, &[], 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn vertical_line_is_not_an_underline() {
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let lines = vec![UnderlineLine {
            x1: 120.0,
            y1: 498.0,
            x2: 120.0,
            y2: 400.0,
            stroke_width: 1.0,
            page: 1,
        }];
        mark_underlined_items(&mut items, &[], &lines, 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn other_pages_graphics_do_not_mark() {
        let mut items = vec![item("text", 100.0, 500.0, 60.0, 10.0)];
        let mut line = hline(99.0, 161.0, 498.5);
        line.page = 2;
        mark_underlined_items(&mut items, &[], &[line], 1);
        assert!(!items[0].is_underline);
    }

    #[test]
    fn repeated_table_row_rules_do_not_mark_cell_text() {
        let mut items = vec![
            item("A", 110.0, 500.0, 20.0, 10.0),
            item("B", 110.0, 480.0, 20.0, 10.0),
            item("C", 110.0, 460.0, 20.0, 10.0),
        ];
        let lines = vec![
            hline(100.0, 150.0, 498.0),
            hline(100.0, 150.0, 478.0),
            hline(100.0, 150.0, 458.0),
        ];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
    }

    #[test]
    fn row_separator_under_spaced_column_labels_is_not_an_underline() {
        let mut items = vec![
            item("Date", 100.0, 500.0, 25.0, 10.0),
            item("Rate", 200.0, 500.0, 25.0, 10.0),
            item("Yield", 300.0, 500.0, 30.0, 10.0),
        ];
        let lines = vec![hline(90.0, 340.0, 498.0)];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
    }

    #[test]
    fn repeated_snug_underlines_survive_ruling_filter() {
        // Dense docs underline many full-width lines: span-similar rules at
        // 3+ y-levels used to be discarded wholesale as table rulings.
        // Each rule here snugly matches one text line, so all must mark.
        let mut items = vec![
            item("first underlined line of text", 50.0, 700.0, 300.0, 11.0),
            item("second underlined line here", 50.0, 650.0, 300.0, 11.0),
            item("third underlined line as well", 50.0, 600.0, 300.0, 11.0),
        ];
        let lines = vec![
            hline(50.0, 350.0, 697.0),
            hline(50.0, 350.0, 647.0),
            hline(50.0, 350.0, 597.0),
        ];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| item.is_underline));
    }

    #[test]
    fn snug_rescue_spans_split_runs_on_one_line() {
        // A single underlined line is often split into several runs (script
        // or font switches). The union of touching runs owns the rule.
        let mut items = vec![
            item("run one", 50.0, 700.0, 100.0, 11.0),
            item("run two", 150.5, 700.0, 100.0, 11.0),
            item("run three", 251.0, 700.0, 99.0, 11.0),
            item("other a", 50.0, 650.0, 300.0, 11.0),
            item("other b", 50.0, 600.0, 300.0, 11.0),
        ];
        let lines = vec![
            hline(50.0, 350.0, 697.0),
            hline(50.0, 350.0, 647.0),
            hline(50.0, 350.0, 597.0),
        ];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items[0].is_underline && items[1].is_underline && items[2].is_underline);
    }

    #[test]
    fn snug_rescue_denied_for_row_with_cell_gaps() {
        // A full-width rule whose baseline row is several items separated by
        // column-sized gaps is a table row separator, not an underline —
        // even when span-similar rules repeat down the page.
        let mut items = vec![
            item("cell a", 50.0, 700.0, 60.0, 11.0),
            item("cell b", 190.0, 700.0, 60.0, 11.0),
            item("cell c", 330.0, 700.0, 70.0, 11.0),
            item("cell d", 50.0, 650.0, 60.0, 11.0),
            item("cell e", 190.0, 650.0, 60.0, 11.0),
            item("cell f", 330.0, 650.0, 70.0, 11.0),
        ];
        let lines = vec![
            hline(50.0, 400.0, 697.0),
            hline(50.0, 400.0, 647.0),
            hline(50.0, 400.0, 597.0),
        ];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
    }

    #[test]
    fn snug_rescue_denied_inside_cell_box() {
        // A rule snugly under one text line but enclosed by a drawn cell
        // box that TILES with vertical neighbors (grid evidence) is a row
        // ruling of a rect-grid table. Isolated boxes (callout panels) do
        // not veto — see repeated_snug_underlines_survive_ruling_filter.
        let mut items = vec![
            item("one wide cell row", 50.0, 700.0, 300.0, 11.0),
            item("second wide cell", 50.0, 650.0, 300.0, 11.0),
            item("third wide cell", 50.0, 600.0, 300.0, 11.0),
        ];
        let lines = vec![
            hline(50.0, 350.0, 697.0),
            hline(50.0, 350.0, 647.0),
            hline(50.0, 350.0, 597.0),
        ];
        let boxes = vec![
            cell_rect(45.0, 690.0, 320.0, 50.0),
            cell_rect(45.0, 640.0, 320.0, 50.0),
            cell_rect(45.0, 590.0, 320.0, 50.0),
        ];

        mark_underlined_items(&mut items, &boxes, &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
    }

    #[test]
    fn same_row_spaced_rule_segments_do_not_mark_column_labels() {
        let mut items = vec![
            item("Date", 100.0, 500.0, 25.0, 10.0),
            item("Rate", 200.0, 500.0, 25.0, 10.0),
            item("Yield", 300.0, 500.0, 30.0, 10.0),
        ];
        let lines = vec![
            hline(98.0, 128.0, 498.0),
            hline(198.0, 228.0, 498.0),
            hline(298.0, 333.0, 498.0),
        ];

        mark_underlined_items(&mut items, &[], &lines, 1);

        assert!(items.iter().all(|item| !item.is_underline));
    }
}
