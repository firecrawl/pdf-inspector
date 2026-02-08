//! Markdown conversion with structure detection
//!
//! This module converts extracted text to markdown, detecting:
//! - Headers (by font size)
//! - Lists (bullet points, numbered lists)
//! - Code blocks (monospace fonts, indentation)
//! - Paragraphs

use crate::extractor::{group_into_lines, TextItem, TextLine};
use std::collections::{HashMap, HashSet};

use regex::Regex;

/// Options for markdown conversion
#[derive(Debug, Clone)]
pub struct MarkdownOptions {
    /// Detect headers by font size
    pub detect_headers: bool,
    /// Detect list items
    pub detect_lists: bool,
    /// Detect code blocks
    pub detect_code: bool,
    /// Base font size for comparison
    pub base_font_size: Option<f32>,
    /// Remove standalone page numbers
    pub remove_page_numbers: bool,
    /// Convert URLs to markdown links
    pub format_urls: bool,
    /// Fix hyphenation (broken words across lines)
    pub fix_hyphenation: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            detect_headers: true,
            detect_lists: true,
            detect_code: true,
            base_font_size: None,
            remove_page_numbers: true,
            format_urls: true,
            fix_hyphenation: true,
        }
    }
}

/// Convert plain text to markdown (basic conversion)
pub fn to_markdown(text: &str, options: MarkdownOptions) -> String {
    let mut output = String::new();
    let mut in_list = false;
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if in_list {
                in_list = false;
            }
            if in_code_block {
                output.push_str("```\n");
                in_code_block = false;
            }
            output.push('\n');
            continue;
        }

        // Detect list items
        if options.detect_lists && is_list_item(trimmed) {
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            continue;
        }

        // Detect code blocks (indented lines)
        if options.detect_code && is_code_like(trimmed) {
            if !in_code_block {
                output.push_str("```\n");
                in_code_block = true;
            }
            output.push_str(trimmed);
            output.push('\n');
            continue;
        } else if in_code_block {
            output.push_str("```\n");
            in_code_block = false;
        }

        // Regular paragraph text
        output.push_str(trimmed);
        output.push('\n');
    }

    if in_code_block {
        output.push_str("```\n");
    }

    output
}

/// Convert positioned text items to markdown with structure detection
pub fn to_markdown_from_items(items: Vec<TextItem>, options: MarkdownOptions) -> String {
    use crate::tables::{detect_tables, table_to_markdown};
    use std::collections::HashSet;

    if items.is_empty() {
        return String::new();
    }

    // Calculate base font size for table detection
    let font_stats = calculate_font_stats_from_items(&items);
    let base_size = options
        .base_font_size
        .unwrap_or(font_stats.most_common_size);

    // Detect tables on each page
    let mut table_items: HashSet<usize> = HashSet::new();
    let mut page_tables: std::collections::HashMap<u32, Vec<(f32, String)>> =
        std::collections::HashMap::new();

    // Group items by page for table detection
    let mut pages: Vec<u32> = items.iter().map(|i| i.page).collect();
    pages.sort();
    pages.dedup();

    for page in pages {
        let page_items: Vec<TextItem> = items.iter().filter(|i| i.page == page).cloned().collect();

        let tables = detect_tables(&page_items, base_size);

        for table in tables {
            // Mark items as belonging to a table
            for &idx in &table.item_indices {
                // Find the global index
                let global_idx = items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.page == page)
                    .nth(idx)
                    .map(|(i, _)| i);
                if let Some(gi) = global_idx {
                    table_items.insert(gi);
                }
            }

            // Get Y position for table insertion (use highest Y in table)
            let table_y = table.rows.first().copied().unwrap_or(0.0);
            let table_md = table_to_markdown(&table);

            page_tables
                .entry(page)
                .or_default()
                .push((table_y, table_md));
        }
    }

    // Filter out table items and process the rest
    let non_table_items: Vec<TextItem> = items
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| !table_items.contains(idx))
        .map(|(_, item)| item)
        .collect();

    let lines = group_into_lines(non_table_items);

    // Convert to markdown, inserting tables at appropriate positions
    to_markdown_from_lines_with_tables(lines, options, page_tables)
}

/// Calculate font stats directly from items (before grouping into lines)
fn calculate_font_stats_from_items(items: &[TextItem]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for item in items {
        if item.font_size >= 9.0 {
            let size_key = (item.font_size * 10.0) as i32;
            *size_counts.entry(size_key).or_insert(0) += 1;
        }
    }

    let most_common_size = size_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats { most_common_size }
}

