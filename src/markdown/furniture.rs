//! Page-furniture stripping for short documents.
//!
//! The document-wide classifier in [`strip_repeated_lines`]
//! proves running headers and footers by cross-page repetition, which needs
//! three-plus pages. This module holds the short-document policy: per-page
//! positional and typographic evidence, plus the dispatcher that picks the
//! right policy for a document.

use std::collections::{HashMap, HashSet};

use crate::types::TextLine;

/// Strip page-edge furniture on documents too short for repetition evidence.
///
/// [`strip_repeated_lines`] proves furniture by cross-page repetition and
/// needs three-plus pages; on shorter documents running headers, footer
/// credits, and page indicators survive untouched. This pass uses positional
/// and typographic evidence instead, and only strips where that evidence is
/// strong; anything ambiguous stays:
///
/// - A page needs enough body to judge (more than 8 lines). One block per edge is
///   considered: the run of up to 2 lines from the page edge at normal
///   leading. Runs of 3+ lines are body-sized content and stay whole.
/// - Isolation: the edge block must sit a clear gap (1.8x the page's median
///   leading) from the next line inward. A footnote block protects itself —
///   its outermost line is at normal leading from the rest of the block,
///   pulling the run past the size cap or the gap inside it.
/// - A block set larger than body text is a display title and stays, as
///   does anything past it.
/// - Blocks with no alphabetic content strip only on the full page-indicator
///   shape: a single short run set no larger than body text ("1 / 1", folio
///   ornaments). Multi-cell numeric rows are form or table data and stay.
/// - Marker-led blocks stay: digit or (digit) enumerations, symbol-marked
///   footnotes ("† ..."), and single-lowercase-letter markers ("a See ...")
///   are annotations that merely sit at the page edge.
/// - Rows of three-plus item clusters stay: figure label rows and table rows
///   share a baseline but are not one text run.
/// - The block's total text must be short (<= 90 chars) and non-structural.
fn strip_edge_furniture(lines: Vec<TextLine>) -> Vec<TextLine> {
    const MIN_PAGE_LINES: usize = 8;
    const MAX_EDGE_BLOCK_LINES: usize = 2;
    const ISOLATION_FACTOR: f32 = 1.8;
    const MAX_FURNITURE_CHARS: usize = 90;

    let mut pages: HashMap<u32, Vec<usize>> = HashMap::new();
    for (idx, line) in lines.iter().enumerate() {
        pages.entry(line.page).or_default().push(idx);
    }

    let mut removal: HashSet<usize> = HashSet::new();
    for indices in pages.values_mut() {
        indices.sort_by(|&a, &b| lines[b].y.total_cmp(&lines[a].y));
        // Strictly more than the floor: on a page at the minimum, the two
        // edge blocks could remove up to half the content.
        if indices.len() <= MIN_PAGE_LINES {
            continue;
        }

        // Char-weighted median font size = the body text size.
        let mut sizes: Vec<(f32, usize)> = Vec::new();
        for &i in indices.iter() {
            for item in &lines[i].items {
                sizes.push((item.font_size, item.text.chars().count()));
            }
        }
        sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
        let total_chars: usize = sizes.iter().map(|&(_, c)| c).sum();
        let mut running = 0usize;
        let mut body_size = 12.0f32;
        for &(size, chars) in &sizes {
            running += chars;
            if running * 2 >= total_chars {
                body_size = size;
                break;
            }
        }

        let mut steps: Vec<f32> = indices
            .windows(2)
            .map(|w| lines[w[0]].y - lines[w[1]].y)
            .filter(|d| *d > 1.0)
            .collect();
        steps.sort_by(|a, b| a.total_cmp(b));
        let median_leading = steps.get(steps.len() / 2).copied().unwrap_or(12.0);
        let isolation = median_leading * ISOLATION_FACTOR;

        let line_font = |i: usize| -> f32 {
            lines[i]
                .items
                .iter()
                .map(|it| it.font_size)
                .fold(0.0, f32::max)
        };

        // Judge one edge. `ordered` lists the page's lines from that edge
        // inward; returns the edge block's indices when it is furniture.
        let judge_edge = |ordered: &[usize]| -> Option<Vec<usize>> {
            // The edge block: run of lines at normal leading from the edge.
            let mut block_len = 1usize;
            while block_len < ordered.len()
                && block_len <= MAX_EDGE_BLOCK_LINES
                && (lines[ordered[block_len - 1]].y - lines[ordered[block_len]].y).abs() < isolation
            {
                block_len += 1;
            }
            if block_len > MAX_EDGE_BLOCK_LINES || block_len >= ordered.len() {
                return None; // body-sized run, or no inward line to isolate from
            }
            let block = &ordered[..block_len];
            let inward = ordered[block_len];
            let gap = (lines[block[block_len - 1]].y - lines[inward].y).abs();
            if gap < isolation {
                return None;
            }
            let text: String = block
                .iter()
                .map(|&i| lines[i].text())
                .collect::<Vec<_>>()
                .join(" ");
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_FURNITURE_CHARS {
                return None;
            }
            // A line of three-plus item clusters separated by box-sized
            // gaps is content sharing a baseline — a figure's label row,
            // or a numeric table row on an undetected borderless table —
            // never furniture. Real furniture is one text run, or two at
            // most (running title on one margin, folio on the other).
            let mut max_clusters = 1usize;
            for &i in block {
                let line = &lines[i];
                let mut xs: Vec<(f32, f32)> = line
                    .items
                    .iter()
                    .map(|it| (it.x, it.x + it.width))
                    .collect();
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let cluster_gap = line_font(i).max(6.0) * 2.0;
                let mut clusters = 1usize;
                let mut run_right = xs.first().map(|&(_, r)| r).unwrap_or(0.0);
                for &(left, right) in xs.iter().skip(1) {
                    if left - run_right > cluster_gap {
                        clusters += 1;
                    }
                    run_right = run_right.max(right);
                }
                if clusters >= 3 {
                    return None;
                }
                max_clusters = max_clusters.max(clusters);
            }
            let has_alpha = trimmed.chars().any(|c| c.is_alphabetic());
            if !has_alpha {
                // A number-only block strips only on the full page-indicator
                // shape: one text run, a handful of characters, set no
                // larger than body text. Multi-cell numeric rows are form or
                // table data, and number-only display content (a cover-page
                // year) is set well above body size.
                if max_clusters > 1
                    || trimmed.chars().count() > 20
                    || block.iter().any(|&i| line_font(i) > body_size * 1.2)
                {
                    return None;
                }
                return Some(block.to_vec());
            }
            // Textual furniture must be set strictly smaller than body text.
            // Same-size edge blocks are content that merely sits at the
            // margin: section headings above their heading gap, affiliation
            // blocks on title pages, continuation paragraphs.
            if block.iter().any(|&i| line_font(i) >= body_size * 0.98) {
                return None;
            }
            if is_structural_line(trimmed) {
                return None;
            }
            // Marker-led edge blocks are annotations, not furniture:
            // digit- or (digit)-led enumerations (footnotes at the bottom,
            // list/legal continuations at the top), symbol-marked footnotes
            // ("† Estimates assume ..."), and single-lowercase-letter
            // markers ("a See the appendix ..."). A capitalized one-letter
            // word ("A Publication of ...") is prose, not a marker.
            let first = trimmed.chars().next();
            let marker_led = first.is_some_and(|c| c.is_numeric())
                || starts_with_paren_marker(trimmed)
                || first.is_some_and(|c| matches!(c, '†' | '‡' | '§' | '¶' | '*'))
                || trimmed.split_whitespace().next().is_some_and(|tok| {
                    tok.chars().count() == 1 && tok.chars().all(|c| c.is_lowercase())
                });
            if marker_led {
                return None;
            }
            Some(block.to_vec())
        };

        if let Some(block) = judge_edge(indices) {
            removal.extend(block);
        }
        let from_bottom: Vec<usize> = indices.iter().rev().copied().collect();
        if let Some(block) = judge_edge(&from_bottom) {
            removal.extend(block);
        }
    }

    if removal.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| (!removal.contains(&idx)).then_some(line))
        .collect()
}

