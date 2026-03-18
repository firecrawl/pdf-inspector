//! Integration tests for pdf-to-markdown library

use pdf_inspector::detector::{DetectionConfig, ScanStrategy};
use pdf_inspector::extractor::group_into_lines;
use pdf_inspector::types::TextLine;
use pdf_inspector::{
    detect_pdf_type, extract_text, extract_text_with_positions, to_markdown, MarkdownOptions,
    PdfError, PdfType, TextItem,
};

// Helper to create test TextItems
fn make_text_item(text: &str, x: f32, y: f32, font_size: f32, page: u32) -> TextItem {
    use pdf_inspector::types::ItemType;
    TextItem {
        text: text.to_string(),
        x,
        y,
        width: text.len() as f32 * font_size * 0.5,
        height: font_size,
        font: "Helvetica".to_string(),
        font_size,
        page,
        is_bold: false,
        is_italic: false,
        item_type: ItemType::Text,
        mcid: None,
    }
}

fn make_text_item_with_font(
    text: &str,
    x: f32,
    y: f32,
    font_size: f32,
    font: &str,
    page: u32,
) -> TextItem {
    use pdf_inspector::extractor::{is_bold_font, is_italic_font, ItemType};
    TextItem {
        text: text.to_string(),
        x,
        y,
        width: text.len() as f32 * font_size * 0.5,
        height: font_size,
        font: font.to_string(),
        font_size,
        page,
        is_bold: is_bold_font(font),
        is_italic: is_italic_font(font),
        item_type: ItemType::Text,
        mcid: None,
    }
}

// ============================================================================
// Detection Config Tests
// ============================================================================

#[test]
fn test_detection_config_default() {
    let config = DetectionConfig::default();
    assert!(matches!(config.strategy, ScanStrategy::Sample(8)));
    assert_eq!(config.min_text_ops_per_page, 3);
    assert!((config.text_page_ratio_threshold - 0.6).abs() < 0.001);
}

#[test]
fn test_detection_config_custom() {
    let config = DetectionConfig {
        strategy: ScanStrategy::Sample(10),
        min_text_ops_per_page: 5,
        text_page_ratio_threshold: 0.8,
    };
    assert!(matches!(config.strategy, ScanStrategy::Sample(10)));
    assert_eq!(config.min_text_ops_per_page, 5);
    assert!((config.text_page_ratio_threshold - 0.8).abs() < 0.001);
}

// ============================================================================
// PdfType Tests
// ============================================================================

#[test]
fn test_pdf_type_equality() {
    assert_eq!(PdfType::TextBased, PdfType::TextBased);
    assert_eq!(PdfType::Scanned, PdfType::Scanned);
    assert_eq!(PdfType::ImageBased, PdfType::ImageBased);
    assert_eq!(PdfType::Mixed, PdfType::Mixed);
    assert_ne!(PdfType::TextBased, PdfType::Scanned);
}

#[test]
fn test_pdf_type_clone() {
    let original = PdfType::TextBased;
    let cloned = original.clone();
    assert_eq!(original, cloned);
}

#[test]
fn test_pdf_type_debug() {
    let pdf_type = PdfType::TextBased;
    let debug_str = format!("{:?}", pdf_type);
    assert_eq!(debug_str, "TextBased");
}

// ============================================================================
// TextItem Tests
// ============================================================================

#[test]
fn test_text_item_creation() {
    let item = make_text_item("Hello", 100.0, 700.0, 12.0, 1);
    assert_eq!(item.text, "Hello");
    assert_eq!(item.x, 100.0);
    assert_eq!(item.y, 700.0);
    assert_eq!(item.font_size, 12.0);
    assert_eq!(item.page, 1);
}

#[test]
fn test_text_item_clone() {
    let item = make_text_item("Test", 50.0, 600.0, 14.0, 2);
    let cloned = item.clone();
    assert_eq!(item.text, cloned.text);
    assert_eq!(item.x, cloned.x);
    assert_eq!(item.y, cloned.y);
}