/// Convert text lines to markdown, inserting tables at appropriate Y positions
fn to_markdown_from_lines_with_tables(
    lines: Vec<TextLine>,
    options: MarkdownOptions,
    page_tables: std::collections::HashMap<u32, Vec<(f32, String)>>,
) -> String {
    if lines.is_empty() && page_tables.is_empty() {
        return String::new();
    }

    // Calculate font statistics
    let font_stats = calculate_font_stats(&lines);
    let base_size = options
        .base_font_size
        .unwrap_or(font_stats.most_common_size);

    // Merge drop caps with following text
    let lines = merge_drop_caps(lines, base_size);

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut in_list = false;
    let mut in_paragraph = false;
    let mut inserted_tables: HashSet<(u32, usize)> = HashSet::new();

    for line in lines {
        // Page break
        if line.page != current_page {
            if current_page > 0 {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str("---\n\n");
            }
            current_page = line.page;
            prev_y = f32::MAX;
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

        // Paragraph break (large Y gap)
        let y_gap = prev_y - line.y;
        let is_para_break = y_gap > base_size * 2.0;
        if is_para_break {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
            }
            if in_list {
                in_list = false;
            }
        }
        prev_y = line.y;

        let text = line.text();
        let trimmed = text.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Detect headers by font size
        if options.detect_headers && trimmed.len() > 3 {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            if let Some(header_level) = detect_header_level(line_font_size, base_size) {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                let prefix = "#".repeat(header_level);
                output.push_str(&format!("{} {}\n\n", prefix, trimmed));
                in_list = false;
                continue;
            }
        }

        // Detect list items
        if options.detect_lists && is_list_item(trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
            }
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            continue;
        } else if in_list && !trimmed.starts_with(char::is_whitespace) {
            in_list = false;
        }

        // Detect code blocks by font
        if options.detect_code {
            let is_mono = line.items.iter().any(|i| is_monospace_font(&i.font));
            if is_mono {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str(&format!("```\n{}\n```\n", trimmed));
                continue;
            }
        }

        // Regular text - join lines within same paragraph with space
        if in_paragraph {
            output.push(' ');
        }
        output.push_str(trimmed);
        in_paragraph = true;
    }

    // Insert any remaining tables at the end
    for (page, tables) in &page_tables {
        for (idx, (_, table_md)) in tables.iter().enumerate() {
            if !inserted_tables.contains(&(*page, idx)) {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push('\n');
                output.push_str(table_md);
                output.push('\n');
            }
        }
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

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut in_list = false;
    let mut in_paragraph = false;

    for line in lines {
        // Page break
        if line.page != current_page {
            if current_page > 0 {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                output.push_str("---\n\n");
            }
            current_page = line.page;
            prev_y = f32::MAX;
        }

        // Paragraph break (large Y gap)
        let y_gap = prev_y - line.y;
        let is_para_break = y_gap > base_size * 2.0;
        if is_para_break {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
            }
            if in_list {
                in_list = false;
            }
        }
        prev_y = line.y;

        let text = line.text();
        let trimmed = text.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Detect headers by font size
        // Skip very short text (likely drop caps or labels)
        if options.detect_headers && trimmed.len() > 3 {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            if let Some(header_level) = detect_header_level(line_font_size, base_size) {
                if in_paragraph {
                    output.push_str("\n\n");
                    in_paragraph = false;
                }
                let prefix = "#".repeat(header_level);
                output.push_str(&format!("{} {}\n\n", prefix, trimmed));
                in_list = false;
                continue;
            }
        }

        // Detect list items
        if options.detect_lists && is_list_item(trimmed) {
            if in_paragraph {
                output.push_str("\n\n");
                in_paragraph = false;
            }
            let formatted = format_list_item(trimmed);
            output.push_str(&formatted);
            output.push('\n');
            in_list = true;
            continue;
        } else if in_list {
            // Check if continuing list or ending
            if !trimmed.starts_with(char::is_whitespace) {
                in_list = false;
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
                output.push_str(&format!("```\n{}\n```\n", trimmed));
                continue;
            }
        }

        // Regular text - join lines within same paragraph with space
        if in_paragraph {
            output.push(' ');
        }
        output.push_str(trimmed);
        in_paragraph = true;
    }

    // Close final paragraph
    if in_paragraph {
        output.push('\n');
    }

    // Clean up and post-process
    clean_markdown(output, &options)
}