/// Strip running headers and footers, choosing evidence by document length:
/// three-plus pages prove furniture by cross-page repetition
/// ([`strip_repeated_lines`]); shorter documents carry no repetition signal
/// and fall back to per-page positional and typographic evidence
/// ([`strip_edge_furniture`]).
pub(crate) fn strip_header_footer_lines(lines: Vec<TextLine>, page_count: u32) -> Vec<TextLine> {
    if lines.is_empty() {
        return lines;
    }
    if page_count < 3 {
        strip_edge_furniture(lines)
    } else {
        strip_repeated_lines(lines, page_count)
    }
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
    let trimmed = ws.trim_start_matches(|c: char| c.is_numeric()).trim_start();
    let trimmed = trimmed
        .trim_end_matches(|c: char| c.is_numeric())
        .trim_end();
    trimmed.to_string()
}

/// A parenthesized annotation marker: a short (≤3 chars) token of digits or
/// lowercase letters closed by ')' — "(1)", "(12)", "(a)", "(iv)", "(١)".
/// Parenthetical prose ("(all amounts in thousands)") is not a marker.
fn starts_with_paren_marker(text: &str) -> bool {
    text.strip_prefix('(').is_some_and(|rest| {
        let marker: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
        !marker.is_empty()
            && marker.chars().count() <= 3
            && marker.chars().all(|c| c.is_numeric() || c.is_lowercase())
            && rest[marker.len()..].starts_with(')')
    })
}

