//! Core line-to-markdown conversion loop with table/image interleaving.

use std::collections::{HashMap, HashSet};

use crate::structure_tree::StructRole;
use crate::types::TextLine;

use super::analysis::{
    bold_heading_level, calculate_font_stats, compute_heading_tiers, compute_paragraph_threshold,
    detect_header_level, font_size_rarity, has_dot_leaders,
};
use super::classify::{
    format_list_item, is_caption_line, is_list_item, is_monospace_font, starts_with_bullet_marker,
};
use super::postprocess::clean_markdown;
use super::preprocess::{merge_drop_caps, merge_heading_lines};
use super::MarkdownOptions;

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
        if total > 0 && isolated as f32 / total as f32 > 0.25 {
            // Too many isolated lines on this page — remove them all
            set.retain(|&i| lines[i].page != page);
        }
    }

    set
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
    page_tables: &mut std::collections::HashMap<u32, Vec<(f32, String)>>,
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

        let first_col_count = count_table_columns(&first_tables[0].1);
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

            let next_col_count = count_table_columns(&next_tables[0].1);
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
                    let table_md = &tables[0].1;
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
                tables[0].1.push_str(&extra_rows);
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
    page_tables: &std::collections::HashMap<u32, Vec<(f32, String)>>,
    page_images: &std::collections::HashMap<u32, Vec<(f32, String)>>,
    inserted_tables: &mut HashSet<(u32, usize)>,
    inserted_images: &mut HashSet<(u32, usize)>,
    output: &mut String,
    in_paragraph: &mut bool,
) {
    if let Some(tables) = page_tables.get(&page) {
        for (idx, (_, table_md)) in tables.iter().enumerate() {
            if !inserted_tables.contains(&(page, idx)) {
                if *in_paragraph {
                    output.push_str("\n\n");
                    *in_paragraph = false;
                }
                output.push('\n');
                output.push_str(table_md);
                output.push('\n');
                inserted_tables.insert((page, idx));
            }
        }
    }
    if let Some(images) = page_images.get(&page) {
        for (idx, (_, image_md)) in images.iter().enumerate() {
            if !inserted_images.contains(&(page, idx)) {
                if *in_paragraph {
                    output.push_str("\n\n");
                    *in_paragraph = false;
                }
                output.push('\n');
                output.push_str(image_md);
                output.push('\n');
                inserted_images.insert((page, idx));
            }
        }
    }
}

/// Convert text lines to markdown, inserting tables and images at appropriate Y positions
pub(super) fn to_markdown_from_lines_with_tables_and_images(
    lines: Vec<TextLine>,
    options: MarkdownOptions,
    page_tables: std::collections::HashMap<u32, Vec<(f32, String)>>,
    page_images: std::collections::HashMap<u32, Vec<(f32, String)>>,
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

    // Pre-scan: identify isolated lines (paragraph break before AND after).
    // These are heading candidates even without bold/large font — common in
    // academic papers where section titles like "Acknowledgements" sit alone
    // between paragraphs at body font size. Inspired by opendataloader's
    // lookahead in HeadingProcessor (prevNode/nextNode context).
    let isolated_lines = find_isolated_lines(&lines, base_size, para_threshold);

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
                    &page_tables,
                    &page_images,
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
                    &page_tables,
                    &page_images,
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

            if options.include_page_numbers {
                output.push_str(&format!("<!-- Page {} -->\n\n", current_page));
            }
        }

        // Check if we should insert a table before this line
        if let Some(tables) = page_tables.get(&current_page) {
            for (idx, (table_y, table_md)) in tables.iter().enumerate() {
                // Insert table when we pass its Y position
                if *table_y > line.y && !inserted_tables.contains(&(current_page, idx)) {
                    if in_paragraph {
                        output.push_str("\n\n");
                        in_paragraph = false;
                    }
                    output.push('\n');
                    output.push_str(table_md);
                    output.push('\n');
                    inserted_tables.insert((current_page, idx));
                }
            }
        }

        // Check if we should insert an image before this line
        if let Some(images) = page_images.get(&current_page) {
            for (idx, (image_y, image_md)) in images.iter().enumerate() {
                // Insert image when we pass its Y position
                if *image_y > line.y && !inserted_images.contains(&(current_page, idx)) {
                    if in_paragraph {
                        output.push_str("\n\n");
                        in_paragraph = false;
                    }
                    output.push('\n');
                    output.push_str(image_md);
                    output.push('\n');
                    inserted_images.insert((current_page, idx));
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
        if (is_para_break || is_band_switch) && in_paragraph {
            output.push_str("\n\n");
            in_paragraph = false;
        }
        // Don't immediately end list on paragraph break
        // Let the continuation check below decide if we're still in a list
        prev_y = line.y;
        prev_x = line_x;

        // Get text with optional bold/italic formatting
        let text = line.text_with_formatting(options.detect_bold, options.detect_italic);
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

        let heuristic_heading = if options.detect_headers
            && !looks_like_list_continuation
            && plain_trimmed.len() > 3
            && plain_trimmed.split_whitespace().count() <= 15
            && !starts_with_bullet_marker(plain_trimmed)
        {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            detect_header_level(line_font_size, base_size, &heading_tiers).or_else(|| {
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
                if score >= 0.5 && standalone && word_count >= 2 && has_strong_signal {
                    Some(bold_heading_level(&heading_tiers))
                } else {
                    None
                }
            })
        } else {
            None
        };

        if let Some(level) = struct_heading.or(heuristic_heading) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
            }
            let prefix = "#".repeat(level);
            // Use plain text for headers to avoid redundant formatting
            output.push_str(&format!("{} {}\n\n", prefix, plain_trimmed));
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
            }
            output.push_str(&format!("> {}\n", trimmed));
            continue;
        }

        // Code block accumulation (struct-tree Code role or monospace font)
        if is_code_line {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
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
        &page_tables,
        &page_images,
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
            &page_tables,
            &page_images,
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

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut in_list = false;
    let mut in_paragraph = false;
    let mut last_list_x: Option<f32> = None;
    let mut prev_had_dot_leaders = false;

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

            if options.include_page_numbers {
                output.push_str(&format!("<!-- Page {} -->\n\n", current_page));
            }
        }

        // Paragraph break: large forward Y gap (normal) or large backward jump
        // (newspaper columns emitted sequentially on the same page).
        let y_gap = prev_y - line.y;
        let is_para_break = y_gap.abs() > para_threshold;
        if is_para_break && in_paragraph {
            output.push_str("\n\n");
            in_paragraph = false;
        }
        // Don't immediately end list on paragraph break
        // Let the continuation check below decide if we're still in a list
        prev_y = line.y;

        // Get text with optional bold/italic formatting
        let text = line.text_with_formatting(options.detect_bold, options.detect_italic);
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
        {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            if let Some(header_level) =
                detect_header_level(line_font_size, base_size, &heading_tiers).or_else(|| {
                    if line_font_size < base_size * 0.95 {
                        return None;
                    }
                    let word_count = plain_trimmed.split_whitespace().count();
                    if !(1..=15).contains(&word_count) {
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
                    if score >= 0.5 && standalone && word_count >= 2 {
                        return Some(bold_heading_level(&heading_tiers));
                    }
                    None
                })
            {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                let prefix = "#".repeat(header_level);
                // Use plain text for headers to avoid redundant formatting
                output.push_str(&format!("{} {}\n\n", prefix, plain_trimmed));
                in_list = false;
                continue;
            }
        }

        // Detect list items
        if options.detect_lists && is_list_item(plain_trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
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