/// Merge drop caps with the appropriate line
/// A drop cap is a single large letter at the start of a paragraph
/// Due to PDF coordinate sorting, the drop cap may appear AFTER the line it belongs to
fn merge_drop_caps(lines: Vec<TextLine>, base_size: f32) -> Vec<TextLine> {
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

/// Font statistics for a document
struct FontStats {
    most_common_size: f32,
}

fn calculate_font_stats(lines: &[TextLine]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for line in lines {
        for item in &line.items {
            // Only count fonts >= 9pt as potential body text
            // Smaller fonts are typically table cells, footnotes, or captions
            if item.font_size >= 9.0 {
                let size_key = (item.font_size * 10.0) as i32; // Round to 0.1
                *size_counts.entry(size_key).or_insert(0) += 1;
            }
        }
    }

    let most_common_size = size_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats { most_common_size }
}

/// Detect header level from font size
fn detect_header_level(font_size: f32, base_size: f32) -> Option<usize> {
    let ratio = font_size / base_size;

    if ratio >= 2.0 {
        Some(1) // H1
    } else if ratio >= 1.5 {
        Some(2) // H2
    } else if ratio >= 1.25 {
        Some(3) // H3
    } else if ratio >= 1.1 {
        Some(4) // H4
    } else {
        None // Regular text
    }
}

/// Check if text looks like a list item
fn is_list_item(text: &str) -> bool {
    let trimmed = text.trim_start();

    // Bullet patterns
    if trimmed.starts_with("• ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("○ ")
        || trimmed.starts_with("● ")
        || trimmed.starts_with("◦ ")
    {
        return true;
    }

    // Numbered list patterns: "1.", "1)", "(1)", "a.", "a)"
    let first_chars: String = trimmed.chars().take(5).collect();
    if first_chars.contains(|c: char| c.is_ascii_digit()) {
        // Check for "1.", "1)", "10."
        if let Some(idx) = first_chars.find(['.', ')']) {
            let prefix = &first_chars[..idx];
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // Letter list: "a.", "a)", "(a)"
    let mut chars = trimmed.chars();
    if let (Some(first), Some(second)) = (chars.next(), chars.next()) {
        if first.is_ascii_alphabetic() && (second == '.' || second == ')') {
            return true;
        }
        if first == '(' && chars.next() == Some(')') {
            return true;
        }
    }

    false
}

/// Format list item to markdown
fn format_list_item(text: &str) -> String {
    let trimmed = text.trim_start();

    // Convert various bullet styles to markdown
    // Note: bullet characters like • are multi-byte in UTF-8, use char indices
    for bullet in &['•', '○', '●', '◦'] {
        if let Some(rest) = trimmed.strip_prefix(*bullet) {
            return format!("- {}", rest.trim_start());
        }
    }

    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return trimmed.to_string();
    }

    // Keep numbered lists as-is (markdown supports them)
    trimmed.to_string()
}

/// Check if text looks like code
fn is_code_like(text: &str) -> bool {
    let trimmed = text.trim();

    // Code patterns
    let code_patterns = [
        // Language keywords
        "import ",
        "export ",
        "from ",
        "const ",
        "let ",
        "var ",
        "function ",
        "class ",
        "def ",
        "pub fn ",
        "fn ",
        "async fn ",
        "impl ",
        // Syntax patterns
        "=> ",
        "-> ",
        ":: ",
        ":= ",
        // Common code endings
    ];

    for pattern in &code_patterns {
        if trimmed.starts_with(pattern) {
            return true;
        }
    }

    // Check for code-like syntax
    let special_chars: usize = trimmed
        .chars()
        .filter(|c| matches!(c, '{' | '}' | '(' | ')' | '[' | ']' | ';' | '=' | '<' | '>'))
        .count();

    if special_chars >= 3 && trimmed.len() < 200 {
        return true;
    }

    // Ends with semicolon or braces
    if trimmed.ends_with(';') || trimmed.ends_with('{') || trimmed.ends_with('}') {
        return true;
    }

    false
}

/// Check if font name indicates monospace
fn is_monospace_font(font_name: &str) -> bool {
    let lower = font_name.to_lowercase();
    let patterns = [
        "courier",
        "consolas",
        "monaco",
        "menlo",
        "mono",
        "fixed",
        "terminal",
        "typewriter",
        "source code",
        "fira code",
        "jetbrains",
        "inconsolata",
        "dejavu sans mono",
        "liberation mono",
    ];

    patterns.iter().any(|p| lower.contains(p))
}

/// Clean up markdown output with post-processing
fn clean_markdown(mut text: String, options: &MarkdownOptions) -> String {
    // Fix hyphenation first (before other processing)
    if options.fix_hyphenation {
        text = fix_hyphenation(&text);
    }

    // Remove standalone page numbers
    if options.remove_page_numbers {
        text = remove_page_numbers(&text);
    }

    // Format URLs as markdown links
    if options.format_urls {
        text = format_urls(&text);
    }

    // Remove excessive newlines (more than 2 in a row)
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    // Trim leading and trailing whitespace, ensure ends with single newline
    text = text.trim().to_string();
    text.push('\n');

    text
}

/// Fix words broken across lines with spaces before the continuation
/// e.g., "Limoeiro do Nort e" -> "Limoeiro do Norte"
fn fix_hyphenation(text: &str) -> String {
    use once_cell::sync::Lazy;

    // Fix "word - word" patterns that should be "word-word" (compound words)
    // But be careful not to break list items (which start with "- ")
    static SPACED_HYPHEN_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ]) - ([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ])").unwrap()
    });

    let result = SPACED_HYPHEN_RE
        .replace_all(text, |caps: &regex::Captures| {
            format!("{}-{}", &caps[1], &caps[2])
        })
        .to_string();

    result
}

