//! Markdown cleanup and post-processing.

use regex::Regex;

use super::{MarkdownOptions, MarkdownProfile};
use crate::text_utils::is_page_number_line;

/// Clean up markdown output with post-processing
pub(crate) fn clean_markdown(mut text: String, options: &MarkdownOptions) -> String {
    if options.profile == MarkdownProfile::Compact {
        // Dot-leader collapse saves tokens but changes source text, so it is
        // reserved for the explicit compact profile.
        text = collapse_dot_leaders(&text);
    }

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

    // Collapse consecutive spaces within text lines.
    // OCR text layers and some PDF producers emit trailing spaces on each
    // text item, which combine with gap-based space insertion to produce
    // double spaces ("Vice  President" instead of "Vice President").
    collapse_consecutive_spaces(&mut text);
    remove_spaces_before_closing_brackets(&mut text);
    remove_spaces_before_sentence_punctuation(&mut text);

    // Remove excessive newlines (more than 2 in a row)
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    // Trim leading and trailing whitespace, ensure ends with single newline
    text = text.trim().to_string();
    text.push('\n');

    text
}

/// Collapse runs of 2+ spaces to a single space within each line.
/// Preserves leading indentation and markdown table pipe alignment.
fn collapse_consecutive_spaces(text: &mut String) {
    let mut result = String::with_capacity(text.len());
    for line in text.split('\n') {
        if !result.is_empty() {
            result.push('\n');
        }
        // Preserve leading whitespace
        let trimmed = line.trim_start();
        let leading = &line[..line.len() - trimmed.len()];
        result.push_str(leading);
        // Collapse inner runs of spaces to single space
        let mut prev_space = false;
        for ch in trimmed.chars() {
            if ch == ' ' {
                if !prev_space {
                    result.push(' ');
                }
                prev_space = true;
            } else {
                prev_space = false;
                result.push(ch);
            }
        }
    }
    *text = result;
}

/// Remove spaces before closing square brackets.
/// Unit markers and markdown links occasionally pick up a gap-inserted space
/// before `]` (e.g. `[kg/m3 ]`), which is cosmetic padding.
fn remove_spaces_before_closing_brackets(text: &mut String) {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == ']' && result.ends_with(' ') {
            result.pop();
        }
        result.push(ch);
    }
    *text = result;
}

/// Remove a stray space before sentence punctuation ("word ." → "word.").
/// Style-boundary item splits (bold/italic/underline runs) can strand a
/// trailing period or comma in its own fragment, and several assembly paths
/// join fragments with spaces. Only fires when the punctuation ends the
/// token (followed by whitespace or end of text), so decimals ("3 .14" stays
/// untouched — no such input exists, but the guard is cheap) and dot leaders
/// (" ... ") are unaffected.
fn remove_spaces_before_sentence_punctuation(text: &mut String) {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        if matches!(ch, '.' | ',' | ';') && result.ends_with(' ') {
            let next = chars.get(i + 1);
            // `|` counts as a token end so table cells get the same fix.
            let token_ends = next.is_none_or(|c| c.is_whitespace() || *c == '|');
            // Never touch runs of dots (ellipsis / dot leaders).
            let in_dot_run = ch == '.' && next == Some(&'.');
            if token_ends && !in_dot_run {
                result.pop();
            }
        }
        result.push(ch);
    }
    *text = result;
}