/// Returns true if the line looks like a list item, heading, or marker-led
/// annotation (should not be stripped).
fn is_structural_line(text: &str) -> bool {
    let t = text.trim_start();
    let numbered =
        t.chars().next().is_some_and(|c| c.is_numeric()) && (t.contains(". ") || t.contains(") "));
    t.starts_with('#')
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("• ")
        || numbered
        || starts_with_paren_marker(t)
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

/// A Y-band's members in document order plus the row's coalesced,
/// comparison-normalized text. Single source for band coalescing: the
/// frequency build and both band passes must key on identical text.
fn coalesced_band(lines: &[TextLine], indices: &[usize]) -> (Vec<usize>, String, String) {
    let mut sorted = indices.to_vec();
    sorted.sort();
    let coalesced: String = sorted
        .iter()
        .map(|&i| lines[i].text())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize_for_comparison(&coalesced);
    (sorted, coalesced, normalized)
}

/// Strip lines that repeat on many distinct pages (running headers/footers).
///
/// A line is considered a repeated header/footer if:
/// 1. Its normalized text appears on `>= max(3, page_count * 30%)` distinct pages
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
fn strip_repeated_lines(lines: Vec<TextLine>, page_count: u32) -> Vec<TextLine> {
    if lines.is_empty() || page_count < 3 {
        return lines;
    }

    // Compute Y range per page (min_y, max_y)
    let mut page_y_range: HashMap<u32, (f32, f32)> = HashMap::new();
    for line in &lines {
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
    for line in &lines {
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

    /// Returns true if the given Y position is among the first or last N distinct
    /// Y positions on the specified page.
    fn is_y_at_edge(y: f32, page: u32, page_sorted_ys: &HashMap<u32, Vec<f32>>, n: usize) -> bool {
        let ys = match page_sorted_ys.get(&page) {
            Some(ys) => ys,
            None => return false,
        };
        if ys.len() <= n * 2 {
            // Page has very few lines — everything is near the edge
            return true;
        }
        // Check if this Y is among the first or last N
        let pos = match ys.iter().position(|&py| (py - y).abs() < 0.1) {
            Some(p) => p,
            None => return false,
        };
        pos < n || pos >= ys.len() - n
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

    // A band with a structural member is one physical row of content: the
    // whole row is protected, because removing only the plain fragments
    // would leave a mangled half-row. Every removal path below honors this.
    // Protection covers the neighboring buckets too: same-row fragments can
    // straddle a rounding boundary (y 100.04 and 100.06 land in different
    // buckets), and a boundary must never split a row's protection.
    let mut protected_bands: HashSet<(u32, i32)> = HashSet::new();
    for (&(page, bucket), indices) in &y_bands {
        if indices
            .iter()
            .any(|&idx| is_structural_line(lines[idx].text().trim()))
        {
            protected_bands.insert((page, bucket - 1));
            protected_bands.insert((page, bucket));
            protected_bands.insert((page, bucket + 1));
        }
    }
    let band_key = |line: &TextLine| -> (u32, i32) { (line.page, (line.y * 10.0).round() as i32) };

    // Build frequency maps using normalize_for_comparison.
    // Individual line text -> distinct pages
    let mut freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut y_positions: HashMap<String, Vec<f32>> = HashMap::new();
    for line in &lines {
        if !is_y_at_edge(line.y, line.page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let text = line.text();
        // Structural checks must see the original text: normalization strips
        // leading digits, so "1. Introduction" would otherwise lose the
        // numbered-heading protection along with its number.
        if is_structural_line(text.trim()) {
            continue;
        }
        let normalized = normalize_for_comparison(&text);
        if normalized.chars().count() < 10 || is_decorative_separator(&normalized) {
            continue;
        }
        freq.entry(normalized.clone())
            .or_default()
            .insert(line.page);
        y_positions.entry(normalized).or_default().push(line.y);
    }

    // Coalesced row text -> distinct pages (for multi-member Y-bands).
    // This catches split column headers where individual fragments don't meet
    // the frequency threshold but the combined row does.
    let mut band_freq: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut band_y_positions: HashMap<String, Vec<f32>> = HashMap::new();
    for (&(page, _), indices) in &y_bands {
        if indices.len() < 2 {
            continue; // single-line bands are already in the individual map
        }
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let (_, coalesced, normalized) = coalesced_band(&lines, indices);
        if is_structural_line(coalesced.trim()) {
            continue;
        }
        if normalized.chars().count() < 10 || is_decorative_separator(&normalized) {
            continue;
        }
        band_freq
            .entry(normalized.clone())
            .or_default()
            .insert(page);
        band_y_positions.entry(normalized).or_default().push(band_y);
    }

    // Compute threshold
    let threshold = 3u32.max(page_count * 30 / 100);

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
    let candidates: HashSet<String> = freq
        .into_iter()
        .filter(|(text, pages)| {
            pages.len() as u32 >= threshold
                && !is_structural_line(text)
                && has_consistent_y(text, &y_positions)
        })
        .map(|(text, _)| text)
        .collect();

    // Identify candidates from coalesced band frequency map
    let band_candidates: HashSet<String> = band_freq
        .into_iter()
        .filter(|(text, pages)| {
            pages.len() as u32 >= threshold
                && !is_structural_line(text)
                && has_consistent_y(text, &band_y_positions)
        })
        .map(|(text, _)| text)
        .collect();

    if candidates.is_empty() && band_candidates.is_empty() {
        return lines;
    }

    // Build removal set.
    // A line is removed if it's at an edge position and:
    //   (a) its individual text matches a candidate, OR
    //   (b) its Y-band's coalesced text matches a band candidate, OR
    //   (c) any sibling in its Y-band was removed (propagation).
    //
    // The first occurrence (lowest page number) of each repeated header/footer
    // is kept so that document titles, column headers, etc. appear once.
    let mut removal_set: HashSet<usize> = HashSet::new();

    // Track which page first shows each candidate (to preserve first occurrence)
    let mut first_page_individual: HashMap<String, u32> = HashMap::new();
    for (idx, line) in lines.iter().enumerate() {
        if !is_y_at_edge(line.y, line.page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let text = line.text();
        // Removal must apply the same original-text structural protection
        // as candidate collection: a structural line can share a normalized
        // key with a non-structural candidate once its leading number is
        // normalized away. Protection extends to the line's whole Y-band —
        // one physical row — so a heading's row-mates are not half-removed.
        if protected_bands.contains(&band_key(line)) {
            continue;
        }
        let normalized = normalize_for_comparison(&text);
        if candidates.contains(&normalized) {
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
        let (_, _, normalized) = coalesced_band(&lines, indices);
        if band_candidates.contains(&normalized) {
            let first = first_page_band.entry(normalized).or_insert(page);
            if page < *first {
                *first = page;
            }
        }
    }
    // Second pass: mark for removal (skip first page)
    for (&(page, y_bucket), indices) in &y_bands {
        if indices.len() < 2 || protected_bands.contains(&(page, y_bucket)) {
            continue;
        }
        let band_y = lines[indices[0]].y;
        if !is_y_at_edge(band_y, page, &page_sorted_ys, EDGE_LINE_COUNT) {
            continue;
        }
        let (sorted_indices, coalesced, normalized) = coalesced_band(&lines, indices);
        if is_structural_line(coalesced.trim()) {
            continue;
        }
        if band_candidates.contains(&normalized) {
            let first = first_page_band.get(&normalized).copied().unwrap_or(0);
            if page > first {
                for &idx in &sorted_indices {
                    removal_set.insert(idx);
                }
            }
        }
    }

    // (c) Y-band sibling propagation: if any member is removed, remove all
    //     members (provided the band is at an edge position and the band is
    //     not structurally protected).
    for (&(page, y_bucket), indices) in &y_bands {
        if protected_bands.contains(&(page, y_bucket)) {
            continue;
        }
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
        return lines;
    }

    lines
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| !removal_set.contains(idx))
        .map(|(_, line)| line)
        .collect()
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
            font_tag: String::new(),
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

    /// A dense single page: 14 unique body lines at 15pt leading starting
    /// from y=650, all 10pt font.
    fn body_page(lines: &mut Vec<TextLine>) {
        for j in 0..14u32 {
            lines.push(make_line(
                &format!("body paragraph line number {j} with ordinary words"),
                10.0,
                1,
                650.0 - j as f32 * 15.0,
                None,
            ));
        }
    }

    #[test]
    fn edge_furniture_strips_isolated_running_header() {
        let mut lines = vec![make_line(
            "martinez et al respiratory research vol nine",
            8.0,
            1,
            760.0, // 110pt above the body: isolated
            None,
        )];
        body_page(&mut lines);
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            !result.iter().any(|l| l.text().contains("martinez")),
            "isolated small running header must be stripped"
        );
        assert_eq!(result.len(), 14);
    }

    #[test]
    fn edge_furniture_keeps_display_title() {
        let mut lines = vec![make_line("Annual Report", 22.0, 1, 760.0, None)];
        body_page(&mut lines);
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("Annual Report")),
            "display title set larger than body must survive"
        );
    }

    #[test]
    fn edge_furniture_strips_numeric_page_indicator() {
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line("1 / 1", 10.0, 1, 380.0, None)); // 65pt below body
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            !result.iter().any(|l| l.text().contains("1 / 1")),
            "isolated non-alphabetic page indicator must be stripped"
        );
    }

    #[test]
    fn edge_furniture_keeps_single_line_footnote() {
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "1 See the appendix for the full derivation",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("appendix")),
            "digit-led footnote text at the bottom must survive"
        );
    }

    #[test]
    fn edge_furniture_keeps_footnote_block() {
        let mut lines = Vec::new();
        body_page(&mut lines);
        // Three-line footnote block at normal internal leading, isolated
        // from the body: the run is body-sized, so it stays whole.
        for j in 0..3u32 {
            lines.push(make_line(
                &format!("footnote continuation words line {j}"),
                8.0,
                1,
                390.0 - j as f32 * 12.0,
                None,
            ));
        }
        let result = strip_header_footer_lines(lines, 1);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("footnote continuation"))
                .count(),
            3,
            "multi-line footnote block must survive whole"
        );
    }

    #[test]
    fn edge_furniture_keeps_two_cell_numeric_row() {
        // Two numeric cells at the bottom edge: form or table data, not a
        // page indicator, which is a single short run.
        let mut lines = Vec::new();
        body_page(&mut lines);
        let mut items = Vec::new();
        for (k, cell) in ["42.50", "38.20"].iter().enumerate() {
            let mut item = make_item(cell, 10.0, None);
            item.x = 50.0 + k as f32 * 200.0;
            item.width = 40.0;
            items.push(item);
        }
        lines.push(TextLine {
            items,
            y: 380.0,
            page: 1,
            adaptive_threshold: 0.10,
        });
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("42.50")),
            "two-cell numeric row must survive as data"
        );
    }

    #[test]
    fn edge_furniture_keeps_symbol_and_letter_marked_footnotes() {
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "† Estimates assume constant returns",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("constant returns")),
            "symbol-marked footnote must survive"
        );

        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "a See the appendix for derivations",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("appendix")),
            "lowercase-letter-marked footnote must survive"
        );
    }

    #[test]
    fn edge_furniture_keeps_paren_letter_marker() {
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "(a) See the statutory definitions above",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("statutory")),
            "(letter)-marked enumeration must survive"
        );
    }

    #[test]
    fn structural_line_survives_normalized_key_collision() {
        // A running head on pages 1-4 normalizes to the same key as a
        // numbered heading on page 5 (normalization strips the heading's
        // leading digit). The heading's structural protection must hold in
        // the removal pass, not just candidate collection.
        let mut lines = Vec::new();
        for page in 1..=4u32 {
            lines.push(make_line(
                ". Introduction extra words here",
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        lines.push(make_line(
            "1. Introduction extra words here",
            10.0,
            5,
            750.0,
            None,
        ));
        for j in 0..20u32 {
            lines.push(make_line(
                &format!("unique body content words five {j} lorem ipsum"),
                10.0,
                5,
                600.0 - j as f32 * 15.0,
                None,
            ));
        }
        let result = strip_repeated_lines(lines, 5);
        assert!(
            result
                .iter()
                .any(|l| l.text().contains("1. Introduction extra")),
            "structural line must survive a normalized-key collision"
        );
    }

    #[test]
    fn edge_furniture_keeps_unicode_paren_marker() {
        // Greek-letter footnote markers are lowercase too: "(α)" earns the
        // same protection as "(a)".
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "(α) See the appendix for the derivation",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("appendix")),
            "Unicode lowercase paren marker must survive"
        );
    }

    #[test]
    fn edge_furniture_keeps_unicode_digit_marker() {
        // Arabic-Indic digits are digits: "(١)" earns the same protection
        // as "(1)".
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "(١) See the appendix for the derivation",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("appendix")),
            "Unicode digit paren marker must survive"
        );
    }

    #[test]
    fn edge_furniture_strips_parenthetical_prose_footer() {
        // Parenthetical lowercase PROSE is not a footnote marker: only a
        // short "(a)"/"(12)"-style token closed by ')' earns protection.
        let mut lines = Vec::new();
        body_page(&mut lines);
        lines.push(make_line(
            "(all amounts in thousands of dollars)",
            8.0,
            1,
            380.0,
            None,
        ));
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            !result.iter().any(|l| l.text().contains("thousands")),
            "parenthetical prose footer must still strip"
        );
    }

    #[test]
    fn band_removal_spares_structural_sibling() {
        // A repeated edge Y-band pairs a plain fragment with a structural
        // sibling; the coalesced text reads non-structural, but the sibling
        // itself must survive removal.
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                "Journal of Applied Testing",
                10.0,
                page,
                750.0,
                None,
            ));
            lines.push(make_line(
                "1. Introduction overview",
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("1. Introduction overview"))
                .count(),
            10,
            "structural band sibling must survive on every page"
        );
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("Journal of Applied Testing"))
                .count(),
            10,
            "the structural member protects its whole physical row"
        );
    }

    #[test]
    fn repetition_length_floor_counts_chars_not_bytes() {
        // A six-character CJK line is 18 UTF-8 bytes: byte counting would
        // let it past the ten-character minimum and classify the repeated
        // short heading as furniture.
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line("第一章序論", 10.0, page, 750.0, None));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("第一章"))
                .count(),
            10,
            "short CJK line must stay under the character-count floor"
        );
    }

    #[test]
    fn repeated_paren_numbered_line_is_not_stripped() {
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                "(1) Sign and date the enclosed form",
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("Sign and date"))
                .count(),
            10,
            "(digit) enumerations are structural and must never be stripped"
        );
    }

    #[test]
    fn repetition_normalizes_unicode_page_numbers() {
        // A running header whose page number uses Arabic-Indic digits must
        // normalize to the same key on every page, or repetition never
        // reaches threshold and the furniture survives.
        let digits = ["١", "٢", "٣", "٤", "٥", "٦", "٧", "٨", "٩", "١٠"];
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                &format!("Annual Review Overview {}", digits[(page - 1) as usize]),
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("Annual Review Overview"))
                .count(),
            1,
            "Unicode-numbered running header must strip to its first occurrence"
        );
    }

    #[test]
    fn repeated_paren_letter_annotation_is_not_stripped() {
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                "(a) See the appendix for details",
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("See the appendix"))
                .count(),
            10,
            "(letter) annotations are structural in the repetition path too"
        );
    }

    #[test]
    fn row_protection_survives_bucket_rounding_boundary() {
        // The structural fragment and its row-mate sit 0.02pt apart across
        // a rounding boundary (buckets 7501 and 7500); neighbor-bucket
        // protection must still shield the plain fragment.
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                "1. Methods overview line",
                10.0,
                page,
                750.06,
                None,
            ));
            lines.push(make_line(
                "plain sibling fragment text",
                10.0,
                page,
                750.04,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        assert_eq!(
            result
                .iter()
                .filter(|l| l.text().contains("plain sibling fragment"))
                .count(),
            10,
            "row-mates across a rounding boundary keep structural protection"
        );
    }

    #[test]
    fn repeated_numbered_heading_is_not_stripped() {
        // The same numbered heading recurring on many pages must keep its
        // structural protection: normalization strips the leading digit, so
        // the check must run against the original text.
        let mut lines = Vec::new();
        for page in 1..=10u32 {
            lines.push(make_line(
                "1. Introduction to the method",
                10.0,
                page,
                750.0,
                None,
            ));
            for j in 0..20u32 {
                lines.push(make_line(
                    &format!("unique body content words {page} {j} lorem ipsum dolor"),
                    10.0,
                    page,
                    600.0 - j as f32 * 15.0,
                    None,
                ));
            }
        }
        let result = strip_repeated_lines(lines, 10);
        let count = result
            .iter()
            .filter(|l| l.text().contains("Introduction to the method"))
            .count();
        assert_eq!(count, 10, "numbered headings must never be stripped");
    }

    #[test]
    fn edge_furniture_keeps_numeric_table_row() {
        // A bottom row of an undetected borderless table: four numeric
        // cells sharing a baseline. No alphabetic content, but three-plus
        // clusters mark it as content, not a page indicator.
        let mut lines = Vec::new();
        body_page(&mut lines);
        let mut items = Vec::new();
        for (k, cell) in ["1,234", "5,678", "9,012", "3,456"].iter().enumerate() {
            let mut item = make_item(cell, 10.0, None);
            item.x = 50.0 + k as f32 * 120.0;
            item.width = 40.0;
            items.push(item);
        }
        lines.push(TextLine {
            items,
            y: 380.0,
            page: 1,
            adaptive_threshold: 0.10,
        });
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("5,678")),
            "numeric table row must survive on cluster evidence"
        );
    }

    #[test]
    fn edge_furniture_keeps_diagram_label_row() {
        // Four small-font labels sharing one baseline with box-sized gaps —
        // a figure's caption row at the page edge, not a running header.
        let mut lines = Vec::new();
        let mut items = Vec::new();
        for (k, label) in ["SOURCE", "TRANSMITTER", "RECEIVER", "DESTINATION"]
            .iter()
            .enumerate()
        {
            let mut item = make_item(label, 7.0, None);
            item.x = k as f32 * 120.0;
            item.width = 60.0;
            items.push(item);
        }
        lines.push(TextLine {
            items,
            y: 760.0,
            page: 1,
            adaptive_threshold: 0.10,
        });
        body_page(&mut lines);
        let result = strip_header_footer_lines(lines, 1);
        assert!(
            result.iter().any(|l| l.text().contains("TRANSMITTER")),
            "scattered diagram label row must survive"
        );
    }

    #[test]
    fn edge_furniture_skips_sparse_pages() {
        // Six lines total: too sparse to judge, nothing may be stripped.
        let lines: Vec<TextLine> = (0..6u32)
            .map(|j| {
                make_line(
                    &format!("sparse content line {j}"),
                    10.0,
                    1,
                    700.0 - j as f32 * 80.0,
                    None,
                )
            })
            .collect();
        let result = strip_header_footer_lines(lines, 1);
        assert_eq!(result.len(), 6, "sparse pages must be untouched");
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
}
