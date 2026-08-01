//! CLI tool for PDF to Markdown conversion

use pdf_inspector::extractor::ItemType;
use pdf_inspector::{
    extract_text_with_positions_pages, process_pdf_with_options, LayoutComplexity, PdfOptions,
    PdfType, ProcessMode, TextItem,
};
use std::collections::HashSet;
use std::env;
use std::fmt::Write;
use std::fs;
use std::process;

/// Escape a string for embedding in a JSON string value.
///
/// Handles all characters that the JSON spec requires to be escaped:
/// backslash, double-quote, and control characters U+0000..U+001F.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if c < '\x20' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn format_ocr_reasons_by_page(reasons: &[pdf_inspector::PageOcrReasons]) -> String {
    reasons
        .iter()
        .map(|entry| {
            let reasons_json = entry
                .reasons
                .iter()
                .map(|reason| format!(r#""{}""#, json_escape(reason)))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"page":{},"reasons":[{}]}}"#, entry.page, reasons_json)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn item_type_label(item_type: &ItemType) -> &'static str {
    match item_type {
        ItemType::Text => "text",
        ItemType::Image => "image",
        ItemType::Link(_) => "link",
        ItemType::FormField => "form_field",
    }
}

fn format_items_json(items: &[TextItem]) -> String {
    let underlined_count = items.iter().filter(|item| item.is_underline).count();
    let items_json = items
        .iter()
        .map(|item| {
            let mcid = item
                .mcid
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string());
            let link_url = match &item.item_type {
                ItemType::Link(url) => format!(r#","url":"{}""#, json_escape(url)),
                _ => String::new(),
            };
            format!(
                r#"{{"text":"{}","page":{},"x":{:.2},"y":{:.2},"width":{:.2},"height":{:.2},"font":"{}","font_size":{:.2},"is_bold":{},"is_italic":{},"is_underline":{},"is_strikeout":{},"item_type":"{}","mcid":{}{}}}"#,
                json_escape(&item.text),
                item.page,
                item.x,
                item.y,
                item.width,
                item.height,
                json_escape(&item.font),
                item.font_size,
                item.is_bold,
                item.is_italic,
                item.is_underline,
                item.is_strikeout,
                item_type_label(&item.item_type),
                mcid,
                link_url,
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    format!(
        r#"{{"total_items":{},"underlined_count":{},"items":[{}]}}"#,
        items.len(),
        underlined_count,
        items_json
    )
}

#[cfg(test)]
mod tests {
    use super::format_items_json;
    use pdf_inspector::extractor::ItemType;
    use pdf_inspector::TextItem;

    #[test]
    fn items_json_includes_position_and_underline_metadata() {
        let items = vec![TextItem {
            text: "A \"quoted\" item".to_string(),
            x: 12.345,
            y: 67.891,
            width: 23.456,
            height: 9.876,
            font: "F1".to_string(),
            font_size: 10.0,
            page: 2,
            is_bold: false,
            is_italic: true,
            is_underline: true,
            is_strikeout: true,
            item_type: ItemType::Text,
            mcid: Some(7),
        }];

        let json = format_items_json(&items);

        assert!(json.contains(r#""text":"A \"quoted\" item""#));
        assert!(json.contains(r#""page":2"#));
        assert!(json.contains(r#""x":12.35"#));
        assert!(json.contains(r#""is_underline":true"#));
        assert!(json.contains(r#""item_type":"text""#));
        assert!(json.contains(r#""mcid":7"#));
    }
}

/// Parse a page specification like "1,3,5-10,20" into a HashSet of page numbers.
fn parse_page_spec(spec: &str) -> Result<HashSet<u32>, String> {
    let mut pages = HashSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if let Some((start, end)) = part.split_once('-') {
            let start: u32 = start
                .trim()
                .parse()
                .map_err(|_| format!("invalid page number: {}", start.trim()))?;
            let end: u32 = end
                .trim()
                .parse()
                .map_err(|_| format!("invalid page number: {}", end.trim()))?;
            if start == 0 || end == 0 {
                return Err("page numbers are 1-indexed".to_string());
            }
            if start > end {
                return Err(format!("invalid range: {}-{}", start, end));
            }
            for p in start..=end {
                pages.insert(p);
            }
        } else {
            let p: u32 = part
                .parse()
                .map_err(|_| format!("invalid page number: {}", part))?;
            if p == 0 {
                return Err("page numbers are 1-indexed".to_string());
            }
            pages.insert(p);
        }
    }
    Ok(pages)
}

fn print_layout_info(layout: &LayoutComplexity) {
    if layout.is_complex {
        eprintln!("Layout: COMPLEX");
        if !layout.pages_with_tables.is_empty() {
            eprintln!("  Pages with tables: {:?}", layout.pages_with_tables);
        }
        if !layout.pages_with_columns.is_empty() {
            eprintln!("  Pages with columns: {:?}", layout.pages_with_columns);
        }
    } else {
        eprintln!("Layout: simple");
    }
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file> [output_file]", args[0]);
        eprintln!("       {} <pdf_file> --json", args[0]);
        eprintln!("       {} <pdf_file> --items-json", args[0]);
        eprintln!("       {} <pdf_file> --raw", args[0]);
        eprintln!();
        eprintln!("Converts PDF to Markdown with smart type detection.");
        eprintln!("Returns early if PDF is scanned (OCR needed).");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --json              Output result as JSON");
        eprintln!("  --items-json        Output positioned TextItem JSON");
        eprintln!("  --raw               Output only markdown (no headers)");
        eprintln!(
            "  --compact           Collapse token-heavy source formatting such as dot leaders"
        );
        eprintln!("  --pages             Insert page break markers (<!-- Page N -->)");
        eprintln!("  --select-pages N    Only process specified pages (e.g. 1,3,5-10)");
        eprintln!("  --password PW       Password for an encrypted PDF");
        eprintln!("  --detect-only       Only detect PDF type (no extraction)");
        eprintln!("  --analyze           Detect + extract + layout analysis (no markdown)");
        process::exit(1);
    }

    let pdf_path = &args[1];
    let json_output = args.iter().any(|a| a == "--json");
    let items_json_output = args.iter().any(|a| a == "--items-json");
    let raw_output = args.iter().any(|a| a == "--raw");
    let compact_output = args.iter().any(|a| a == "--compact");
    let page_numbers = args.iter().any(|a| a == "--pages");
    let detect_only = args.iter().any(|a| a == "--detect-only");
    let analyze = args.iter().any(|a| a == "--analyze");

    // Parse --password value
    let password = args.iter().position(|a| a == "--password").map(|i| {
        args.get(i + 1)
            .unwrap_or_else(|| {
                eprintln!("Error: --password requires a value");
                process::exit(1);
            })
            .clone()
    });

    // Parse --select-pages value
    let page_filter = args
        .iter()
        .position(|a| a == "--select-pages")
        .map(|i| {
            args.get(i + 1)
                .unwrap_or_else(|| {
                    eprintln!("Error: --select-pages requires a value (e.g. 1,3,5-10)");
                    process::exit(1);
                })
                .as_str()
        })
        .map(|spec| {
            parse_page_spec(spec).unwrap_or_else(|e| {
                eprintln!("Error: invalid --select-pages value: {}", e);
                process::exit(1);
            })
        });

    if items_json_output {
        match extract_text_with_positions_pages(pdf_path, page_filter.as_ref()) {
            Ok(items) => println!("{}", format_items_json(&items)),
            Err(e) => {
                println!(r#"{{"error":"{}"}}"#, json_escape(&e.to_string()));
                process::exit(1);
            }
        }
        return;
    }

    let output_file = args
        .get(2)
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str());

    let process_mode = if detect_only {
        ProcessMode::DetectOnly
    } else if analyze {
        ProcessMode::Analyze
    } else {
        ProcessMode::Full
    };

    let mut options = PdfOptions::new().mode(process_mode);
    if compact_output {
        options.markdown.profile = pdf_inspector::MarkdownProfile::Compact;
    }
    options.markdown.include_page_numbers = page_numbers;
    if let Some(pages) = page_filter {
        options.page_filter = Some(pages);
    }
    options.password = password;

    match process_pdf_with_options(pdf_path, options) {
        Ok(result) => {
            if detect_only || analyze {
                // Non-full modes: output detection/analysis info
                let pdf_type_str = match result.pdf_type {
                    PdfType::TextBased => "text_based",
                    PdfType::Scanned => "scanned",
                    PdfType::ImageBased => "image_based",
                    PdfType::Mixed => "mixed",
                };

                if json_output {
                    let ocr_pages: Vec<String> = result
                        .pages_needing_ocr
                        .iter()
                        .map(|p| p.to_string())
                        .collect();
                    let table_pages: Vec<String> = result
                        .layout
                        .pages_with_tables
                        .iter()
                        .map(|p| p.to_string())
                        .collect();
                    let col_pages: Vec<String> = result
                        .layout
                        .pages_with_columns
                        .iter()
                        .map(|p| p.to_string())
                        .collect();
                    let ocr_reasons = format_ocr_reasons_by_page(&result.ocr_reasons_by_page);
                    println!(
                        r#"{{"pdf_type":"{}","page_count":{},"processing_time_ms":{},"pages_needing_ocr":[{}],"ocr_reasons_by_page":[{}],"is_complex":{},"pages_with_tables":[{}],"pages_with_columns":[{}],"has_encoding_issues":{}}}"#,
                        pdf_type_str,
                        result.page_count,
                        result.processing_time_ms,
                        ocr_pages.join(","),
                        ocr_reasons,
                        result.layout.is_complex,
                        table_pages.join(","),
                        col_pages.join(","),
                        result.has_encoding_issues,
                    );
                } else {
                    eprintln!("Type: {}", pdf_type_str);
                    eprintln!("Pages: {}", result.page_count);
                    eprintln!("Processing time: {}ms", result.processing_time_ms);
                    if !result.pages_needing_ocr.is_empty() {
                        eprintln!("Pages needing OCR: {:?}", result.pages_needing_ocr);
                    }
                    if analyze {
                        print_layout_info(&result.layout);
                    }
                }
            } else if json_output {
                let md_escaped = result
                    .markdown
                    .as_ref()
                    .map(|m| json_escape(m))
                    .unwrap_or_default();

                let ocr_pages: Vec<String> = result
                    .pages_needing_ocr
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                let table_pages: Vec<String> = result
                    .layout
                    .pages_with_tables
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                let col_pages: Vec<String> = result
                    .layout
                    .pages_with_columns
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                let ocr_reasons = format_ocr_reasons_by_page(&result.ocr_reasons_by_page);
                println!(
                    r#"{{"pdf_type":"{}","page_count":{},"has_text":{},"processing_time_ms":{},"markdown_length":{},"pages_needing_ocr":[{}],"ocr_reasons_by_page":[{}],"is_complex":{},"pages_with_tables":[{}],"pages_with_columns":[{}],"has_encoding_issues":{},"markdown":"{}"}}"#,
                    match result.pdf_type {
                        PdfType::TextBased => "text_based",
                        PdfType::Scanned => "scanned",
                        PdfType::ImageBased => "image_based",
                        PdfType::Mixed => "mixed",
                    },
                    result.page_count,
                    result.markdown.is_some(),
                    result.processing_time_ms,
                    result.markdown.as_ref().map(|m| m.len()).unwrap_or(0),
                    ocr_pages.join(","),
                    ocr_reasons,
                    result.layout.is_complex,
                    table_pages.join(","),
                    col_pages.join(","),
                    result.has_encoding_issues,
                    md_escaped
                );
            } else if raw_output {
                // Raw output - just the markdown, no headers
                match result.pdf_type {
                    PdfType::TextBased | PdfType::Mixed => {
                        if let Some(markdown) = &result.markdown {
                            print!("{}", markdown);
                        }
                    }
                    PdfType::Scanned | PdfType::ImageBased => {
                        eprintln!("Error: PDF requires OCR (type: {:?})", result.pdf_type);
                        process::exit(2);
                    }
                }
            } else {
                // Verbose output with headers
                eprintln!("PDF to Markdown Conversion");
                eprintln!("==========================");
                eprintln!("File: {}", pdf_path);
                eprintln!();

                match result.pdf_type {
                    PdfType::TextBased => {
                        eprintln!("Type: TEXT-BASED (direct extraction)");
                        eprintln!("Pages: {}", result.page_count);
                        eprintln!("Processing time: {}ms", result.processing_time_ms);
                        print_layout_info(&result.layout);
                        if !result.pages_needing_ocr.is_empty() {
                            eprintln!("Pages needing OCR: {:?}", result.pages_needing_ocr);
                        }

                        if let Some(markdown) = &result.markdown {
                            if let Some(output) = output_file {
                                fs::write(output, markdown).expect("Failed to write output file");
                                eprintln!();
                                eprintln!("Markdown written to: {}", output);
                                eprintln!("Length: {} characters", markdown.len());
                            } else {
                                eprintln!();
                                eprintln!("--- Markdown Output ---");
                                eprintln!();
                                println!("{}", markdown);
                            }
                        }
                    }
                    PdfType::Scanned | PdfType::ImageBased => {
                        eprintln!(
                            "Type: {} (OCR required)",
                            if result.pdf_type == PdfType::Scanned {
                                "SCANNED"
                            } else {
                                "IMAGE-BASED"
                            }
                        );
                        eprintln!("Pages: {}", result.page_count);
                        eprintln!("Processing time: {}ms", result.processing_time_ms);
                        eprintln!();
                        eprintln!("This PDF requires OCR for text extraction.");
                        eprintln!("Consider using MinerU or similar OCR tool.");
                        process::exit(2);
                    }
                    PdfType::Mixed => {
                        eprintln!("Type: MIXED (partial text extraction)");
                        eprintln!("Pages: {}", result.page_count);
                        eprintln!("Processing time: {}ms", result.processing_time_ms);
                        print_layout_info(&result.layout);

                        if let Some(markdown) = &result.markdown {
                            eprintln!();
                            if result.pages_needing_ocr.is_empty() {
                                eprintln!("Note: Some pages may contain images that require OCR.");
                            } else {
                                eprintln!("Pages needing OCR: {:?}", result.pages_needing_ocr);
                            }
                            eprintln!();

                            if let Some(output) = output_file {
                                fs::write(output, markdown).expect("Failed to write output file");
                                eprintln!("Markdown written to: {}", output);
                                eprintln!("Length: {} characters", markdown.len());
                            } else {
                                eprintln!("--- Markdown Output ---");
                                eprintln!();
                                println!("{}", markdown);
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            if json_output {
                println!(r#"{{"error":"{}"}}"#, e);
            } else {
                eprintln!("Error: {}", e);
            }
            process::exit(1);
        }
    }
}
