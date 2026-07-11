//! Line preprocessing: heading merging, drop cap handling, and repeated line removal.

use std::collections::{HashMap, HashSet};

use crate::structure_tree::StructRole;
use crate::types::TextLine;

use super::analysis::detect_header_level;

/// Resolve a heading level for a line, considering both struct-tree roles and font heuristics.
/// Struct-tree headings take priority.
fn effective_heading_level(
    line: &TextLine,
    base_size: f32,
    heading_tiers: &[f32],
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
) -> Option<usize> {
    // Check struct-tree role first
    if let Some(roles) = struct_roles {
        if let Some(page_roles) = roles.get(&line.page) {
            for item in &line.items {
                if let Some(mcid) = item.mcid {
                    if let Some(role) = page_roles.get(&mcid) {
                        let level = match role {
                            StructRole::H => Some(1),
                            StructRole::H1 => Some(1),
                            StructRole::H2 => Some(2),
                            StructRole::H3 => Some(3),
                            StructRole::H4 => Some(4),
                            StructRole::H5 => Some(5),
                            StructRole::H6 => Some(6),
                            _ => None,
                        };
                        if level.is_some() {
                            return level;
                        }
                    }
                }
            }
        }
    }

    // Fall back to font-size heuristic
    let font = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
    detect_header_level(font, base_size, heading_tiers)
}

/// Merge consecutive heading lines at the same level into a single line.
///
/// When a heading wraps across multiple text lines (e.g., "About Glenair, the Mission-Critical"
/// and "Interconnect Company"), each fragment becomes a separate `# Header` in the output.
/// This function detects consecutive lines at the same heading tier on the same page
/// with a small Y gap and merges them into one line.
///
/// Both font-size heuristic headings and struct-tree tagged headings are considered.
pub(crate) fn merge_heading_lines(
    lines: Vec<TextLine>,
    base_size: f32,
    heading_tiers: &[f32],
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
) -> Vec<TextLine> {
    if lines.is_empty() {
        return lines;
    }

    let mut result: Vec<TextLine> = Vec::with_capacity(lines.len());

    for line in lines {
        let line_level = effective_heading_level(&line, base_size, heading_tiers, struct_roles);
        let line_font = line.items.first().map(|i| i.font_size).unwrap_or(base_size);

        // Check if the previous line is a heading at the same level on the same page
        let should_merge = if let (Some(prev), Some(curr_level)) = (result.last(), line_level) {
            let prev_level = effective_heading_level(prev, base_size, heading_tiers, struct_roles);
            let same_page = prev.page == line.page;
            let same_level = prev_level == Some(curr_level);
            let y_gap = prev.y - line.y;
            // Merge if gap is within ~2x the font size (normal line wrap spacing)
            let close_enough = y_gap > 0.0 && y_gap < line_font * 2.0;
            // Don't merge if combined text would be too long — real headings are short.
            // This prevents merging body-text lines that are mis-tagged as headings.
            let prev_words = prev.text().split_whitespace().count();
            let curr_words = line.text().split_whitespace().count();
            let not_too_long = prev_words + curr_words <= 20;
            same_page && same_level && close_enough && not_too_long
        } else {
            false
        };

        if should_merge {
            // Append this line's items to the previous line
            let prev = result.last_mut().unwrap();
            // Add a space-bearing TextItem to separate the merged text
            if let Some(first_item) = line.items.first() {
                let mut space_item = first_item.clone();
                space_item.text = format!(" {}", space_item.text.trim_start());
                prev.items.push(space_item);
            }
            for item in line.items.into_iter().skip(1) {
                prev.items.push(item);
            }
        } else {
            result.push(line);
        }
    }

    result
}

