//! CLI tool for detecting PDF type (text-based vs scanned)

use pdf_inspector::{
    detect_pdf_type, detector::estimate_page_count_from_bytes, process_pdf_with_options,
    PdfOptions, PdfType, ProcessMode,
};
use std::env;
use std::fmt::Write;
use std::fs;
use std::process;
use std::time::Instant;

/// Escape a string for embedding in a JSON string value.
fn format_detector_ocr_reasons(reasons: &std::collections::BTreeMap<u32, Vec<String>>) -> String {
    reasons
        .iter()
        .map(|(page, page_reasons)| {
            let reasons_json = page_reasons
                .iter()
                .map(|reason| format!(r#""{}""#, json_escape(reason)))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"page":{},"reasons":[{}]}}"#, page, reasons_json)
        })
        .collect::<Vec<_>>()
        .join(",")
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

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file>", args[0]);
        eprintln!("       {} <pdf_file> --json", args[0]);
        eprintln!("       {} <pdf_file> --analyze", args[0]);
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --json       Output result as JSON");
        eprintln!("  --analyze    Also run layout analysis (tables, columns)");
        process::exit(1);
    }

    let pdf_path = &args[1];
    let json_output = args.iter().any(|a| a == "--json");
    let analyze = args.iter().any(|a| a == "--analyze");

    let start = Instant::now();

    if analyze {
        run_analyze(pdf_path, json_output, start);
    } else {
        run_detect_only(pdf_path, json_output, start);
    }
}

fn pdf_type_str(pdf_type: &PdfType) -> &'static str {
    match pdf_type {
        PdfType::TextBased => "text_based",
        PdfType::Scanned => "scanned",
        PdfType::ImageBased => "image_based",
        PdfType::Mixed => "mixed",
    }
}

fn page_count_hint(pdf_path: &str) -> Option<u32> {
    fs::read(pdf_path)
        .ok()
        .map(|bytes| estimate_page_count_from_bytes(&bytes))
        .filter(|&count| count > 0)
}

fn print_error(e: &pdf_inspector::PdfError, pdf_path: &str, json_output: bool) {
    if json_output {
        if let Some(count) = page_count_hint(pdf_path) {
            println!(
                r#"{{"error":"{}","page_count_hint":{}}}"#,
                json_escape(&e.to_string()),
                count
            );
        } else {
            println!(r#"{{"error":"{}"}}"#, json_escape(&e.to_string()));
        }
    } else {
        eprintln!("Error: {}", e);
        if let Some(count) = page_count_hint(pdf_path) {
            eprintln!("Page count hint: {}", count);
        }
    }
}

fn run_analyze(pdf_path: &str, json_output: bool, start: Instant) {
    match process_pdf_with_options(pdf_path, PdfOptions::new().mode(ProcessMode::Analyze)) {
        Ok(result) => {
            let elapsed = start.elapsed();

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
                    r#"{{"pdf_type":"{}","page_count":{},"pages_needing_ocr":[{}],"ocr_reasons_by_page":[{}],"is_complex":{},"pages_with_tables":[{}],"pages_with_columns":[{}],"detection_time_ms":{}}}"#,
                    pdf_type_str(&result.pdf_type),
                    result.page_count,
                    ocr_pages.join(","),
                    ocr_reasons,
                    result.layout.is_complex,
                    table_pages.join(","),
                    col_pages.join(","),
                    elapsed.as_millis()
                );
            } else {
                println!("PDF Type Detection + Layout Analysis");
                println!("=====================================");
                println!("File: {}", pdf_path);
                println!();
                println!(
                    "Type: {}",
                    match result.pdf_type {
                        PdfType::TextBased => "TEXT-BASED (extractable text)",
                        PdfType::Scanned => "SCANNED (OCR needed)",
                        PdfType::ImageBased => "IMAGE-BASED (mostly images, OCR may help)",
                        PdfType::Mixed => "MIXED (some text, some images)",
                    }
                );
                println!("Page count: {}", result.page_count);
                if !result.pages_needing_ocr.is_empty() {
                    println!("Pages needing OCR: {:?}", result.pages_needing_ocr);
                    for entry in &result.ocr_reasons_by_page {
                        println!("  page {}: {}", entry.page, entry.reasons.join(", "));
                    }
                }
                println!();
                if result.layout.is_complex {
                    println!("Layout: COMPLEX");
                    if !result.layout.pages_with_tables.is_empty() {
                        println!("  Pages with tables: {:?}", result.layout.pages_with_tables);
                    }
                    if !result.layout.pages_with_columns.is_empty() {
                        println!(
                            "  Pages with columns: {:?}",
                            result.layout.pages_with_columns
                        );
                    }
                } else {
                    println!("Layout: simple");
                }
                println!();
                println!("Detection time: {}ms", elapsed.as_millis());
            }
        }
        Err(e) => {
            print_error(&e, pdf_path, json_output);
            process::exit(1);
        }
    }
}

