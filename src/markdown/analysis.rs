//! Font statistics, heading detection, and document structure analysis.

use std::collections::HashMap;

use crate::types::{TextItem, TextLine};
use log::debug;

/// Font statistics for a document
pub(crate) struct FontStats {
    pub(crate) most_common_size: f32,
    /// Font size frequency distribution (size_key → line count).
    /// Used for rarity-based heading detection.
    pub(crate) size_counts: HashMap<i32, usize>,
    /// Total number of lines counted.
    pub(crate) total_lines: usize,
}

/// Compute how rare a font size is in the document (0.0 = most common, 1.0 = unique).
/// Mirrors opendataloader's font rarity boosting approach: heading fonts appear on
/// far fewer lines than body text, so their percentile rank is high.
pub(crate) fn font_size_rarity(font_size: f32, stats: &FontStats) -> f32 {
    if stats.total_lines == 0 {
        return 0.0;
    }
    let key = (font_size * 10.0) as i32;
    let count = stats.size_counts.get(&key).copied().unwrap_or(0);
    // Rarity = 1 - (frequency ratio). A size used on 1/100 lines has rarity ~0.99.
    1.0 - (count as f32 / stats.total_lines as f32)
}

/// Calculate font stats directly from items (before grouping into lines)
pub(crate) fn calculate_font_stats_from_items(items: &[TextItem]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for item in items {
        if item.font_size >= 9.0 {
            let size_key = (item.font_size * 10.0) as i32;
            *size_counts.entry(size_key).or_insert(0) += 1;
        }
    }

    let total_lines = size_counts.values().sum();

    // Break ties by preferring the smaller font size for deterministic output
    let most_common_size = size_counts
        .iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
        })
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats {
        most_common_size,
        size_counts,
        total_lines,
    }
}

/// Calculate font stats from grouped lines
pub(crate) fn calculate_font_stats(lines: &[TextLine]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for line in lines {
        // Count once per line (first item) to give each line equal weight
        // Prevents small captions/footnotes from skewing the base
        if let Some(first) = line.items.first() {
            if first.font_size >= 9.0 {
                let size_key = (first.font_size * 10.0) as i32;
                *size_counts.entry(size_key).or_insert(0) += 1;
            }
        }
    }

    let total_lines = size_counts.values().sum();

    // Break ties by preferring the smaller font size for deterministic output
    let most_common_size = size_counts
        .iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
        })
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats {
        most_common_size,
        size_counts,
        total_lines,
    }
}

/// Determine the heading level for a bold-only line that didn't meet the font-size
/// threshold.  These are common in academic papers where section headings are bold
/// at the same size as body text.
///
/// Returns a level below the lowest font-size tier (or H2 when no tiers exist).
pub(crate) fn bold_heading_level(heading_tiers: &[f32]) -> usize {
    let level = heading_tiers.len() + 1;
    // Clamp to 1..=6 — if no font-size tiers, bold headings become H2
    // (H1 is reserved for titles which are typically larger)
    level.clamp(2, 6)
}

/// Detect TOC-style lines that contain dot leaders (e.g., "Section Name .... 42").
/// These lines should never be joined with adjacent lines into a paragraph.
/// Handles both consecutive dots ("....") and spaced dots ("...   ...").
pub(crate) fn has_dot_leaders(text: &str) -> bool {
    // Consecutive dots (4+)
    if text.contains("....") {
        return true;
    }
    // Spaced dot leaders: "..." followed by whitespace and more dots
    // Count occurrences of "..." (3+ dots) — if 2+ groups, it's a dot leader
    let mut dot_groups = 0;
    let mut consecutive_dots = 0;
    for ch in text.chars() {
        if ch == '.' {
            consecutive_dots += 1;
        } else {
            if consecutive_dots >= 3 {
                dot_groups += 1;
            }
            consecutive_dots = 0;
        }
    }
    if consecutive_dots >= 3 {
        dot_groups += 1;
    }
    dot_groups >= 2
}

/// Detect a table-of-contents entry: a line ending in a page number preceded by
/// a dot-leader group (e.g. "Measurement Lab worksheet ... 3"). `has_dot_leaders`
/// misses single-group leaders ("..."), but a trailing "<dots> <number>" is a
/// strong TOC signal on its own. Such lines must never be promoted to headings.
pub(crate) fn is_toc_entry_line(text: &str) -> bool {
    let trimmed = text.trim_end();
    let digits = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 || digits > 4 {
        return false;
    }
    let before_number = trimmed[..trimmed.len() - digits].trim_end();
    let dots = before_number
        .chars()
        .rev()
        .take_while(|c| *c == '.')
        .count();
    dots >= 3
}