/// Merge drop caps with the appropriate line.
/// A drop cap is a single large letter at the start of a paragraph.
/// Due to PDF coordinate sorting, the drop cap may appear AFTER the line it belongs to.
pub(crate) fn merge_drop_caps(lines: Vec<TextLine>, base_size: f32) -> Vec<TextLine> {
    let mut result: Vec<TextLine> = Vec::with_capacity(lines.len());

    for line in &lines {
        let text = line.text();
        let trimmed = text.trim();

        // Check if this looks like a drop cap:
        // 1. Single character (or single char + space)
        // 2. Much larger than base font (3x or more)
        // 3. The character is uppercase
        let is_drop_cap = trimmed.len() <= 2
            && line.items.first().map(|i| i.font_size).unwrap_or(0.0) >= base_size * 2.5
            && trimmed
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);

        if is_drop_cap {
            let drop_char = trimmed.chars().next().unwrap();

            // Find the first line that starts with lowercase and is at the START of a paragraph
            // (i.e., preceded by a header or non-lowercase-starting line)
            let mut target_idx: Option<usize> = None;

            for (idx, prev_line) in result.iter().enumerate() {
                if prev_line.page != line.page {
                    continue;
                }

                let prev_text = prev_line.text();
                let prev_trimmed = prev_text.trim();

                // Check if this line starts with lowercase
                if prev_trimmed
                    .chars()
                    .next()
                    .map(|c| c.is_lowercase())
                    .unwrap_or(false)
                {
                    // Check if previous line exists and doesn't start with lowercase
                    // (meaning this is the start of a paragraph)
                    let is_para_start = if idx == 0 {
                        true
                    } else {
                        let before = result[idx - 1].text();
                        let before_trimmed = before.trim();
                        !before_trimmed
                            .chars()
                            .next()
                            .map(|c| c.is_lowercase())
                            .unwrap_or(true)
                    };

                    if is_para_start {
                        target_idx = Some(idx);
                        break;
                    }
                }
            }

            // Merge with the target line
            if let Some(idx) = target_idx {
                if let Some(first_item) = result[idx].items.first_mut() {
                    let prev_text = first_item.text.trim().to_string();
                    first_item.text = format!("{}{}", drop_char, prev_text);
                }
            }
            // Don't add the drop cap line itself
            continue;
        }

        result.push(line.clone());
    }

    result
}

/// Normalize whitespace in a string for comparison: trim and collapse internal runs of whitespace.
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize text for frequency comparison: collapse whitespace and strip leading/trailing
/// digit sequences (page numbers). E.g., "Chapter 3 — Page 5" and "Chapter 3 — Page 6"
/// both normalize to "Chapter 3 — Page".
fn normalize_for_comparison(s: &str) -> String {
    let ws = normalize_whitespace(s);
    let trimmed = ws
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start();
    let trimmed = trimmed
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end();
    trimmed.to_string()
}

/// Compact a comparison key for fuzzy matching of damaged running headers.
///
/// Some tagged PDFs emit running footer text with overlapping fragments, so one
/// page may read "F rom ..." while later pages read "F om r ...". Exact
/// normalized text still drives candidate discovery; this compact form is only
/// used when deciding whether a one-off edge line is close enough to an already
/// repeated candidate.
fn compact_comparison_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn bounded_levenshtein(a: &str, b: &str, max_distance: usize) -> Option<usize> {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    if a_chars.len().abs_diff(b_chars.len()) > max_distance {
        return None;
    }

    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];

    for (i, a_ch) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        let mut row_min = curr[0];

        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = usize::from(a_ch != b_ch);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
            row_min = row_min.min(curr[j + 1]);
        }

        if row_min > max_distance {
            return None;
        }

        std::mem::swap(&mut prev, &mut curr);
    }

    let distance = prev[b_chars.len()];
    (distance <= max_distance).then_some(distance)
}

fn matches_candidate(
    normalized: &str,
    candidates: &HashSet<String>,
    compact_candidates: &[String],
) -> bool {
    if candidates.contains(normalized) {
        return true;
    }

    if !has_broken_word_spacing(normalized) {
        return false;
    }

    let compact = compact_comparison_key(normalized);
    if compact.len() < 20 {
        return false;
    }

    compact_candidates.iter().any(|candidate| {
        candidate.len().abs_diff(compact.len()) <= 2
            && bounded_levenshtein(&compact, candidate, 2).is_some()
    })
}

fn ends_with_hyphen(raw: &str) -> bool {
    matches!(
        raw.chars().last(),
        Some('-' | '\u{00ad}' | '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}')
    )
}

fn suspicious_short_token(raw: &str, alpha: &str, contains_equals: bool) -> bool {
    let len = alpha.chars().count();
    if len == 0 || len > 2 || ends_with_hyphen(raw) {
        return false;
    }

    if contains_equals {
        let raw_alpha: String = raw.chars().filter(|c| c.is_alphabetic()).collect();
        if raw_alpha.chars().all(|c| c.is_uppercase()) {
            return false;
        }
    }

    true
}