/// Collapse dot leaders (runs of 4+ dots) into " ... "
/// Common in tables of contents: "Introduction...............................1" -> "Introduction ... 1"
fn collapse_dot_leaders(text: &str) -> String {
    use once_cell::sync::Lazy;
    static DOT_LEADER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.{4,}").unwrap());

    DOT_LEADER_RE.replace_all(text, " ... ").to_string()
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

/// Remove isolated page-number expressions from Markdown.
fn remove_page_numbers(text: &str) -> String {
    let mut result = Vec::new();
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Check for page number patterns
        if is_page_number_line(trimmed) {
            // Check context to determine if this is isolated
            let prev_is_break = i > 0 && lines[i - 1].trim() == "---";
            let next_is_break = i + 1 < lines.len() && lines[i + 1].trim() == "---";
            let prev_is_empty = i > 0 && lines[i - 1].trim().is_empty();
            let next_is_empty = i + 1 < lines.len() && lines[i + 1].trim().is_empty();

            // Check if it's on its own line (surrounded by empty lines or page breaks)
            let is_isolated = (prev_is_break || prev_is_empty || i == 0)
                && (next_is_break || next_is_empty || i + 1 == lines.len());

            // Also remove numbers that appear right before a page break
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
        // Use safe character boundary checking for multi-byte UTF-8
        let before = {
            let mut check_start = start.saturating_sub(2);
            // Find a valid character boundary
            while check_start > 0 && !text.is_char_boundary(check_start) {
                check_start -= 1;
            }
            if check_start < start && text.is_char_boundary(start) {
                &text[check_start..start]
            } else {
                ""
            }
        };
        let already_linked = before.ends_with("](") || before.ends_with("](");

        // Also check if it's inside square brackets (link text)
        // Ensure we're slicing at a valid char boundary
        let prefix = if text.is_char_boundary(start) {
            &text[..start]
        } else {
            // Find the nearest valid boundary before start
            let mut safe_start = start;
            while safe_start > 0 && !text.is_char_boundary(safe_start) {
                safe_start -= 1;
            }
            &text[..safe_start]
        };
        let open_brackets = prefix.matches('[').count();
        let close_brackets = prefix.matches(']').count();
        let inside_link_text = open_brackets > close_brackets;

        // Ensure mat boundaries are valid char boundaries
        let safe_last_end = if text.is_char_boundary(last_end) {
            last_end
        } else {
            let mut pos = last_end;
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };
        let safe_start = if text.is_char_boundary(start) {
            start
        } else {
            let mut pos = start;
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };
        let safe_end = if text.is_char_boundary(mat.end()) {
            mat.end()
        } else {
            let mut pos = mat.end();
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };

        if already_linked || inside_link_text {
            // Already formatted, keep as-is
            if safe_last_end <= safe_end {
                result.push_str(&text[safe_last_end..safe_end]);
            }
        } else {
            // Add text before this URL
            if safe_last_end <= safe_start {
                result.push_str(&text[safe_last_end..safe_start]);
            }
            // Format as markdown link
            result.push_str(&format!("[{}]({})", url, url));
        }
        last_end = safe_end;
    }

    // Add remaining text (ensure valid char boundary)
    let safe_last_end = if text.is_char_boundary(last_end) {
        last_end
    } else {
        let mut pos = last_end;
        while pos < text.len() && !text.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    };
    if safe_last_end < text.len() {
        result.push_str(&text[safe_last_end..]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fidelity_profile_preserves_dot_leaders() {
        let input = "Introduction............................1".to_string();
        let result = clean_markdown(input.clone(), &MarkdownOptions::default());
        assert_eq!(result, format!("{input}\n"));
    }

    #[test]
    fn compact_profile_collapses_dot_leaders() {
        let input = "Introduction............................1".to_string();
        let options = MarkdownOptions {
            profile: MarkdownProfile::Compact,
            ..MarkdownOptions::default()
        };
        assert_eq!(clean_markdown(input, &options), "Introduction ... 1\n");
    }

    // --- collapse_dot_leaders ---

    #[test]
    fn test_collapse_dot_leaders_four_or_more_dots() {
        assert_eq!(
            collapse_dot_leaders("Introduction............................1"),
            "Introduction ... 1"
        );
    }

    #[test]
    fn test_collapse_dot_leaders_three_dots_unchanged() {
        assert_eq!(collapse_dot_leaders("wait...what"), "wait...what");
    }

    #[test]
    fn test_collapse_dot_leaders_no_dots() {
        assert_eq!(collapse_dot_leaders("Hello World"), "Hello World");
    }

    #[test]
    fn test_collapse_dot_leaders_mixed() {
        let input = "Chapter 1.......10\nSome text... ok\nChapter 2........20";
        let result = collapse_dot_leaders(input);
        assert!(result.contains("Chapter 1 ... 10"));
        assert!(result.contains("Some text... ok"));
        assert!(result.contains("Chapter 2 ... 20"));
    }

    // --- remove_spaces_before_closing_brackets ---

    #[test]
    fn test_remove_spaces_before_closing_brackets() {
        let mut input = "Density [kg/m3 ] and [linked text ](https://example.com)".to_string();
        remove_spaces_before_closing_brackets(&mut input);
        assert_eq!(
            input,
            "Density [kg/m3] and [linked text](https://example.com)"
        );
    }

    // --- remove_spaces_before_sentence_punctuation ---

    #[test]
    fn strips_space_before_trailing_period() {
        let mut t = "Foreign insurance companies . The provisions".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "Foreign insurance companies. The provisions");
    }

    #[test]
    fn strips_space_before_period_at_cell_boundary() {
        let mut t = "|Applicability date .|This section|".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "|Applicability date.|This section|");
    }

    #[test]
    fn keeps_dot_leaders_and_ellipses() {
        let mut t = "Introduction ... 1".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "Introduction ... 1");
    }

    #[test]
    fn keeps_mid_token_periods() {
        let mut t = "version 3 .14 released".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "version 3 .14 released");
    }

    // --- fix_hyphenation ---

    #[test]
    fn test_fix_hyphenation_spaced_hyphen() {
        assert_eq!(fix_hyphenation("Limoeiro - Norte"), "Limoeiro-Norte");
    }

    #[test]
    fn test_fix_hyphenation_list_item_unchanged() {
        assert_eq!(
            fix_hyphenation("- item one\n- item two"),
            "- item one\n- item two"
        );
    }

    #[test]
    fn test_fix_hyphenation_accented_chars() {
        assert_eq!(fix_hyphenation("São - Paulo"), "São-Paulo");
    }

    #[test]
    fn test_fix_hyphenation_multiple_instances() {
        assert_eq!(
            fix_hyphenation("one - two and three - four"),
            "one-two and three-four"
        );
    }

    // --- is_page_number_line ---

    #[test]
    fn test_is_page_number_digits_1_to_4() {
        assert!(is_page_number_line("1"));
        assert!(is_page_number_line("42"));
        assert!(is_page_number_line("123"));
        assert!(is_page_number_line("9999"));
        assert!(!is_page_number_line("12345"));
    }

    #[test]
    fn test_is_page_number_page_x() {
        assert!(is_page_number_line("Page 5"));
        assert!(is_page_number_line("page 12"));
        assert!(is_page_number_line("Page123"));
    }

    #[test]
    fn test_is_page_number_page_x_of_y() {
        assert!(is_page_number_line("Page 3 of 10"));
        assert!(is_page_number_line("page 1 of 5"));
        assert!(is_page_number_line("Page 3 of 10 Report header"));
    }

    #[test]
    fn test_is_page_number_x_of_y() {
        assert!(is_page_number_line("3 of 10"));
    }

    #[test]
    fn test_is_page_number_centered_dash() {
        assert!(is_page_number_line("- 5 -"));
        assert!(is_page_number_line("-12-"));
    }

    #[test]
    fn test_is_page_number_page_of() {
        assert!(is_page_number_line("Page of"));
        assert!(is_page_number_line("page of 10"));
    }

    #[test]
    fn test_is_page_number_empty() {
        assert!(!is_page_number_line(""));
    }

    #[test]
    fn test_is_page_number_non_match() {
        assert!(!is_page_number_line("Hello World"));
        assert!(!is_page_number_line("Chapter 1"));
        assert!(!is_page_number_line("Total: 500"));
        assert!(!is_page_number_line("PAGE0-PARA2-END-MARKER-0"));
    }

    #[test]
    fn test_is_page_number_labeled_running_header() {
        assert!(is_page_number_line("Page 42 Chapter 5"));
        assert!(is_page_number_line("Page 42 explains the result"));
    }

    // --- remove_page_numbers ---

    #[test]
    fn test_remove_page_numbers_isolated_number() {
        let input = "Some text\n\n42\n\nMore text";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n42\n"));
        assert!(result.contains("Some text"));
        assert!(result.contains("More text"));
    }

    #[test]
    fn test_remove_page_numbers_before_break() {
        let input = "Content\n\n5\n---\nNext page";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n5\n"));
    }

    #[test]
    fn test_remove_page_numbers_in_context_kept() {
        let input = "Line A\nLine B\n42\nLine C\nLine D";
        let result = remove_page_numbers(input);
        assert!(result.contains("42"));
    }

    #[test]
    fn test_remove_page_numbers_labeled_header_with_content() {
        let input = "Content\n\nPage 42 explains the result\n---\nEnd";
        let result = remove_page_numbers(input);

        assert!(!result.contains("Page 42 explains the result"));
        assert!(result.contains("Content"));
        assert!(result.contains("End"));
    }

    #[test]
    fn test_remove_page_numbers_preserves_page_prefixed_content() {
        let input = "PAGE0-PARA2-START substantive report text PAGE0-PARA2-END-MARKER-0";
        let result = remove_page_numbers(input);

        assert_eq!(result, input);
    }

    #[test]
    fn test_remove_page_numbers_multiple_patterns() {
        let input = "\n1\n\nContent\n\n2\n\n---\nMore\n\n3\n";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n1\n"));
        assert!(!result.contains("\n2\n"));
        assert!(!result.contains("\n3\n"));
    }

    #[test]
    fn test_remove_page_numbers_empty() {
        assert_eq!(remove_page_numbers(""), "");
    }

    // --- format_urls ---

    #[test]
    fn test_format_urls_bare_url() {
        let result = format_urls("Visit https://example.com for info");
        assert!(result.contains("[https://example.com](https://example.com)"));
    }

    #[test]
    fn test_format_urls_already_linked() {
        let input = "[click](https://example.com)";
        assert_eq!(format_urls(input), input);
    }

    #[test]
    fn test_format_urls_inside_brackets() {
        let input = "[https://example.com](https://example.com)";
        let result = format_urls(input);
        assert!(!result.contains("[["));
    }

    #[test]
    fn test_format_urls_multiple() {
        let input = "See https://a.com and https://b.com";
        let result = format_urls(input);
        assert!(result.contains("[https://a.com](https://a.com)"));
        assert!(result.contains("[https://b.com](https://b.com)"));
    }

    #[test]
    fn test_format_urls_no_urls() {
        let input = "No links here";
        assert_eq!(format_urls(input), input);
    }
}