/// A heading that announces a table of contents ("Contents", "Table of
/// Contents"). Lines after it on the same page are ToC entries — section
/// titles that look exactly like headings but must not be promoted.
pub(crate) fn is_toc_marker_heading(text: &str) -> bool {
    let t = text.trim().trim_end_matches(':').trim().to_lowercase();
    matches!(t.as_str(), "contents" | "table of contents")
}

/// Lines that resemble headings structurally but are display-math fragments:
/// equations ending in an equation number ("S = kB ln W, (2)") or equation
/// lead-ins ("Rearranging Equation (8) gives:"). Both carry an "(N)" equation
/// reference — but a trailing "(N)" alone is not enough: real headings end
/// with parenthesized numbers too ("Nicaea (325)", appendix numbering), so
/// the suffix form additionally requires math evidence — an "=" in the line
/// or a comma immediately before the number, both present in every display
/// equation and absent from name-plus-number headings. A bare trailing colon
/// is NOT a fragment signal either: real headings frequently end with colons
/// ("Procedure:", "Steps for Using the Microscope:").
pub(crate) fn is_heading_fragment(text: &str) -> bool {
    let t = text.trim_end();

    // A lowercase-initial one-or-two-word "heading" is a mid-sentence
    // fragment beside display math ("or inversely", "and therefore") —
    // real headings that short start uppercase. Measured as spurious
    // headings on academic docs (fire-pdf ENG-5029 / opendataloader MHS).
    {
        let words: Vec<&str> = t.split_whitespace().collect();
        if words.len() <= 2 {
            if let Some(first_alpha) = t.chars().find(|c| c.is_alphabetic()) {
                if first_alpha.is_lowercase() {
                    return true;
                }
            }
        }
    }

    fn is_equation_number(s: &str) -> bool {
        s.strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(|inner| {
                !inner.is_empty() && inner.len() <= 3 && inner.chars().all(|c| c.is_ascii_digit())
            })
    }

    // Equation-number suffix with math evidence: "S = kB ln W, (2)"
    let mut rev = t.rsplit(' ');
    let last = rev.next().unwrap_or("");
    if is_equation_number(last) {
        // Page-of-total running headers: "LIVSMEDELSVERKET PM 2 (10)"
        if let Some(prev_word) = t.rsplit(' ').nth(1) {
            if let (Ok(page), Some(total)) = (
                prev_word.parse::<u32>(),
                last.trim_start_matches('(')
                    .trim_end_matches(')')
                    .parse::<u32>()
                    .ok(),
            ) {
                if page <= total {
                    return true;
                }
            }
        }
        let punct_before = rev
            .next()
            .is_some_and(|w| w.ends_with(',') || w.ends_with(':'));
        let has_math_op = t.chars().any(|c| {
            matches!(
                c,
                '=' | '<'
                    | '>'
                    | '≤'
                    | '≥'
                    | '≪'
                    | '≫'
                    | '≈'
                    | '≠'
                    | '±'
                    | '∑'
                    | '∫'
                    | '√'
                    | '∝'
            )
        });
        if punct_before || has_math_op {
            return true;
        }
    }
    // Lead-in: ends with a colon AND references an equation number inline
    if t.ends_with(':') && t.split_whitespace().any(is_equation_number) {
        return true;
    }
    false
}

