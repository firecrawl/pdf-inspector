//! Integration tests for pdf-to-markdown library

use pdf_inspector::detector::{estimate_page_count_from_bytes, DetectionConfig, ScanStrategy};
use pdf_inspector::extractor::group_into_lines;
use pdf_inspector::types::ItemType;
use pdf_inspector::types::TextLine;
use pdf_inspector::{
    detect_pdf_type, detect_vector_grid_in_region_mem, extract_pages_markdown,
    extract_pages_markdown_mem, extract_tables_in_regions_mem, extract_text,
    extract_text_in_regions_mem, extract_text_with_positions, extract_text_with_positions_mem,
    process_pdf_mem, process_pdf_with_options, to_markdown, MarkdownOptions, PdfError, PdfOptions,
    PdfType, TextItem,
};
use std::collections::HashSet;

fn make_minimal_text_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];

    fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &str) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body.as_bytes());
        pdf.extend_from_slice(b"\nendobj\n");
    }

    add_object(
        &mut pdf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );

    let content = "BT /F1 12 Tf 100 700 Td (Hello World) Tj 0 -14 Td (Second Line) Tj 0 -14 Td (Third Line) Tj ET";
    add_object(
        &mut pdf,
        &mut offsets,
        4,
        &format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ),
    );
    add_object(
        &mut pdf,
        &mut offsets,
        5,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );

    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len(),
            xref_start
        )
        .as_bytes(),
    );

    pdf
}

fn truncate_eof_marker(mut pdf: Vec<u8>) -> Vec<u8> {
    assert!(pdf.ends_with(b"%%EOF"));
    pdf.pop();
    pdf
}

fn add_leading_tab(mut pdf: Vec<u8>) -> Vec<u8> {
    pdf.insert(0, b'\t');
    pdf
}

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
        is_underline: false,
        is_strikeout: false,
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
        is_underline: false,
        is_strikeout: false,
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
    assert_eq!(opts.profile, pdf_inspector::MarkdownProfile::Fidelity);
    assert!(opts.detect_headers);
    assert!(opts.detect_lists);
    assert!(opts.detect_code);
    assert!(opts.base_font_size.is_none());
}

#[test]
fn test_markdown_options_custom() {
    let opts = MarkdownOptions {
        profile: pdf_inspector::MarkdownProfile::Compact,
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
    assert_eq!(opts.profile, pdf_inspector::MarkdownProfile::Compact);
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
fn test_process_pdf_mem_repairs_truncated_eof_marker() {
    let pdf = truncate_eof_marker(make_minimal_text_pdf());

    let result = process_pdf_mem(&pdf).expect("truncated %%EO marker should be repaired");

    assert_eq!(result.pdf_type, PdfType::TextBased);
    assert_eq!(result.page_count, 1);
    assert!(
        result
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Hello World"),
        "repaired PDF should still extract text"
    );
}

#[test]
fn test_process_pdf_mem_repairs_leading_tab_and_truncated_eof() {
    let pdf = add_leading_tab(truncate_eof_marker(make_minimal_text_pdf()));

    let result = process_pdf_mem(&pdf).expect("leading whitespace + %%EO should be repaired");

    assert_eq!(result.pdf_type, PdfType::TextBased);
    assert_eq!(result.page_count, 1);
    assert!(
        result
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Hello World"),
        "repaired PDF should still extract text"
    );
}

#[test]
fn test_detect_pdf_type_repairs_container_from_path() {
    let pdf = add_leading_tab(truncate_eof_marker(make_minimal_text_pdf()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken-container.pdf");
    std::fs::write(&path, pdf).unwrap();

    let result = detect_pdf_type(&path).expect("detector should use shared repair loader");

    assert_eq!(result.pdf_type, PdfType::TextBased);
    assert_eq!(result.page_count, 1);
    assert_eq!(result.pages_with_text, 1);
}

#[test]
fn test_extract_text_mem_uses_container_repair() {
    let pdf = truncate_eof_marker(make_minimal_text_pdf());

    let text = pdf_inspector::extractor::extract_text_mem(&pdf)
        .expect("plain text extraction should use shared repair loader");

    assert!(text.contains("Hello World"));
}

#[test]
fn test_estimate_page_count_from_bytes_excludes_pages_tree() {
    let pdf = add_leading_tab(truncate_eof_marker(make_minimal_text_pdf()));

    assert_eq!(estimate_page_count_from_bytes(&pdf), 1);
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
        ocr_reasons_by_page: std::collections::BTreeMap::new(),
    };
    assert!(detection_result.pages_needing_ocr.is_empty());

    let process_result = pdf_inspector::PdfProcessResult {
        pdf_type: PdfType::TextBased,
        markdown: None,
        page_count: 1,
        processing_time_ms: 0,
        pages_needing_ocr: vec![1, 3],
        ocr_reasons_by_page: Vec::new(),
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

#[test]
fn test_identity_h_no_tounicode_suppresses_garbage() {
    // shinagawa_identity_h.pdf uses YuGothic with Identity-H encoding and no
    // usable ToUnicode CMap. The raw CID bytes (e.g. 0x08 0x37, 0x0E 0x0F)
    // contain non-ASCII high bytes and previously fell through to the
    // per-byte Latin-1 fallback, producing high-Latin-1 mojibake that
    // `is_cid_garbage` flagged. The Type0/CID guard in
    // `extract_text_from_operand` now emits one U+FFFD per CID instead of
    // mojibake; `detect_encoding_issues` trips on that and suppresses the
    // markdown / flags the page for OCR — so we still pass this test, but
    // via the deliberate marker path rather than by accident.
    let buf = std::fs::read("tests/fixtures/shinagawa_identity_h.pdf").unwrap();

    // Pre-suppression check: the raw text items must contain the U+FFFD
    // markers that prove the Type0/CID fallback fired. This pins the
    // mechanism so a future regression that re-enables Latin-1 mojibake
    // would fail loudly here, not just silently change the suppression
    // chain to one that depends on `is_cid_garbage` + high-Latin-1 chars.
    let items = pdf_inspector::extractor::extract_text_with_positions_mem(&buf).unwrap();
    let combined: String = items.iter().map(|i| i.text.as_str()).collect();
    assert!(
        combined.contains('\u{FFFD}'),
        "Type0/CID font with unparseable ToUnicode CMap should emit U+FFFD per CID; \
         got {} chars: {:?}",
        combined.len(),
        &combined[..combined.len().min(100)]
    );
    assert!(
        !combined
            .chars()
            .any(|c| ('\u{0080}'..='\u{00FF}').contains(&c)),
        "Latin-1 mojibake (high bytes) must not leak from Type0/CID fallback; got: {:?}",
        &combined[..combined.len().min(100)]
    );

    let result = pdf_inspector::process_pdf_mem(&buf).unwrap();

    // Page 1 should be flagged for OCR
    assert!(
        result.pages_needing_ocr.contains(&1),
        "Page with Identity-H font without ToUnicode should be flagged for OCR"
    );

    // Markdown should be empty (garbage suppressed)
    let md = result.markdown.unwrap_or_default();
    assert!(
        md.trim().is_empty(),
        "Garbage CID text should be suppressed, got {} chars: {:?}",
        md.len(),
        &md[..md.len().min(100)]
    );
}

#[test]
fn test_rotated_table_layout_correction() {
    // tnagriculture_06_12.pdf has landscape content in a portrait page via
    // a 90° CCW text matrix [0, b, -b, 0, tx, ty].  Without rotation
    // correction, the table is read sideways (jumbled numbers).
    let result =
        process_pdf_with_options("tests/fixtures/tnagriculture_06_12.pdf", PdfOptions::new())
            .unwrap();
    let md = result.markdown.unwrap_or_default();

    // Title should appear near the top
    assert!(
        md.contains("DISTRICT WISE PRODUCTION OF SPICES AND CONDIMENTS"),
        "Should extract the table title"
    );

    // District names should be readable (not jumbled with numbers)
    assert!(
        md.contains("Ariyalur"),
        "Should extract district name Ariyalur"
    );
    assert!(
        md.contains("Coimbatore"),
        "Should extract district name Coimbatore"
    );

    // Spice column headers should appear
    assert!(
        md.contains("CARDAMOM"),
        "Should extract spice header CARDAMOM"
    );
    assert!(
        md.contains("RED CHILLIES"),
        "Should extract spice header RED CHILLIES"
    );

    // Table should be formatted as markdown table (has pipe delimiters)
    let has_table_row = md
        .lines()
        .any(|l: &str| l.contains('|') && l.contains("Ariyalur"));
    assert!(
        has_table_row,
        "District data should be in a markdown table row"
    );
}

// =========================================================================
// extract_text_in_regions_mem tests
// =========================================================================

/// Build full-page region args for `page_count` pages.
/// Uses a generously large bbox (1200x1200) to capture any page size.
fn full_page_regions(page_count: u32) -> Vec<(u32, Vec<[f32; 4]>)> {
    (0..page_count)
        .map(|p| (p, vec![[0.0, 0.0, 1200.0, 1200.0]]))
        .collect()
}

/// Normalize text for comparison: lowercase, strip non-alphanumeric, split into words.
fn normalize_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 3)
        .collect()
}

/// Fraction of normalized words in `a` that also appear in `b`.
fn word_overlap_ratio(a: &str, b: &str) -> f64 {
    let words_a = normalize_words(a);
    if words_a.is_empty() {
        return if normalize_words(b).is_empty() {
            1.0
        } else {
            0.0
        };
    }
    let words_b = normalize_words(b);
    let overlap = words_a.intersection(&words_b).count();
    overlap as f64 / words_a.len() as f64
}

#[test]
fn test_extract_regions_mem_basic_text_pdf() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let result = process_pdf_mem(&buf).unwrap();
    let page_count = result.page_count;

    let regions = extract_text_in_regions_mem(&buf, &full_page_regions(page_count)).unwrap();
    assert_eq!(regions.len(), page_count as usize);

    // Each result should have exactly 1 region (we passed one per page)
    for r in &regions {
        assert_eq!(r.regions.len(), 1);
    }

    // First page should have non-empty text
    let first = &regions[0].regions[0];
    assert!(!first.text.trim().is_empty(), "First page should have text");
    assert_eq!(regions[0].page, 0);
}

#[test]
fn test_extract_regions_mem_identity_h_needs_ocr() {
    let buf = std::fs::read("tests/fixtures/shinagawa_identity_h.pdf").unwrap();
    let regions =
        extract_text_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();
    assert_eq!(regions.len(), 1);
    assert!(
        regions[0].regions[0].needs_ocr,
        "Identity-H font without ToUnicode should trigger needs_ocr"
    );
}

/// ParseBench `text_simple__att10k.pdf` (issue #118): the producer authored a
/// broken ToUnicode CMap that shifts every character by a per-range constant,
/// and the embedded subset font has no `cmap` table to recover from. The
/// resulting ciphertext is 100% printable ASCII, so it must be caught by the
/// substitution-cipher statistics and routed to OCR instead of served silently.
#[test]
fn test_extract_pages_mem_shifted_cipher_tounicode_needs_ocr() {
    let buf = std::fs::read("tests/fixtures/shifted_cipher_tounicode.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, None).unwrap();

    assert_eq!(result.pages.len(), 1);
    assert!(
        result.pages[0].needs_ocr,
        "shifted-cipher garbled page should be flagged needs_ocr"
    );
    assert!(
        result.pages[0].markdown.is_empty(),
        "garbled markdown should be suppressed"
    );
    assert_eq!(result.pages_needing_ocr, vec![1]);
    assert_eq!(
        result.pages[0].ocr_reason.as_deref(),
        Some("suspected_garbled_text")
    );
}

#[test]
fn test_extract_regions_mem_multiple_regions_per_page() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let regions = extract_text_in_regions_mem(
        &buf,
        &[(
            0,
            vec![
                [0.0, 0.0, 300.0, 100.0],   // small top-left
                [0.0, 0.0, 1200.0, 1200.0], // full page
            ],
        )],
    )
    .unwrap();

    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].regions.len(), 2);

    let small_len = regions[0].regions[0].text.len();
    let full_len = regions[0].regions[1].text.len();
    assert!(
        full_len >= small_len,
        "Full-page region ({full_len}) should have at least as much text as small region ({small_len})"
    );
}

