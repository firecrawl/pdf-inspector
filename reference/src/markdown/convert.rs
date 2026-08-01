//! Core line-to-markdown conversion loop with table/image interleaving.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::structure_tree::StructRole;
use crate::types::TextLine;

use super::analysis::{
    bold_heading_level, calculate_font_stats, compute_heading_tiers, compute_paragraph_threshold,
    detect_header_level, font_size_rarity, has_dot_leaders, is_heading_fragment, is_toc_entry_line,
    is_toc_marker_heading,
};
use super::classify::{
    format_list_item, is_caption_line, is_list_item, is_monospace_font, starts_with_bullet_marker,
};
use super::heading::classify_heading_sequences;
use super::postprocess::clean_markdown;
use super::preprocess::{merge_drop_caps, merge_heading_lines};
use super::{item_is_in_chart_region, MarkdownOptions, CHART_SEPARATOR_PAD};

/// Logical stream geometry for a page where one full-width chart separates
/// two prose columns. Positioned non-text blocks use this same ordering so a
/// right-column table or image cannot jump ahead of left-column prose.
#[derive(Debug, Clone, Copy)]
pub(super) struct ChartProseOrder {
    split_x: f32,
    chart_region: (f32, f32, f32, f32),
}

impl ChartProseOrder {
    pub(super) fn new(split_x: f32, chart_region: (f32, f32, f32, f32)) -> Self {
        Self {
            split_x,
            chart_region,
        }
    }
}

/// Markdown block with its physical position and optional logical chart-page
/// stream. Tables and images share this representation because both are
/// removed before text-line grouping and reinserted during conversion.
#[derive(Debug, Clone)]
pub(super) struct PositionedMarkdown {
    y: f32,
    x: f32,
    markdown: String,
    chart_order: Option<ChartProseOrder>,
}

impl PositionedMarkdown {
    pub(super) fn new(
        y: f32,
        x: f32,
        markdown: String,
        chart_order: Option<ChartProseOrder>,
    ) -> Self {
        Self {
            y,
            x,
            markdown,
            chart_order,
        }
    }
}

fn chart_stream_position(
    y: f32,
    x: f32,
    claimed_by_chart: bool,
    order: ChartProseOrder,
) -> (u8, u8) {
    let (_, y0, _, y1) = order.chart_region;
    let low = y0.min(y1) - CHART_SEPARATOR_PAD;
    let high = y0.max(y1) + CHART_SEPARATOR_PAD;
    let in_chart_zone = claimed_by_chart || (y >= low && y <= high);
    let zone = if in_chart_zone {
        1
    } else if y > high {
        0
    } else {
        2
    };
    let column = if in_chart_zone || x < order.split_x {
        0
    } else {
        1
    };
    (zone, column)
}

fn positioned_block_precedes_line(block: &PositionedMarkdown, line: &TextLine) -> bool {
    let Some(order) = block.chart_order else {
        return block.y > line.y;
    };
    let line_x = line.items.first().map(|item| item.x).unwrap_or(0.0);
    let line_claimed_by_chart = line
        .items
        .iter()
        .any(|item| item_is_in_chart_region(item, &[order.chart_region]));
    let block_position = chart_stream_position(block.y, block.x, false, order);
    let line_position = chart_stream_position(line.y, line_x, line_claimed_by_chart, order);
    block_position < line_position || (block_position == line_position && block.y > line.y)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PositionedBlockKind {
    Table,
    Image,
}

type PositionedBlockRef<'a> = (PositionedBlockKind, usize, &'a PositionedMarkdown);

fn compare_positioned_blocks(
    (a_kind, a_idx, a): &PositionedBlockRef<'_>,
    (b_kind, b_idx, b): &PositionedBlockRef<'_>,
) -> Ordering {
    if let (Some(a_order), Some(b_order)) = (a.chart_order, b.chart_order) {
        let a_position = chart_stream_position(a.y, a.x, false, a_order);
        let b_position = chart_stream_position(b.y, b.x, false, b_order);
        return a_position
            .cmp(&b_position)
            .then_with(|| b.y.total_cmp(&a.y))
            .then_with(|| a.x.total_cmp(&b.x))
            .then_with(|| a_kind.cmp(b_kind))
            .then_with(|| a_idx.cmp(b_idx));
    }

    // Preserve the legacy ordering for ordinary pages: tables in detection
    // order, followed by images in input order. Chart pages give every block
    // a chart order and use the logical stream comparison above.
    a_kind.cmp(b_kind).then_with(|| a_idx.cmp(b_idx))
}

fn positioned_blocks_for_page<'a>(
    page: u32,
    page_tables: &'a HashMap<u32, Vec<PositionedMarkdown>>,
    page_images: &'a HashMap<u32, Vec<PositionedMarkdown>>,
) -> Vec<PositionedBlockRef<'a>> {
    let mut blocks = Vec::new();
    if let Some(tables) = page_tables.get(&page) {
        blocks.extend(
            tables
                .iter()
                .enumerate()
                .map(|(idx, table)| (PositionedBlockKind::Table, idx, table)),
        );
    }
    if let Some(images) = page_images.get(&page) {
        blocks.extend(
            images
                .iter()
                .enumerate()
                .map(|(idx, image)| (PositionedBlockKind::Image, idx, image)),
        );
    }
    blocks.sort_by(compare_positioned_blocks);
    blocks
}

/// Pre-scan struct heading tags to find levels that are overused — i.e., tagged on
/// so many lines that they clearly represent body text, not real headings.
/// Returns the set of heading levels (1–6) that should be suppressed.
///
/// Some PDFs (e.g. British Academy grant guidance) tag every numbered paragraph
/// line as H2, producing hundreds of false headings. We detect this by checking
/// if any heading level accounts for >25% of tagged lines.
fn detect_overused_struct_heading_levels(
    lines: &[TextLine],
    struct_roles: Option<
        &std::collections::HashMap<u32, std::collections::HashMap<i64, StructRole>>,
    >,
) -> HashSet<usize> {
    let mut overused = HashSet::new();
    let Some(roles) = struct_roles else {
        return overused;
    };

    let mut level_counts: HashMap<usize, usize> = HashMap::new();
    let mut total = 0usize;

    for line in lines {
        if let Some(role) = resolve_line_struct_role(line, roles) {
            total += 1;
            if let Some(level) = struct_role_heading_level(&role) {
                *level_counts.entry(level).or_insert(0) += 1;
            }
        }
    }

    if total < 20 {
        return overused;
    }

    for (&level, &count) in &level_counts {
        let ratio = count as f32 / total as f32;
        if ratio > 0.15 {
            log::debug!(
                "struct heading H{} overused: {}/{} lines ({:.0}%), suppressing",
                level,
                count,
                total,
                ratio * 100.0
            );
            overused.insert(level);
        }
    }

    overused
}