/// Compute the Y-gap threshold for paragraph break detection.
///
/// Instead of using a fixed multiple of base_size (which fails for double-spaced
/// documents), we compute the document's typical (median) line spacing and use
/// a multiplier on that. A gap significantly larger than typical indicates a
/// paragraph break.
///
/// Fallback: if we can't compute typical spacing, use base_size * 1.8.
pub(crate) fn compute_paragraph_threshold(lines: &[TextLine], base_size: f32) -> f32 {
    let fallback = base_size * 1.8;

    // Collect Y gaps between consecutive lines on the same page
    let mut gaps: Vec<f32> = Vec::new();
    let mut prev_y: Option<(u32, f32)> = None;

    for line in lines {
        if let Some((prev_page, py)) = prev_y {
            if line.page == prev_page {
                let gap = py - line.y;
                // Only consider positive gaps within a reasonable range
                // (skip huge gaps from page headers/footers)
                if gap > 0.0 && gap < base_size * 10.0 {
                    gaps.push(gap);
                }
            }
        }
        prev_y = Some((line.page, line.y));
    }

    if gaps.len() < 5 {
        return fallback;
    }

    gaps.sort_by(|a, b| a.total_cmp(b));

    let median = gaps[gaps.len() / 2];

    let threshold = (median * 1.3).max(base_size * 1.5);

    debug!(
        "paragraph_threshold: base_size={:.1} median_gap={:.1} threshold={:.1} ({} gaps sampled)",
        base_size,
        median,
        threshold,
        gaps.len()
    );

    if log::log_enabled!(log::Level::Debug) {
        // Gap histogram
        let buckets: &[f32] = &[0.0, 0.5, 1.0, 1.2, 1.5, 1.8, 2.0, 2.5, 3.0, 5.0, 10.0];
        for i in 0..buckets.len() - 1 {
            let count = gaps
                .iter()
                .filter(|&&g| {
                    let r = g / base_size;
                    r >= buckets[i] && r < buckets[i + 1]
                })
                .count();
            if count > 0 {
                debug!(
                    "  gap_ratio {:.1}-{:.1}: {}",
                    buckets[i],
                    buckets[i + 1],
                    count
                );
            }
        }
        let over = gaps.iter().filter(|&&g| g / base_size >= 10.0).count();
        if over > 0 {
            debug!("  gap_ratio 10.0+: {}", over);
        }
    }

    // Per-line detail: Y position, gap, ratio, bold, text preview, paragraph marker
    if log::log_enabled!(log::Level::Trace) {
        let mut prev: Option<(u32, f32)> = None;
        for line in lines {
            let font_size = line.items.first().map(|i| i.font_size).unwrap_or(0.0);
            let is_bold = line.items.first().map(|i| i.is_bold).unwrap_or(false);
            let text = line.text();
            let display: String = text.chars().take(80).collect();

            let (gap_str, ratio_str, marker) = if let Some((pp, py)) = prev {
                if pp == line.page {
                    let gap = py - line.y;
                    let ratio = gap / base_size;
                    let is_para = gap > threshold;
                    (
                        format!("{:8.1}", gap),
                        format!("{:8.2}", ratio),
                        if is_para { " <<PARA>>" } else { "" },
                    )
                } else {
                    ("     ---".to_string(), "     ---".to_string(), "")
                }
            } else {
                ("     ---".to_string(), "     ---".to_string(), "")
            };

            log::trace!(
                "  p={} y={:8.1} gap={} ratio={} fs={:5.1} {}  {}{}",
                line.page,
                line.y,
                gap_str,
                ratio_str,
                font_size,
                if is_bold { "B" } else { " " },
                display,
                marker
            );

            prev = Some((line.page, line.y));
        }
    }

    threshold
}

/// Discover distinct heading font-size tiers in the document.
/// Returns tiers sorted largest-first (tier 0 = H1, tier 1 = H2, …).
/// Sizes within 0.5pt are clustered into the same tier. Capped at 4 tiers.
pub(crate) fn compute_heading_tiers(lines: &[TextLine], base_size: f32) -> Vec<f32> {
    let mut heading_sizes: Vec<f32> = Vec::new();

    for line in lines {
        if let Some(first) = line.items.first() {
            if first.font_size / base_size >= 1.2 {
                // Digit-only lines (page numbers, issue numbers) must not
                // define heading tiers: a large bold folio claims tier 0 and
                // blocks the bold-size fallback for the document's real
                // same-size headings.
                let text = line.text();
                let t = text.trim();
                if !t.is_empty() && t.chars().all(|c| !c.is_alphabetic()) {
                    continue;
                }
                heading_sizes.push(first.font_size);
            }
        }
    }

    // Sort descending
    heading_sizes.sort_by(|a, b| b.total_cmp(a));

    // Cluster sizes within 0.5pt into same tier (use first value as representative)
    let mut tiers: Vec<f32> = Vec::new();
    for size in heading_sizes {
        let already_in_tier = tiers.iter().any(|&t| (t - size).abs() < 0.5);
        if !already_in_tier {
            tiers.push(size);
        }
    }

    // Books often set section headings barely above body size (e.g. 11pt
    // bold over 10pt text). When nothing clears the 1.2x ratio gate, fall
    // back to bold lines modestly larger than body so those documents still
    // get an H1 instead of every bold heading defaulting to H2.
    if tiers.is_empty() {
        let mut bold_sizes: Vec<f32> = lines
            .iter()
            .filter(|line| {
                let text = line.text();
                let t = text.trim();
                !t.is_empty() && t.chars().any(|c| c.is_alphabetic())
            })
            .filter_map(|line| line.items.first())
            .filter(|it| it.is_bold && it.font_size / base_size >= 1.05)
            .map(|it| it.font_size)
            .collect();
        bold_sizes.sort_by(|a, b| b.total_cmp(a));
        for size in bold_sizes {
            if !tiers.iter().any(|&t| (t - size).abs() < 0.5) {
                tiers.push(size);
            }
        }
    }

    // Cap at 4 tiers
    tiers.truncate(4);
    tiers
}

