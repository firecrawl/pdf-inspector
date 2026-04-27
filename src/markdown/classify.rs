//! Line classification: captions, lists, code detection.

/// Check if text is a figure/table caption or source citation
pub(crate) fn is_caption_line(text: &str) -> bool {
    let trimmed = text.trim();

    // Caption prefixes that always match (always followed by identifiers)
    let always_prefixes = [
        "Figura ",
        "Fig. ",
        "Fig ",
        "Tabela ",
        "Source:",
        "Fonte:",
        "Source ",
        "Fonte ",
        "Note:",
        "Nota:",
        "Chart ",
        "Gráfico ",
        "Graph ",
        "Diagram ",
        "Image ",
        "Imagem ",
        "Photo ",
        "Foto ",
    ];
    for prefix in &always_prefixes {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }

    // "Figure" and "Table" need a digit/reference after them to distinguish
    // captions ("Table 1", "Figure 3.2") from headings ("Table of Contents")
    for prefix in ["Figure ", "Table "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if rest
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit() || c == '(' || c == '#')
            {
                return true;
            }
        }
    }

    // Check case-insensitive patterns — require digit or punctuation after
    // prefix to avoid matching "Table of Contents" or "Figure drawing" etc.
    let lower = trimmed.to_lowercase();
    for pfx in ["figure ", "table "] {
        if let Some(rest) = lower.strip_prefix(pfx) {
            if rest
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit() || c == '(' || c == '#')
            {
                return true;
            }
        }
    }
    if lower.starts_with("source:") {
        return true;
    }

    false
}

/// Check if text starts with an unambiguous bullet marker (●, •, ○, ◦).
///
/// Narrower than [`is_list_item`]: it excludes numbered/lettered patterns
/// like `1.` or `a)`, which legitimately appear as section headings in many
/// documents. Used by the heading classifier to reject bullet lines without
/// also demoting numbered headings.
pub(crate) fn starts_with_bullet_marker(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("• ")
        || trimmed.starts_with("● ")
        || trimmed.starts_with("○ ")
        || trimmed.starts_with("◦ ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
}

/// Check if text looks like a list item
pub(crate) fn is_list_item(text: &str) -> bool {
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
pub(crate) fn format_list_item(text: &str) -> String {
    let trimmed = text.trim_start();

    // Convert various bullet styles to markdown
    // Note: bullet characters like • are multi-byte in UTF-8, use char indices
    for bullet in &['•', '○', '●', '◦'] {
        if let Some(rest) = trimmed.strip_prefix(*bullet) {
            return format!("- {}", rest.trim_start());
        }
        // Bullet inside a leading bold/italic run (e.g. "**● Label:** rest").
        // The run wraps both the marker and the following label because both
        // use a bold font in the PDF.
        for wrapper in ["**", "*"] {
            if let Some(after_open) = trimmed.strip_prefix(wrapper) {
                if let Some(rest) = after_open.strip_prefix(*bullet) {
                    return format!("- {}{}", wrapper, rest.trim_start());
                }
            }
        }
    }

    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return trimmed.to_string();
    }

    // Keep numbered lists as-is (markdown supports them)
    trimmed.to_string()
}

/// Check if text looks like code
pub(crate) fn is_code_like(text: &str) -> bool {
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
pub(crate) fn is_monospace_font(font_name: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_list_item_plain_bullet() {
        assert_eq!(format_list_item("● Item"), "- Item");
        assert_eq!(format_list_item("• Item"), "- Item");
    }

    #[test]
    fn format_list_item_bullet_inside_bold() {
        // PDF that uses bold font for both the marker and the label produces
        // a single bold run like "**● Label:** rest"; the bullet must still
        // be stripped and the bold wrapper preserved on the label.
        assert_eq!(
            format_list_item("**● Fraud: Willing cooperation;**"),
            "- **Fraud: Willing cooperation;**"
        );
        assert_eq!(
            format_list_item("**● Label:** rest of line"),
            "- **Label:** rest of line"
        );
        assert_eq!(format_list_item("*● Italic:* rest"), "- *Italic:* rest");
    }

    #[test]
    fn format_list_item_already_dash() {
        assert_eq!(format_list_item("- existing"), "- existing");
    }

    #[test]
    fn is_list_item_with_bullet_space() {
        assert!(is_list_item("● Item"));
        assert!(is_list_item("• Item"));
        assert!(is_list_item("- Item"));
    }
}
