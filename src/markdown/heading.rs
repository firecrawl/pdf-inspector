//! Deterministic document-sequence heading classification.
//!
//! The line-local classifier in `convert` is intentionally conservative, but
//! that means it misses headings whose font is smaller than the document body
//! (sidebars are a common example). This module looks for repeated visual
//! styles and coherent numbering across the whole line sequence. A line is
//! promoted only when another heading-like line supplies independent support;
//! one-off bold labels do not become headings merely because they are rare.

use std::collections::{HashMap, HashSet};

use crate::types::TextLine;

use super::analysis::{bold_heading_level, detect_header_level, line_is_mostly_bold};
use super::analysis::{is_heading_fragment, is_toc_entry_line};
use super::classify::{is_caption_line, is_list_item, starts_with_bullet_marker};

const X_BUCKET_POINTS: f32 = 24.0;
const BASELINE_PEER_TOLERANCE: f32 = 2.0;
const MIN_FONT_RATIO: f32 = 0.72;
const MAX_SEQUENCE_DENSITY_PERCENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VisualStyle {
    font: String,
    x_bucket: i32,
    bold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NumberingKind {
    Decimal,
    Roman,
}

#[derive(Debug, Clone)]
struct Candidate {
    line_idx: usize,
    font_size: f32,
    style: VisualStyle,
    numbering: Option<(NumberingKind, usize, Vec<u32>)>,
}

fn dominant_font(line: &TextLine) -> Option<String> {
    let mut weights: HashMap<&str, usize> = HashMap::new();
    for item in &line.items {
        let weight = item.text.trim().chars().count().max(1);
        *weights.entry(item.font.as_str()).or_default() += weight;
    }
    weights
        .into_iter()
        .max_by(|(font_a, weight_a), (font_b, weight_b)| {
            weight_a.cmp(weight_b).then_with(|| font_b.cmp(font_a))
        })
        .map(|(font, _)| font.to_string())
}

/// Character-weighted font size for the visible title portion of a line.
///
/// Section numbers are sometimes emitted as a separate, smaller text item
/// (or even as superscript). Using the first item's size would then reject the
/// whole heading even though the title itself has the document's heading
/// size. Character weighting makes the longer title win while preserving the
/// exact size for ordinary one-item lines.
fn dominant_font_size(line: &TextLine) -> Option<f32> {
    let mut weights: HashMap<i32, usize> = HashMap::new();
    for item in &line.items {
        let bucket = (item.font_size * 10.0).round() as i32;
        let weight = item.text.trim().chars().count().max(1);
        *weights.entry(bucket).or_default() += weight;
    }
    weights
        .into_iter()
        .max_by(|(size_a, weight_a), (size_b, weight_b)| {
            weight_a.cmp(weight_b).then_with(|| size_a.cmp(size_b))
        })
        .map(|(bucket, _)| bucket as f32 / 10.0)
}

fn document_body_font(lines: &[TextLine]) -> Option<String> {
    let mut weights: HashMap<&str, usize> = HashMap::new();
    for line in lines {
        for item in &line.items {
            if item.is_bold {
                continue;
            }
            let weight = item.text.trim().chars().count();
            *weights.entry(item.font.as_str()).or_default() += weight;
        }
    }
    weights
        .into_iter()
        .max_by(|(font_a, weight_a), (font_b, weight_b)| {
            weight_a.cmp(weight_b).then_with(|| font_b.cmp(font_a))
        })
        .map(|(font, _)| font.to_string())
}

fn document_body_x_bucket(lines: &[TextLine]) -> Option<i32> {
    let mut weights: HashMap<i32, usize> = HashMap::new();
    for line in lines {
        let Some(first) = line.items.first() else {
            continue;
        };
        if line_is_mostly_bold(line) {
            continue;
        }
        let weight = line.text().trim().chars().count();
        *weights
            .entry((first.x / X_BUCKET_POINTS).round() as i32)
            .or_default() += weight;
    }
    weights
        .into_iter()
        .max_by(|(bucket_a, weight_a), (bucket_b, weight_b)| {
            weight_a.cmp(weight_b).then_with(|| bucket_b.cmp(bucket_a))
        })
        .map(|(bucket, _)| bucket)
}

fn visual_style(line: &TextLine) -> Option<VisualStyle> {
    let first = line.items.first()?;
    Some(VisualStyle {
        font: dominant_font(line)?,
        x_bucket: (first.x / X_BUCKET_POINTS).round() as i32,
        bold: line_is_mostly_bold(line),
    })
}

/// Shared with `analysis::starts_with_numbering_prefix` so the veto
/// exemption and the heading parser agree on what a roman numeral is.
pub(super) fn roman_value(token: &str) -> Option<u32> {
    if token.is_empty() || token.len() > 8 {
        return None;
    }
    let value = |c| match c {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        _ => None,
    };
    let values: Vec<u32> = token.chars().map(value).collect::<Option<_>>()?;
    let mut total = 0i32;
    for (idx, current) in values.iter().copied().enumerate() {
        if values.get(idx + 1).is_some_and(|&next| current < next) {
            total -= current as i32;
        } else {
            total += current as i32;
        }
    }
    (total > 0).then_some(total as u32)
}

fn parse_numbering(text: &str) -> Option<(NumberingKind, usize, Vec<u32>)> {
    let first = text.split_whitespace().next()?;
    let has_delimiter = first.ends_with(['.', ')', ':']);
    if !has_delimiter {
        return None;
    }
    let token = first.trim_end_matches(['.', ')', ':']);
    if token.is_empty() {
        return None;
    }

    let decimal: Option<Vec<u32>> = token
        .split('.')
        .map(|part| {
            (!part.is_empty() && part.len() <= 3 && part.chars().all(|c| c.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect();
    if let Some(parts) = decimal.filter(|parts| !parts.is_empty()) {
        return Some((NumberingKind::Decimal, parts.len(), parts));
    }

    let roman = roman_value(token)?;
    Some((NumberingKind::Roman, 1, vec![roman]))
}

fn has_additional_decimal_numbering(text: &str) -> bool {
    text.split_whitespace().skip(1).any(|word| {
        let token = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let mut parts = token.split('.').filter(|part| !part.is_empty());
        let Some(first) = parts.next() else {
            return false;
        };
        first.chars().all(|c| c.is_ascii_digit())
            && parts.clone().next().is_some()
            && parts.all(|part| part.chars().all(|c| c.is_ascii_digit()))
    })
}

fn numbering_forms_hierarchy(left: &[u32], right: &[u32]) -> bool {
    left.len() != right.len() && (left.starts_with(right) || right.starts_with(left))
}

/// A compact parent/child numbering run is more likely an ordered list than
/// a document section hierarchy. Genuine section headings normally have body
/// content between levels; requiring at least two intervening lines keeps
/// nested `1.` / `1.1.` list items in the list formatter.
fn numbering_has_section_separation(
    left: &Candidate,
    right: &Candidate,
    lines: &[TextLine],
) -> bool {
    lines[left.line_idx].page != lines[right.line_idx].page
        || left.line_idx.abs_diff(right.line_idx) >= 3
}

/// Fixed-size sidebar labels need stronger evidence than typography alone.
///
/// Table headers and parallel-column fragments often repeat the same small,
/// bold font at a displaced x position. Their peer text may survive as a
/// separate line or may already be grouped into the same `TextLine`.
fn has_displaced_baseline_peer(lines: &[TextLine], line_idx: usize) -> bool {
    let line = &lines[line_idx];
    let Some(x) = line.items.first().map(|item| item.x) else {
        return false;
    };

    let mut items: Vec<_> = line.items.iter().collect();
    items.sort_by(|left, right| left.x.total_cmp(&right.x));
    if items.windows(2).any(|pair| {
        let left_edge = pair[0].x + pair[0].width.max(0.0);
        pair[1].x - left_edge >= X_BUCKET_POINTS
    }) {
        return true;
    }

    lines.iter().enumerate().any(|(other_idx, other)| {
        other_idx != line_idx
            && other.page == line.page
            && (other.y - line.y).abs() <= BASELINE_PEER_TOLERANCE
            && other
                .items
                .first()
                .is_some_and(|item| (item.x - x).abs() >= X_BUCKET_POINTS)
    })
}

fn complete_sidebar_label(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.ends_with('-') {
        return false;
    }

    let last_word = trimmed
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_ascii_lowercase();
    const CONTINUATION_WORDS: &[&str] = &[
        "a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to",
        "with",
    ];
    if CONTINUATION_WORDS.contains(&last_word.as_str()) {
        return false;
    }

    // Margin references such as "G 02" are navigation codes, not headings.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    !(words.len() == 2
        && words[0].chars().count() == 1
        && words[0].chars().all(|c| c.is_alphabetic())
        && words[1].chars().all(|c| c.is_ascii_digit()))
}

fn title_like(text: &str, numbered: bool, bold: bool) -> bool {
    let trimmed = text.trim();
    let word_count = trimmed.split_whitespace().count();
    if !(1..=12).contains(&word_count)
        || !(4..=140).contains(&trimmed.chars().count())
        || !trimmed.chars().any(|c| c.is_alphabetic())
        || trimmed.ends_with(['.', ',', ';'])
    {
        return false;
    }
    if starts_with_bullet_marker(trimmed)
        || is_caption_line(trimmed)
        || is_toc_entry_line(trimmed)
        || is_heading_fragment(trimmed)
    {
        return false;
    }
    // A visible numbered prefix is allowed through even though the generic
    // list recognizer also matches it; the sequence checks below distinguish
    // a section run from an ordinary ordered list.
    if !numbered && is_list_item(trimmed) {
        return false;
    }

    let alpha_words: Vec<&str> = trimmed
        .split_whitespace()
        .filter(|word| word.chars().any(|c| c.is_alphabetic()))
        .collect();
    let capitalized = alpha_words
        .iter()
        .filter(|word| {
            word.chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(|c| c.is_uppercase())
        })
        .count();
    numbered || bold || capitalized * 2 >= alpha_words.len().max(1)
}

fn sequence_level(candidate: &Candidate, base_size: f32, heading_tiers: &[f32]) -> usize {
    if let Some((_, depth, _)) = &candidate.numbering {
        return (*depth).clamp(1, 6);
    }
    detect_header_level(
        candidate.font_size,
        base_size,
        heading_tiers,
        candidate.style.bold,
    )
    .unwrap_or_else(|| bold_heading_level(heading_tiers))
}

/// Return heading levels supported by a repeated visual or numbering sequence.
///
/// `excluded_lines` contains wrapped bold paragraphs, chart labels, and lines
/// with explicit non-heading structure roles. They cannot support another
/// line's candidacy, which prevents table headers and list labels from
/// manufacturing a false sequence.
pub(super) fn classify_heading_sequences(
    lines: &[TextLine],
    base_size: f32,
    heading_tiers: &[f32],
    isolated_lines: &HashSet<usize>,
    excluded_lines: &HashSet<usize>,
) -> HashMap<usize, usize> {
    let body_font = document_body_font(lines);
    let body_x_bucket = document_body_x_bucket(lines);
    let mut candidates = Vec::new();

    for (line_idx, line) in lines.iter().enumerate() {
        if excluded_lines.contains(&line_idx) {
            log::trace!(
                "heading sequence excludes line {}: {:?}",
                line_idx,
                line.text()
            );
            continue;
        }
        if line.items.is_empty() {
            continue;
        }
        let Some(font_size) = dominant_font_size(line) else {
            continue;
        };
        if font_size < base_size * MIN_FONT_RATIO {
            continue;
        }
        let text = line.text();
        let numbering = parse_numbering(text.trim());
        if numbering.is_some() && has_additional_decimal_numbering(text.trim()) {
            continue;
        }
        let Some(style) = visual_style(line) else {
            continue;
        };
        if !title_like(text.trim(), numbering.is_some(), style.bold) {
            continue;
        }
        let candidate = Candidate {
            line_idx,
            font_size,
            style,
            numbering,
        };
        log::trace!(
            "heading sequence candidate {}: page={} style={:?} size={:.1} isolated={} text={:?}",
            line_idx,
            line.page,
            candidate.style,
            candidate.font_size,
            isolated_lines.contains(&line_idx),
            text.trim()
        );
        candidates.push(candidate);
    }

    log::debug!(
        "heading sequence: {} lines, {} candidates, {} excluded",
        lines.len(),
        candidates.len(),
        excluded_lines.len()
    );

    let mut decisions = HashMap::new();
    let eligible_line_count = lines
        .iter()
        .enumerate()
        .filter(|(line_idx, line)| !excluded_lines.contains(line_idx) && !line.items.is_empty())
        .count();
    let sparse_candidate_population =
        candidates.len() * 100 <= eligible_line_count * MAX_SEQUENCE_DENSITY_PERCENT;

    // Repeated visual styles: same font, emphasis, and approximate indent.
    // Repetition alone is weak evidence because captions, author lists, and
    // table rows repeat too. Promote only a genuinely displaced smaller
    // sidebar style, or a style containing a coherent numbered run.
    let mut visual_groups: HashMap<VisualStyle, Vec<&Candidate>> = HashMap::new();
    for candidate in &candidates {
        visual_groups
            .entry(candidate.style.clone())
            .or_default()
            .push(candidate);
    }
    for group in visual_groups.values() {
        let max_sequence_lines = (eligible_line_count * MAX_SEQUENCE_DENSITY_PERCENT / 100).max(4);
        if group.len() < 2 || group.len() > max_sequence_lines {
            continue;
        }
        let style = &group[0].style;
        let distinct_bold_face =
            style.bold && body_font.as_ref().is_none_or(|body| style.font != *body);
        let supported_numbered: Vec<&Candidate> = group
            .iter()
            .copied()
            .filter(|candidate| {
                candidate.numbering.is_some()
                    && (candidate.font_size >= base_size * 1.05
                        || (candidate.style.bold
                            && body_font
                                .as_ref()
                                .is_none_or(|body| candidate.style.font != *body)))
            })
            .collect();
        let mut hierarchical_lines = HashSet::new();
        for (idx, left) in supported_numbered.iter().enumerate() {
            let (left_kind, _, left_parts) = left.numbering.as_ref().expect("filtered above");
            for right in supported_numbered.iter().skip(idx + 1) {
                let (right_kind, _, right_parts) =
                    right.numbering.as_ref().expect("filtered above");
                if left_kind == right_kind
                    && numbering_forms_hierarchy(left_parts, right_parts)
                    && numbering_has_section_separation(left, right, lines)
                {
                    hierarchical_lines.insert(left.line_idx);
                    hierarchical_lines.insert(right.line_idx);
                }
            }
        }
        let sidebar_size_span = group
            .iter()
            .map(|candidate| candidate.font_size)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), size| {
                (min.min(size), max.max(size))
            });
        let distinct_sidebar_labels: HashSet<String> = group
            .iter()
            .map(|candidate| lines[candidate.line_idx].text().trim().to_lowercase())
            .collect();
        let fixed_size_sidebar_evidence = sidebar_size_span.1 - sidebar_size_span.0 < 0.4
            && distinct_sidebar_labels.len() >= 2
            && group.iter().all(|candidate| {
                complete_sidebar_label(&lines[candidate.line_idx].text())
                    && !has_displaced_baseline_peer(lines, candidate.line_idx)
            });
        let displaced_sidebar = distinct_bold_face
            && sparse_candidate_population
            && (sidebar_size_span.1 - sidebar_size_span.0 >= 0.4 || fixed_size_sidebar_evidence)
            && group
                .iter()
                .all(|candidate| lines[candidate.line_idx].page == lines[group[0].line_idx].page)
            && group
                .windows(2)
                .all(|pair| pair[1].line_idx.saturating_sub(pair[0].line_idx) >= 4)
            && body_x_bucket.is_some_and(|body_x| (style.x_bucket - body_x).abs() >= 4)
            && group
                .iter()
                .all(|candidate| candidate.font_size < base_size * 0.95);
        if hierarchical_lines.is_empty() && !displaced_sidebar {
            continue;
        }
        for candidate in group {
            if !displaced_sidebar && !hierarchical_lines.contains(&candidate.line_idx) {
                continue;
            }
            let evidence = if hierarchical_lines.contains(&candidate.line_idx) {
                "numbering hierarchy"
            } else {
                "sidebar style"
            };
            log::debug!(
                "heading sequence promotes line {} via {} {:?}",
                candidate.line_idx,
                evidence,
                lines[candidate.line_idx].text()
            );
            decisions.insert(
                candidate.line_idx,
                sequence_level(candidate, base_size, heading_tiers),
            );
        }
    }

    decisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemType, TextItem};

    fn line(text: &str, y: f32, x: f32, size: f32, font: &str, bold: bool) -> TextLine {
        TextLine {
            items: vec![TextItem {
                text: text.into(),
                x,
                y,
                width: text.len() as f32 * size * 0.45,
                height: size,
                font: font.into(),
                font_size: size,
                page: 1,
                is_bold: bold,
                is_italic: false,
                is_underline: false,
                is_strikeout: false,
                item_type: ItemType::Text,
                mcid: None,
            }],
            y,
            page: 1,
            adaptive_threshold: 0.1,
        }
    }

    #[test]
    fn repeated_sidebar_style_promotes_smaller_headings() {
        let mut lines = vec![
            line(
                "Body paragraph with ordinary prose.",
                700.0,
                72.0,
                13.0,
                "Body",
                false,
            ),
            line(
                "Cellular Cycle and Replication",
                600.0,
                462.0,
                10.5,
                "SidebarBold",
                true,
            ),
            line(
                "More body paragraph text continues here.",
                500.0,
                72.0,
                13.0,
                "Body",
                false,
            ),
            line(
                "Another body paragraph separates sidebar sections.",
                470.0,
                72.0,
                13.0,
                "Body",
                false,
            ),
            line(
                "The main narrative continues down the page.",
                440.0,
                72.0,
                13.0,
                "Body",
                false,
            ),
            line(
                "Mitosis and Meiosis",
                400.0,
                462.0,
                11.0,
                "SidebarBold",
                true,
            ),
        ];
        for idx in 0..6 {
            lines.push(line(
                &format!("Additional body paragraph {idx} continues here."),
                300.0 - idx as f32 * 20.0,
                72.0,
                13.0,
                "Body",
                false,
            ));
        }
        let isolated = HashSet::from([1usize]);
        let decisions = classify_heading_sequences(&lines, 13.0, &[], &isolated, &HashSet::new());
        assert!(decisions.contains_key(&1));
        assert!(decisions.contains_key(&5));
    }

    #[test]
    fn singleton_bold_label_does_not_form_sequence() {
        let lines = vec![
            line(
                "Body paragraph with ordinary prose.",
                700.0,
                72.0,
                12.0,
                "Body",
                false,
            ),
            line("Important Label", 650.0, 72.0, 12.0, "Bold", true),
            line(
                "The explanation continues as prose.",
                630.0,
                72.0,
                12.0,
                "Body",
                false,
            ),
        ];
        let decisions = classify_heading_sequences(
            &lines,
            12.0,
            &[],
            &HashSet::from([1usize]),
            &HashSet::new(),
        );
        assert!(decisions.is_empty());
    }

    #[test]
    fn excluded_bold_run_cannot_support_peer() {
        let lines = vec![
            line("First Bold Sentence", 700.0, 72.0, 12.0, "Bold", true),
            line("Second Bold Sentence", 680.0, 72.0, 12.0, "Bold", true),
        ];
        let decisions = classify_heading_sequences(
            &lines,
            12.0,
            &[],
            &HashSet::from([0usize]),
            &HashSet::from([1usize]),
        );
        assert!(decisions.is_empty());
    }

    #[test]
    fn coherent_numbered_hierarchy_is_promoted() {
        let lines = vec![
            line("7. Theory", 700.0, 72.0, 12.0, "Section", true),
            line("body prose", 680.0, 72.0, 12.0, "Body", false),
            line("body prose continues", 660.0, 72.0, 12.0, "Body", false),
            line("7.1. Free Vortex", 500.0, 72.0, 12.0, "Section", true),
        ];
        let decisions = classify_heading_sequences(
            &lines,
            12.0,
            &[],
            &HashSet::from([0usize, 3usize]),
            &HashSet::new(),
        );
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn dominant_title_size_survives_smaller_number_prefix() {
        let mut parent = line("7.", 700.0, 72.0, 6.0, "Section", true);
        parent.items.push(TextItem {
            text: "Theory".into(),
            x: 86.0,
            y: 700.0,
            width: 38.0,
            height: 12.0,
            font: "Section".into(),
            font_size: 12.0,
            page: 1,
            is_bold: true,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        });
        let lines = vec![
            parent,
            line("body prose", 680.0, 72.0, 12.0, "Body", false),
            line("body prose continues", 660.0, 72.0, 12.0, "Body", false),
            line("7.1. Free Vortex", 500.0, 72.0, 12.0, "Section", true),
        ];

        let decisions =
            classify_heading_sequences(&lines, 12.0, &[], &HashSet::new(), &HashSet::new());
        assert_eq!(decisions.get(&0), Some(&1));
        assert_eq!(decisions.get(&3), Some(&2));
    }

    #[test]
    fn compact_nested_ordered_list_is_not_promoted() {
        let lines = vec![
            line("1. Configure Server", 700.0, 72.0, 12.0, "ListBold", true),
            line("1.1. Install Package", 680.0, 72.0, 12.0, "ListBold", true),
            line("Explanation", 660.0, 72.0, 12.0, "Body", false),
        ];
        let decisions = classify_heading_sequences(
            &lines,
            12.0,
            &[],
            &HashSet::from([0usize, 1usize]),
            &HashSet::new(),
        );
        assert!(decisions.is_empty());
    }

    #[test]
    fn complete_three_heading_sidebar_style_is_promoted() {
        let mut lines = Vec::new();
        for idx in 0..18 {
            if idx == 1 || idx == 6 || idx == 11 {
                let size = match idx {
                    1 => 10.2,
                    6 => 10.7,
                    _ => 10.4,
                };
                lines.push(line(
                    &format!("Sidebar Topic {idx}"),
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    size,
                    "SidebarBold",
                    true,
                ));
            } else {
                lines.push(line(
                    &format!("Ordinary body paragraph number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }

        let decisions =
            classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &HashSet::new());
        assert_eq!(decisions.len(), 3);
        assert!(decisions.contains_key(&1));
        assert!(decisions.contains_key(&6));
        assert!(decisions.contains_key(&11));
    }

    #[test]
    fn fixed_size_sidebar_style_is_promoted() {
        let mut lines = Vec::new();
        for idx in 0..12 {
            if idx == 1 || idx == 6 {
                lines.push(line(
                    &format!("Sidebar Topic {idx}"),
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    10.5,
                    "SidebarBold",
                    true,
                ));
            } else {
                lines.push(line(
                    &format!("Ordinary body paragraph number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }

        let decisions =
            classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &HashSet::new());
        assert_eq!(decisions.len(), 2);
        assert!(decisions.contains_key(&1));
        assert!(decisions.contains_key(&6));
    }

    #[test]
    fn fixed_size_parallel_table_headers_are_not_promoted() {
        let mut lines = Vec::new();
        for idx in 0..12 {
            if idx == 1 || idx == 7 {
                lines.push(line(
                    &format!("Table Column {idx}"),
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    10.5,
                    "TableBold",
                    true,
                ));
            } else {
                lines.push(line(
                    &format!("Ordinary body paragraph number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }
        // These lines occupy another table column on the exact header
        // baselines. They may be separated in reading order, so line-index
        // adjacency is not sufficient to recognize the table shape.
        lines.push(line("Peer value one.", 675.0, 300.0, 10.5, "Body", false));
        lines.push(line("Peer value two.", 525.0, 300.0, 10.5, "Body", false));

        let decisions =
            classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn fixed_size_same_line_table_cells_are_not_promoted() {
        let mut lines = Vec::new();
        for idx in 0..12 {
            if idx == 1 || idx == 7 {
                let mut table_row = line(
                    &format!("Table Label {idx}"),
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    10.5,
                    "TableBold",
                    true,
                );
                let mut peer = line("Peer", table_row.y, 620.0, 10.5, "TableBold", true);
                table_row.items.append(&mut peer.items);
                lines.push(table_row);
            } else {
                lines.push(line(
                    &format!("Ordinary body paragraph number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }

        let decisions =
            classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn repeated_fixed_size_table_label_is_not_promoted() {
        let mut lines = Vec::new();
        for idx in 0..12 {
            if idx == 1 || idx == 7 {
                lines.push(line(
                    "Carrying",
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    10.5,
                    "TableBold",
                    true,
                ));
            } else {
                lines.push(line(
                    &format!("Ordinary body paragraph number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }

        let decisions =
            classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn excluded_lines_do_not_dilute_sidebar_density() {
        let mut lines = Vec::new();
        for idx in 0..10 {
            if idx == 1 || idx == 6 {
                lines.push(line(
                    &format!("Sidebar Topic {idx}"),
                    700.0 - idx as f32 * 25.0,
                    462.0,
                    10.5,
                    "SidebarBold",
                    true,
                ));
            } else {
                lines.push(line(
                    &format!("Excluded chart label number {idx}."),
                    700.0 - idx as f32 * 25.0,
                    72.0,
                    13.0,
                    "Body",
                    false,
                ));
            }
        }
        let excluded = HashSet::from([0usize, 2, 3, 4, 5, 7, 8, 9]);

        let decisions = classify_heading_sequences(&lines, 13.0, &[], &HashSet::new(), &excluded);
        assert!(decisions.is_empty());
    }

    #[test]
    fn roman_and_decimal_numbers_do_not_form_a_hierarchy() {
        let lines = vec![
            line("I. Theory", 700.0, 72.0, 12.0, "Section", true),
            line("body prose", 680.0, 72.0, 12.0, "Body", false),
            line("1.1. Free Vortex", 500.0, 72.0, 12.0, "Section", true),
        ];
        let decisions = classify_heading_sequences(
            &lines,
            12.0,
            &[],
            &HashSet::from([0usize, 2usize]),
            &HashSet::new(),
        );
        assert!(decisions.is_empty());
    }

    #[test]
    fn unstyled_numbered_references_are_not_promoted() {
        let lines = vec![
            line(
                "Body paragraph with ordinary prose.",
                700.0,
                48.0,
                10.0,
                "Body",
                false,
            ),
            line(
                "2. Statistics Canada registered apprenticeship table",
                300.0,
                48.0,
                8.0,
                "Reference",
                false,
            ),
            line(
                "3. Statistics Canada farm gate table",
                280.0,
                48.0,
                8.0,
                "Reference",
                false,
            ),
        ];
        let decisions =
            classify_heading_sequences(&lines, 10.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn body_size_numbered_instructions_are_not_promoted() {
        let lines = vec![
            line(
                "1. The following entries to the table are removed:",
                700.0,
                120.0,
                12.0,
                "Body",
                false,
            ),
            line(
                "2. The following entries are added to the table:",
                500.0,
                120.0,
                12.0,
                "Body",
                false,
            ),
        ];
        let decisions =
            classify_heading_sequences(&lines, 12.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn table_rows_with_embedded_numbering_are_not_promoted() {
        let lines = vec![
            line("1. Values 1.1 Awareness", 700.0, 48.0, 11.0, "Bold", true),
            line("2. Systems 2.1 Thinking", 680.0, 48.0, 11.0, "Bold", true),
        ];
        let decisions =
            classify_heading_sequences(&lines, 11.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }

    #[test]
    fn repeated_chart_labels_do_not_form_a_sidebar_sequence() {
        let lines = vec![
            line("Body paragraph", 700.0, 48.0, 12.0, "Body", false),
            line("STRONGLY", 600.0, 420.0, 10.0, "Label", true),
            line("AGREE", 580.0, 420.0, 10.0, "Label", true),
            line("STRONGLY", 400.0, 520.0, 10.0, "Label", true),
            line("DISAGREE", 380.0, 520.0, 10.0, "Label", true),
        ];
        let decisions =
            classify_heading_sequences(&lines, 12.0, &[], &HashSet::new(), &HashSet::new());
        assert!(decisions.is_empty());
    }
}