/// Pre-scan lines to find "isolated" ones: short lines with paragraph breaks both
/// before and after.  These are heading candidates even at body font size — common
/// in academic papers ("Acknowledgements", "B.3 Prompt Engineering").
fn find_isolated_lines(lines: &[TextLine], base_size: f32, para_threshold: f32) -> HashSet<usize> {
    let mut set = HashSet::new();
    for i in 0..lines.len() {
        let line = &lines[i];
        let plain = line.text();
        let trimmed = plain.trim();
        let word_count = trimmed.split_whitespace().count();
        if !(1..=6).contains(&word_count) || trimmed.len() <= 3 {
            continue;
        }
        let font_size = line.items.first().map(|it| it.font_size).unwrap_or(0.0);
        if font_size < base_size * 0.95 {
            continue;
        }
        if is_list_item(trimmed) || is_caption_line(trimmed) {
            continue;
        }

        // Reject lines that look like wrapped paragraph text:
        // ends with hyphen, comma, preposition, or lowercase continuation
        let last_char = trimmed.chars().last().unwrap_or(' ');
        if last_char == '-' || last_char == ',' || last_char == ';' {
            continue;
        }
        // Last word is a common continuation word → wrapped paragraph
        let last_word = trimmed.split_whitespace().last().unwrap_or("");
        let continuation_words = [
            "the", "a", "an", "and", "or", "of", "in", "to", "for", "with", "by", "on", "at",
            "from", "as", "is", "are", "was", "were", "be", "that", "this", "their", "its", "our",
            "your", "has", "have", "had", "not",
        ];
        if continuation_words.contains(&last_word.to_lowercase().as_str()) {
            continue;
        }

        // Paragraph break BEFORE
        let break_before = if i == 0 {
            true
        } else {
            let prev = &lines[i - 1];
            prev.page != line.page || (prev.y - line.y).abs() > para_threshold
        };

        // Paragraph break AFTER
        let break_after = if i + 1 >= lines.len() {
            true
        } else {
            let next = &lines[i + 1];
            next.page != line.page || (line.y - next.y).abs() > para_threshold
        };

        if !break_before || !break_after {
            continue;
        }

        set.insert(i);
    }

    // Density guard: if too many lines on a page are "isolated", they're
    // all paragraph lines in a multi-column layout, not headings.  Real
    // headings are rare — at most ~20% of lines on a page.
    let mut page_line_counts: HashMap<u32, (usize, usize)> = HashMap::new(); // (total, isolated)
    for (i, line) in lines.iter().enumerate() {
        let entry = page_line_counts.entry(line.page).or_insert((0, 0));
        entry.0 += 1;
        if set.contains(&i) {
            entry.1 += 1;
        }
    }
    for (&page, &(total, isolated)) in &page_line_counts {
        // The ratio only means something on pages dense enough for a
        // multi-column misfire; on sparse pages (covers, ToC pages with a
        // lone title) one isolated line is 25%+ of the page and exactly the
        // line isolation exists to find.
        if total >= 10 && isolated as f32 / total as f32 > 0.25 {
            set.retain(|&i| lines[i].page != page);
        }
    }

    set
}

/// Pre-scan body-size all-bold runs that are too long to be headings.
///
/// Some academic PDFs use an all-bold abstract/summary paragraph immediately
/// after the author block.  A line-local bold heading heuristic sees each
/// wrapped visual line as "standalone" once the first line is misclassified,
/// producing a stack of `##` headings.  Multi-line body-size bold runs with a
/// paragraph-sized word count should stay paragraph text.
/// Merge 2-3 consecutive all-bold body-size lines into one line when the
/// group is isolated (paragraph break before and after) and short enough to
/// be a heading. Longer/wordier bold runs are wrapped bold paragraphs and
/// are left for `find_wrapped_bold_paragraph_lines` to suppress.
/// "9.5. ", "12.3.1. " — section-numbered heading prefix followed by a word.
fn starts_with_section_number(t: &str) -> bool {
    let t = t.trim_start();
    let mut rest = t;
    let mut groups = 0;
    loop {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > 3 {
            break;
        }
        groups += 1;
        rest = &rest[digits..];
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
        } else {
            break;
        }
    }
    // Two components minimum ("9.5. "): a single "1. " is an ordered list
    // item, and this prefix bypasses the isolation checks entirely.
    groups >= 2
        && rest.starts_with(char::is_whitespace)
        && rest.trim_start().starts_with(|c: char| c.is_alphabetic())
}

fn merge_wrapped_bold_heading_groups(
    lines: Vec<TextLine>,
    base_size: f32,
    para_threshold: f32,
) -> Vec<TextLine> {
    let mut out: Vec<TextLine> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        if !is_body_size_all_bold_line(&lines[i], base_size) {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut word_count = lines[i].text().split_whitespace().count();
        while end + 1 < lines.len()
            && is_body_size_all_bold_line(&lines[end + 1], base_size)
            && is_wrapped_same_style_line(&lines[end], &lines[end + 1], para_threshold)
        {
            end += 1;
            word_count += lines[end].text().split_whitespace().count();
        }
        let line_count = end - start + 1;
        // Column-local isolation: on interleaved multi-column pages the
        // vector neighbors may be the other column's lines, so judge the
        // break by x-overlapping lines only.
        let gx0 = lines[start..=end]
            .iter()
            .flat_map(|l| l.items.iter().map(|i| i.x))
            .fold(f32::INFINITY, f32::min);
        let gx1 = lines[start..=end]
            .iter()
            .flat_map(|l| l.items.iter().map(|i| i.x + i.width))
            .fold(f32::NEG_INFINITY, f32::max);
        let overlaps_x = |l: &TextLine| {
            let lx0 = l.items.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
            let lx1 = l
                .items
                .iter()
                .map(|i| i.x + i.width)
                .fold(f32::NEG_INFINITY, f32::max);
            lx0 < gx1 && lx1 > gx0
        };
        let page = lines[start].page;
        let break_before = !lines.iter().any(|l| {
            l.page == page
                && l.y > lines[start].y
                && l.y - lines[start].y <= para_threshold
                && overlaps_x(l)
        });
        let break_after = !lines.iter().any(|l| {
            l.page == page
                && l.y < lines[end].y
                && lines[end].y - l.y <= para_threshold
                && overlaps_x(l)
        });
        let numbered = starts_with_section_number(&lines[start].text());
        if (2..=3).contains(&line_count)
            && word_count <= 15
            && ((break_before && break_after) || numbered)
        {
            let mut merged = lines[start].clone();
            for l in &lines[start + 1..=end] {
                merged.items.extend(l.items.iter().cloned());
            }
            out.push(merged);
        } else {
            for l in &lines[start..=end] {
                out.push(l.clone());
            }
        }
        i = end + 1;
    }
    out
}

fn find_wrapped_bold_paragraph_lines(
    lines: &[TextLine],
    base_size: f32,
    para_threshold: f32,
) -> HashSet<usize> {
    let mut set = HashSet::new();
    let mut i = 0usize;

    while i < lines.len() {
        if !is_body_size_all_bold_line(&lines[i], base_size) {
            i += 1;
            continue;
        }

        let start = i;
        let mut end = i;
        let mut word_count = lines[i].text().split_whitespace().count();

        while end + 1 < lines.len()
            && is_body_size_all_bold_line(&lines[end + 1], base_size)
            && is_wrapped_same_style_line(&lines[end], &lines[end + 1], para_threshold)
        {
            end += 1;
            word_count += lines[end].text().split_whitespace().count();
        }

        let line_count = end - start + 1;
        if line_count >= 3 && word_count > 20 {
            for idx in start..=end {
                set.insert(idx);
            }
        }

        i = end + 1;
    }

    set
}

fn is_body_size_all_bold_line(line: &TextLine, base_size: f32) -> bool {
    let Some(first) = line.items.first() else {
        return false;
    };
    first.font_size >= base_size * 0.95
        && first.font_size < base_size * 1.2
        && line
            .items
            .iter()
            .all(|item| item.is_bold && (item.font_size - first.font_size).abs() < 0.5)
}

fn is_wrapped_same_style_line(prev: &TextLine, next: &TextLine, para_threshold: f32) -> bool {
    if prev.page != next.page {
        return false;
    }

    let y_gap = prev.y - next.y;
    if !(y_gap > 0.0 && y_gap <= para_threshold) {
        return false;
    }

    let prev_x = prev.items.first().map(|item| item.x).unwrap_or(0.0);
    let next_x = next.items.first().map(|item| item.x).unwrap_or(0.0);
    (prev_x - next_x).abs() <= 40.0
}

/// Resolve the dominant structure role for a text line by looking up its items' MCIDs.
///
/// Returns the first non-container role found (skipping Document/Part/Sect/Div/NonStruct/Span).
/// These wrapper roles don't carry useful semantic info for markdown generation.
fn resolve_line_struct_role(
    line: &TextLine,
    struct_roles: &std::collections::HashMap<u32, std::collections::HashMap<i64, StructRole>>,
) -> Option<StructRole> {
    let page_roles = struct_roles.get(&line.page)?;
    for item in &line.items {
        if let Some(mcid) = item.mcid {
            if let Some(role) = page_roles.get(&mcid) {
                match role {
                    // Skip container/wrapper roles — not useful for line classification
                    StructRole::Document
                    | StructRole::Part
                    | StructRole::Art
                    | StructRole::Sect
                    | StructRole::Div
                    | StructRole::NonStruct
                    | StructRole::Span
                    | StructRole::Private => continue,
                    _ => return Some(role.clone()),
                }
            }
        }
    }
    None
}