#[test]
fn test_extract_regions_mem_nonexistent_page() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let regions =
        extract_text_in_regions_mem(&buf, &[(9999, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();
    assert_eq!(regions.len(), 1);
    assert!(
        regions[0].regions[0].needs_ocr,
        "Nonexistent page should trigger needs_ocr"
    );
}

#[test]
fn test_extract_regions_mem_empty_region() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let regions = extract_text_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 0.0, 0.0]])]).unwrap();
    assert_eq!(regions.len(), 1);
    assert!(
        regions[0].regions[0].needs_ocr,
        "Zero-area region should trigger needs_ocr"
    );
}

#[test]
fn test_extract_regions_mem_not_a_pdf() {
    let result = extract_text_in_regions_mem(b"not a pdf", &[(0, vec![[0.0, 0.0, 100.0, 100.0]])]);
    assert!(result.is_err(), "Non-PDF input should return an error");
}

#[test]
fn test_extract_regions_mem_rotated_page_not_false_empty() {
    let buf = std::fs::read("tests/fixtures/tnagriculture_06_12.pdf").unwrap();
    let regions =
        extract_text_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].regions.len(), 1);
    let region = &regions[0].regions[0];
    assert!(
        !region.text.trim().is_empty(),
        "Rotated page full-region extraction should not be empty"
    );
    assert!(
        !region.needs_ocr,
        "Rotated page with native text should not be flagged for OCR fallback"
    );
    assert!(
        region
            .text
            .contains("DISTRICT WISE PRODUCTION OF SPICES AND CONDIMENTS"),
        "Expected known title from rotated fixture in extracted region text"
    );
}

#[test]
fn test_collect_text_in_region_keeps_partial_overlap_items() {
    let item = make_text_item("EdgeWord", 100.0, 700.0, 12.0, 1);
    // Region intersects only the left edge of the item. Center x=124 falls
    // outside x=[95,120], so center-only containment would drop it.
    let text = pdf_inspector::collect_text_in_region(&[item], 95.0, 80.0, 120.0, 110.0, 800.0);
    assert!(
        text.contains("EdgeWord"),
        "Partially overlapping items should be retained in region extraction"
    );
}

#[test]
fn test_collect_text_in_region_uses_rtl_sorting() {
    let items = vec![
        make_text_item("بكم", 240.0, 700.0, 12.0, 1),
        make_text_item("مرحبا", 300.0, 700.0, 12.0, 1),
    ];
    let text = pdf_inspector::collect_text_in_region(&items, 0.0, 0.0, 600.0, 800.0, 800.0);
    assert_eq!(
        text, "مرحبا بكم",
        "Region path should reuse RTL-aware line sorting"
    );
}

// =========================================================================
// Fast vs normal extraction comparison
// =========================================================================

/// For each text-based fixture PDF, compare `extract_text_in_regions_mem` (fast path)
/// against `process_pdf_mem` (normal path). If the fast path claims needs_ocr=false
/// for a page, verify the extracted text has meaningful overlap with the normal
/// markdown output — catching silent quality regressions.
#[test]
fn test_extract_regions_fast_vs_normal_comparison() {
    let fixtures = [
        "tests/fixtures/nexo-price-en.pdf",
        "tests/fixtures/td9264.pdf",
        "tests/fixtures/p1244-1996.pdf",
        "tests/fixtures/real-estate-pricing.pdf",
        "tests/fixtures/2013-app2.pdf",
        "tests/fixtures/firecrawl_docs_tagged.pdf",
        "tests/fixtures/thermo-freon12.pdf",
    ];

    for fixture in &fixtures {
        let buf = std::fs::read(fixture).unwrap();
        let normal = process_pdf_mem(&buf).unwrap();
        let normal_md = normal.markdown.as_deref().unwrap_or("");
        let page_count = normal.page_count;
        let ocr_pages: HashSet<u32> = normal.pages_needing_ocr.iter().copied().collect();

        let regions = extract_text_in_regions_mem(&buf, &full_page_regions(page_count)).unwrap();

        assert_eq!(
            regions.len(),
            page_count as usize,
            "{fixture}: result count should match page count"
        );

        for pr in &regions {
            let region = &pr.regions[0];
            if !region.needs_ocr && !region.text.trim().is_empty() {
                // Fast path claims this text is trustworthy.
                // Check that its words appear in the normal markdown output.
                let overlap = word_overlap_ratio(&region.text, normal_md);
                assert!(
                    overlap >= 0.3,
                    "{fixture} page {}: fast path says needs_ocr=false but only {:.0}% word \
                     overlap with normal extraction (threshold 30%). \
                     Fast text sample: {:?}",
                    pr.page,
                    overlap * 100.0,
                    &region.text[..region.text.len().min(200)],
                );
            }

            // If fast path flags needs_ocr but normal path didn't, that's overly
            // conservative but not a bug — just worth knowing.
            if region.needs_ocr && !ocr_pages.contains(&(pr.page + 1)) {
                eprintln!(
                    "INFO: {fixture} page {}: fast path says needs_ocr=true but normal path extracted fine (conservative, not a bug)",
                    pr.page,
                );
            }
        }
    }
}

// =========================================================================
// extract_tables_in_regions_mem tests
// =========================================================================

#[test]
fn test_extract_tables_in_regions_table_pdf() {
    // tnagriculture has a clear table with district names and spice columns
    let buf = std::fs::read("tests/fixtures/tnagriculture_06_12.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].regions.len(), 1);

    let region = &results[0].regions[0];
    // Should detect a table with pipe-delimited markdown
    if !region.needs_ocr {
        assert!(
            region.text.contains('|'),
            "Table output should contain pipe delimiters"
        );
        // Should have separator row
        assert!(
            region.text.lines().any(|l| l.contains("---")),
            "Table output should contain separator row"
        );
    }
}

#[test]
fn test_extract_tables_in_regions_non_table_region() {
    // Use a small region that likely won't contain enough items for a table
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 50.0, 50.0]])]).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].regions.len(), 1);

    let region = &results[0].regions[0];
    // Small region with few items should fall back to needs_ocr
    assert!(
        region.needs_ocr,
        "Non-table region should set needs_ocr = true"
    );
    assert!(
        region.text.is_empty(),
        "Non-table region should have empty text"
    );
}

