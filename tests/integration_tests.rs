//! Integration tests for pdf-to-markdown library

use pdf_inspector::detector::DetectionConfig;
use pdf_inspector::extractor::{group_into_lines, TextLine};
use pdf_inspector::{
    detect_pdf_type, extract_text, extract_text_with_positions, to_markdown, MarkdownOptions,
    PdfType, TextItem,
};

// Helper to create test TextItems
fn make_text_item(text: &str, x: f32, y: f32, font_size: f32, page: u32) -> TextItem {
    use pdf_inspector::extractor::ItemType;
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
    }
}

// ============================================================================
// Detection Config Tests
// ============================================================================

#[test]
fn test_detection_config_default() {
    let config = DetectionConfig::default();
    assert_eq!(config.max_pages_to_sample, 5);
    assert_eq!(config.min_text_ops_per_page, 3);
    assert!((config.text_page_ratio_threshold - 0.6).abs() < 0.001);
}

#[test]
fn test_detection_config_custom() {
    let config = DetectionConfig {
        max_pages_to_sample: 10,
        min_text_ops_per_page: 5,
        text_page_ratio_threshold: 0.8,
    };
    assert_eq!(config.max_pages_to_sample, 10);
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
    };
    assert_eq!(line.text(), "Single");
}

#[test]
fn test_text_line_empty() {
    let line = TextLine {
        items: vec![],
        y: 700.0,
        page: 1,
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
        },
        TextLine {
            items: vec![make_text_item("Second", 100.0, 680.0, 12.0, 1)],
            y: 680.0,
            page: 1,
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
    // Four heading tiers: 24.0 → H1, 18.0 → H2, 15.0 → H3, 13.5 → H4
    let items = vec![
        make_text_item("H1 Title", 100.0, 850.0, 24.0, 1),
        make_text_item("H2 Title", 100.0, 800.0, 18.0, 1),
        make_text_item("H3 Title", 100.0, 750.0, 15.0, 1),
        make_text_item("H4 Title", 100.0, 700.0, 13.5, 1),
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