/// Map a StructRole heading variant to a markdown heading level (1–6).
fn struct_role_heading_level(role: &StructRole) -> Option<usize> {
    match role {
        StructRole::H => Some(1), // Generic heading → H1
        StructRole::H1 => Some(1),
        StructRole::H2 => Some(2),
        StructRole::H3 => Some(3),
        StructRole::H4 => Some(4),
        StructRole::H5 => Some(5),
        StructRole::H6 => Some(6),
        _ => None,
    }
}

/// Merge continuation tables that span across page breaks.
///
/// When consecutive pages each have exactly one table with the same number of columns
/// AND both pages are table-only (no non-table text), treat them as a single table.
/// We strip their header+separator rows and append their data rows to the first page's
/// table, then remove them from later pages.
pub(super) fn merge_continuation_tables(
    page_tables: &mut std::collections::HashMap<u32, Vec<PositionedMarkdown>>,
    table_only_pages: &HashSet<u32>,
) {
    let mut sorted_pages: Vec<u32> = page_tables.keys().copied().collect();
    sorted_pages.sort();

    if sorted_pages.len() < 2 {
        return;
    }

    // Find runs of consecutive pages that each have exactly one table with matching columns
    let mut i = 0;
    while i < sorted_pages.len() {
        let first_page = sorted_pages[i];
        let first_tables = match page_tables.get(&first_page) {
            Some(t) if t.len() == 1 => t,
            _ => {
                i += 1;
                continue;
            }
        };

        // First page must be table-only to start a merge chain
        if !table_only_pages.contains(&first_page) {
            i += 1;
            continue;
        }

        let first_col_count = count_table_columns(&first_tables[0].markdown);
        if first_col_count == 0 {
            i += 1;
            continue;
        }

        // Collect continuation pages (must also be table-only)
        let mut continuation_pages = Vec::new();
        let mut j = i + 1;
        while j < sorted_pages.len() {
            let next_page = sorted_pages[j];
            // Must be consecutive page numbers
            let prev_page = if continuation_pages.is_empty() {
                first_page
            } else {
                *continuation_pages.last().unwrap()
            };
            if next_page != prev_page + 1 {
                break;
            }

            // Continuation page must be table-only
            if !table_only_pages.contains(&next_page) {
                break;
            }

            let next_tables = match page_tables.get(&next_page) {
                Some(t) if t.len() == 1 => t,
                _ => break,
            };

            let next_col_count = count_table_columns(&next_tables[0].markdown);
            if next_col_count != first_col_count {
                break;
            }

            continuation_pages.push(next_page);
            j += 1;
        }

        if !continuation_pages.is_empty() {
            // Collect data rows from continuation pages
            let mut extra_rows = String::new();
            for &cont_page in &continuation_pages {
                if let Some(tables) = page_tables.get(&cont_page) {
                    let table_md = &tables[0].markdown;
                    // Skip header row (line 1) and separator row (line 2), keep the rest
                    for (line_idx, line) in table_md.lines().enumerate() {
                        if line_idx >= 2 {
                            extra_rows.push_str(line);
                            extra_rows.push('\n');
                        }
                    }
                }
            }

            // Append continuation rows to the first page's table
            if let Some(tables) = page_tables.get_mut(&first_page) {
                tables[0].markdown.push_str(&extra_rows);
            }

            // Remove continuation pages from the map
            for &cont_page in &continuation_pages {
                page_tables.remove(&cont_page);
            }

            // Skip past the merged pages
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Count the number of columns in a markdown table by counting `|` in the separator row.
fn count_table_columns(table_md: &str) -> usize {
    // The separator row is the second line, containing "| --- | --- |"
    if let Some(sep_line) = table_md.lines().nth(1) {
        if sep_line.contains("---") {
            // Count cells: number of | minus 1 (leading |), but handle edge cases
            let pipes = sep_line.chars().filter(|&c| c == '|').count();
            return if pipes >= 2 { pipes - 1 } else { 0 };
        }
    }
    0
}

/// Flush any remaining tables and images for a given page
fn flush_page_tables_and_images(
    page: u32,
    page_blocks: &HashMap<u32, Vec<PositionedBlockRef<'_>>>,
    inserted_tables: &mut HashSet<(u32, usize)>,
    inserted_images: &mut HashSet<(u32, usize)>,
    output: &mut String,
    in_paragraph: &mut bool,
) {
    let Some(blocks) = page_blocks.get(&page) else {
        return;
    };
    for &(kind, idx, block) in blocks {
        let already_inserted = match kind {
            PositionedBlockKind::Table => inserted_tables.contains(&(page, idx)),
            PositionedBlockKind::Image => inserted_images.contains(&(page, idx)),
        };
        if already_inserted {
            continue;
        }
        if *in_paragraph {
            output.push_str("\n\n");
            *in_paragraph = false;
        }
        output.push('\n');
        output.push_str(&block.markdown);
        output.push('\n');
        match kind {
            PositionedBlockKind::Table => {
                inserted_tables.insert((page, idx));
            }
            PositionedBlockKind::Image => {
                inserted_images.insert((page, idx));
            }
        }
    }
}

/// Convert text lines to markdown, inserting tables and images at appropriate Y positions
pub(super) fn to_markdown_from_lines_with_tables_and_images(
    lines: Vec<TextLine>,
    options: MarkdownOptions,
    page_tables: std::collections::HashMap<u32, Vec<PositionedMarkdown>>,
    page_images: std::collections::HashMap<u32, Vec<PositionedMarkdown>>,
    page_chart_regions: &std::collections::HashMap<u32, Vec<(f32, f32, f32, f32)>>,
    band_split_pages: &HashSet<u32>,
    struct_roles: Option<
        &std::collections::HashMap<u32, std::collections::HashMap<i64, StructRole>>,
    >,
) -> String {
    if lines.is_empty() && page_tables.is_empty() && page_images.is_empty() {
        return String::new();
    }

    // Calculate font statistics
    let font_stats = calculate_font_stats(&lines);
    let base_size = options
        .base_font_size
        .unwrap_or(font_stats.most_common_size);

    // Merge drop caps with following text
    let lines = merge_drop_caps(lines, base_size);

    // Discover heading tiers for this document
    let heading_tiers = compute_heading_tiers(&lines, base_size);

    // Merge consecutive heading lines at the same level (e.g., wrapped titles)
    let lines = merge_heading_lines(lines, base_size, &heading_tiers, struct_roles);

    // Compute the typical line spacing for paragraph break detection.
    // For double-spaced documents (like legal/government PDFs), the normal
    // line spacing can be 2.3x base_size, which would exceed a fixed 1.8x
    // threshold and cause every line to be treated as a paragraph break.
    let para_threshold = compute_paragraph_threshold(&lines, base_size);

    // Merge wrapped bold headings: a 2-3 line group of consecutive all-bold
    // body-size lines that is isolated as a group (paragraph break before
    // and after) is one heading that wrapped. Left split, the internal line
    // gap breaks each line's isolation and neither classifies as a heading —
    // the whole group then merges into the following body paragraph.
    let lines = if std::env::var("PI_NO_MERGE").is_ok() {
        lines
    } else {
        merge_wrapped_bold_heading_groups(lines, base_size, para_threshold)
    };

    // Pre-scan: identify isolated lines (paragraph break before AND after).
    // These are heading candidates even without bold/large font — common in
    // academic papers where section titles like "Acknowledgements" sit alone
    // between paragraphs at body font size. Inspired by opendataloader's
    // lookahead in HeadingProcessor (prevNode/nextNode context).
    let isolated_lines = find_isolated_lines(&lines, base_size, para_threshold);
    let wrapped_bold_paragraph_lines =
        find_wrapped_bold_paragraph_lines(&lines, base_size, para_threshold);

    let mut sequence_excluded_lines = wrapped_bold_paragraph_lines.clone();
    for (line_idx, line) in lines.iter().enumerate() {
        if page_chart_regions.get(&line.page).is_some_and(|regions| {
            line.items
                .iter()
                .any(|item| item_is_in_chart_region(item, regions))
        }) {
            sequence_excluded_lines.insert(line_idx);
        }
    }
    if let Some(roles) = struct_roles {
        for (line_idx, line) in lines.iter().enumerate() {
            if resolve_line_struct_role(line, roles)
                .is_some_and(|role| role.is_non_heading_content())
            {
                sequence_excluded_lines.insert(line_idx);
            }
        }
    }
    let sequence_heading_levels = classify_heading_sequences(
        &lines,
        base_size,
        &heading_tiers,
        &isolated_lines,
        &sequence_excluded_lines,
    );

    // Detect struct heading levels that are overused (body text mistagged as headings)
    let overused_heading_levels = detect_overused_struct_heading_levels(&lines, struct_roles);

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut prev_x = 0.0f32;
    let mut in_list = false;
    let mut in_paragraph = false;
    let mut last_list_x: Option<f32> = None;
    let mut in_code_block = false;
    let mut prev_had_dot_leaders = false;
    let mut paragraph_in_wrapped_bold_run = false;
    let mut toc_suppress_page: Option<u32> = None;
    let mut inserted_tables: HashSet<(u32, usize)> = HashSet::new();
    let mut inserted_images: HashSet<(u32, usize)> = HashSet::new();

    // Collect all pages that have tables or images (including image-only pages)
    let mut all_content_pages: Vec<u32> = page_tables
        .keys()
        .chain(page_images.keys())
        .copied()
        .collect();
    all_content_pages.sort();
    all_content_pages.dedup();
    // Build the unified table/image order once per page. This is only a
    // meaningful sort on chart/prose pages; ordinary pages retain their
    // legacy table-then-image order without repeating work for every line.
    let page_blocks: HashMap<u32, Vec<PositionedBlockRef<'_>>> = all_content_pages
        .iter()
        .map(|&page| {
            (
                page,
                positioned_blocks_for_page(page, &page_tables, &page_images),
            )
        })
        .collect();

    for (line_idx, line) in lines.iter().enumerate() {
        // Page break
        if line.page != current_page {
            // Flush current page's remaining tables and images
            if current_page > 0 {
                if in_code_block {
                    output.push_str("```\n");
                    in_code_block = false;
                }
                flush_page_tables_and_images(
                    current_page,
                    &page_blocks,
                    &mut inserted_tables,
                    &mut inserted_images,
                    &mut output,
                    &mut in_paragraph,
                );
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str("\n\n");
            }

            // Flush any intermediate pages (image-only or table-only) between
            // current_page and line.page that have no text lines
            for &p in &all_content_pages {
                if p <= current_page {
                    continue;
                }
                if p >= line.page {
                    break;
                }
                flush_page_tables_and_images(
                    p,
                    &page_blocks,
                    &mut inserted_tables,
                    &mut inserted_images,
                    &mut output,
                    &mut in_paragraph,
                );
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str("\n\n");
            }

            current_page = line.page;
            prev_y = f32::MAX;
            prev_x = 0.0;
            paragraph_in_wrapped_bold_run = false;

            if options.include_page_numbers {
                output.push_str(&format!("<!-- Page {} -->\n\n", current_page));
            }
        }

        // Insert tables and images through one ordered stream. Chart/prose
        // pages sort by zone, column, and physical Y; ordinary pages retain
        // the legacy table-then-image input order.
        if let Some(blocks) = page_blocks.get(&current_page) {
            for &(kind, idx, block) in blocks {
                let already_inserted = match kind {
                    PositionedBlockKind::Table => inserted_tables.contains(&(current_page, idx)),
                    PositionedBlockKind::Image => inserted_images.contains(&(current_page, idx)),
                };
                if positioned_block_precedes_line(block, line) && !already_inserted {
                    if in_paragraph {
                        output.push_str("\n\n");
                        in_paragraph = false;
                        paragraph_in_wrapped_bold_run = false;
                    }
                    output.push('\n');
                    output.push_str(&block.markdown);
                    output.push('\n');
                    match kind {
                        PositionedBlockKind::Table => {
                            inserted_tables.insert((current_page, idx));
                        }
                        PositionedBlockKind::Image => {
                            inserted_images.insert((current_page, idx));
                        }
                    }
                }
            }
        }

        // Paragraph break: large forward Y gap (normal) or large backward jump
        // (newspaper columns emitted sequentially on the same page).
        let y_gap = prev_y - line.y;
        let line_x = line.items.first().map(|i| i.x).unwrap_or(0.0);
        let is_para_break = y_gap.abs() > para_threshold;
        // Also break when X jumps significantly at the same Y level on
        // pages with band-split side-by-side layout.  This prevents
        // interleaved left/right band lines from merging into one paragraph.
        let is_band_switch = band_split_pages.contains(&line.page)
            && y_gap.abs() <= para_threshold
            && (prev_x - line_x).abs() > 50.0
            && prev_y < f32::MAX;
        let line_all_bold = !line.items.is_empty() && line.items.iter().all(|item| item.is_bold);
        let line_in_wrapped_bold_run = wrapped_bold_paragraph_lines.contains(&line_idx);
        let is_bold_to_regular_break = in_paragraph
            && paragraph_in_wrapped_bold_run
            && !line_in_wrapped_bold_run
            && !line_all_bold
            && y_gap > base_size * 1.2
            && y_gap <= para_threshold;
        if (is_para_break || is_band_switch || is_bold_to_regular_break) && in_paragraph {
            output.push_str("\n\n");
            in_paragraph = false;
            paragraph_in_wrapped_bold_run = false;
        }
        // Don't immediately end list on paragraph break
        // Let the continuation check below decide if we're still in a list
        prev_y = line.y;
        prev_x = line_x;

        // Get text with optional bold/italic formatting
        let text = line.text_with_formatting(
            options.detect_bold,
            options.detect_italic,
            options.detect_underline,
        );
        let trimmed = text.trim();

        // Also get plain text for pattern matching (list detection, captions, etc.)
        let plain_text = line.text();
        let plain_trimmed = plain_text.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Detect figure/table captions and source citations
        // These should be on their own line followed by a paragraph break
        let struct_role = struct_roles.and_then(|roles| resolve_line_struct_role(line, roles));

        // Determine if this line is code (struct-tree or font-based) for block accumulation
        let is_code_line = struct_role
            .as_ref()
            .is_some_and(|r| matches!(r, StructRole::Code))
            || (options.detect_code && line.items.iter().any(|i| is_monospace_font(&i.font)));

        // Close code block when transitioning to non-code
        if in_code_block && !is_code_line {
            output.push_str("```\n");
            in_code_block = false;
        }

        if struct_role
            .as_ref()
            .is_some_and(|r| matches!(r, StructRole::Caption))
            || is_caption_line(plain_trimmed)
        {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            output.push_str(trimmed);
            output.push_str("\n\n");
            continue;
        }

        // Detect headers: structure-tree headings win, then font-size heuristics.
        // Structure roles ADD headings (e.g. same-size text tagged H2) but do NOT
        // suppress headings that the font heuristic would detect (some tagged PDFs
        // mark obvious headings as P or Span).
        let struct_heading = struct_role
            .as_ref()
            .and_then(struct_role_heading_level)
            .filter(|level| !overused_heading_levels.contains(level));

        // Protect wrapped list items: when inside a list, a visually-continuing
        // line (same indent, line-wrap spacing) must not be reclassified as a
        // heading by the font heuristic — PDFs often bold the lead phrase of a
        // list item across multiple wrap lines, and an all-bold middle line
        // would otherwise split one item into a heading + stray body text.
        // We gate on the document's paragraph threshold so genuine section
        // headings that follow a numbered paragraph (y_gap > para_threshold)
        // remain detectable.
        let looks_like_list_continuation = in_list
            && match (last_list_x, line.items.first().map(|i| i.x)) {
                (Some(list_x), Some(curr_x)) => {
                    let x_ok = curr_x >= list_x - 5.0 && curr_x <= list_x + 50.0;
                    let y_ok = y_gap >= 0.0 && y_gap <= para_threshold;
                    x_ok && y_ok && !is_list_item(plain_trimmed)
                }
                _ => false,
            };

        // Lines explicitly tagged with a non-heading content role must never
        // be promoted by the visual heuristic — a tagged list item, quote, or
        // code line can look exactly like a heading (short, isolated).
        let non_heading_role = struct_role
            .as_ref()
            .is_some_and(StructRole::is_non_heading_content);
        let heuristic_heading = if options.detect_headers
            && !non_heading_role
            && !is_code_line
            && !looks_like_list_continuation
            && plain_trimmed.len() > 3
            && plain_trimmed.split_whitespace().count() <= 15
            && !starts_with_bullet_marker(plain_trimmed)
            && !is_toc_entry_line(plain_trimmed)
            && !is_heading_fragment(plain_trimmed)
            && toc_suppress_page != Some(line.page)
        {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            detect_header_level(
                line_font_size,
                base_size,
                &heading_tiers,
                crate::markdown::analysis::line_is_mostly_bold(line),
            )
            .or_else(|| {
                // Rarity-based heading detection (inspired by opendataloader).
                // Heading probability scoring with lookahead context.
                // Score = rarity * 0.5 + bold * 0.3 + standalone * 0.2
                //       + isolated * 0.3 (paragraph break before AND after)
                // Only consider lines at or above body font size.
                if line_font_size < base_size * 0.95 {
                    return None;
                }
                let word_count = plain_trimmed.split_whitespace().count();
                if !(1..=15).contains(&word_count) {
                    return None;
                }
                if wrapped_bold_paragraph_lines.contains(&line_idx) {
                    return None;
                }
                let rarity = font_size_rarity(line_font_size, &font_stats);
                let all_bold = !line.items.is_empty() && line.items.iter().all(|i| i.is_bold);
                let standalone = !in_paragraph;
                let isolated = isolated_lines.contains(&line_idx);

                let score = rarity * 0.5
                    + if all_bold { 0.3 } else { 0.0 }
                    + if standalone { 0.2 } else { 0.0 }
                    + if isolated { 0.3 } else { 0.0 };

                // Require standalone + at least one strong signal.
                // Non-bold, non-isolated lines need very high rarity (≥0.97)
                // to avoid classifying ordinary body text as headings in
                // multi-column layouts where column switches break
                // paragraph continuity and minor font-size variation
                // inflates rarity scores.
                let has_strong_signal = all_bold || isolated || (rarity >= 0.97 && word_count <= 8);
                // Single-word headings ("IMPLEMENTATION", "CONTENTS",
                // "Replace") are common. All-bold single words qualify when
                // standalone (paragraph break before / page top) — headings
                // hug their section's first paragraph, so requiring a break
                // after as well missed most of them. Mixed bold lead-ins
                // ("Note: ...") are excluded by all_bold.
                let enough_words = word_count >= 2 || (all_bold && plain_trimmed.len() >= 4);
                let numbered_bold = all_bold && starts_with_section_number(plain_trimmed);
                if numbered_bold
                    || (score >= 0.5 && standalone && enough_words && has_strong_signal)
                {
                    Some(bold_heading_level(&heading_tiers))
                } else {
                    None
                }
            })
            .or_else(|| sequence_heading_levels.get(&line_idx).copied())
        } else {
            None
        };

        if let Some(level) = struct_heading.or(heuristic_heading) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            let prefix = "#".repeat(level);
            // Plain text for headers (no redundant bold/italic inside `#`),
            // but underline is preserved: `<u>` carries meaning `#` doesn't.
            let heading_text = if options.detect_underline {
                line.text_with_formatting(false, false, true)
            } else {
                plain_text.clone()
            };
            output.push_str(&format!("{} {}\n\n", prefix, heading_text.trim()));
            if is_toc_marker_heading(plain_trimmed) {
                toc_suppress_page = Some(line.page);
            }
            in_list = false;
            continue;
        }

        // Structure-tree list item (LI only — LBody is a continuation, not a new item).
        // Some tagged PDFs use a "flat" style where every wrapped line in a list item
        // gets its own MCID tagged directly under LI. When we're already inside a list
        // and the line has no visible bullet marker, treat it as a continuation (falls
        // through to the continuation logic below) rather than a new list item.
        if struct_role
            .as_ref()
            .is_some_and(|r| matches!(r, StructRole::LI))
            && !is_list_item(plain_trimmed)
            && !in_list
        {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            output.push_str(&format!("- {}", trimmed));
            output.push('\n');
            in_list = true;
            last_list_x = line.items.first().map(|i| i.x);
            continue;
        }

        // Detect list items
        if options.detect_lists && is_list_item(plain_trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            last_list_x = line.items.first().map(|i| i.x);
            continue;
        } else if in_list {
            // Check if this line is a continuation of the previous list item
            // Continuations have similar X position and reasonable Y gap
            let line_x = line.items.first().map(|i| i.x);
            let is_continuation = if let (Some(list_x), Some(curr_x)) = (last_list_x, line_x) {
                // Continuation criteria:
                // 1. X is at or past the list text position
                // 2. Y gap is not too large (max ~5 line heights)
                // 3. Not a new list item
                let x_ok = curr_x >= list_x - 5.0 && curr_x <= list_x + 50.0;
                let y_ok = y_gap < base_size * 7.0;
                x_ok && y_ok && !is_list_item(plain_trimmed) && !has_dot_leaders(plain_trimmed)
            } else {
                false
            };

            if is_continuation {
                // Append to previous list item with a space
                if output.ends_with('\n') {
                    output.pop();
                    output.push(' ');
                }
                output.push_str(trimmed);
                output.push('\n');
                continue;
            } else {
                in_list = false;
                last_list_x = None;
            }
        }

        // Structure-tree block quote
        if struct_role
            .as_ref()
            .is_some_and(|r| matches!(r, StructRole::BlockQuote))
        {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            output.push_str(&format!("> {}\n", trimmed));
            continue;
        }

        // Code block accumulation (struct-tree Code role or monospace font)
        if is_code_line {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            if !in_code_block {
                output.push_str("```\n");
                in_code_block = true;
            }
            output.push_str(plain_trimmed);
            output.push('\n');
            continue;
        }

        // Regular text - join lines within same paragraph with space
        let cur_dot_leaders = has_dot_leaders(plain_trimmed);
        if in_paragraph {
            if cur_dot_leaders || prev_had_dot_leaders {
                output.push('\n');
            } else {
                output.push(' ');
            }
        }
        output.push_str(trimmed);
        paragraph_in_wrapped_bold_run = if in_paragraph {
            paragraph_in_wrapped_bold_run || line_in_wrapped_bold_run
        } else {
            line_in_wrapped_bold_run
        };
        in_paragraph = true;
        prev_had_dot_leaders = cur_dot_leaders;
    }

    // Close any trailing code block
    if in_code_block {
        output.push_str("```\n");
    }

    // Flush current page and any remaining pages with tables/images
    // (handles table-only pages after the last text line, and trailing image-only pages)
    flush_page_tables_and_images(
        current_page,
        &page_blocks,
        &mut inserted_tables,
        &mut inserted_images,
        &mut output,
        &mut in_paragraph,
    );
    for &p in &all_content_pages {
        if p <= current_page {
            continue;
        }
        flush_page_tables_and_images(
            p,
            &page_blocks,
            &mut inserted_tables,
            &mut inserted_images,
            &mut output,
            &mut in_paragraph,
        );
    }

    // Close final paragraph
    if in_paragraph {
        output.push('\n');
    }

    // Clean up and post-process
    clean_markdown(output, &options)
}

/// Convert text lines to markdown
pub fn to_markdown_from_lines(lines: Vec<TextLine>, options: MarkdownOptions) -> String {
    if lines.is_empty() {
        return String::new();
    }

    // Calculate font statistics
    let font_stats = calculate_font_stats(&lines);
    let base_size = options
        .base_font_size
        .unwrap_or(font_stats.most_common_size);

    // Merge drop caps with following text
    let lines = merge_drop_caps(lines, base_size);

    // Discover heading tiers for this document
    let heading_tiers = compute_heading_tiers(&lines, base_size);

    // Merge consecutive heading lines at the same level (e.g., wrapped titles)
    let lines = merge_heading_lines(lines, base_size, &heading_tiers, None);

    // Compute the typical line spacing for paragraph break detection
    let para_threshold = compute_paragraph_threshold(&lines, base_size);

    let isolated_lines = find_isolated_lines(&lines, base_size, para_threshold);
    let wrapped_bold_paragraph_lines =
        find_wrapped_bold_paragraph_lines(&lines, base_size, para_threshold);
    let sequence_heading_levels = classify_heading_sequences(
        &lines,
        base_size,
        &heading_tiers,
        &isolated_lines,
        &wrapped_bold_paragraph_lines,
    );

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut in_list = false;
    let mut in_paragraph = false;
    let mut last_list_x: Option<f32> = None;
    let mut prev_had_dot_leaders = false;
    let mut paragraph_in_wrapped_bold_run = false;
    let mut toc_suppress_page: Option<u32> = None;

    for (line_idx, line) in lines.iter().enumerate() {
        // Page break
        if line.page != current_page {
            if current_page > 0 {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str("\n\n");
            }
            current_page = line.page;
            prev_y = f32::MAX;
            in_list = false;
            last_list_x = None;
            prev_had_dot_leaders = false;
            paragraph_in_wrapped_bold_run = false;

            if options.include_page_numbers {
                output.push_str(&format!("<!-- Page {} -->\n\n", current_page));
            }
        }

        // Paragraph break: large forward Y gap (normal) or large backward jump
        // (newspaper columns emitted sequentially on the same page).
        let y_gap = prev_y - line.y;
        let is_para_break = y_gap.abs() > para_threshold;
        let line_all_bold = !line.items.is_empty() && line.items.iter().all(|item| item.is_bold);
        let line_in_wrapped_bold_run = wrapped_bold_paragraph_lines.contains(&line_idx);
        let is_bold_to_regular_break = in_paragraph
            && paragraph_in_wrapped_bold_run
            && !line_in_wrapped_bold_run
            && !line_all_bold
            && y_gap > base_size * 1.2
            && y_gap <= para_threshold;
        if (is_para_break || is_bold_to_regular_break) && in_paragraph {
            output.push_str("\n\n");
            in_paragraph = false;
            paragraph_in_wrapped_bold_run = false;
        }
        // Don't immediately end list on paragraph break
        // Let the continuation check below decide if we're still in a list
        prev_y = line.y;

        // Get text with optional bold/italic formatting
        let text = line.text_with_formatting(
            options.detect_bold,
            options.detect_italic,
            options.detect_underline,
        );
        let trimmed = text.trim();

        // Also get plain text for pattern matching
        let plain_text = line.text();
        let plain_trimmed = plain_text.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Detect figure/table captions and source citations
        // These should be on their own line followed by a paragraph break
        if is_caption_line(plain_trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            output.push_str(trimmed);
            output.push_str("\n\n");
            continue;
        }

        // Detect headers by font size
        // Skip very short text (drop caps/labels) and very long text (body paragraphs)
        if options.detect_headers
            && plain_trimmed.len() > 3
            && plain_trimmed.split_whitespace().count() <= 15
            && !is_toc_entry_line(plain_trimmed)
            && !is_heading_fragment(plain_trimmed)
            && toc_suppress_page != Some(line.page)
            && !(options.detect_code && line.items.iter().any(|i| is_monospace_font(&i.font)))
        {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            if let Some(header_level) = detect_header_level(
                line_font_size,
                base_size,
                &heading_tiers,
                crate::markdown::analysis::line_is_mostly_bold(line),
            )
            .or_else(|| {
                if line_font_size < base_size * 0.95 {
                    return None;
                }
                let word_count = plain_trimmed.split_whitespace().count();
                if !(1..=15).contains(&word_count) {
                    return None;
                }
                if wrapped_bold_paragraph_lines.contains(&line_idx) {
                    return None;
                }
                let rarity = font_size_rarity(line_font_size, &font_stats);
                let all_bold = !line.items.is_empty() && line.items.iter().all(|i| i.is_bold);
                let standalone = !in_paragraph;
                let isolated = isolated_lines.contains(&line_idx);
                let score = rarity * 0.5
                    + if all_bold { 0.3 } else { 0.0 }
                    + if standalone { 0.2 } else { 0.0 }
                    + if isolated { 0.3 } else { 0.0 };
                let enough_words =
                    word_count >= 2 || (all_bold && isolated && plain_trimmed.len() >= 4);
                if score >= 0.5 && standalone && enough_words {
                    return Some(bold_heading_level(&heading_tiers));
                }
                None
            })
            .or_else(|| sequence_heading_levels.get(&line_idx).copied())
            {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                    paragraph_in_wrapped_bold_run = false;
                }
                let prefix = "#".repeat(header_level);
                // Plain text for headers, except underline (see above).
                let heading_text = if options.detect_underline {
                    line.text_with_formatting(false, false, true)
                } else {
                    plain_text.clone()
                };
                output.push_str(&format!("{} {}\n\n", prefix, heading_text.trim()));
                if is_toc_marker_heading(plain_trimmed) {
                    toc_suppress_page = Some(line.page);
                }
                in_list = false;
                continue;
            }
        }

        // Detect list items
        if options.detect_lists && is_list_item(plain_trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
                paragraph_in_wrapped_bold_run = false;
            }
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            last_list_x = line.items.first().map(|i| i.x);
            continue;
        } else if in_list {
            // Check if this line is a continuation of the previous list item
            let line_x = line.items.first().map(|i| i.x);
            let is_continuation = if let (Some(list_x), Some(curr_x)) = (last_list_x, line_x) {
                // Continuation criteria:
                // 1. X is at or past the list text position
                // 2. Y gap is not too large (max ~5 line heights)
                // 3. Not a new list item
                let x_ok = curr_x >= list_x - 5.0 && curr_x <= list_x + 50.0;
                let y_ok = y_gap < base_size * 7.0;
                x_ok && y_ok && !is_list_item(plain_trimmed) && !has_dot_leaders(plain_trimmed)
            } else {
                false
            };

            if is_continuation {
                // Append to previous list item with a space
                if output.ends_with('\n') {
                    output.pop();
                    output.push(' ');
                }
                output.push_str(trimmed);
                output.push('\n');
                continue;
            } else {
                in_list = false;
                last_list_x = None;
            }
        }

        // Detect code blocks by font
        if options.detect_code {
            let is_mono = line.items.iter().any(|i| is_monospace_font(&i.font));
            if is_mono {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                    paragraph_in_wrapped_bold_run = false;
                }
                // Use plain text for code blocks
                output.push_str(&format!("```\n{}\n```\n", plain_trimmed));
                continue;
            }
        }

        // Regular text - join lines within same paragraph with space
        let cur_dot_leaders = has_dot_leaders(plain_trimmed);
        if in_paragraph {
            if cur_dot_leaders || prev_had_dot_leaders {
                output.push('\n');
            } else {
                output.push(' ');
            }
        }
        output.push_str(trimmed);
        paragraph_in_wrapped_bold_run = if in_paragraph {
            paragraph_in_wrapped_bold_run || line_in_wrapped_bold_run
        } else {
            line_in_wrapped_bold_run
        };
        in_paragraph = true;
        prev_had_dot_leaders = cur_dot_leaders;
    }

    // Close final paragraph
    if in_paragraph {
        output.push('\n');
    }

    // Clean up and post-process
    clean_markdown(output, &options)
}

