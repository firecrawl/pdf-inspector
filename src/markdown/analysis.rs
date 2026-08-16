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
/// True when the line opens with a section number ("3.", "2.1.4", "IV)").
///
/// Mirrors the acceptance of `heading::parse_numbering` rather than the
/// stricter `convert::starts_with_section_number`, which deliberately
/// requires two components because it bypasses isolation checks. Here a
/// single "1." counts: numbering is independent evidence of a heading, and
/// `heading.rs` applies its numbered-prefix allowance *after* consulting
/// `is_heading_fragment`, so without this exemption a numbered
/// sentence-case heading would be vetoed before that allowance can run.
fn starts_with_numbering_prefix(t: &str) -> bool {
    let Some(first) = t.split_whitespace().next() else {
        return false;
    };
    let has_delimiter = first.ends_with(['.', ')', ':']);
    let token = first.trim_end_matches(['.', ')', ':']);
    if token.is_empty() {
        return false;
    }
    let parts: Vec<&str> = token.split('.').collect();
    let decimal = parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()));
    if decimal {
        // "1." / "2.1." carry a delimiter; "2.3 Title" is written without
        // one, so a multi-component number is accepted bare. A bare single
        // number ("3 apples") is not — that is ordinary prose.
        return has_delimiter || parts.len() >= 2;
    }
    // Roman numerals go through the heading parser's own grammar so the two
    // agree: uppercase I/V/X/L/C only, at most 8 characters. A looser rule
    // here would exempt markers the parser rejects — "iv)" or "d)" from an
    // alphabetical list — letting an ordinary list item bypass the veto and
    // reach heading promotion.
    //
    // A delimiter is also required: a bare leading "I" is the pronoun far
    // more often than a section number.
    has_delimiter && crate::markdown::heading::roman_value(token).is_some()
}