#[test]
fn test_extract_tables_in_regions_empty_region() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let results = extract_tables_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 0.0, 0.0]])]).unwrap();

    assert_eq!(results.len(), 1);
    let region = &results[0].regions[0];
    assert!(region.needs_ocr);
    assert!(region.text.is_empty());
}

#[test]
fn test_extract_tables_in_regions_identity_h_needs_ocr() {
    let buf = std::fs::read("tests/fixtures/shinagawa_identity_h.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(0, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();

    assert_eq!(results.len(), 1);
    let region = &results[0].regions[0];
    assert!(region.needs_ocr, "Identity-H font should trigger needs_ocr");
}

#[test]
fn test_extract_tables_in_regions_not_a_pdf() {
    let result =
        extract_tables_in_regions_mem(b"not a pdf", &[(0, vec![[0.0, 0.0, 100.0, 100.0]])]);
    assert!(result.is_err());
}

#[test]
fn test_extract_tables_in_regions_nonexistent_page() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(9999, vec![[0.0, 0.0, 1200.0, 1200.0]])]).unwrap();

    assert_eq!(results.len(), 1);
    let region = &results[0].regions[0];
    assert!(region.needs_ocr);
    assert!(region.text.is_empty());
}

#[test]
fn test_bits_pilani_page4_table_detection() {
    // Page 4 (0-indexed 3) has a table with multi-line wrapped headers and
    // numeric data columns. The heuristic detector previously failed because:
    // 1. Header items at different X positions than data created extra column
    //    clusters (6 cols instead of 4)
    // 2. Spanning super-header row ("First Degree | First Degree") produced
    //    duplicate header cells that looks_like_partial_table_ex rejected
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(3, vec![[0.0, 0.0, 612.0, 792.0]])]).unwrap();
    assert_eq!(results.len(), 1);
    let region = &results[0].regions[0];
    assert!(
        !region.needs_ocr,
        "Page 4 table should be detected, got needs_ocr=true"
    );
    assert!(
        region.text.contains("BIO"),
        "Should contain department name BIO"
    );
    assert!(region.text.contains("8.23"), "Should contain numeric data");
}

#[test]
fn test_bits_pilani_page8_table_detection() {
    // Page 8 (0-indexed 7) has a numbered-row table that already worked.
    // Verify it still works after changes.
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();
    let results =
        extract_tables_in_regions_mem(&buf, &[(7, vec![[0.0, 0.0, 612.0, 792.0]])]).unwrap();
    assert_eq!(results.len(), 1);
    let region = &results[0].regions[0];
    assert!(!region.needs_ocr, "Page 8 table should still be detected");
}

#[test]
fn test_extract_tables_in_regions_uses_line_grid() {
    // Stroked-grid table (m/l/S path operators forming a 2x2 grid).
    // The heuristic text-only detector handles the same cells already,
    // so this guards that the line-backed path doesn't regress: the
    // markdown still contains all four data cells.
    let buf = synthetic_vector_grid_pdf(false);
    let results =
        extract_tables_in_regions_mem(&buf, &[(0, vec![[40.0, 50.0, 220.0, 760.0]])]).unwrap();
    let region = &results[0].regions[0];
    assert!(
        !region.needs_ocr,
        "stroked-grid table should be extracted, got needs_ocr=true"
    );
    for tok in ["A1", "B1", "A2", "B2"] {
        assert!(
            region.text.contains(tok),
            "expected '{tok}' in output, got: {}",
            region.text
        );
    }
    assert!(
        region.text.contains('|'),
        "expected pipe-delimited markdown"
    );
}

// =========================================================================
// extract_tables_with_structure_mem tests (TSR-aware path)
// =========================================================================

/// Build an 8-element 4-corner polygon `[x1,y1, x2,y1, x2,y2, x1,y2]` from
/// an axis-aligned rect — matches the format SLANet emits for cell bboxes.
fn poly(x1: f32, y1: f32, x2: f32, y2: f32) -> Vec<f32> {
    vec![x1, y1, x2, y1, x2, y2, x1, y2]
}

fn synthetic_dense_table_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let content_id = doc.new_object_id();

    doc.objects.insert(
        font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }
        .into(),
    );

    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 10.into()]),
        Operation::new("Td", vec![20.into(), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal("Branch Name")]),
        Operation::new("Td", vec![100.into(), 0.into()]),
        Operation::new("Tj", vec![Object::string_literal("Deposits")]),
        Operation::new("Td", vec![Object::Integer(-100), Object::Real(-16.8)]),
        Operation::new("Tj", vec![Object::string_literal("Oak Street")]),
        Operation::new("Td", vec![100.into(), 0.into()]),
        Operation::new("Tj", vec![Object::string_literal("100")]),
        Operation::new("Td", vec![Object::Integer(-100), Object::Real(-16.8)]),
        Operation::new("Tj", vec![Object::string_literal("Boardwalk")]),
        Operation::new("Td", vec![100.into(), 0.into()]),
        Operation::new("Tj", vec![Object::string_literal("200")]),
        Operation::new("ET", vec![]),
    ];
    let content = Content { operations }.encode().unwrap();
    doc.objects
        .insert(content_id, Stream::new(dictionary! {}, content).into());

    doc.objects.insert(
        page_id,
        dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 800.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => font_id,
                },
            },
            "Contents" => content_id,
        }
        .into(),
    );
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn synthetic_vector_grid_pdf(two_tables: bool) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    fn push_grid(
        operations: &mut Vec<Operation>,
        x_left: i64,
        x_mid: i64,
        x_right: i64,
        y_top: i64,
        y_mid: i64,
        y_bottom: i64,
    ) {
        for y in [y_top, y_mid, y_bottom] {
            operations.push(Operation::new("m", vec![x_left.into(), y.into()]));
            operations.push(Operation::new("l", vec![x_right.into(), y.into()]));
        }
        for x in [x_left, x_mid, x_right] {
            operations.push(Operation::new("m", vec![x.into(), y_bottom.into()]));
            operations.push(Operation::new("l", vec![x.into(), y_top.into()]));
        }
        operations.push(Operation::new("S", vec![]));
    }

    fn push_text(operations: &mut Vec<Operation>, x: i64, y: i64, text: &str) {
        operations.push(Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
        ));
        operations.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    }

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let content_id = doc.new_object_id();

    doc.objects.insert(
        font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }
        .into(),
    );

    let mut operations = Vec::new();
    push_grid(&mut operations, 50, 130, 210, 740, 710, 670);
    if two_tables {
        push_grid(&mut operations, 50, 130, 210, 560, 530, 490);
    }

    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F1".into(), 10.into()]));
    push_text(&mut operations, 70, 724, "A1");
    push_text(&mut operations, 150, 724, "B1");
    push_text(&mut operations, 70, 688, "A2");
    push_text(&mut operations, 150, 688, "B2");
    if two_tables {
        push_text(&mut operations, 70, 544, "C1");
        push_text(&mut operations, 150, 544, "D1");
        push_text(&mut operations, 70, 508, "C2");
        push_text(&mut operations, 150, 508, "D2");
    }
    operations.push(Operation::new("ET", vec![]));

    let content = Content { operations }.encode().unwrap();
    doc.objects
        .insert(content_id, Stream::new(dictionary! {}, content).into());

    doc.objects.insert(
        page_id,
        dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 800.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => font_id,
                },
            },
            "Contents" => content_id,
        }
        .into(),
    );
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn synthetic_vector_grid_three_row_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let content_id = doc.new_object_id();

    doc.objects.insert(
        font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }
        .into(),
    );

    let mut operations = Vec::new();
    for y in [740, 710, 680, 650] {
        operations.push(Operation::new("m", vec![50.into(), y.into()]));
        operations.push(Operation::new("l", vec![210.into(), y.into()]));
    }
    for x in [50, 130, 210] {
        operations.push(Operation::new("m", vec![x.into(), 650.into()]));
        operations.push(Operation::new("l", vec![x.into(), 740.into()]));
    }
    operations.push(Operation::new("S", vec![]));

    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F1".into(), 10.into()]));
    for (x, y, text) in [
        (70, 724, "Branch"),
        (150, 724, "Deposits"),
        (70, 694, "Oak"),
        (150, 694, "100"),
        (70, 664, "Boardwalk"),
        (150, 664, "200"),
    ] {
        operations.push(Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
        ));
        operations.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    }
    operations.push(Operation::new("ET", vec![]));

    let content = Content { operations }.encode().unwrap();
    doc.objects
        .insert(content_id, Stream::new(dictionary! {}, content).into());
    doc.objects.insert(
        page_id,
        dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 800.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => font_id,
                },
            },
            "Contents" => content_id,
        }
        .into(),
    );
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.75,
        "expected {actual} to be close to {expected}"
    );
}