fn is_uppercase_heavy(text: &str) -> bool {
    let mut alpha = 0usize;
    let mut uppercase = 0usize;
    let mut lowercase = 0usize;

    for ch in text.chars().filter(|ch| ch.is_alphabetic()) {
        alpha += 1;
        if ch.is_uppercase() {
            uppercase += 1;
        } else if ch.is_lowercase() {
            lowercase += 1;
        }
    }

    alpha >= 12 && lowercase == 0 && uppercase * 100 / alpha >= 80
}

fn has_broken_word_spacing(text: &str) -> bool {
    if is_uppercase_heavy(text) {
        return false;
    }

    let contains_equals = text.contains('=');
    let tokens: Vec<(usize, bool)> = text
        .split_whitespace()
        .filter_map(|raw| {
            let alpha: String = raw
                .chars()
                .filter(|c| c.is_alphabetic())
                .flat_map(|c| c.to_lowercase())
                .collect();
            let len = alpha.chars().count();
            (len > 0).then(|| (len, suspicious_short_token(raw, &alpha, contains_equals)))
        })
        .collect();

    if tokens.len() < 4 {
        return false;
    }

    let suspicious_tokens = tokens.iter().filter(|(_, suspicious)| *suspicious).count();
    if suspicious_tokens < 3 {
        return false;
    }

    let split_word_windows = tokens
        .windows(3)
        .filter(|window| window[0].0 >= 3 && window[1].1 && window[2].0 >= 3)
        .count();
    let adjacent_fragments = tokens
        .windows(2)
        .filter(|window| window[0].1 && window[1].1)
        .count();

    suspicious_tokens as f32 / tokens.len() as f32 >= 0.35
        && (split_word_windows > 0 || adjacent_fragments > 0)
}

/// Returns true if the line looks like a list item or heading (should not be stripped).
fn is_structural_line(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('#')
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("• ")
        || t.chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            && (t.contains(". ") || t.contains(") "))
}

/// Returns true if a line consists entirely of a single repeated character
/// (e.g., "----------", "**************", "============").
fn is_decorative_separator(text: &str) -> bool {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    chars.all(|c| c == first)
}

/// Strip lines that repeat on many distinct pages (running headers/footers).
///
/// A line is considered a repeated header/footer if:
/// 1. Its normalized text appears on enough distinct pages. The normal threshold
///    is document-wide; visibly broken/letter-spaced running text can use a
///    capped chapter-level threshold in long books.
/// 2. It is at least 10 characters long
/// 3. It doesn't look like a structural element (heading, list item)
/// 4. It consistently appears in the top or bottom N distinct Y positions
/// 5. Its Y positions across pages have low variance (consistent placement),
///    distinguishing true headers/footers from table content that happens to
///    land near page margins
/// 6. It is not a decorative separator (repeated single character)
///
/// Additionally, TextLines at the same Y position on a page are grouped into
/// "Y-bands." When any member of a Y-band is stripped, all siblings in that
/// band are also stripped. This handles split column headers where individual
/// fragments may not independently meet the frequency threshold.
///
/// Page numbers are stripped from line text before comparison, so headers like
/// "Chapter 3 — Page 5" and "Chapter 3 — Page 6" are treated as the same text.
pub(crate) fn strip_repeated_lines(lines: Vec<TextLine>, page_count: u32) -> Vec<TextLine> {
    let removal_set = find_repeated_line_indices(&lines, page_count);
    if removal_set.is_empty() {
        return lines;
    }

    lines
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| !removal_set.contains(idx))
        .map(|(_, line)| line)
        .collect()
}