/// True when the line reads as a title rather than a sentence: every
/// content word (ignoring minor words) starts uppercase. Used to spare real
/// headings from the dangling-verb veto — "Bond Yields" is a section title,
/// "the method yields" is a stranded clause, and only the casing tells them
/// apart.
fn looks_title_case(t: &str) -> bool {
    const MINOR: &[&str] = &[
        "a", "an", "the", "of", "and", "or", "for", "to", "in", "on", "at", "by", "with", "from",
        "as", "is", "are", "that", "than", "into",
    ];
    let mut content = 0usize;
    let mut capitalized = 0usize;
    for w in t.split_whitespace() {
        let cleaned: String = w.chars().filter(|c| c.is_alphabetic()).collect();
        if cleaned.is_empty() {
            continue;
        }
        if MINOR.contains(&cleaned.to_lowercase().as_str()) {
            continue;
        }
        content += 1;
        if cleaned.chars().next().is_some_and(char::is_uppercase) {
            capitalized += 1;
        }
    }
    // A single content word ("Yields") is a title by default.
    content == 0 || capitalized == content
}

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

    // Dangling clause: a stranded sentence lead-in ends on a relational
    // verb with no terminal punctuation — "Note that the exact error equals"
    // left ahead of its formula when a phantom table dissolved.
    //
    // Gated on the line reading as prose rather than a title. Case is the
    // discriminator the trailing word alone cannot provide: a heading is
    // title case ("Bond Yields", "The Method Yields") while a stranded
    // lead-in is sentence case ("the method yields"). Without this gate the
    // veto eats real headings — "Bond Yields", "Crop Yields" and any wrapped
    // title-case heading the preprocessor failed to merge.
    if !t.ends_with(['.', '!', '?', ':', ';', ')', ']'])
        && !looks_title_case(t)
        && !starts_with_numbering_prefix(t)
    {
        if let Some(last) = t.split_whitespace().next_back() {
            let word: String = last
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            // Relational verbs only, and only those with no common noun
            // sense. "yields" was dropped for exactly that reason: "Bond
            // Yields" is a real section title. Function words, copulas and
            // auxiliaries were measured and rejected outright — a heading
            // that wraps across lines ends on those, and suppressing them
            // destroyed real IRS Publication 17 headings.
            const DANGLING_TAIL: &[&str] =
                &["equals", "denotes", "implies", "satisfies", "signifies"];
            if DANGLING_TAIL.contains(&word.as_str()) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod fragment_heading_tests {
    use super::is_heading_fragment;

    #[test]
    fn dangling_tail_marks_stranded_clause() {
        // opendataloader 01030000000144: left behind when a phantom table
        // dissolved, ahead of its formula on the next line.
        assert!(is_heading_fragment("Note that the exact error equals"));
        assert!(is_heading_fragment("The remainder term satisfies"));
        assert!(is_heading_fragment("we conclude that the sum equals"));
    }

    #[test]
    fn real_headings_survive() {
        assert!(!is_heading_fragment("Introduction"));
        assert!(!is_heading_fragment("Error Analysis"));
        assert!(!is_heading_fragment("Materials and Methods"));
        assert!(!is_heading_fragment("Results"));
        assert!(!is_heading_fragment("3.2 Richardson Extrapolation"));
        assert!(!is_heading_fragment("Discussion and Conclusions"));
        // Terminal punctuation means the clause is complete.
        assert!(!is_heading_fragment("What is a Derivative?"));
        assert!(!is_heading_fragment("Procedure:"));
        assert!(!is_heading_fragment("Note that this is important."));
    }

    #[test]
    fn title_case_headings_ending_in_a_verb_survive() {
        // "yields" is also a plural noun; these are real section titles.
        assert!(!is_heading_fragment("Bond Yields"));
        assert!(!is_heading_fragment("Crop Yields"));
        assert!(!is_heading_fragment("Dividend Yields"));
        assert!(!is_heading_fragment("Yields"));
        // A wrapped title-case heading whose first line ends on a listed
        // verb must survive even if the preprocessor failed to merge it.
        assert!(!is_heading_fragment("The Theorem Implies"));
        assert!(!is_heading_fragment("What This Denotes"));
    }

    #[test]
    fn numbered_sentence_case_headings_survive() {
        // heading.rs consults is_heading_fragment BEFORE applying its
        // numbered-prefix allowance, so the veto must not pre-empt it.
        assert!(!is_heading_fragment("1. What the model implies"));
        assert!(!is_heading_fragment("2.3 How the estimator satisfies"));
        assert!(!is_heading_fragment("IV) What this denotes"));
        // Without numbering the same wording is still a stranded clause.
        assert!(is_heading_fragment("What the model implies"));
        // A bare leading number or pronoun is prose, not numbering.
        assert!(is_heading_fragment("3 apples and what that implies"));
        assert!(is_heading_fragment("I think the model implies"));
        // Markers heading::parse_numbering rejects must not be exempted
        // either, or an ordinary list item bypasses the veto: lowercase
        // roman, alphabetical markers, and over-long tokens.
        assert!(is_heading_fragment("iv) the estimator satisfies"));
        assert!(is_heading_fragment("d) the value implies"));
        // Unsupported character (M is outside the parser's I/V/X/L/C set).
        assert!(is_heading_fragment("MMMM. the value implies"));
        // Over-long token: nine valid characters, so this exercises the
        // 8-character bound rather than the character set.
        assert!(is_heading_fragment("IIIIIIIII. the value implies"));
        // Eight is still within the bound and stays exempt.
        assert!(!is_heading_fragment("IIIIIIII. What this implies"));
        // Uppercase roman within the parser's grammar is still exempt.
        assert!(!is_heading_fragment("IV. What this denotes"));
        assert!(!is_heading_fragment("XII) What this implies"));
    }

    #[test]
    fn wrapped_headings_are_not_fragments() {
        // A heading that wraps across lines ends on a function word. These
        // are real headings from IRS Publication 17 and must survive.
        assert!(!is_heading_fragment("Casualty and"));
        assert!(!is_heading_fragment("Rule 10. You Must Be at"));
        assert!(!is_heading_fragment("Higher Standard Deduction for"));
        assert!(!is_heading_fragment("Qualifying Child of"));
        assert!(!is_heading_fragment("When Can I Withdraw or"));
        // Copulas and auxiliaries also end real wrapped headings.
        assert!(!is_heading_fragment("Rule 15. Your AGI Must Be"));
        assert!(!is_heading_fragment("What Medical Expenses Are"));
        assert!(!is_heading_fragment("Rule 13. You Must Have"));
        assert!(!is_heading_fragment("When Can a Roth IRA Be"));
    }

    #[test]
    fn dangling_check_is_case_insensitive() {
        // All-caps is not sentence case, so the veto must not fire there.
        assert!(!is_heading_fragment("THE REMAINDER EQUALS"));
    }
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