#[test]
fn test_detect_vector_grid_in_region_line_pdf() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};

    let buf = synthetic_vector_grid_pdf(false);
    let crop = [50.0_f32, 60.0, 210.0, 130.0];
    let detected = detect_vector_grid_in_region_mem(&buf, 0, crop, 72.0)
        .unwrap()
        .expect("ruled vector table should be detected");

    assert_eq!(detected.cell_bboxes.len(), 4);
    assert_eq!(
        detected
            .structure_tokens
            .iter()
            .filter(|tok| tok.as_str() == "<td></td>")
            .count(),
        4
    );
    assert_eq!(detected.structure_tokens.first().unwrap(), "<table>");
    assert_eq!(detected.structure_tokens.last().unwrap(), "</table>");

    let first = &detected.cell_bboxes[0];
    assert_close(first[0], 0.0);
    assert_close(first[1], 0.0);
    assert_close(first[2], 80.0);
    assert_close(first[3], 30.0);

    let markdown = extract_tables_with_structure_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: crop,
            render_dpi: 72.0,
            structure_tokens: detected.structure_tokens,
            cell_bboxes: detected.cell_bboxes,
        }],
    )
    .unwrap()
    .remove(0);

    assert!(markdown.contains("A1"));
    assert!(markdown.contains("B1"));
    assert!(markdown.contains("A2"));
    assert!(markdown.contains("B2"));
}

#[test]
fn test_detect_vector_grid_in_region_text_pdf_returns_none() {
    let buf = make_minimal_text_pdf();
    let detected =
        detect_vector_grid_in_region_mem(&buf, 0, [0.0, 0.0, 300.0, 800.0], 72.0).unwrap();
    assert!(detected.is_none());
}

#[test]
fn test_detect_vector_grid_in_region_filters_to_requested_table() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};

    let buf = synthetic_vector_grid_pdf(true);
    let second_table_crop = [50.0_f32, 240.0, 210.0, 310.0];
    let detected = detect_vector_grid_in_region_mem(&buf, 0, second_table_crop, 72.0)
        .unwrap()
        .expect("second ruled table should be detected");

    assert_eq!(detected.cell_bboxes.len(), 4);
    let markdown = extract_tables_with_structure_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: second_table_crop,
            render_dpi: 72.0,
            structure_tokens: detected.structure_tokens,
            cell_bboxes: detected.cell_bboxes,
        }],
    )
    .unwrap()
    .remove(0);

    assert!(markdown.contains("C1"));
    assert!(markdown.contains("D2"));
    assert!(!markdown.contains("A1"));
    assert!(!markdown.contains("B2"));
}

#[test]
fn test_extract_tables_with_structure_real_pdf_bits_pilani() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};
    // Hand-crafted TSR fixture targeting page 4 (0-indexed=3) of
    // bits_pilani_feedback.pdf, which contains a clean tabular layout.
    //
    // We construct a 2×2 table:
    //   row 0 (header): "Department"   "Core Courses"
    //   row 1 (data):   "BIO"          "8.23"
    //
    // The PDF page is US Letter (792pt tall). We render at 72 dpi so
    // image-px maps 1:1 to PDF-pt — that lets us write cell bboxes in
    // the same units as our hand-measured page-pt coordinates.
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();

    // The PDF page is A4 in points (≈595.44 × 841.68). The table sits in
    // the upper part of the page; we crop a window large enough to enclose
    // both rows we care about.
    //
    // Crop bounds in PDF points (top-left origin):
    //   x: 80..280, y: 170..240
    let crop = [80.0_f32, 170.0, 280.0, 240.0];
    let dpi = 72.0_f32;

    // Cell bboxes in CROP image-pixel space (= crop-relative PDF-pt at
    // 72 dpi). The y ranges are tightened against neighbouring rows
    // ("First Degree" above the header at native y=666.7, "Feedback Score"
    // between the header and data rows at native y=640.9, "CE" below the
    // BIO row at native y=591.1) so each cell only overlaps its target
    // text item.
    let cell_bboxes = vec![
        // Header row: y crop-relative (7, 18) → page-pt y (177, 188)
        poly(10.0, 7.0, 100.0, 18.0), // "Department"   (item at page-pt x=107.1)
        poly(110.0, 7.0, 200.0, 18.0), // "Core Courses" (item at page-pt x=199.0)
        // Data row: y crop-relative (35, 60) → page-pt y (205, 230)
        poly(10.0, 35.0, 100.0, 60.0), // "BIO"  (item at page-pt x=104.1)
        poly(110.0, 35.0, 200.0, 60.0), // "8.23" (item at page-pt x=221.2)
    ];

    // Minimal SLANet-style token stream: a 2-row table with a thead and tbody.
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let inputs = vec![TsrTableInput {
        page: 3,
        crop_pdf_pt_bbox: crop,
        render_dpi: dpi,
        structure_tokens: tokens,
        cell_bboxes,
    }];

    let mds = extract_tables_with_structure_mem(&buf, &inputs).unwrap();
    assert_eq!(mds.len(), 1);
    let md = &mds[0];

    // Hand-written gold standard for the rendered markdown.
    let expected = "|Department|Core Courses|\n|---|---|\n|BIO|8.23|\n";
    assert_eq!(
        md, expected,
        "structured-table markdown should match the gold standard exactly\nactual: {md}"
    );
}

#[test]
fn test_extract_tables_with_structure_dense_overlapping_slanet_boxes() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    // Rows are spaced 16.8pt apart, while the SLANet-style boxes are 40pt
    // tall and overlap adjacent rows. Text must still land in only one row.
    let cell_bboxes = vec![
        poly(10.0, 72.0, 100.0, 112.0),
        poly(90.0, 72.0, 180.0, 112.0),
        poly(10.0, 88.8, 100.0, 128.8),
        poly(90.0, 88.8, 180.0, 128.8),
        poly(10.0, 105.6, 100.0, 145.6),
        poly(90.0, 105.6, 180.0, 145.6),
    ];

    let mds = extract_tables_with_structure_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();

    let expected = "|Branch Name|Deposits|\n|---|---|\n|Oak Street|100|\n|Boardwalk|200|\n";
    assert_eq!(mds[0], expected);
    assert!(!mds[0].contains("Branch Name Oak Street"));
    assert!(!mds[0].contains("Oak Street Boardwalk"));
}

#[test]
fn test_extract_tables_with_structure_input_order_preserved() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();

    // Two inputs; both target the same page but with different shapes.
    // We just need to confirm we get 2 outputs in the same order.
    let make_input = |toks: Vec<&str>, cells: Vec<Vec<f32>>| TsrTableInput {
        page: 3,
        crop_pdf_pt_bbox: [80.0, 170.0, 280.0, 240.0],
        render_dpi: 72.0,
        structure_tokens: toks.into_iter().map(String::from).collect(),
        cell_bboxes: cells,
    };

    let inputs = vec![
        make_input(
            vec!["<table>", "<tr>", "<td></td>", "</tr>", "</table>"],
            vec![poly(10.0, 35.0, 100.0, 60.0)],
        ),
        make_input(
            vec!["<table>", "<tr>", "<td></td>", "</tr>", "</table>"],
            vec![poly(110.0, 35.0, 200.0, 60.0)],
        ),
    ];

    let mds = extract_tables_with_structure_mem(&buf, &inputs).unwrap();
    assert_eq!(mds.len(), 2);
    assert!(
        mds[0].contains("BIO"),
        "input 0 should pull 'BIO': {}",
        mds[0]
    );
    assert!(
        mds[1].contains("8.23"),
        "input 1 should pull '8.23': {}",
        mds[1]
    );
}

#[test]
fn test_extract_tables_with_structure_out_of_range_page() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();

    let inputs = vec![TsrTableInput {
        page: 9999,
        crop_pdf_pt_bbox: [0.0, 0.0, 100.0, 100.0],
        render_dpi: 72.0,
        structure_tokens: vec![
            "<table>".into(),
            "<tr>".into(),
            "<td></td>".into(),
            "</tr>".into(),
            "</table>".into(),
        ],
        cell_bboxes: vec![poly(0.0, 0.0, 50.0, 50.0)],
    }];

    let mds = extract_tables_with_structure_mem(&buf, &inputs).unwrap();
    assert_eq!(mds.len(), 1);
    assert!(
        mds[0].is_empty(),
        "out-of-range page should yield empty string"
    );
}

#[test]
fn test_extract_tables_with_structure_not_a_pdf() {
    use pdf_inspector::extract_tables_with_structure_mem;
    let result = extract_tables_with_structure_mem(b"not a pdf", &[]);
    assert!(result.is_err());
}

#[test]
fn test_extract_tables_with_structure_empty_inputs() {
    use pdf_inspector::extract_tables_with_structure_mem;
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();
    let mds = extract_tables_with_structure_mem(&buf, &[]).unwrap();
    assert!(mds.is_empty());
}

