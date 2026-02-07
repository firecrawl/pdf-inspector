//! Markdown conversion with structure detection
//!
//! This module converts extracted text to markdown, detecting:
//! - Headers (by font size)
//! - Lists (bullet points, numbered lists)
//! - Code blocks (monospace fonts, indentation)
//! - Paragraphs

use crate::extractor::{group_into_lines, TextItem, TextLine};
use std::collections::HashMap;

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
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            detect_headers: true,
            detect_lists: true,
            detect_code: true,
            base_font_size: None,
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
    let lines = group_into_lines(items);
    to_markdown_from_lines(lines, options)
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

    let mut output = String::new();
    let mut current_page = 0u32;
    let mut prev_y = f32::MAX;
    let mut in_list = false;

    for line in lines {
        // Page break
        if line.page != current_page {
            if current_page > 0 {
                output.push_str("\n---\n\n");
            }
            current_page = line.page;
            prev_y = f32::MAX;
        }

        // Paragraph break (large Y gap)
        let y_gap = prev_y - line.y;
        if y_gap > base_size * 2.0 && !output.ends_with("\n\n") {
            if in_list {
                in_list = false;
            }
            output.push('\n');
        }
        prev_y = line.y;

        let text = line.text();
        let trimmed = text.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Detect headers by font size
        if options.detect_headers {
            let line_font_size = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
            if let Some(header_level) = detect_header_level(line_font_size, base_size) {
                let prefix = "#".repeat(header_level);
                output.push_str(&format!("{} {}\n\n", prefix, trimmed));
                in_list = false;
                continue;
            }
        }

        // Detect list items
        if options.detect_lists && is_list_item(trimmed) {
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
                output.push_str(&format!("```\n{}\n```\n", trimmed));
                continue;
            }
        }

        // Regular text
        output.push_str(trimmed);
        output.push('\n');
    }

    // Clean up excessive newlines
    clean_markdown(output)
}

/// Font statistics for a document
struct FontStats {
    most_common_size: f32,
}

fn calculate_font_stats(lines: &[TextLine]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for line in lines {
        for item in &line.items {
            let size_key = (item.font_size * 10.0) as i32; // Round to 0.1
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
    if trimmed.len() >= 2 {
        let first = trimmed.chars().next().unwrap();
        let second = trimmed.chars().nth(1).unwrap();
        if first.is_ascii_alphabetic() && (second == '.' || second == ')') {
            return true;
        }
        if first == '(' && trimmed.chars().nth(2) == Some(')') {
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

/// Clean up markdown output
fn clean_markdown(mut text: String) -> String {
    // Remove excessive newlines (more than 2 in a row)
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    // Ensure ends with single newline
    text = text.trim_end().to_string();
    text.push('\n');

    text
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