fn run_detect_only(pdf_path: &str, json_output: bool, start: Instant) {
    // Use the low-level detect_pdf_type for richer output (pages_sampled etc.),
    // but also call detect_pdf to demonstrate the unified API.
    match detect_pdf_type(pdf_path) {
        Ok(result) => {
            let elapsed = start.elapsed();

            if json_output {
                let ocr_pages: Vec<String> = result
                    .pages_needing_ocr
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                let ocr_reasons = format_detector_ocr_reasons(&result.ocr_reasons_by_page);
                println!(
                    r#"{{"pdf_type":"{}","page_count":{},"pages_sampled":{},"pages_with_text":{},"confidence":{:.2},"title":{},"ocr_recommended":{},"pages_needing_ocr":[{}],"ocr_reasons_by_page":[{}],"detection_time_ms":{}}}"#,
                    pdf_type_str(&result.pdf_type),
                    result.page_count,
                    result.pages_sampled,
                    result.pages_with_text,
                    result.confidence,
                    result
                        .title
                        .as_ref()
                        .map(|t| format!("\"{}\"", json_escape(t)))
                        .unwrap_or_else(|| "null".to_string()),
                    result.ocr_recommended,
                    ocr_pages.join(","),
                    ocr_reasons,
                    elapsed.as_millis()
                );
            } else {
                println!("PDF Type Detection Results");
                println!("==========================");
                println!("File: {}", pdf_path);
                println!();
                println!(
                    "Type: {}",
                    match result.pdf_type {
                        PdfType::TextBased => "TEXT-BASED (extractable text)",
                        PdfType::Scanned => "SCANNED (OCR needed)",
                        PdfType::ImageBased => "IMAGE-BASED (mostly images, OCR may help)",
                        PdfType::Mixed => "MIXED (some text, some images)",
                    }
                );
                println!("Confidence: {:.0}%", result.confidence * 100.0);
                println!();
                println!("Page count: {}", result.page_count);
                println!("Pages sampled: {}", result.pages_sampled);
                println!("Pages with text: {}", result.pages_with_text);
                println!(
                    "OCR recommended: {}",
                    if result.ocr_recommended { "YES" } else { "NO" }
                );
                if !result.pages_needing_ocr.is_empty() {
                    if result.pages_needing_ocr.len() == result.page_count as usize {
                        println!("Pages needing OCR: all (of {})", result.page_count);
                    } else {
                        println!(
                            "Pages needing OCR: {:?} (of {})",
                            result.pages_needing_ocr, result.page_count
                        );
                    }
                    for (page, reasons) in &result.ocr_reasons_by_page {
                        println!("  page {}: {}", page, reasons.join(", "));
                    }
                }
                if let Some(title) = &result.title {
                    println!("Title: {}", title);
                }
                println!();
                println!("Detection time: {}ms", elapsed.as_millis());
                println!();

                // Recommendations
                if result.ocr_recommended {
                    match result.pdf_type {
                        PdfType::Mixed => {
                            println!("Recommendation: Use OCR - images provide essential context (template PDF)");
                        }
                        PdfType::Scanned => {
                            println!("Recommendation: Use OCR (MinerU or similar)");
                        }
                        PdfType::ImageBased => {
                            println!("Recommendation: Use OCR for best results");
                        }
                        _ => {
                            println!("Recommendation: Use OCR for complete extraction");
                        }
                    }
                } else {
                    println!("Recommendation: Use direct text extraction (fast)");
                }
            }
        }
        Err(e) => {
            print_error(&e, pdf_path, json_output);
            process::exit(1);
        }
    }
}