#[test]
fn test_extract_tables_with_structure_cells_real_pdf_bits_pilani() {
    use pdf_inspector::{extract_tables_with_structure_cells_mem, TsrTableInput};
    // Same fixture as test_extract_tables_with_structure_real_pdf_bits_pilani
    // but exercising the cell-level API. Verifies that callers receive
    // structured per-cell metadata (row/col/spans/is_header/page_pt_bbox)
    // alongside the extracted text.
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();

    let crop = [80.0_f32, 170.0, 280.0, 240.0];
    let dpi = 72.0_f32;
    let cell_bboxes = vec![
        poly(10.0, 7.0, 100.0, 18.0),
        poly(110.0, 7.0, 200.0, 18.0),
        poly(10.0, 35.0, 100.0, 60.0),
        poly(110.0, 35.0, 200.0, 60.0),
    ];
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let inputs = vec![TsrTableInput {
        page: 3,
        crop_pdf_pt_bbox: crop,
        render_dpi: dpi,
        structure_tokens: tokens,
        cell_bboxes,
    }];

    let cells_lists = extract_tables_with_structure_cells_mem(&buf, &inputs).unwrap();
    assert_eq!(cells_lists.len(), 1);
    let cells = &cells_lists[0];
    assert_eq!(cells.len(), 4);

    // Header row: both cells flagged as headers (they were in <thead>/<th>).
    assert!(cells[0].is_header);
    assert!(cells[1].is_header);
    assert_eq!((cells[0].row, cells[0].col), (0, 0));
    assert_eq!((cells[1].row, cells[1].col), (0, 1));
    assert_eq!(cells[0].text, "Department");
    assert_eq!(cells[1].text, "Core Courses");

    // Data row: not flagged as header.
    assert!(!cells[2].is_header);
    assert!(!cells[3].is_header);
    assert_eq!((cells[2].row, cells[2].col), (1, 0));
    assert_eq!((cells[3].row, cells[3].col), (1, 1));
    assert_eq!(cells[2].text, "BIO");
    assert_eq!(cells[3].text, "8.23");

    // Every cell carries a non-degenerate page-pt bbox.
    for c in cells {
        let [x1, y1, x2, y2] = c.page_pt_bbox;
        assert!(
            x1 < x2 && y1 < y2,
            "cell bbox should be non-empty: {:?}",
            c.page_pt_bbox
        );
    }
}

#[test]
fn test_extract_tables_with_structure_separator_after_thead() {
    use pdf_inspector::{extract_tables_with_structure_mem, TsrTableInput};
    // Re-run the same 2x2 fixture but assert exact markdown output: with
    // <thead> + <th> headers, the separator should land after the header
    // row (which is also row 0 here, so the gold-standard hasn't changed).
    let buf = std::fs::read("tests/fixtures/bits_pilani_feedback.pdf").unwrap();

    let crop = [80.0_f32, 170.0, 280.0, 240.0];
    let dpi = 72.0_f32;
    let cell_bboxes = vec![
        poly(10.0, 7.0, 100.0, 18.0),
        poly(110.0, 7.0, 200.0, 18.0),
        poly(10.0, 35.0, 100.0, 60.0),
        poly(110.0, 35.0, 200.0, 60.0),
    ];
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let mds = extract_tables_with_structure_mem(
        &buf,
        &[TsrTableInput {
            page: 3,
            crop_pdf_pt_bbox: crop,
            render_dpi: dpi,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();
    assert_eq!(mds.len(), 1);
    assert_eq!(mds[0], "|Department|Core Courses|\n|---|---|\n|BIO|8.23|\n");
}

// =========================================================================
// extract_tables_with_structure_auto_mem tests (TSR + heuristic fallback)
// =========================================================================

#[test]
fn test_auto_passes_through_clean_tsr_output() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // Cells fit each visible row cleanly. Same shape as the existing
    // dense-overlap regression test — TSR should produce clean output
    // and the auto wrapper should pass through with no fallback.
    let cell_bboxes = vec![
        poly(10.0, 72.0, 100.0, 112.0),
        poly(90.0, 72.0, 180.0, 112.0),
        poly(10.0, 88.8, 100.0, 128.8),
        poly(90.0, 88.8, 180.0, 128.8),
        poly(10.0, 105.6, 100.0, 145.6),
        poly(90.0, 105.6, 180.0, 145.6),
    ];

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].fallback_reason.is_none(),
        "expected no fallback, got {:?}",
        results[0].fallback_reason
    );
    assert!(results[0].markdown.contains("Oak Street"));
    assert!(results[0].markdown.contains("Boardwalk"));
    assert!(!results[0].markdown.contains("Oak Street Boardwalk"));
}

#[test]
fn test_auto_expands_multi_row_in_cell() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    // TSR returns only 2 rows for what's actually 3 visible PDF rows.
    // Row 1's cells are tall enough to encompass both Oak Street and
    // Boardwalk text — the FNBO row-undercount pattern.
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // Header row at top-left y=[88, 105] (covers "Branch Name"/"Deposits"
    // at native y=700, top-left y≈92-103). The "data" row at top-left
    // y=[105, 145] is intentionally tall — covers BOTH the Oak Street
    // line (top-left y≈108-119) AND the Boardwalk line (y≈124-135).
    let cell_bboxes = vec![
        poly(10.0, 88.0, 100.0, 105.0),
        poly(90.0, 88.0, 180.0, 105.0),
        poly(10.0, 105.0, 100.0, 145.0),
        poly(90.0, 105.0, 180.0, 145.0),
    ];

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].fallback_reason.as_deref(),
        Some("multi_row_in_cell_expanded"),
        "expected multi_row_in_cell_expanded, got {:?}",
        results[0].fallback_reason
    );
    // The in-place expansion should preserve all three PDF rows.
    let md = &results[0].markdown;
    assert!(md.contains("Oak Street"), "missing Oak Street: {md}");
    assert!(md.contains("Boardwalk"), "missing Boardwalk: {md}");
    assert!(md.contains("100"), "missing 100: {md}");
    assert!(md.contains("200"), "missing 200: {md}");
    assert!(
        !md.contains("Oak Street Boardwalk"),
        "rows should not remain compressed: {md}"
    );
}

#[test]
fn test_auto_expands_under_counted_vector_grid_rows() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_vector_grid_three_row_pdf();
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let crop = [50.0, 60.0, 210.0, 150.0];
    let cell_bboxes = vec![
        poly(0.0, 0.0, 80.0, 30.0),
        poly(80.0, 0.0, 160.0, 30.0),
        poly(0.0, 30.0, 80.0, 90.0),
        poly(80.0, 30.0, 160.0, 90.0),
    ];

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: crop,
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].fallback_reason.as_deref(),
        Some("multi_row_in_cell_expanded")
    );
    let md = &results[0].markdown;
    assert!(md.contains("|Branch|Deposits|"), "missing header: {md}");
    assert!(md.contains("|Oak|100|"), "missing row 1: {md}");
    assert!(md.contains("|Boardwalk|200|"), "missing row 2: {md}");
    assert!(
        !md.contains("Oak Boardwalk"),
        "rows stayed compressed: {md}"
    );
}

#[test]
fn test_auto_keeps_wrapped_header_vector_grid_doc51() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = std::fs::read("tests/fixtures/government_positions_women.pdf").unwrap();
    let crop = [0.0, 0.0, 612.0, 792.0];
    let grid = detect_vector_grid_in_region_mem(&buf, 0, crop, 200.0)
        .unwrap()
        .expect("expected doc 51 vector grid");
    assert_eq!(
        grid.cell_bboxes.len(),
        36,
        "doc 51 should have a 9x4 vector grid"
    );

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: crop,
            render_dpi: 200.0,
            structure_tokens: grid.structure_tokens,
            cell_bboxes: grid.cell_bboxes,
        }],
    )
    .unwrap();

    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert!(
        r.fallback_reason.is_none(),
        "wrapped header/label text should not trigger heuristic fallback: {:?}\n{}",
        r.fallback_reason,
        r.markdown
    );
    let md = &r.markdown;
    assert!(md.contains("Government Position"), "missing header: {md}");
    assert!(
        md.contains("Aquino Administration"),
        "missing Aquino header: {md}"
    );
    assert!(
        md.contains("Ramos Administration"),
        "missing Ramos header: {md}"
    );
    assert!(
        md.contains("City Municipal Councilor"),
        "row label was truncated: {md}"
    );
    assert!(
        !md.contains("|Position||Administration"),
        "heuristic fallback split the header row: {md}"
    );
}