/// Boldness of a line judged by character mass, so a heading with an
/// unbold section-number prefix ("4. " + bold title) still counts as bold.
pub(crate) fn line_is_mostly_bold(line: &TextLine) -> bool {
    let (bold, total) = line.items.iter().fold((0usize, 0usize), |(b, t), it| {
        let n = it.text.trim().chars().count();
        (b + if it.is_bold { n } else { 0 }, t + n)
    });
    total > 0 && bold * 2 >= total
}

/// Detect header level from font size using document-specific heading tiers.
/// When tiers are available, maps tier 0→H1, tier 1→H2, etc.
/// Falls back to ratio-based thresholds when no tiers exist.
pub(crate) fn detect_header_level(
    font_size: f32,
    base_size: f32,
    heading_tiers: &[f32],
    is_bold: bool,
) -> Option<usize> {
    let ratio = font_size / base_size;

    // Tier matches are trusted below the 1.2x gate (down to 1.05x) only for
    // bold lines: sub-gate tiers come from the bold fallback, and honoring
    // them for non-bold text at the same size would promote captions.
    if (1.05..1.2).contains(&ratio) && is_bold && !heading_tiers.is_empty() {
        for (i, &tier_size) in heading_tiers.iter().enumerate() {
            if (font_size - tier_size).abs() < 0.5 {
                return Some(i + 1); // tier 0 → H1, tier 1 → H2, etc.
            }
        }
    }

    if ratio < 1.2 {
        return None; // Regular text
    }

    if !heading_tiers.is_empty() {
        // Match font_size to a tier (within 0.5pt tolerance)
        for (i, &tier_size) in heading_tiers.iter().enumerate() {
            if (font_size - tier_size).abs() < 0.5 {
                return Some(i + 1); // tier 0 → H1, tier 1 → H2, etc.
            }
        }
        // No tier match but large ratio — assign level after last tier
        if ratio >= 1.5 {
            let level = (heading_tiers.len() + 1).min(4);
            return Some(level);
        }
        // No tier match and small ratio — not a heading
        return None;
    }

    // Fallback: original ratio-based thresholds (no tiers discovered)
    if ratio >= 2.0 {
        Some(1)
    } else if ratio >= 1.5 {
        Some(2)
    } else if ratio >= 1.25 {
        Some(3)
    } else {
        Some(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(text: &str, font_size: f32, bold: bool, y: f32) -> crate::types::TextLine {
        let item = crate::types::TextItem {
            text: text.into(),
            x: 72.0,
            y,
            width: text.len() as f32 * font_size * 0.5,
            height: font_size,
            font: "Test".into(),
            font_size,
            page: 1,
            is_bold: bold,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        };
        crate::types::TextLine {
            items: vec![item],
            y,
            page: 1,
            adaptive_threshold: 0.10,
        }
    }

    #[test]
    fn digit_only_lines_do_not_define_tiers() {
        // A 14pt bold page number must not claim tier 0 — that both demotes
        // every real heading a level and blocks the bold-size fallback.
        let lines = vec![
            line_of("76", 14.0, true, 760.0),
            line_of("Replace", 11.0, true, 700.0),
            line_of("body text at eleven points", 11.0, false, 680.0),
        ];
        let tiers = compute_heading_tiers(&lines, 11.0);
        assert!(tiers.is_empty(), "page number claimed a tier: {tiers:?}");
    }

    #[test]
    fn bold_fallback_tiers_when_nothing_clears_ratio_gate() {
        // 10pt body, 11pt bold section headings (book-style): no size clears
        // 1.2x, so bold sizes modestly above body form the tiers.
        let lines = vec![
            line_of("4. Entropy", 11.0, true, 700.0),
            line_of("body text about entropy", 10.0, false, 680.0),
            line_of("5. The dynamics", 11.0, true, 500.0),
        ];
        let tiers = compute_heading_tiers(&lines, 10.0);
        assert_eq!(tiers, vec![11.0]);
        assert_eq!(detect_header_level(11.0, 10.0, &tiers, true), Some(1));
        // Non-bold text at the fallback size must not become a heading.
        assert_eq!(detect_header_level(11.0, 10.0, &tiers, false), None);
        // Non-tier body text stays regular.
        assert_eq!(detect_header_level(10.0, 10.0, &tiers, true), None);
    }

    #[test]
    fn bold_fallback_skipped_when_real_tiers_exist() {
        let lines = vec![
            line_of("Chapter One", 18.0, false, 700.0),
            line_of("bold label", 11.0, true, 600.0),
            line_of("body", 10.0, false, 580.0),
        ];
        let tiers = compute_heading_tiers(&lines, 10.0);
        assert_eq!(tiers, vec![18.0]);
        // The 11pt bold label does not match any tier and stays non-heading.
        assert_eq!(detect_header_level(11.0, 10.0, &tiers, true), None);
    }

    #[test]
    fn toc_entry_with_single_dot_group() {
        assert!(is_toc_entry_line("Measurement Lab worksheet ... 3"));
        assert!(is_toc_entry_line("Results ........ 12"));
        assert!(is_toc_entry_line("Appendix B...42"));
    }

    #[test]
    fn non_toc_lines_pass() {
        assert!(!is_toc_entry_line(
            "6.2. Expectations for Re-Hiring Employees"
        ));
        assert!(!is_toc_entry_line("What happened in 2020"));
        assert!(!is_toc_entry_line("IMPLEMENTATION"));
        // Ellipsis without a trailing page number
        assert!(!is_toc_entry_line("and so it goes ..."));
        // Long numbers are data, not page refs
        assert!(!is_toc_entry_line("ISBN ... 97814"));
    }

    #[test]
    fn toc_marker_headings() {
        assert!(is_toc_marker_heading("Contents"));
        assert!(is_toc_marker_heading("CONTENTS"));
        assert!(is_toc_marker_heading("Table of Contents"));
        assert!(is_toc_marker_heading("Table of contents:"));
        assert!(!is_toc_marker_heading("Contents of the Shipment"));
        assert!(!is_toc_marker_heading("Introduction"));
    }

    #[test]
    fn heading_fragments() {
        // Equation lead-ins: colon ending + inline equation reference
        assert!(is_heading_fragment("or inversely"));
        assert!(is_heading_fragment("and therefore"));
        assert!(!is_heading_fragment("Introduction"));
        assert!(!is_heading_fragment("iPhone Sales Strategy Overview")); // 4 words, exempt
        assert!(is_heading_fragment("Rearranging Equation (8) gives:"));
        // Display-equation neighbours ending in an equation number
        assert!(is_heading_fragment("S = kB ln W, (2)"));
        assert!(is_heading_fragment("E = mc2 (12)"));
        assert!(is_heading_fragment("x + y = z, (3)"));
        // Page-of-total running headers
        assert!(is_heading_fragment("LIVSMEDELSVERKET PM 2 (10)"));
        // Comparison-operator evidence and colon-before-number
        assert!(is_heading_fragment(
            "PLL\u{fe} PHH\u{226a} PLH\u{fe} PHL: (12)"
        ));
        // Real headings pass — including name-plus-number and colon-ended ones
        assert!(!is_heading_fragment("Nicaea (325)"));
        assert!(!is_heading_fragment(
            "\u{627}\u{644}\u{645}\u{644}\u{62d}\u{642} \u{631}\u{642}\u{645} (1)"
        ));
        assert!(!is_heading_fragment("4. Entropy"));
        assert!(!is_heading_fragment("Procedure:"));
        assert!(!is_heading_fragment("Steps for Using the Microscope:"));
        assert!(!is_heading_fragment("Changing objectives:"));
        assert!(!is_heading_fragment("Sales by Region (2024)"));
        assert!(!is_heading_fragment("Results (preliminary)"));
    }
}