fn find_repeated_line_indices(lines: &[TextLine], page_count: u32) -> HashSet<usize> {
    if lines.is_empty() || page_count < 3 {
        return HashSet::new();
    }

    // Compute Y range per page (min_y, max_y)
    let mut page_y_range: HashMap<u32, (f32, f32)> = HashMap::new();
    for line in lines {
        let entry = page_y_range.entry(line.page).or_insert((line.y, line.y));
        if line.y < entry.0 {
            entry.0 = line.y;
        }
        if line.y > entry.1 {
            entry.1 = line.y;
        }
    }

    // Build sorted Y values per page, so we can check line rank (position from edge)
    let mut page_sorted_ys: HashMap<u32, Vec<f32>> = HashMap::new();
    for line in lines {
        page_sorted_ys.entry(line.page).or_default().push(line.y);
    }
    for ys in page_sorted_ys.values_mut() {
        ys.sort_by(|a, b| a.total_cmp(b));
        ys.dedup();
    }

    // A line is in the page margin if it's among the first or last N distinct
    // Y positions on that page. This is more robust than a percentage-based zone
    // because it catches actual edge lines regardless of how much content fills
    // the page. N=5 accommodates multi-line headers/footers and repeated form
    // column headers (e.g., 5-row IRS form headers) that sit just inside the
    // page margin.
    const EDGE_LINE_COUNT: usize = 5;

    fn y_position_rank(
        y: f32,
        page: u32,
        page_sorted_ys: &HashMap<u32, Vec<f32>>,
    ) -> Option<(usize, usize)> {
        let ys = page_sorted_ys.get(&page)?;
        let pos = ys.iter().position(|&py| (py - y).abs() < 0.1)?;
        Some((pos, ys.len()))
    }

    /// Returns true if the given Y position is among the first or last N distinct
    /// Y positions on the specified page.
    fn is_y_at_edge(y: f32, page: u32, page_sorted_ys: &HashMap<u32, Vec<f32>>, n: usize) -> bool {
        let Some((pos, len)) = y_position_rank(y, page, page_sorted_ys) else {
            return false;
        };
        if len <= n * 2 {
            // Page has very few lines — everything is near the edge
            return true;
        }
        pos < n || pos >= len - n
    }

    fn is_y_at_strict_lower_edge(
        y: f32,
        page: u32,
        page_sorted_ys: &HashMap<u32, Vec<f32>>,
        n: usize,
    ) -> bool {
        let Some((pos, len)) = y_position_rank(y, page, page_sorted_ys) else {
            return false;
        };
        len > n * 2 && pos < n
    }

    // Average page span for normalizing Y variance
    let avg_span = {
        let total: f32 = page_y_range.values().map(|(lo, hi)| hi - lo).sum();
        if page_y_range.is_empty() {
            1.0
        } else {
            (total / page_y_range.len() as f32).max(1.0)
        }
    };

    // Build Y-bands: group line indices by (page, quantized_y).
    // Lines at the same Y position (within ~0.1pt) on the same page form a band.
    let mut y_bands: HashMap<(u32, i32), Vec<usize>> = HashMap::new();
    for (idx, line) in lines.iter().enumerate() {
        let y_bucket = (line.y * 10.0).round() as i32;
        y_bands.entry((line.page, y_bucket)).or_default().push(idx);
    }

    // Build frequency maps using normalize_for_comparison.
    // Individual line text -> distinct pages
    let mut freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut bottom_freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut y_positions: HashMap<String, Vec<f32>> = HashMap::new();
    for line in lines {
        if !is_y_at_edge(line.y, line.page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let text = line.text();
        let normalized = normalize_for_comparison(&text);
        if normalized.len() < 10 || is_decorative_separator(&normalized) {
            continue;
        }
        freq.entry(normalized.clone())
            .or_default()
            .insert(line.page);
        if is_y_at_strict_lower_edge(line.y, line.page, &page_sorted_ys, EDGE_LINE_COUNT) {
            bottom_freq
                .entry(normalized.clone())
                .or_default()
                .insert(line.page);
        }
        y_positions.entry(normalized).or_default().push(line.y);
    }

    // Coalesced row text -> distinct pages (for multi-member Y-bands).
    // This catches split column headers where individual fragments don't meet
    // the frequency threshold but the combined row does.
    let mut band_freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut band_bottom_freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut band_y_positions: HashMap<String, Vec<f32>> = HashMap::new();
    for (&(page, _), indices) in &y_bands {
        if indices.len() < 2 {
            continue; // single-line bands are already in the individual map
        }
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let mut sorted_indices = indices.clone();
        sorted_indices.sort();
        let coalesced: String = sorted_indices
            .iter()
            .map(|&i| lines[i].text())
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = normalize_for_comparison(&coalesced);
        if normalized.len() < 10 || is_decorative_separator(&normalized) {
            continue;
        }
        band_freq
            .entry(normalized.clone())
            .or_default()
            .insert(page);
        if is_y_at_strict_lower_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            band_bottom_freq
                .entry(normalized.clone())
                .or_default()
                .insert(page);
        }
        band_y_positions.entry(normalized).or_default().push(band_y);
    }

    // Compute thresholds. Keep the conservative document-wide threshold for
    // clean text, and allow a lower cap only for visibly broken/letter-spaced
    // running headers in books where each chapter has its own footer/header.
    let document_threshold = 3u32.max(page_count * 30 / 100);
    let garbled_chapter_threshold = 3u32.max((page_count * 30 / 100).min(8));
    let remove_all_bottom_threshold = document_threshold.min(garbled_chapter_threshold);
    let meets_frequency_threshold =
        |text: &str, pages: &HashSet<u32>, bottom_pages: &HashMap<String, HashSet<u32>>| -> bool {
            pages.len() as u32 >= document_threshold
                || (has_broken_word_spacing(text)
                    && bottom_pages
                        .get(text)
                        .is_some_and(|pages| pages.len() as u32 >= garbled_chapter_threshold))
        };
    let should_remove_all_occurrences =
        |text: &str, bottom_pages: &HashMap<String, HashSet<u32>>| -> bool {
            has_broken_word_spacing(text)
                && bottom_pages
                    .get(text)
                    .is_some_and(|pages| pages.len() as u32 >= remove_all_bottom_threshold)
        };

    // Check Y-position consistency: headers/footers appear at the same position
    // on every page, table content varies. Require normalized stddev < 5% of
    // average page span.
    let has_consistent_y = |text: &str, positions: &HashMap<String, Vec<f32>>| -> bool {
        let pos = match positions.get(text) {
            Some(p) if p.len() >= 2 => p,
            _ => return true, // single occurrence — allow
        };
        let n = pos.len() as f32;
        let mean = pos.iter().sum::<f32>() / n;
        let variance = pos.iter().map(|y| (y - mean).powi(2)).sum::<f32>() / n;
        let stddev = variance.sqrt();
        stddev / avg_span < 0.05
    };

    // Identify candidates from individual frequency map
    let mut remove_all_candidates: HashSet<String> = HashSet::new();
    let mut candidates: HashSet<String> = HashSet::new();
    for (text, pages) in freq {
        if meets_frequency_threshold(&text, &pages, &bottom_freq)
            && !is_structural_line(&text)
            && has_consistent_y(&text, &y_positions)
        {
            if should_remove_all_occurrences(&text, &bottom_freq) {
                remove_all_candidates.insert(text.clone());
            }
            candidates.insert(text);
        }
    }
    let compact_candidates: Vec<String> = candidates
        .iter()
        .filter(|text| has_broken_word_spacing(text))
        .map(|text| compact_comparison_key(text))
        .filter(|text| text.len() >= 20)
        .collect();
    let compact_remove_all_candidates: Vec<String> = remove_all_candidates
        .iter()
        .filter(|text| has_broken_word_spacing(text))
        .map(|text| compact_comparison_key(text))
        .filter(|text| text.len() >= 20)
        .collect();

    // Identify candidates from coalesced band frequency map
    let mut remove_all_band_candidates: HashSet<String> = HashSet::new();
    let mut band_candidates: HashSet<String> = HashSet::new();
    for (text, pages) in band_freq {
        if meets_frequency_threshold(&text, &pages, &band_bottom_freq)
            && !is_structural_line(&text)
            && has_consistent_y(&text, &band_y_positions)
        {
            if should_remove_all_occurrences(&text, &band_bottom_freq) {
                remove_all_band_candidates.insert(text.clone());
            }
            band_candidates.insert(text);
        }
    }
    let compact_band_candidates: Vec<String> = band_candidates
        .iter()
        .filter(|text| has_broken_word_spacing(text))
        .map(|text| compact_comparison_key(text))
        .filter(|text| text.len() >= 20)
        .collect();
    let compact_remove_all_band_candidates: Vec<String> = remove_all_band_candidates
        .iter()
        .filter(|text| has_broken_word_spacing(text))
        .map(|text| compact_comparison_key(text))
        .filter(|text| text.len() >= 20)
        .collect();

    if candidates.is_empty() && band_candidates.is_empty() {
        return HashSet::new();
    }

    // Build removal set.
    // A line is removed if it's at an edge position and:
    //   (a) its individual text matches a candidate, OR
    //   (b) its Y-band's coalesced text matches a band candidate, OR
    //   (c) any sibling in its Y-band was removed (propagation).
    //
    // The first occurrence (lowest page number) of each repeated line is kept
    // so that document titles, column headers, etc. appear once. Visibly broken
    // footers proven by repeated lower-edge placement are removed from every
    // matching edge occurrence, including sparse first pages.
    let mut removal_set: HashSet<usize> = HashSet::new();

    // Track which page first shows each candidate (to preserve first occurrence)
    let mut first_page_individual: HashMap<String, u32> = HashMap::new();
    for (idx, line) in lines.iter().enumerate() {
        if !is_y_at_edge(line.y, line.page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let text = line.text();
        let normalized = normalize_for_comparison(&text);
        if matches_candidate(&normalized, &candidates, &compact_candidates) {
            if matches_candidate(
                &normalized,
                &remove_all_candidates,
                &compact_remove_all_candidates,
            ) {
                removal_set.insert(idx);
                continue;
            }
            let first = first_page_individual.entry(normalized).or_insert(line.page);
            if line.page > *first {
                removal_set.insert(idx);
            } else if line.page == *first {
                // Keep this occurrence (first page)
            }
        }
    }

    // Track first page for band candidates
    let mut first_page_band: HashMap<String, u32> = HashMap::new();
    // First pass: find first page for each band candidate
    for (&(page, _), indices) in &y_bands {
        if indices.len() < 2 {
            continue;
        }
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let mut sorted_indices = indices.clone();
        sorted_indices.sort();
        let coalesced: String = sorted_indices
            .iter()
            .map(|&i| lines[i].text())
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = normalize_for_comparison(&coalesced);
        if matches_candidate(&normalized, &band_candidates, &compact_band_candidates) {
            let first = first_page_band.entry(normalized).or_insert(page);
            if page < *first {
                *first = page;
            }
        }
    }
    // Second pass: mark for removal (skip first page)
    for (&(page, _), indices) in &y_bands {
        if indices.len() < 2 {
            continue;
        }
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let mut sorted_indices = indices.clone();
        sorted_indices.sort();
        let coalesced: String = sorted_indices
            .iter()
            .map(|&i| lines[i].text())
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = normalize_for_comparison(&coalesced);
        if matches_candidate(&normalized, &band_candidates, &compact_band_candidates) {
            if matches_candidate(
                &normalized,
                &remove_all_band_candidates,
                &compact_remove_all_band_candidates,
            ) {
                for &idx in &sorted_indices {
                    removal_set.insert(idx);
                }
                continue;
            }
            let first = first_page_band.get(&normalized).copied().unwrap_or(0);
            if page > first {
                for &idx in &sorted_indices {
                    removal_set.insert(idx);
                }
            }
        }
    }

    // (c) Y-band sibling propagation: if any member is removed, remove all
    //     members (provided the band is at an edge position).
    for (&(page, _), indices) in &y_bands {
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        if indices.iter().any(|idx| removal_set.contains(idx)) {
            for &idx in indices {
                removal_set.insert(idx);
            }
        }
    }

    if removal_set.is_empty() {
        return HashSet::new();
    }

    removal_set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemType, TextItem};

    fn make_item(text: &str, font_size: f32, mcid: Option<i64>) -> TextItem {
        TextItem {
            text: text.to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: font_size,
            font: "TestFont".to_string(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid,
        }
    }

    fn make_line(text: &str, font_size: f32, page: u32, y: f32, mcid: Option<i64>) -> TextLine {
        TextLine {
            items: vec![make_item(text, font_size, mcid)],
            y,
            page,
            adaptive_threshold: 0.10,
        }
    }

    #[test]
    fn test_has_broken_word_spacing_detects_split_words() {
        assert!(has_broken_word_spacing(
            "F rom p rese rva tion to access a nd be yond"
        ));
        assert!(has_broken_word_spacing("Conve rs ing w ith the pas t"));
        assert!(has_broken_word_spacing("The Na tional Arch ives (U K)"));
    }

    #[test]
    fn test_has_broken_word_spacing_ignores_normal_short_words() {
        assert!(!has_broken_word_spacing(
            "Reunir talento e empresas é um dos fatores po- sitivos para comunidades de sucesso"
        ));
        assert!(!has_broken_word_spacing("Witnessed on behalf of"));
        assert!(!has_broken_word_spacing(
            "V = Volume in m3/kg H = Enthalpy in kJ/kg S = Entropy in kJ/kg.K"
        ));
        assert!(!has_broken_word_spacing(
            "TITULAR DEL PODER EJECUTIVO FEDERAL, A TRAVÉS DE LA SECRETARÍA DE ECONOMÍA, A HACER VALER EL PRINCIPIO DE"
        ));
    }

    #[test]
    fn test_merge_struct_tree_headings() {
        // Two consecutive lines tagged as H2 via struct tree, same font size as body
        let lines = vec![
            make_line(
                "Historical Context for Operations in Snow",
                12.0,
                1,
                700.0,
                Some(10),
            ),
            make_line("Lake District", 12.0, 1, 686.0, Some(11)),
            make_line("Body text paragraph.", 12.0, 1, 660.0, Some(12)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H2);
        roles.insert(11i64, StructRole::H2);
        roles.insert(12i64, StructRole::P);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should merge two H2 lines into one");
        let merged_text = result[0].text();
        assert!(
            merged_text.contains("Snow") && merged_text.contains("Lake"),
            "merged heading should contain both fragments: {merged_text}"
        );
    }

    #[test]
    fn test_no_merge_different_struct_levels() {
        // Two consecutive lines tagged as different heading levels
        let lines = vec![
            make_line("Chapter 1", 12.0, 1, 700.0, Some(10)),
            make_line("Introduction", 12.0, 1, 686.0, Some(11)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H1);
        roles.insert(11i64, StructRole::H2);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should not merge different heading levels");
    }

    #[test]
    fn test_no_merge_heading_with_body() {
        // A heading line followed by a body paragraph line
        let lines = vec![
            make_line("Introduction", 12.0, 1, 700.0, Some(10)),
            make_line("This is body text.", 12.0, 1, 686.0, Some(11)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H1);
        roles.insert(11i64, StructRole::P);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should not merge heading with body text");
    }

    #[test]
    fn test_merge_font_headings_still_works() {
        // Original font-size based merging should still work without struct roles
        let lines = vec![
            make_line("A Very Long Heading That", 18.0, 1, 700.0, None),
            make_line("Wraps to Next Line", 18.0, 1, 682.0, None),
            make_line("Body text.", 12.0, 1, 660.0, None),
        ];

        let heading_tiers = vec![18.0];
        let result = merge_heading_lines(lines, 12.0, &heading_tiers, None);
        assert_eq!(result.len(), 2, "should merge font-based heading lines");
    }

    #[test]
    fn test_strip_repeated_keeps_first_occurrence() {
        // Simulate a repeated page header on 10 pages.
        // Each page has a running header at y=750 and many unique body lines.
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            // Header at top
            lines.push(make_line(
                "VOICE OF SOUTH MARION May fifteen twenty twenty five",
                10.0,
                page,
                750.0,
                None,
            ));
            // Body content — unique text per line per page (no digits to strip)
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!(
                        "parcel r-{:04}-{:03} owner smith address oak street",
                        page * 100 + j,
                        page
                    ),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }

        let result = strip_repeated_lines(lines, 10);

        // The header should appear exactly once (page 1)
        let header_count = result
            .iter()
            .filter(|l| l.text().contains("VOICE OF SOUTH MARION"))
            .count();
        assert_eq!(header_count, 1, "repeated header should be kept once");

        // First occurrence should be on page 1
        let first_header = result
            .iter()
            .find(|l| l.text().contains("VOICE OF SOUTH MARION"))
            .unwrap();
        assert_eq!(first_header.page, 1, "first occurrence should be on page 1");
    }

    #[test]
    fn test_strip_repeated_clean_bottom_footers_kept_below_document_threshold() {
        let mut lines = Vec::new();
        for page in 1..=8u32 {
            for row in 0..12u32 {
                lines.push(make_line(
                    &format!("unique body content page {page} row {row}"),
                    9.5,
                    page,
                    600.0 - row as f32 * 20.0,
                    None,
                ));
            }
            lines.push(make_line(
                &format!("Chapter running footer {}", 90 + page),
                7.5,
                page,
                39.5,
                None,
            ));
        }

        let result = strip_repeated_lines(lines, 200);

        let footer_count = result
            .iter()
            .filter(|line| line.text().contains("Chapter running footer"))
            .count();
        assert_eq!(
            footer_count, 8,
            "clean repeated footer should not use the lower garbled-text threshold"
        );
    }

    #[test]
    fn test_strip_repeated_garbled_bottom_footers_removes_all_occurrences_in_long_doc() {
        let mut lines = Vec::new();
        for page in 1..=8u32 {
            for row in 0..12u32 {
                lines.push(make_line(
                    &format!("unique body content page {page} row {row}"),
                    9.5,
                    page,
                    600.0 - row as f32 * 20.0,
                    None,
                ));
            }
            lines.push(make_line(
                &format!("M L a t the Na tional Libra ry of N orwa y {}", 90 + page),
                7.5,
                page,
                39.5,
                None,
            ));
        }

        let result = strip_repeated_lines(lines, 200);

        assert!(
            result
                .iter()
                .all(|line| !line.text().contains("Na tional Libra")),
            "garbled bottom running footer should be removed from every page"
        );
        assert!(
            result
                .iter()
                .any(|line| line.text().contains("unique body content page 1 row 0")),
            "body text should be preserved"
        );
    }

    #[test]
    fn test_strip_repeated_document_wide_garbled_footers_removes_all_occurrences() {
        let mut lines = Vec::new();
        for page in 1..=8u32 {
            for row in 0..12u32 {
                lines.push(make_line(
                    &format!("unique body content page {page} row {row}"),
                    9.5,
                    page,
                    600.0 - row as f32 * 20.0,
                    None,
                ));
            }
            lines.push(make_line(
                &format!("F rom p rese rva tion to access a nd be yond {}", 90 + page),
                7.5,
                page,
                39.5,
                None,
            ));
        }

        let result = strip_repeated_lines(lines, 8);

        let footer_count = result
            .iter()
            .filter(|line| line.text().contains("be yond"))
            .count();
        assert_eq!(
            footer_count, 0,
            "document-wide garbled footers should be removed from every page"
        );
    }

    #[test]
    fn test_strip_repeated_sparse_uppercase_headers_keep_first_occurrence() {
        let mut lines = Vec::new();
        for page in 1..=5u32 {
            lines.push(make_line(
                "PROPOSICIÓN CON PUNTO DE ACUERDO POR EL QUE EL SENADO DE LA REPÚBLICA",
                8.0,
                page,
                720.0,
                None,
            ));
            lines.push(make_line(
                "A TRAVÉS DE LA SECRETARÍA DE ECONOMÍA",
                8.0,
                page,
                704.0,
                None,
            ));
            lines.push(make_line(
                &format!("unique sparse-page body text {page}"),
                10.0,
                page,
                620.0,
                None,
            ));
        }

        let result = strip_repeated_lines(lines, 5);

        let title_count = result
            .iter()
            .filter(|line| line.text().contains("PROPOSICIÓN CON PUNTO"))
            .count();
        assert_eq!(
            title_count, 1,
            "sparse repeated heading should keep the first occurrence"
        );
    }

    #[test]
    fn test_strip_repeated_bottom_footers_matches_minor_garbling() {
        let mut lines = Vec::new();
        for page in 1..=9u32 {
            for row in 0..12u32 {
                lines.push(make_line(
                    &format!("distinct paragraph text page {page} row {row}"),
                    9.5,
                    page,
                    600.0 - row as f32 * 20.0,
                    None,
                ));
            }

            let footer = if page == 1 {
                "F rom p rese rva tion to access a nd be yond 95"
            } else {
                "F om r p rese rva tion to access a nd be yond 97"
            };
            lines.push(make_line(footer, 7.5, page, 39.5, None));
        }

        let result = strip_repeated_lines(lines, 200);

        assert!(
            result.iter().all(|line| !line.text().contains("be yond")),
            "fuzzy footer variant should be removed once the repeated form is detected"
        );
        assert!(
            result.iter().any(|line| line
                .text()
                .contains("distinct paragraph text page 9 row 11")),
            "non-footer edge-adjacent body text should be preserved"
        );
    }

    #[test]
    fn test_strip_repeated_bottom_footers_matches_sparse_first_page_variant() {
        let mut lines = Vec::new();
        for page in 1..=9u32 {
            let body_rows = if page == 1 { 3 } else { 12 };
            for row in 0..body_rows {
                lines.push(make_line(
                    &format!("distinct paragraph text page {page} row {row}"),
                    9.5,
                    page,
                    600.0 - row as f32 * 20.0,
                    None,
                ));
            }

            let footer = if page == 1 {
                "F rom p rese rva tion to access a nd be yond 95"
            } else {
                "F om r p rese rva tion to access a nd be yond 97"
            };
            lines.push(make_line(footer, 7.5, page, 39.5, None));
        }

        let result = strip_repeated_lines(lines, 200);

        assert!(
            result.iter().all(|line| !line.text().contains("be yond")),
            "sparse first page variant should be removed once later lower-edge footers prove the candidate"
        );
        assert!(
            result
                .iter()
                .any(|line| line.text().contains("distinct paragraph text page 1 row 0")),
            "sparse first page body text should be preserved"
        );
    }
}