#[test]
fn test_auto_returns_empty_inputs() {
    use pdf_inspector::extract_tables_with_structure_auto_mem;
    let buf = synthetic_dense_table_pdf();
    let results = extract_tables_with_structure_auto_mem(&buf, &[]).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_auto_does_not_fire_on_legit_rowspan_cell() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    // 2 columns, 3 rows in the visible PDF. SLANet emits a 2-row table
    // where the LEFT cell of row 1 is a rowspan=2 cell that legitimately
    // covers Oak Street + Boardwalk on two visual lines. The right
    // column has two normal rows. multi_row_in_cell must NOT fire on
    // the rowspan=2 cell.
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        // First data cell explicitly declares rowspan=2.
        "<td",
        " rowspan=\"2\"",
        ">",
        "</td>",
        "<td></td>",
        "</tr>",
        "<tr>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // Header row, then a tall left cell covering both data lines, plus
    // two narrow right cells (one per line).
    let cell_bboxes = vec![
        poly(10.0, 88.0, 100.0, 105.0),
        poly(90.0, 88.0, 180.0, 105.0),
        poly(10.0, 105.0, 100.0, 145.0), // rowspan=2 — covers both lines
        poly(90.0, 105.0, 180.0, 122.0), // row 1 only
        poly(90.0, 122.0, 180.0, 145.0), // row 2 only
    ];

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].fallback_reason.is_none(),
        "rowspan=2 cell containing 2 visual lines should not trip multi_row_in_cell, got reason={:?}",
        results[0].fallback_reason,
    );
}

#[test]
fn test_auto_expands_when_heuristic_region_is_empty() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    // Same shape as the multi_row_in_cell regression — a tall data cell
    // that catches Oak Street + Boardwalk. The crop bbox we pass points
    // at a strip of the page that has NO text items, so the old heuristic
    // fallback would be empty. Expansion uses the cell bboxes directly.
    let tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // Cell bboxes overlap the actual PDF text (so multi_row_in_cell
    // fires) — but the crop_pdf_pt_bbox we hand to the heuristic is a
    // wholly-empty region of the page. The heuristic should return "".
    let cell_bboxes = vec![
        poly(10.0, 88.0, 100.0, 105.0),
        poly(90.0, 88.0, 180.0, 105.0),
        poly(10.0, 105.0, 100.0, 145.0),
        poly(90.0, 105.0, 180.0, 145.0),
    ];

    let results = extract_tables_with_structure_auto_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            // Crop is at the BOTTOM of the page where there's no text.
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 50.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert_eq!(
        r.fallback_reason.as_deref(),
        Some("multi_row_in_cell_expanded"),
        "expected expansion despite empty heuristic region, got {:?}",
        r.fallback_reason,
    );
    assert!(
        r.markdown.contains("|Oak Street|100|"),
        "missing row 1: {}",
        r.markdown
    );
    assert!(
        r.markdown.contains("|Boardwalk|200|"),
        "missing row 2: {}",
        r.markdown
    );
}

#[test]
fn test_auto_isolates_per_input_failures() {
    use pdf_inspector::{extract_tables_with_structure_auto_mem, TsrTableInput};

    let buf = synthetic_dense_table_pdf();
    let good_tokens: Vec<String> = [
        "<table>",
        "<thead>",
        "<tr>",
        "<th></th>",
        "<th></th>",
        "</tr>",
        "</thead>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // A clean input that should pass through with no fallback.
    let good_input = TsrTableInput {
        page: 0,
        crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
        render_dpi: 72.0,
        structure_tokens: good_tokens,
        cell_bboxes: vec![
            poly(10.0, 72.0, 100.0, 112.0),
            poly(90.0, 72.0, 180.0, 112.0),
            poly(10.0, 88.8, 100.0, 128.8),
            poly(90.0, 88.8, 180.0, 128.8),
            poly(10.0, 105.6, 100.0, 145.6),
            poly(90.0, 105.6, 180.0, 145.6),
        ],
    };
    // A bad input that targets a non-existent page. The detection
    // helper short-circuits on missing pages with Ok(None), so this
    // shouldn't itself crash, but pairing it with a flagged input
    // exercises the per-input control flow regardless. The point of
    // this test is that one input's outcome doesn't poison the other.
    let bad_input = TsrTableInput {
        page: 9999,
        crop_pdf_pt_bbox: [0.0, 0.0, 100.0, 100.0],
        render_dpi: 72.0,
        structure_tokens: vec![
            "<table>".into(),
            "<tr>".into(),
            "<td></td>".into(),
            "</tr>".into(),
            "</table>".into(),
        ],
        cell_bboxes: vec![poly(0.0, 0.0, 50.0, 50.0)],
    };

    let results = extract_tables_with_structure_auto_mem(&buf, &[good_input, bad_input]).unwrap();
    assert_eq!(results.len(), 2);
    // Good input still produces non-empty TSR markdown with no fallback.
    assert!(
        results[0].fallback_reason.is_none(),
        "good input should pass through, got reason={:?}",
        results[0].fallback_reason,
    );
    assert!(results[0].markdown.contains("Oak Street"));
    // Bad input collapses to empty markdown but doesn't take the
    // batch down with it.
    assert_eq!(results[1].markdown, "");
}

// =========================================================================
// extract_pages_markdown_mem tests
// =========================================================================

#[test]
fn test_extract_pages_markdown_basic() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[0, 1])).unwrap();

    assert_eq!(result.pages.len(), 2);
    assert_eq!(result.pages[0].page, 0);
    assert_eq!(result.pages[1].page, 1);
    // Text-based PDF should produce non-empty markdown
    assert!(!result.pages[0].markdown.is_empty());
    assert!(!result.pages[0].needs_ocr);
}

#[test]
fn test_extract_pages_markdown_page_ordering() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    // Request pages in non-sequential order
    let result = extract_pages_markdown_mem(&buf, Some(&[1, 0])).unwrap();

    assert_eq!(result.pages.len(), 2);
    // Results should match input order, not document order
    assert_eq!(result.pages[0].page, 1);
    assert_eq!(result.pages[1].page, 0);
}

#[test]
fn test_extract_pages_markdown_out_of_range() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[9999])).unwrap();

    assert_eq!(result.pages.len(), 1);
    assert_eq!(result.pages[0].page, 9999);
    assert!(result.pages[0].markdown.is_empty());
    assert!(result.pages[0].needs_ocr);
    assert!(result.pages_needing_ocr.contains(&10000)); // 1-indexed
}

#[test]
fn test_extract_pages_markdown_empty_pages_list() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[])).unwrap();
    assert!(result.pages.is_empty());
}

#[test]
fn test_extract_pages_markdown_single_page() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[0])).unwrap();

    assert_eq!(result.pages.len(), 1);
    assert_eq!(result.pages[0].page, 0);
    assert!(!result.pages[0].markdown.is_empty());
    assert!(!result.pages[0].needs_ocr);
}

#[test]
fn test_extract_pages_markdown_invalid_buffer() {
    let result = extract_pages_markdown_mem(b"not a pdf", Some(&[0]));
    assert!(result.is_err());
}

#[test]
fn test_extract_pages_markdown_gid_pages_need_ocr() {
    // shinagawa_identity_h.pdf has GID-encoded fonts
    let buf = std::fs::read("tests/fixtures/shinagawa_identity_h.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[0])).unwrap();

    assert_eq!(result.pages.len(), 1);
    assert!(result.pages[0].needs_ocr);
    assert!(result.pages_needing_ocr.contains(&1)); // 1-indexed
}

#[test]
fn test_extract_pages_markdown_classification_with_tables() {
    // nexo-price-en.pdf is known to have tables
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let page_count = process_pdf_mem(&buf).unwrap().page_count;
    let page_indices: Vec<u32> = (0..page_count).collect();
    let result = extract_pages_markdown_mem(&buf, Some(&page_indices)).unwrap();

    assert!(
        !result.pages_with_tables.is_empty(),
        "nexo-price-en.pdf should have pages with tables"
    );
    assert!(result.is_complex);
}

#[test]
fn test_extract_pages_markdown_simple_pdf_no_complexity() {
    // bare_name_struct.pdf is a simple document with a heading and code block
    let buf = std::fs::read("tests/fixtures/bare_name_struct.pdf").unwrap();
    let result = extract_pages_markdown_mem(&buf, Some(&[0])).unwrap();

    assert!(result.pages_with_tables.is_empty());
    assert!(result.pages_with_columns.is_empty());
    assert!(!result.is_complex);
}

#[test]
fn test_extract_pages_markdown_classification_matches_process_pdf() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let full = process_pdf_mem(&buf).unwrap();
    let page_count = full.page_count;
    let page_indices: Vec<u32> = (0..page_count).collect();
    let result = extract_pages_markdown_mem(&buf, Some(&page_indices)).unwrap();

    assert_eq!(
        result.pages_with_tables, full.layout.pages_with_tables,
        "pages_with_tables should match process_pdf"
    );
    assert_eq!(
        result.pages_with_columns, full.layout.pages_with_columns,
        "pages_with_columns should match process_pdf"
    );
}

#[test]
fn test_extract_pages_markdown_consistency_with_process_pdf() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();

    // Get full process_pdf output
    let full = process_pdf_mem(&buf).unwrap();
    let full_md = full.markdown.unwrap_or_default();

    // Get per-page output for all pages
    let page_count = full.page_count;
    let page_indices: Vec<u32> = (0..page_count).collect();
    let result = extract_pages_markdown_mem(&buf, Some(&page_indices)).unwrap();

    // Concatenated per-page markdown should contain substantial overlap with
    // the full output (exact match not expected due to header/footer stripping
    // and cross-page paragraph merging differences)
    let concat: String = result
        .pages
        .iter()
        .map(|p| p.markdown.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    // Both should be non-empty for a text-based PDF
    assert!(!full_md.is_empty());
    assert!(!concat.is_empty());

    // The per-page version should contain at least 50% of the full content's
    // length (accounting for header/footer stripping differences)
    assert!(
        concat.len() * 2 >= full_md.len(),
        "per-page concat ({} chars) is too short vs full ({} chars)",
        concat.len(),
        full_md.len()
    );
}