/// Remove standalone page numbers (lines that are just 1-4 digit numbers)
fn remove_page_numbers(text: &str) -> String {
    let mut result = Vec::new();
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Check if this line is just a number (1-4 digits)
        if trimmed.len() <= 4 && !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit())
        {
            // Check context to determine if this is a page number
            let prev_is_break = i > 0 && lines[i - 1].trim() == "---";
            let next_is_break = i + 1 < lines.len() && lines[i + 1].trim() == "---";
            let prev_is_empty = i > 0 && lines[i - 1].trim().is_empty();
            let next_is_empty = i + 1 < lines.len() && lines[i + 1].trim().is_empty();

            // Check if it's on its own line (surrounded by empty lines or page breaks)
            let is_isolated = (prev_is_break || prev_is_empty || i == 0)
                && (next_is_break || next_is_empty || i + 1 == lines.len());

            // Also remove numbers that appear right before a page break
            // (common pattern: content ends, page number, then ---)
            let before_break = i + 1 < lines.len()
                && (lines[i + 1].trim() == "---"
                    || (i + 2 < lines.len()
                        && lines[i + 1].trim().is_empty()
                        && lines[i + 2].trim() == "---"));

            if is_isolated || before_break {
                continue;
            }
        }

        result.push(*line);
    }

    result.join("\n")
}

/// Convert URLs to markdown links
fn format_urls(text: &str) -> String {
    use once_cell::sync::Lazy;

    // Match URLs - we'll check context manually to avoid formatting already-linked URLs
    static URL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"https?://[^\s<>\)\]]+[^\s<>\)\]\.\,;]").unwrap());

    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;

    for mat in URL_RE.find_iter(text) {
        let start = mat.start();
        let url = mat.as_str();

        // Check if this URL is already in a markdown link by looking at preceding chars
        let before = if start >= 2 {
            &text[start - 2..start]
        } else {
            ""
        };
        let already_linked = before.ends_with("](") || before.ends_with("](");

        // Also check if it's inside square brackets (link text)
        let prefix = &text[..start];
        let open_brackets = prefix.matches('[').count();
        let close_brackets = prefix.matches(']').count();
        let inside_link_text = open_brackets > close_brackets;

        if already_linked || inside_link_text {
            // Already formatted, keep as-is
            result.push_str(&text[last_end..mat.end()]);
        } else {
            // Add text before this URL
            result.push_str(&text[last_end..start]);
            // Format as markdown link
            result.push_str(&format!("[{}]({})", url, url));
        }
        last_end = mat.end();
    }

    // Add remaining text
    result.push_str(&text[last_end..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_list_item() {
        assert!(is_list_item("• Item one"));
        assert!(is_list_item("- Item two"));
        assert!(is_list_item("* Item three"));
        assert!(is_list_item("1. First"));
        assert!(is_list_item("2) Second"));
        assert!(is_list_item("a. Letter item"));
        assert!(!is_list_item("Regular text"));
    }

    #[test]
    fn test_format_list_item() {
        assert_eq!(format_list_item("• Item"), "- Item");
        assert_eq!(format_list_item("- Item"), "- Item");
        assert_eq!(format_list_item("1. First"), "1. First");
    }

    #[test]
    fn test_is_code_like() {
        assert!(is_code_like("const x = 5;"));
        assert!(is_code_like("function foo() {"));
        assert!(is_code_like("import React from 'react'"));
        assert!(!is_code_like("This is regular text."));
    }

    #[test]
    fn test_detect_header_level() {
        assert_eq!(detect_header_level(24.0, 12.0), Some(1));
        assert_eq!(detect_header_level(18.0, 12.0), Some(2));
        assert_eq!(detect_header_level(15.0, 12.0), Some(3));
        assert_eq!(detect_header_level(12.0, 12.0), None);
    }

    #[test]
    fn test_to_markdown() {
        let text = "• First item\n• Second item\n\nRegular paragraph.";
        let md = to_markdown(text, MarkdownOptions::default());
        assert!(md.contains("- First item"));
        assert!(md.contains("- Second item"));
    }
}