#[cfg(test)]
mod tests {

    #[test]
    fn section_number_prefix_detection() {
        assert!(starts_with_section_number(
            "9.5. Adapting to the New Normal"
        ));
        assert!(starts_with_section_number("12.3.1. Deep subsection"));
        assert!(starts_with_section_number("2.1 Systems thinking"));
        assert!(!starts_with_section_number("1. First item in a list"));
        assert!(!starts_with_section_number("24% in October 2020."));
        assert!(!starts_with_section_number("2020 was a hard year"));
        assert!(!starts_with_section_number("Introduction"));
    }

    use super::*;
    use crate::structure_tree::StructRole;
    use crate::types::TextItem;
    use std::collections::HashMap;

    fn make_item(text: &str, page: u32, mcid: Option<i64>) -> TextItem {
        TextItem {
            text: text.to_string(),
            x: 72.0,
            y: 700.0,
            width: 100.0,
            height: 12.0,
            font: "Helvetica".to_string(),
            font_size: 12.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid,
        }
    }

    fn make_line(items: Vec<TextItem>) -> TextLine {
        let y = items.first().map(|i| i.y).unwrap_or(0.0);
        let page = items.first().map(|i| i.page).unwrap_or(1);
        TextLine {
            items,
            y,
            page,
            adaptive_threshold: 0.10,
        }
    }