#[test]
fn test_extract_pages_markdown_none_returns_all_pages() {
    let buf = std::fs::read("tests/fixtures/nexo-price-en.pdf").unwrap();
    let page_count = process_pdf_mem(&buf).unwrap().page_count;

    let result = extract_pages_markdown_mem(&buf, None).unwrap();

    assert_eq!(result.pages.len() as u32, page_count);
    for (i, page) in result.pages.iter().enumerate() {
        assert_eq!(page.page, i as u32, "pages should be in document order");
    }
}

#[test]
fn test_extract_pages_markdown_path_api() {
    let path = "tests/fixtures/nexo-price-en.pdf";
    let buf = std::fs::read(path).unwrap();

    let via_path = extract_pages_markdown(path, Some(&[0])).unwrap();
    let via_mem = extract_pages_markdown_mem(&buf, Some(&[0])).unwrap();

    assert_eq!(via_path.pages.len(), via_mem.pages.len());
    assert_eq!(via_path.pages[0].markdown, via_mem.pages[0].markdown);
    assert_eq!(via_path.pages[0].needs_ocr, via_mem.pages[0].needs_ocr);
    assert_eq!(via_path.is_complex, via_mem.is_complex);
}

#[test]
fn test_extract_pages_markdown_path_none_returns_all_pages() {
    let path = "tests/fixtures/nexo-price-en.pdf";
    let page_count = process_pdf_mem(&std::fs::read(path).unwrap())
        .unwrap()
        .page_count;

    let result = extract_pages_markdown(path, None).unwrap();
    assert_eq!(result.pages.len() as u32, page_count);
}

// ============================================================================
// PROBE: investigate dense-cell text-assignment failure mode (failure mode 2)
// ============================================================================

fn synthetic_wide_row_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let content_id = doc.new_object_id();

    doc.objects.insert(
        font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }
        .into(),
    );

    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 10.into()]),
        Operation::new("Td", vec![20.into(), 700.into()]),
        // A single Tj that visually spans multiple cells. This mirrors PDFs
        // where a row's address/role/email columns are emitted as one literal
        // string with embedded spaces, producing one wide TextItem.
        Operation::new(
            "Tj",
            vec![Object::string_literal("Name JobTitle Email Phone")],
        ),
        Operation::new("ET", vec![]),
    ];
    let content = Content { operations }.encode().unwrap();
    doc.objects
        .insert(content_id, Stream::new(dictionary! {}, content).into());

    doc.objects.insert(
        page_id,
        dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 800.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => font_id,
                },
            },
            "Contents" => content_id,
        }
        .into(),
    );
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn test_extract_tables_with_structure_distributes_wide_item_across_cells() {
    use pdf_inspector::{extract_tables_with_structure_cells_mem, TsrTableInput};

    // Reproduces failure mode 2: a row of multi-token text rendered as one
    // Tj produces a single wide TextItem that visually spans multiple cells.
    // The current first-match-by-center routing parks the entire item in
    // whichever cell holds the item's center, leaving the other cells empty.
    // See production samples in scrape_id 019de788-ff41-... where 10-column
    // grids ended up with row text packed into one cell.
    let buf = synthetic_wide_row_pdf();

    // Helvetica 10pt with width=0 falls back to char_count*font_size*0.5.
    // "Name JobTitle Email Phone" is 25 chars → effective_width 125pt,
    // text starts at PDF (20, 700), top-down y=[90, 100], char_w≈5pt.
    // Tokens land at:
    //   "Name"     chars 0-3   center≈x=30
    //   "JobTitle" chars 5-12  center≈x=65
    //   "Email"    chars 14-18 center≈x=100
    //   "Phone"    chars 20-24 center≈x=130
    let cell_bboxes = vec![
        poly(15.0, 88.0, 50.0, 102.0),
        poly(50.0, 88.0, 85.0, 102.0),
        poly(85.0, 88.0, 120.0, 102.0),
        poly(120.0, 88.0, 155.0, 102.0),
    ];

    let tokens: Vec<String> = [
        "<table>",
        "<tbody>",
        "<tr>",
        "<td></td>",
        "<td></td>",
        "<td></td>",
        "<td></td>",
        "</tr>",
        "</tbody>",
        "</table>",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let cells_lists = extract_tables_with_structure_cells_mem(
        &buf,
        &[TsrTableInput {
            page: 0,
            crop_pdf_pt_bbox: [0.0, 0.0, 200.0, 800.0],
            render_dpi: 72.0,
            structure_tokens: tokens,
            cell_bboxes,
        }],
    )
    .unwrap();

    let cells = &cells_lists[0];
    assert_eq!(cells.len(), 4);
    assert_eq!(
        cells[0].text, "Name",
        "cell 0 should hold 'Name', got {:?}",
        cells[0].text
    );
    assert_eq!(
        cells[1].text, "JobTitle",
        "cell 1 should hold 'JobTitle', got {:?}",
        cells[1].text
    );
    assert_eq!(
        cells[2].text, "Email",
        "cell 2 should hold 'Email', got {:?}",
        cells[2].text
    );
    assert_eq!(
        cells[3].text, "Phone",
        "cell 3 should hold 'Phone', got {:?}",
        cells[3].text
    );
}

// ============================================================================
// PROPER TEST: synthetic Type0/Identity-H PDF with malformed ToUnicode CMap
// ============================================================================
//
// Complements the existing real-PDF fixture `shinagawa_identity_h.pdf` by
// building a minimal Type0 / Identity-H font in process. We control:
//   * the byte stream emitted by Tj (a 2-byte CID containing one high byte),
//   * the malformed ToUnicode contents (junk bytes that won't parse), and
//   * the DescendantFonts shape (just enough for `parse_type0_widths` to set
//     `is_cid=true`, which is what the new guard in `extract_text_from_operand`
//     keys off of).
// No fixture file or external license to worry about.

fn synthetic_type0_broken_tounicode_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let cid_font_id = doc.new_object_id();
    let descriptor_id = doc.new_object_id();
    let tounicode_id = doc.new_object_id();
    let cid_system_info_id = doc.new_object_id();
    let content_id = doc.new_object_id();

    // Type0 font with Identity-H encoding and a broken ToUnicode reference.
    doc.objects.insert(
        font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "AAAAAA+SyntheticCID",
            "Encoding" => "Identity-H",
            "DescendantFonts" => vec![cid_font_id.into()],
            "ToUnicode" => tounicode_id,
        }
        .into(),
    );

    // CIDSystemInfo and a minimal CIDFontType2 descendant. parse_type0_widths
    // walks DescendantFonts → returns FontWidthInfo with is_cid=true. That's
    // the only thing the new Latin-1 guard needs to see.
    doc.objects.insert(
        cid_system_info_id,
        dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        }
        .into(),
    );
    doc.objects.insert(
        cid_font_id,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "BaseFont" => "AAAAAA+SyntheticCID",
            "CIDSystemInfo" => cid_system_info_id,
            "FontDescriptor" => descriptor_id,
            "DW" => 1000,
        }
        .into(),
    );
    doc.objects.insert(
        descriptor_id,
        dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "AAAAAA+SyntheticCID",
            "Flags" => 4,
            "FontBBox" => vec![Object::Integer(-100), Object::Integer(-100), 1000.into(), 1000.into()],
            "ItalicAngle" => 0,
            "Ascent" => 800,
            "Descent" => Object::Integer(-200),
            "CapHeight" => 700,
            "StemV" => 80,
        }
        .into(),
    );

    // Intentionally malformed ToUnicode stream — just junk bytes. ToUnicode
    // CMap parsing will fail, so `font_cmaps.get_by_obj` returns None and
    // `has_cmap` stays false. The reference still exists in the font dict,
    // so `font_tounicode_refs` contains the entry — but the new guard now
    // routes off `is_cid` from font_widths instead, which is robust to a
    // failed CMap parse.
    doc.objects.insert(
        tounicode_id,
        Stream::new(dictionary! {}, b"this is not a valid CMap stream".to_vec()).into(),
    );

    // Tj with a 2-byte CID stream containing a non-ASCII high byte.
    // Pre-fix this would have decoded as Latin-1 to "\u{00CD}\u{00D9}" ("ÍÙ").
    // Post-fix it should produce U+FFFD per CID.
    let cid_bytes = vec![0xCD_u8, 0xD9, 0xCD, 0xD9];
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F0".into(), 12.into()]),
        Operation::new("Td", vec![50.into(), 100.into()]),
        Operation::new(
            "Tj",
            vec![Object::String(cid_bytes, lopdf::StringFormat::Hexadecimal)],
        ),
        Operation::new("ET", vec![]),
    ];
    let content = Content { operations }.encode().unwrap();
    doc.objects
        .insert(content_id, Stream::new(dictionary! {}, content).into());

    doc.objects.insert(
        page_id,
        dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F0" => font_id,
                },
            },
            "Contents" => content_id,
        }
        .into(),
    );
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn test_synthetic_type0_broken_tounicode_emits_fffd_not_latin1_mojibake() {
    let buf = synthetic_type0_broken_tounicode_pdf();

    let items = pdf_inspector::extractor::extract_text_with_positions_mem(&buf).unwrap();
    let combined: String = items.iter().map(|i| i.text.as_str()).collect();

    // Mojibake leak check: 2-byte CID 0xCDD9 must NOT come out as "ÍÙ"
    // (U+00CD U+00D9). That was the production scrape symptom.
    assert!(
        !combined.contains('\u{00CD}'),
        "Latin-1 mojibake leaked from Type0 font: {combined:?}"
    );
    assert!(
        !combined.contains('\u{00D9}'),
        "Latin-1 mojibake leaked from Type0 font: {combined:?}"
    );

    // Marker presence: Type0/CID + non-ASCII bytes must produce U+FFFD so
    // `detect_encoding_issues` can flag the page for OCR downstream.
    assert!(
        combined.contains('\u{FFFD}'),
        "Type0 font with malformed ToUnicode CMap should emit U+FFFD per CID; got: {combined:?}"
    );

    // End-to-end check: the page is correctly routed to OCR.
    let result = pdf_inspector::process_pdf_mem(&buf).unwrap();
    assert!(
        result.pages_needing_ocr.contains(&1),
        "Type0 page with broken ToUnicode + non-ASCII bytes must be flagged for OCR; \
         pages_needing_ocr={:?}",
        result.pages_needing_ocr
    );
}