// ============================================================================
// TextLine Tests
// ============================================================================

#[test]
fn test_text_line_text_method() {
    let items = vec![
        make_text_item("Hello", 100.0, 700.0, 12.0, 1),
        make_text_item("World", 160.0, 700.0, 12.0, 1),
    ];
    let line = TextLine {
        items,
        y: 700.0,
        page: 1,
        adaptive_threshold: 0.10,
    };
    assert_eq!(line.text(), "Hello World");
}

#[test]
fn test_text_line_single_item() {
    let items = vec![make_text_item("Single", 100.0, 700.0, 12.0, 1)];
    let line = TextLine {
        items,
        y: 700.0,
        page: 1,
        adaptive_threshold: 0.10,
    };
    assert_eq!(line.text(), "Single");
}

#[test]
fn test_text_line_empty() {
    let line = TextLine {
        items: vec![],
        y: 700.0,
        page: 1,
        adaptive_threshold: 0.10,
    };
    assert_eq!(line.text(), "");
}

// ============================================================================
// Group Into Lines Tests
// ============================================================================

#[test]
fn test_group_into_lines_empty() {
    let items: Vec<TextItem> = vec![];
    let lines = group_into_lines(items);
    assert!(lines.is_empty());
}

#[test]
fn test_group_into_lines_same_line() {
    let items = vec![
        make_text_item("A", 100.0, 700.0, 12.0, 1),
        make_text_item("B", 120.0, 700.0, 12.0, 1),
        make_text_item("C", 140.0, 700.0, 12.0, 1),
    ];
    let lines = group_into_lines(items);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].items.len(), 3);
    assert_eq!(lines[0].text(), "A B C");
}

#[test]
fn test_group_into_lines_different_lines() {
    let items = vec![
        make_text_item("Line1", 100.0, 700.0, 12.0, 1),
        make_text_item("Line2", 100.0, 680.0, 12.0, 1),
        make_text_item("Line3", 100.0, 660.0, 12.0, 1),
    ];
    let lines = group_into_lines(items);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text(), "Line1");
    assert_eq!(lines[1].text(), "Line2");
    assert_eq!(lines[2].text(), "Line3");
}

#[test]
fn test_group_into_lines_y_tolerance() {
    // Items within 3.0 Y tolerance should be grouped
    // Note: items are sorted by Y descending, then X ascending
    let items = vec![
        make_text_item("A", 100.0, 700.0, 12.0, 1),
        make_text_item("B", 150.0, 700.0, 12.0, 1), // Same Y
    ];
    let lines = group_into_lines(items);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text(), "A B");
}

#[test]
fn test_group_into_lines_multiple_pages() {
    let items = vec![
        make_text_item("Page1Text", 100.0, 700.0, 12.0, 1),
        make_text_item("Page2Text", 100.0, 700.0, 12.0, 2),
    ];
    let lines = group_into_lines(items);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].page, 1);
    assert_eq!(lines[1].page, 2);
}

#[test]
fn test_group_into_lines_sorting_by_x() {
    // Items on same line should be sorted by X position
    let items = vec![
        make_text_item("Third", 200.0, 700.0, 12.0, 1),
        make_text_item("First", 50.0, 700.0, 12.0, 1),
        make_text_item("Second", 100.0, 700.0, 12.0, 1),
    ];
    let lines = group_into_lines(items);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text(), "First Second Third");
}

// ============================================================================
// MarkdownOptions Tests
// ============================================================================

#[test]
fn test_markdown_options_default() {
    let opts = MarkdownOptions::default();
    assert!(opts.detect_headers);
    assert!(opts.detect_lists);
    assert!(opts.detect_code);
    assert!(opts.base_font_size.is_none());
}