    fn line_at(text: &str, page: u32, y: f32) -> TextLine {
        let mut item = make_item(text, page, None);
        item.y = y;
        make_line(vec![item])
    }

    #[test]
    fn chart_page_blocks_follow_zone_and_column_stream() {
        let line = |text: &str, x: f32, y: f32| {
            let mut item = make_item(text, 1, None);
            item.x = x;
            item.y = y;
            make_line(vec![item])
        };
        // Logical newspaper order for the prose zone: all left-column lines,
        // then all right-column lines, even though their physical Y values
        // jump back upward at the column switch.
        let lines = vec![
            line("Left column upper prose.", 90.0, 700.0),
            line("Left column lower prose.", 90.0, 500.0),
            line("Right column upper prose.", 340.0, 700.0),
            line("Right column lower prose.", 340.0, 500.0),
        ];
        let order = ChartProseOrder::new(280.0, (100.0, 300.0, 500.0, 400.0));
        let mut tables = HashMap::new();
        tables.insert(
            1,
            vec![
                // Detection order is deliberately right before left.
                PositionedMarkdown::new(
                    600.0,
                    340.0,
                    "| Right metric | Value |\n|---|---|\n| A | 1 |\n".into(),
                    Some(order),
                ),
                PositionedMarkdown::new(
                    550.0,
                    90.0,
                    "| Left metric | Value |\n|---|---|\n| B | 2 |\n".into(),
                    Some(order),
                ),
            ],
        );
        let mut images = HashMap::new();
        images.insert(
            1,
            vec![
                PositionedMarkdown::new(
                    575.0,
                    90.0,
                    "![Left column figure](left-image)\n".into(),
                    Some(order),
                ),
                PositionedMarkdown::new(
                    575.0,
                    340.0,
                    "![Right column figure](right-image)\n".into(),
                    Some(order),
                ),
            ],
        );

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            tables,
            images,
            &HashMap::new(),
            &HashSet::from([1]),
            None,
        );
        let positions = [
            "Left column upper prose.",
            "![Left column figure](left-image)",
            "| Left metric | Value |",
            "Left column lower prose.",
            "Right column upper prose.",
            "| Right metric | Value |",
            "![Right column figure](right-image)",
            "Right column lower prose.",
        ]
        .map(|needle| {
            md.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in {md}"))
        });
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "blocks must follow the logical chart-page stream: {md}"
        );
    }

    #[test]
    fn isolated_lines_kept_on_sparse_pages() {
        // A ToC page with a lone title and one entry far below: the density
        // ratio is 50% but the page is too sparse for the multi-column
        // misfire the guard targets — the title must stay isolated.
        let lines = vec![
            line_at("CONTENTS", 1, 700.0),
            line_at("Chapter One 5", 1, 500.0),
        ];
        let isolated = find_isolated_lines(&lines, 12.0, 20.0);
        assert!(
            isolated.contains(&0),
            "sparse-page title must stay isolated"
        );
    }

    #[test]
    fn isolated_lines_wiped_on_dense_pages() {
        // 12 short lines all with paragraph gaps — the multi-column misfire
        // shape. The guard must clear them all.
        let lines: Vec<TextLine> = (0..12)
            .map(|i| line_at("Short column line", 1, 700.0 - i as f32 * 50.0))
            .collect();
        let isolated = find_isolated_lines(&lines, 12.0, 20.0);
        assert!(
            isolated.is_empty(),
            "dense page of isolated lines must be wiped"
        );
    }

    #[test]
    fn test_struct_role_heading() {
        let lines = vec![
            make_line(vec![make_item("Introduction", 1, Some(0))]),
            make_line(vec![{
                let mut item = make_item("Body text here.", 1, Some(1));
                item.y = 680.0;
                item
            }]),
        ];

        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::H1);
        page_roles.insert(1i64, StructRole::P);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(
            md.contains("# Introduction"),
            "Should have H1 heading: {md}"
        );
        assert!(
            md.contains("Body text here."),
            "Should have body text: {md}"
        );
    }

    #[test]
    fn test_struct_role_list_item() {
        let lines = vec![make_line(vec![make_item("First item", 1, Some(0))])];

        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::LI);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(
            md.contains("- First item"),
            "Should format as list item: {md}"
        );
    }

    #[test]
    fn test_struct_role_li_flat_continuation_lines_merge() {
        // Regression: some tagged PDFs put each wrapped visual line of a list
        // item under its own MCID, all tagged directly as LI. Continuation
        // lines (no bullet marker) must merge into the bulleted parent item,
        // not each become their own list item.
        let make = |text: &str, mcid: i64, x: f32, y: f32| {
            let mut item = make_item(text, 1, Some(mcid));
            item.x = x;
            item.y = y;
            item
        };
        let lines = vec![
            make_line(vec![make("● First item that wraps onto", 0, 90.0, 322.0)]),
            make_line(vec![make("a continuation line.", 1, 108.0, 306.0)]),
            make_line(vec![make("● Second bullet also wraps", 2, 90.0, 290.0)]),
            make_line(vec![make("to a second line here.", 3, 108.0, 274.0)]),
        ];

        let mut page_roles = HashMap::new();
        for mcid in 0..4 {
            page_roles.insert(mcid, StructRole::LI);
        }
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(
            md.contains("- First item that wraps onto a continuation line."),
            "continuation should merge into first bullet: {md}"
        );
        assert!(
            md.contains("- Second bullet also wraps to a second line here."),
            "continuation should merge into second bullet: {md}"
        );
        assert!(
            !md.contains("- a continuation line."),
            "continuation line should not get its own bullet: {md}"
        );
        assert!(
            !md.contains("- to a second line here."),
            "continuation line should not get its own bullet: {md}"
        );
    }

    #[test]
    fn test_struct_role_blockquote() {
        let lines = vec![make_line(vec![make_item("Quoted text", 1, Some(0))])];

        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::BlockQuote);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(
            md.contains("> Quoted text"),
            "Should format as blockquote: {md}"
        );
    }

    #[test]
    fn test_struct_role_heading_levels() {
        let mcids = vec![
            (StructRole::H1, "Title"),
            (StructRole::H2, "Section"),
            (StructRole::H3, "Subsection"),
        ];

        let mut lines = Vec::new();
        let mut page_roles = HashMap::new();
        for (i, (role, text)) in mcids.iter().enumerate() {
            let mut item = make_item(text, 1, Some(i as i64));
            item.y = 700.0 - (i as f32 * 30.0);
            lines.push(make_line(vec![item]));
            page_roles.insert(i as i64, role.clone());
        }

        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(md.contains("# Title"), "H1 → #: {md}");
        assert!(md.contains("## Section"), "H2 → ##: {md}");
        assert!(md.contains("### Subsection"), "H3 → ###: {md}");
    }

    #[test]
    fn test_no_struct_roles_falls_back_to_heuristics() {
        let mut item = make_item("Big Title", 1, None);
        item.font_size = 24.0;
        item.height = 24.0;

        let lines = vec![
            make_line(vec![item]),
            make_line(vec![{
                let mut body = make_item("Normal body text.", 1, None);
                body.y = 660.0;
                body
            }]),
        ];

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            None,
        );

        assert!(
            md.contains("# Big Title"),
            "Font heuristic should detect heading: {md}"
        );
    }

    #[test]
    fn test_resolve_line_struct_role_skips_containers() {
        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::Div);
        page_roles.insert(1i64, StructRole::H2);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let line = make_line(vec![
            make_item("Part ", 1, Some(0)),
            make_item("Title", 1, Some(1)),
        ]);

        let role = resolve_line_struct_role(&line, &roles);
        assert_eq!(role, Some(StructRole::H2));
    }

    #[test]
    fn test_struct_role_code() {
        let lines = vec![make_line(vec![make_item("fn main() {}", 1, Some(0))])];

        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::Code);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        assert!(
            md.contains("```\nfn main() {}\n```"),
            "Should format as code block: {md}"
        );
    }

    #[test]
    fn test_rarity_heading_requires_strong_signal() {
        // Simulate a two-column academic paper where body text lines become
        // "standalone" due to column switches.  Body text at the same font
        // size as most of the document should NOT be classified as headings
        // just because of moderate rarity + standalone.
        //
        // Regression: previously, lines with rarity ~0.62 and standalone=true
        // scored 0.51 (>=0.5 threshold), producing hundreds of false ## headings.

        // Create many body-text lines at font_size=10.9 (most common)
        let mut lines = Vec::new();
        for i in 0..20 {
            let mut item = make_item("This is ordinary body text in a paragraph.", 1, None);
            item.font_size = 10.9;
            item.y = 700.0 - i as f32 * 14.0;
            lines.push(make_line(vec![item]));
        }
        // A few lines at a slightly different size (simulating column B text)
        for i in 0..10 {
            let mut item = make_item("Another body text line from the second column.", 1, None);
            item.font_size = 11.0; // slightly different → non-zero rarity
            item.y = 700.0 - i as f32 * 14.0;
            item.x = 320.0; // right column
            lines.push(make_line(vec![item]));
        }
        // One genuine bold heading
        let mut heading_item = make_item("3 Philosophical Perspectives", 1, None);
        heading_item.font_size = 10.9;
        heading_item.is_bold = true;
        heading_item.y = 200.0;
        lines.push(make_line(vec![heading_item]));

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            None,
        );

        // The bold heading should be detected
        assert!(
            md.contains("## 3 Philosophical Perspectives"),
            "Bold heading should be detected: {md}"
        );

        // Body text lines should NOT be headings
        let heading_count = md.lines().filter(|l| l.starts_with("##")).count();
        assert!(
            heading_count <= 2,
            "Expected at most 2 headings but found {heading_count} in:\n{md}"
        );
    }

    #[test]
    fn test_wrapped_bold_abstract_is_not_split_into_headings() {
        // Regression for arXiv 1107.1353: the opening abstract paragraph is
        // entirely bold at body size. The first wrapped lines used to become
        // separate H2 headings, and the following body paragraph was joined to
        // the bold abstract because the paragraph gap is modest.
        let make = |text: &str, y: f32, font_size: f32, bold: bool| {
            let mut item = make_item(text, 1, None);
            item.y = y;
            item.font_size = font_size;
            item.height = font_size;
            item.is_bold = bold;
            item
        };

        let lines = vec![
            make_line(vec![make(
                "Quantum Nature of Light Measured With a Single Detector",
                747.7,
                25.0,
                true,
            )]),
            make_line(vec![make(
                "Gesine A. Steudle1*, Stefan Schietinger1, David Höckel1",
                651.1,
                11.0,
                false,
            )]),
            make_line(vec![make(
                "Zwiller2, and Oliver Benson1",
                638.5,
                11.0,
                false,
            )]),
            make_line(vec![make(
                "The introduction of light quanta by Einstein in 1905 triggered strong efforts to",
                607.5,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "demonstrate the quantum properties of light directly, without involving matter",
                594.8,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "quantization. It however took more than seven decades for the quantum granularity",
                582.2,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "of light to be observed in the fluorescence of single atoms. Single atoms emit",
                569.5,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "photons one at a time, this is typically demonstrated with a Hanbury-Brown-Twiss",
                556.9,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "Our work significantly simplifies a widely used photon-correlation technique.",
                544.2,
                11.0,
                true,
            )]),
            make_line(vec![make(
                "A photon is a single excitation of a mode of the electromagnetic field.",
                528.7,
                11.0,
                false,
            )]),
        ];

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            None,
        );

        assert!(
            md.contains("# Quantum Nature of Light Measured With a Single Detector"),
            "title should remain a heading: {md}"
        );
        assert!(
            !md.contains("## The introduction")
                && !md.contains("## demonstrate")
                && !md.contains("## quantization"),
            "bold abstract lines should not become headings: {md}"
        );
        assert!(
            md.contains("technique.**\n\nA photon is a single excitation"),
            "body paragraph should be separated from bold abstract: {md}"
        );
    }

    #[test]
    fn test_struct_role_code_multiline_accumulation() {
        let mut line1 = make_item("fn main() {", 1, Some(0));
        line1.y = 700.0;
        let mut line2 = make_item("    println!(\"hello\");", 1, Some(1));
        line2.y = 688.0;
        let mut line3 = make_item("}", 1, Some(2));
        line3.y = 676.0;

        let lines = vec![
            make_line(vec![line1]),
            make_line(vec![line2]),
            make_line(vec![line3]),
        ];

        let mut page_roles = HashMap::new();
        page_roles.insert(0i64, StructRole::Code);
        page_roles.insert(1i64, StructRole::Code);
        page_roles.insert(2i64, StructRole::Code);
        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            Some(&roles),
        );

        // Should produce a single fenced block, not three separate ones
        assert!(
            md.contains("```\nfn main() {\nprintln!(\"hello\");\n}\n```"),
            "Should accumulate consecutive code lines into one block: {md}"
        );
        // Should NOT have adjacent fences
        assert!(
            !md.contains("```\n```"),
            "Should not have adjacent close/open fences: {md}"
        );
    }

    #[test]
    fn test_overused_struct_heading_suppressed() {
        // Simulate a PDF where H2 is mistagged on body text lines.
        // 30 lines total: 5 tagged H1 (real headings), 20 tagged H2 (mistagged body),
        // 5 tagged P.
        let mut lines = Vec::new();
        let mut page_roles = HashMap::new();
        let mut mcid = 0i64;

        for i in 0..30 {
            let mut item = make_item(&format!("Line {i}"), 1, Some(mcid));
            item.y = 700.0 - (i as f32 * 15.0);
            lines.push(make_line(vec![item]));

            let role = if i < 5 {
                StructRole::H1
            } else if i < 25 {
                StructRole::H2
            } else {
                StructRole::P
            };
            page_roles.insert(mcid, role);
            mcid += 1;
        }

        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let overused = detect_overused_struct_heading_levels(&lines, Some(&roles));
        // H2 is on 20/30 = 67% of lines — should be suppressed
        assert!(
            overused.contains(&2),
            "H2 should be detected as overused: {:?}",
            overused
        );
        // H1 is on 5/30 = 17% — should also be suppressed at >15% threshold
        assert!(
            overused.contains(&1),
            "H1 at 17% should also be suppressed: {:?}",
            overused
        );
    }

    #[test]
    fn test_normal_struct_headings_not_suppressed() {
        // Normal document: a few headings, mostly body text
        let mut lines = Vec::new();
        let mut page_roles = HashMap::new();
        let mut mcid = 0i64;

        for i in 0..50 {
            let mut item = make_item(&format!("Line {i}"), 1, Some(mcid));
            item.y = 700.0 - (i as f32 * 14.0);
            lines.push(make_line(vec![item]));

            let role = if i % 10 == 0 {
                StructRole::H1 // 5 headings out of 50 = 10%
            } else {
                StructRole::P
            };
            page_roles.insert(mcid, role);
            mcid += 1;
        }

        let mut roles = HashMap::new();
        roles.insert(1u32, page_roles);

        let overused = detect_overused_struct_heading_levels(&lines, Some(&roles));
        assert!(
            overused.is_empty(),
            "No heading level should be overused: {:?}",
            overused
        );
    }

    #[test]
    fn test_wrapped_bold_lead_in_list_item_not_heading() {
        // Regression: numbered-list items whose bold "lead" phrase wraps onto
        // a second line (e.g. definitions in system cards) must not have the
        // wrapped line reclassified as a heading. The middle line is
        // all_bold + standalone (in_paragraph=false while in_list), which
        // previously tripped the rarity heuristic and emitted #### in the
        // middle of the item, splitting the body into stray bullets.
        let make = |text: &str, x: f32, y: f32, bold: bool| {
            let mut item = make_item(text, 1, None);
            item.x = x;
            item.y = y;
            item.is_bold = bold;
            item
        };

        let lines = vec![
            // "1. **bold lead phrase start**"
            make_line(vec![
                make("1. ", 72.0, 700.0, false),
                make(
                    "Chemical and biological weapons threat model 1 (CB-1): Non-novel",
                    90.0,
                    700.0,
                    true,
                ),
            ]),
            // wrapped continuation of the bold lead — all_bold, same indent
            make_line(vec![make(
                "chemical/biological weapons production capabilities: A model has CB-1",
                90.0,
                686.0,
                true,
            )]),
            // body text of the same list item
            make_line(vec![make(
                "capabilities if it has the ability to significantly help individuals.",
                90.0,
                672.0,
                false,
            )]),
        ];

        let md = to_markdown_from_lines_with_tables_and_images(
            lines,
            MarkdownOptions::default(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
            None,
        );

        assert!(
            !md.contains("#### "),
            "wrapped bold lead must not become a heading: {md}"
        );
        assert!(
            md.lines().filter(|l| l.starts_with("- ")).count() == 0,
            "continuation body must not become a stray bullet: {md}"
        );
        assert!(
            md.contains("1. ") && md.contains("A model has CB-1"),
            "numbered list item should remain intact: {md}"
        );
    }
}