// ============================================================================
// Image XObject emission
// ============================================================================

/// Build a minimal PDF containing one Image XObject placed at a known CTM.
/// `image_ctm` is the 6-element matrix applied to the unit square by the
/// `Do` operator (per PDF spec section 8.9.5 "Image Coordinate System").
/// For an axis-aligned image at `(x, y)` with size `w × h`, that's
/// `[w, 0, 0, h, x, y]`.
fn make_pdf_with_image(image_ctm: [f32; 6]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];

    fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &str) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body.as_bytes());
        pdf.extend_from_slice(b"\nendobj\n");
    }
    fn add_stream_object(
        pdf: &mut Vec<u8>,
        offsets: &mut Vec<usize>,
        id: usize,
        dict: &str,
        stream_bytes: &[u8],
    ) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(
            format!("<< {} /Length {} >>\nstream\n", dict, stream_bytes.len()).as_bytes(),
        );
        pdf.extend_from_slice(stream_bytes);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    // 1: catalog → 2: pages → 3: page with XObject /Im0 → 4: content stream
    // 5: font → 6: image XObject (1×1 grayscale)
    add_object(
        &mut pdf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >> \
         /Contents 4 0 R >>",
    );
    let [a, b, c, d, e, f] = image_ctm;
    // BT/ET around a small text item just so the page isn't classified as
    // image-only (which would route to a different code path). Then save
    // graphics state, apply the image CTM, invoke Im0, restore.
    let content = format!(
        "BT /F1 12 Tf 100 700 Td (Hi) Tj ET\nq {} {} {} {} {} {} cm /Im0 Do Q",
        a, b, c, d, e, f
    );
    add_stream_object(&mut pdf, &mut offsets, 4, "", content.as_bytes());
    add_object(
        &mut pdf,
        &mut offsets,
        5,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    // 1×1 grayscale image; the single byte is mid-gray. Contents don't
    // matter to the extractor — it only cares about the XObject's
    // /Subtype and the CTM at the `Do` operator.
    let image_pixel = [128u8];
    add_stream_object(
        &mut pdf,
        &mut offsets,
        6,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &image_pixel,
    );

    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len(),
            xref_start
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn test_extract_text_with_positions_emits_image_bboxes() {
    // Place a 200×100 image at (50, 600) in PDF user space (origin
    // bottom-left). The Do operator applies the CTM to a unit square,
    // so for an axis-aligned image, CTM = [w, 0, 0, h, x, y].
    let pdf = make_pdf_with_image([200.0, 0.0, 0.0, 100.0, 50.0, 600.0]);
    let items = extract_text_with_positions_mem(&pdf).expect("extract");

    let images: Vec<&TextItem> = items
        .iter()
        .filter(|i| matches!(i.item_type, ItemType::Image))
        .collect();
    assert_eq!(
        images.len(),
        1,
        "expected exactly one Image item, got items: {:?}",
        items
            .iter()
            .map(|i| (&i.text, &i.item_type))
            .collect::<Vec<_>>()
    );
    let img = images[0];
    assert!((img.x - 50.0).abs() < 0.01, "x={}", img.x);
    assert!((img.y - 600.0).abs() < 0.01, "y={}", img.y);
    assert!((img.width - 200.0).abs() < 0.01, "width={}", img.width);
    assert!((img.height - 100.0).abs() < 0.01, "height={}", img.height);
    assert_eq!(img.page, 1);
    // text field carries the legacy `[Image: <resource-name>]` form that
    // the markdown emitter already knows how to parse.
    assert_eq!(img.text, "[Image: Im0]");
}

#[test]
fn test_image_xobject_bbox_handles_rotated_ctm() {
    // 90° rotation CTM: a unit square at the origin maps to a square
    // rotated counter-clockwise about (0,0), then translated to (200, 300).
    // For a 100×100 image, that's CTM = [0, 100, -100, 0, 200, 300]
    // (apply the rotation: (1,0) → (0,100); (0,1) → (-100,0)).
    // The page-space corners are:
    //   (0,0) → (200, 300)
    //   (1,0) → (200, 400)
    //   (1,1) → (100, 400)
    //   (0,1) → (100, 300)
    // → AABB: x=100..200 (w=100), y=300..400 (h=100).
    let pdf = make_pdf_with_image([0.0, 100.0, -100.0, 0.0, 200.0, 300.0]);
    let items = extract_text_with_positions_mem(&pdf).expect("extract");
    let img = items
        .iter()
        .find(|i| matches!(i.item_type, ItemType::Image))
        .expect("image item");
    assert!((img.x - 100.0).abs() < 0.01, "x={}", img.x);
    assert!((img.y - 300.0).abs() < 0.01, "y={}", img.y);
    assert!((img.width - 100.0).abs() < 0.01, "width={}", img.width);
    assert!((img.height - 100.0).abs() < 0.01, "height={}", img.height);
}

#[test]
fn test_image_emission_does_not_change_default_markdown() {
    // Default `MarkdownOptions::include_images = false` — adding image
    // emission MUST NOT make `extract_pages_markdown` start producing
    // `![Image: …]` placeholders for everyone. Existing callers that
    // upgrade should see no diff in their markdown.
    let pdf = make_pdf_with_image([200.0, 0.0, 0.0, 100.0, 50.0, 600.0]);
    let result = extract_pages_markdown_mem(&pdf, None).expect("extract");
    assert_eq!(result.pages.len(), 1);
    assert!(
        !result.pages[0].markdown.contains("Image:"),
        "default markdown leaked an image placeholder: {:?}",
        result.pages[0].markdown
    );
}

#[test]
fn test_markdown_options_default_has_include_images_false() {
    // Explicit assertion so anyone flipping this back catches it in CI.
    // See `MarkdownOptions::default` in src/markdown/mod.rs for the
    // long-form rationale.
    let opts = MarkdownOptions::default();
    assert!(!opts.include_images);
}

#[test]
fn encrypted_pdf_decrypts_with_correct_password() {
    let path = "tests/fixtures/encrypted-secret123.pdf";

    // No password: the file is encrypted and can't be read.
    let no_pw = process_pdf_with_options(path, PdfOptions::new());
    assert!(
        matches!(no_pw, Err(PdfError::Encrypted)),
        "expected Encrypted without a password, got {no_pw:?}"
    );

    // Wrong password: still rejected.
    let wrong = process_pdf_with_options(path, PdfOptions::new().password("wrong"));
    assert!(
        matches!(wrong, Err(PdfError::Encrypted)),
        "expected Encrypted with a wrong password, got {wrong:?}"
    );

    // Correct password: decrypts and extracts real content.
    let ok = process_pdf_with_options(path, PdfOptions::new().password("secret123"))
        .expect("correct password should decrypt");
    let md = ok.markdown.unwrap_or_default();
    // Assert a stable fixture token so a garbled-but-long extraction (the
    // encrypted-stream regression this guards) still fails the test.
    assert!(
        md.contains("Procurement"),
        "decrypted markdown should contain the fixture's real text, got {} chars",
        md.len()
    );
}

#[test]
fn pdf_options_debug_redacts_password() {
    let opts = PdfOptions::new().password("secret123");
    let dbg = format!("{opts:?}");
    assert!(
        !dbg.contains("secret123"),
        "password leaked in Debug: {dbg}"
    );
    assert!(dbg.contains("REDACTED"), "expected redaction marker: {dbg}");
}