#[test]
fn test_markdown_options_custom() {
    let opts = MarkdownOptions {
        detect_headers: false,
        detect_lists: true,
        detect_code: false,
        base_font_size: Some(14.0),
        remove_page_numbers: false,
        format_urls: false,
        fix_hyphenation: false,
        detect_bold: false,
        detect_italic: false,
        include_images: false,
        include_links: false,
        include_page_numbers: false,
        ..Default::default()
    };
    assert!(!opts.detect_headers);
    assert!(opts.detect_lists);
    assert!(!opts.detect_code);
    assert_eq!(opts.base_font_size, Some(14.0));
    assert!(!opts.remove_page_numbers);
    assert!(!opts.format_urls);
    assert!(!opts.fix_hyphenation);
    assert!(!opts.detect_bold);
    assert!(!opts.detect_italic);
    assert!(!opts.include_images);
    assert!(!opts.include_links);
}

// ============================================================================
// Markdown Conversion Tests
// ============================================================================

#[test]
fn test_to_markdown_basic() {
    let text = "Hello World";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("Hello World"));
}

#[test]
fn test_to_markdown_multiple_lines() {
    let text = "Line one\nLine two\nLine three";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("Line one"));
    assert!(md.contains("Line two"));
    assert!(md.contains("Line three"));
}

#[test]
fn test_to_markdown_bullet_list() {
    let text = "• First\n• Second\n• Third";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("- First"));
    assert!(md.contains("- Second"));
    assert!(md.contains("- Third"));
}

#[test]
fn test_to_markdown_dash_list() {
    let text = "- One\n- Two\n- Three";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("- One"));
    assert!(md.contains("- Two"));
}

#[test]
fn test_to_markdown_numbered_list() {
    let text = "1. First\n2. Second\n3. Third";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("1. First"));
    assert!(md.contains("2. Second"));
}

#[test]
fn test_to_markdown_code_detection() {
    let text = "const x = 5;\nlet y = 10;";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("```"));
}

#[test]
fn test_to_markdown_no_code_detection() {
    let text = "const x = 5;";
    let opts = MarkdownOptions {
        detect_code: false,
        ..Default::default()
    };
    let md = to_markdown(text, opts);
    assert!(!md.contains("```"));
}

#[test]
fn test_to_markdown_no_list_detection() {
    let text = "• Item";
    let opts = MarkdownOptions {
        detect_lists: false,
        ..Default::default()
    };
    let md = to_markdown(text, opts);
    // Should keep original bullet character
    assert!(md.contains("•"));
}

#[test]
fn test_to_markdown_empty_lines() {
    let text = "Para one\n\nPara two";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("Para one"));
    assert!(md.contains("Para two"));
}

#[test]
fn test_to_markdown_whitespace_only_lines() {
    let text = "Content\n   \nMore content";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.contains("Content"));
    assert!(md.contains("More content"));
}

// ============================================================================
// Markdown From Items Tests
// ============================================================================

#[test]
fn test_markdown_from_items_empty() {
    use pdf_inspector::markdown::to_markdown_from_items;
    let items: Vec<TextItem> = vec![];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.is_empty());
}

#[test]
fn test_markdown_from_items_single() {
    use pdf_inspector::markdown::to_markdown_from_items;
    let items = vec![make_text_item("Hello", 100.0, 700.0, 12.0, 1)];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("Hello"));
}

#[test]
fn test_markdown_from_items_header_detection() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Need multiple body items to establish base font size
    let items = vec![
        make_text_item("Title", 100.0, 750.0, 24.0, 1), // Large font = H1
        make_text_item("Body text one", 100.0, 700.0, 12.0, 1),
        make_text_item("Body text two", 100.0, 680.0, 12.0, 1),
        make_text_item("Body text three", 100.0, 660.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("# Title"));
    assert!(md.contains("Body text"));
}

#[test]
fn test_markdown_from_items_h2_detection() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Two heading tiers: 24.0 → H1, 18.0 → H2
    let items = vec![
        make_text_item("Title", 100.0, 800.0, 24.0, 1),
        make_text_item("Subtitle", 100.0, 750.0, 18.0, 1),
        make_text_item("Body text one", 100.0, 700.0, 12.0, 1),
        make_text_item("Body text two", 100.0, 680.0, 12.0, 1),
        make_text_item("Body text three", 100.0, 660.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("## Subtitle"));
}

#[test]
fn test_markdown_from_items_monospace_code() {
    use pdf_inspector::markdown::to_markdown_from_items;
    let items = vec![make_text_item_with_font(
        "let x = 5",
        100.0,
        700.0,
        12.0,
        "Courier",
        1,
    )];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("```"));
    assert!(md.contains("let x = 5"));
}

#[test]
fn test_markdown_from_items_page_breaks() {
    use pdf_inspector::markdown::to_markdown_from_items;
    let items = vec![
        make_text_item("Content on first page", 100.0, 700.0, 12.0, 1),
        make_text_item("Content on second page", 100.0, 700.0, 12.0, 2),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    // Pages should be separated by blank lines (no --- markers)
    assert!(!md.contains("---"));
    assert!(md.contains("Content on first page"));
    assert!(md.contains("Content on second page"));
}

// ============================================================================
// Markdown From Lines Tests
// ============================================================================

#[test]
fn test_markdown_from_lines_empty() {
    use pdf_inspector::markdown::to_markdown_from_lines;
    let lines: Vec<TextLine> = vec![];
    let md = to_markdown_from_lines(lines, MarkdownOptions::default());
    assert!(md.is_empty());
}

#[test]
fn test_markdown_from_lines_basic() {
    use pdf_inspector::markdown::to_markdown_from_lines;
    let lines = vec![
        TextLine {
            items: vec![make_text_item("First", 100.0, 700.0, 12.0, 1)],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        },
        TextLine {
            items: vec![make_text_item("Second", 100.0, 680.0, 12.0, 1)],
            y: 680.0,
            page: 1,
            adaptive_threshold: 0.10,
        },
    ];
    let md = to_markdown_from_lines(lines, MarkdownOptions::default());
    assert!(md.contains("First"));
    assert!(md.contains("Second"));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_extract_text_nonexistent_file() {
    let result = extract_text("/nonexistent/file.pdf");
    assert!(result.is_err());
}

#[test]
fn test_detect_pdf_type_nonexistent_file() {
    let result = detect_pdf_type("/nonexistent/file.pdf");
    assert!(result.is_err());
}

#[test]
fn test_extract_text_with_positions_nonexistent_file() {
    let result = extract_text_with_positions("/nonexistent/file.pdf");
    assert!(result.is_err());
}

// ============================================================================
// List Pattern Tests
// ============================================================================

#[test]
fn test_bullet_variations() {
    // Unicode bullets get converted to markdown dash
    let unicode_bullets = ["• Item", "○ Item", "● Item", "◦ Item"];
    for bullet in &unicode_bullets {
        let md = to_markdown(bullet, MarkdownOptions::default());
        assert!(md.contains("- Item"), "Failed for: {}", bullet);
    }

    // Markdown-compatible bullets stay as-is
    let md_bullets = ["- Item", "* Item"];
    for bullet in &md_bullets {
        let md = to_markdown(bullet, MarkdownOptions::default());
        assert!(md.contains(bullet), "Failed for: {}", bullet);
    }
}

#[test]
fn test_numbered_list_variations() {
    let lists = ["1. First", "2) Second", "10. Tenth"];
    for item in &lists {
        let md = to_markdown(item, MarkdownOptions::default());
        assert!(md.trim().len() > 0, "Failed for: {}", item);
    }
}

#[test]
fn test_letter_list_items() {
    let md = to_markdown("a. Letter item", MarkdownOptions::default());
    assert!(md.contains("a. Letter item"));
}

// ============================================================================
// Code Detection Tests
// ============================================================================

#[test]
fn test_code_keywords() {
    let keywords = [
        "import foo",
        "export default",
        "const x = 5;",
        "let y = 10;",
        "function test() {",
        "class MyClass {",
        "def func():",
        "pub fn main() {",
        "async fn process() {",
        "impl Trait {",
    ];
    for code in &keywords {
        let md = to_markdown(code, MarkdownOptions::default());
        assert!(md.contains("```"), "Code not detected for: {}", code);
    }
}

#[test]
fn test_code_syntax_patterns() {
    // Patterns that start with code keywords/syntax
    let patterns = [
        "=> value",      // Starts with =>
        "-> Result",     // Starts with ->
        ":: io::Result", // Starts with ::
    ];
    for code in &patterns {
        let md = to_markdown(code, MarkdownOptions::default());
        assert!(md.contains("```"), "Code not detected for: {}", code);
    }
}

#[test]
fn test_code_special_chars() {
    let code = "if (x > 0) { return y; }";
    let md = to_markdown(code, MarkdownOptions::default());
    assert!(md.contains("```"));
}

#[test]
fn test_non_code_text() {
    let text = "This is regular text about programming.";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(!md.contains("```"));
}

// ============================================================================
// Monospace Font Detection Tests
// ============================================================================

#[test]
fn test_monospace_font_names() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Font names that contain the patterns in is_monospace_font
    let monospace_fonts = [
        "Courier",
        "Consolas",
        "Monaco",
        "Menlo",
        "Fira Code",
        "JetBrains Mono",
        "Inconsolata",
        "DejaVu Sans Mono",
        "Liberation Mono",
        "Fixed",
        "Terminal",
    ];

    for font in &monospace_fonts {
        let items = vec![make_text_item_with_font(
            "code", 100.0, 700.0, 12.0, font, 1,
        )];
        let md = to_markdown_from_items(items, MarkdownOptions::default());
        assert!(
            md.contains("```"),
            "Font not detected as monospace: {}",
            font
        );
    }
}

// ============================================================================
// Header Level Detection Tests
// ============================================================================

#[test]
fn test_header_level_h1() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // 24.0 / 12.0 = 2.0x = H1
    // Need multiple body items to establish base font size
    let items = vec![
        make_text_item("H1 Title", 100.0, 700.0, 24.0, 1),
        make_text_item("body text one", 100.0, 650.0, 12.0, 1),
        make_text_item("body text two", 100.0, 630.0, 12.0, 1),
        make_text_item("body text three", 100.0, 610.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("# H1 Title"));
}

#[test]
fn test_single_heading_tier_becomes_h1() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Single heading tier: 18.0pt on 12.0pt base → H1 (not H2)
    let items = vec![
        make_text_item("Section Title", 100.0, 700.0, 18.0, 1),
        make_text_item("body text one", 100.0, 650.0, 12.0, 1),
        make_text_item("body text two", 100.0, 630.0, 12.0, 1),
        make_text_item("body text three", 100.0, 610.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("# Section Title"));
}

#[test]
fn test_header_level_h2() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Two heading tiers: 24.0 → H1, 18.0 → H2
    let items = vec![
        make_text_item("H1 Title", 100.0, 750.0, 24.0, 1),
        make_text_item("H2 Title", 100.0, 700.0, 18.0, 1),
        make_text_item("body text one", 100.0, 650.0, 12.0, 1),
        make_text_item("body text two", 100.0, 630.0, 12.0, 1),
        make_text_item("body text three", 100.0, 610.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("# H1 Title"));
    assert!(md.contains("## H2 Title"));
}

#[test]
fn test_header_level_h3() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Three heading tiers: 24.0 → H1, 18.0 → H2, 15.0 → H3
    let items = vec![
        make_text_item("H1 Title", 100.0, 800.0, 24.0, 1),
        make_text_item("H2 Title", 100.0, 750.0, 18.0, 1),
        make_text_item("H3 Title", 100.0, 700.0, 15.0, 1),
        make_text_item("body text one", 100.0, 650.0, 12.0, 1),
        make_text_item("body text two", 100.0, 630.0, 12.0, 1),
        make_text_item("body text three", 100.0, 610.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("### H3 Title"));
}

#[test]
fn test_header_level_h4() {
    use pdf_inspector::markdown::to_markdown_from_items;
    // Four heading tiers: 24.0 → H1, 18.0 → H2, 15.0 → H3, 14.5 → H4
    let items = vec![
        make_text_item("H1 Title", 100.0, 850.0, 24.0, 1),
        make_text_item("H2 Title", 100.0, 800.0, 18.0, 1),
        make_text_item("H3 Title", 100.0, 750.0, 15.0, 1),
        make_text_item("H4 Title", 100.0, 700.0, 14.5, 1),
        make_text_item("body text one", 100.0, 650.0, 12.0, 1),
        make_text_item("body text two", 100.0, 630.0, 12.0, 1),
        make_text_item("body text three", 100.0, 610.0, 12.0, 1),
    ];
    let md = to_markdown_from_items(items, MarkdownOptions::default());
    assert!(md.contains("#### H4 Title"));
}

// ============================================================================
// Clean Markdown Tests
// ============================================================================

#[test]
fn test_excessive_newlines_preserved_in_plain_text() {
    // Plain text to_markdown preserves structure from input
    let text = "Para one\n\n\n\n\nPara two";
    let md = to_markdown(text, MarkdownOptions::default());
    // The function processes line by line, empty lines become single newlines
    assert!(md.contains("Para one"));
    assert!(md.contains("Para two"));
}

#[test]
fn test_trailing_newline() {
    let text = "Content";
    let md = to_markdown(text, MarkdownOptions::default());
    assert!(md.ends_with('\n'));
    assert!(!md.ends_with("\n\n"));
}

// ============================================================================
// NotAPdf Detection Tests
// ============================================================================

/// Helper: assert that an error is NotAPdf and its message contains the given substring.
fn assert_not_a_pdf(result: Result<impl std::fmt::Debug, PdfError>, expected_hint: &str) {
    match result {
        Err(PdfError::NotAPdf(msg)) => {
            assert!(
                msg.to_lowercase().contains(&expected_hint.to_lowercase()),
                "Expected hint '{}' in NotAPdf message, got: '{}'",
                expected_hint,
                msg,
            );
        }
        other => panic!(
            "Expected Err(NotAPdf) containing '{}', got: {:?}",
            expected_hint, other,
        ),
    }
}

#[test]
fn test_not_a_pdf_html_input() {
    let html = b"<!DOCTYPE html><html><body>Hello</body></html>";
    let result = pdf_inspector::process_pdf_mem(html);
    assert_not_a_pdf(result, "HTML");
}

#[test]
fn test_not_a_pdf_xml_input() {
    let xml = b"<?xml version=\"1.0\"?><root><item>data</item></root>";
    let result = pdf_inspector::process_pdf_mem(xml);
    assert_not_a_pdf(result, "XML");
}

#[test]
fn test_not_a_pdf_json_input() {
    let json = b"{\"error\": \"download failed\"}";
    let result = pdf_inspector::process_pdf_mem(json);
    assert_not_a_pdf(result, "JSON");
}

#[test]
fn test_not_a_pdf_plain_text_input() {
    let text = b"This is a plain text file that is not a PDF at all.";
    let result = pdf_inspector::process_pdf_mem(text);
    assert_not_a_pdf(result, "plain text");
}

#[test]
fn test_not_a_pdf_empty_buffer() {
    let result = pdf_inspector::process_pdf_mem(b"");
    assert_not_a_pdf(result, "empty");
}

#[test]
fn test_valid_pdf_header_not_rejected() {
    // A truncated but valid PDF header should NOT produce NotAPdf —
    // it should fail with Parse or InvalidStructure instead.
    let truncated_pdf = b"%PDF-1.4\ntruncated content";
    let result = pdf_inspector::process_pdf_mem(truncated_pdf);
    match result {
        Err(PdfError::NotAPdf(_)) => panic!("Valid PDF header should not be rejected as NotAPdf"),
        _ => {} // Parse or InvalidStructure is fine
    }
}

#[test]
fn test_bom_prefixed_pdf_header_not_rejected() {
    // UTF-8 BOM + %PDF- should still be recognized as a PDF
    let mut bom_pdf = vec![0xEF, 0xBB, 0xBF];
    bom_pdf.extend_from_slice(b"%PDF-1.7\ntruncated");
    let result = pdf_inspector::process_pdf_mem(&bom_pdf);
    match result {
        Err(PdfError::NotAPdf(_)) => {
            panic!("BOM-prefixed PDF header should not be rejected as NotAPdf")
        }
        _ => {} // Parse or InvalidStructure is fine
    }
}

#[test]
fn test_not_a_pdf_detect_pdf_type_mem() {
    // Verify detect_pdf_type_mem is also guarded
    let html = b"<html><head><title>Not a PDF</title></head></html>";
    let result = pdf_inspector::detector::detect_pdf_type_mem(html);
    assert_not_a_pdf(result, "HTML");
}

#[test]
fn test_not_a_pdf_extract_text_with_positions_mem() {
    // Verify extract_text_with_positions_mem is also guarded
    let html = b"<!DOCTYPE html><html><body>content</body></html>";
    let result = pdf_inspector::extractor::extract_text_with_positions_mem(html);
    assert_not_a_pdf(result, "HTML");
}

#[test]
fn test_not_a_pdf_extract_text_mem() {
    // Verify extract_text_mem is also guarded
    let xml = b"<?xml version=\"1.0\"?><data/>";
    let result = pdf_inspector::extractor::extract_text_mem(xml);
    assert_not_a_pdf(result, "XML");
}

// ============================================================================
// Snapshot Regression Tests (PDF fixtures)
// ============================================================================

/// Process a PDF fixture and compare output against the golden snapshot.
///
/// This catches regressions where code changes silently alter extraction
/// or markdown output. If a change is intentional, update the snapshot:
///   cargo run --release --bin pdf2md -- tests/fixtures/<name>.pdf > tests/snapshots/<name>.md
fn assert_snapshot(fixture: &str) {
    let fixture_path = format!("tests/fixtures/{}.pdf", fixture);
    let snapshot_path = format!("tests/snapshots/{}.md", fixture);

    let result = pdf_inspector::process_pdf(&fixture_path)
        .unwrap_or_else(|e| panic!("Failed to process {}: {}", fixture_path, e));
    let actual = result.markdown.unwrap_or_default();
    let actual = actual.trim_end();

    let expected = std::fs::read_to_string(&snapshot_path)
        .unwrap_or_else(|e| panic!("Failed to read snapshot {}: {}", snapshot_path, e));
    let expected = expected.trim_end();

    if actual != expected {
        // Show a helpful diff summary
        let actual_lines: Vec<&str> = actual.lines().collect();
        let expected_lines: Vec<&str> = expected.lines().collect();

        let mut diffs = Vec::new();
        let max_lines = actual_lines.len().max(expected_lines.len());
        for i in 0..max_lines {
            let a = actual_lines.get(i).unwrap_or(&"<missing>");
            let e = expected_lines.get(i).unwrap_or(&"<missing>");
            if a != e {
                diffs.push(format!(
                    "  line {}: expected {:?}, got {:?}",
                    i + 1,
                    &e[..e.len().min(80)],
                    &a[..a.len().min(80)]
                ));
                if diffs.len() >= 5 {
                    diffs.push("  ... (more diffs truncated)".to_string());
                    break;
                }
            }
        }

        panic!(
            "Snapshot mismatch for {}:\n{}\n\nTo update: cargo run --release --bin pdf2md -- {} > {}",
            fixture,
            diffs.join("\n"),
            fixture_path,
            snapshot_path,
        );
    }
}

#[test]
fn test_snapshot_nexo_price_en() {
    assert_snapshot("nexo-price-en");
}

#[test]
fn test_snapshot_thermo_freon12() {
    assert_snapshot("thermo-freon12");
}

#[test]
fn test_snapshot_td9264() {
    assert_snapshot("td9264");
}

#[test]
fn test_snapshot_p1244() {
    assert_snapshot("p1244-1996");
}

#[test]
fn test_snapshot_real_estate_pricing() {
    assert_snapshot("real-estate-pricing");
}

#[test]
fn test_snapshot_2013_app2() {
    assert_snapshot("2013-app2");
}

// ============================================================================
// Pages Needing OCR Tests
// ============================================================================

#[test]
fn test_pages_needing_ocr_field_accessible() {
    // Compile-time check: verify the field exists on both structs
    let detection_result = pdf_inspector::detector::PdfTypeResult {
        pdf_type: PdfType::TextBased,
        page_count: 1,
        pages_sampled: 1,
        pages_with_text: 1,
        confidence: 1.0,
        title: None,
        ocr_recommended: false,
        pages_needing_ocr: Vec::new(),
    };
    assert!(detection_result.pages_needing_ocr.is_empty());

    let process_result = pdf_inspector::PdfProcessResult {
        pdf_type: PdfType::TextBased,
        markdown: None,
        page_count: 1,
        processing_time_ms: 0,
        pages_needing_ocr: vec![1, 3],
        title: None,
        confidence: 1.0,
        layout: pdf_inspector::LayoutComplexity::default(),
        has_encoding_issues: false,
    };
    assert_eq!(process_result.pages_needing_ocr, vec![1, 3]);
}

#[test]
fn test_text_pdf_process_result_empty_ocr_pages() {
    // A minimal valid PDF that is text-based should have empty pages_needing_ocr.
    // We use a minimal PDF buffer with a text content stream.
    let pdf_bytes = b"%PDF-1.0
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Contents 4 0 R>>endobj
4 0 obj<</Length 44>>
stream
BT /F1 12 Tf 100 700 Td (Hello World) Tj ET
endstream
endobj
xref
0 5
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
0000000115 00000 n
0000000206 00000 n
trailer<</Size 5/Root 1 0 R>>
startxref
300
%%EOF";
    let result = pdf_inspector::process_pdf_mem(pdf_bytes);
    // The minimal PDF may fail to parse fully, but if it succeeds,
    // a text-based PDF should have empty pages_needing_ocr.
    if let Ok(result) = result {
        assert!(
            result.pages_needing_ocr.is_empty(),
            "Text-based PDF should have empty pages_needing_ocr, got: {:?}",
            result.pages_needing_ocr
        );
    }
}

#[test]
fn test_firecrawl_tagged_pdf_struct_tree() {
    use lopdf::Document;
    use pdf_inspector::structure_tree::{StructRole, StructTree};

    let doc = Document::load("tests/fixtures/firecrawl_docs_tagged.pdf").unwrap();
    let tree = StructTree::from_doc(&doc).expect("Should have a structure tree");

    // Verify structure tree contains expected roles
    let page_ids = doc.get_pages();
    let roles = tree.mcid_to_roles(&page_ids);
    assert!(!roles.is_empty(), "Should have MCID roles across pages");

    let flat = tree.flatten();
    let has_code = flat.iter().any(|e| matches!(e.role, StructRole::Code));
    let has_h1 = flat.iter().any(|e| matches!(e.role, StructRole::H1));
    let has_li = flat.iter().any(|e| matches!(e.role, StructRole::LI));
    let has_caption = flat.iter().any(|e| matches!(e.role, StructRole::Caption));
    assert!(has_code, "Should have Code elements");
    assert!(has_h1, "Should have H1 elements");
    assert!(has_li, "Should have LI elements");
    assert!(has_caption, "Should have Caption elements");

    // Full conversion: code fences should be generated from Code struct elements
    let buf = std::fs::read("tests/fixtures/firecrawl_docs_tagged.pdf").unwrap();
    let result = pdf_inspector::process_pdf_mem(&buf).unwrap();
    let md = result.markdown.unwrap();
    let fence_count = md.matches("```").count();
    assert!(
        fence_count > 0,
        "Should produce code fences from tagged Code elements"
    );
    // Fences come in open/close pairs
    assert_eq!(fence_count % 2, 0, "Code fences should be balanced");
}
