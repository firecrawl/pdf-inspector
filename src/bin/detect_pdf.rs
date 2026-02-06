//! CLI tool for detecting PDF type (text-based vs scanned)

use pdf_to_markdown::{detect_pdf_type, PdfType};
use std::env;
use std::process;
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file>", args[0]);
        eprintln!("       {} <pdf_file> --json", args[0]);
        process::exit(1);
    }

    let pdf_path = &args[1];
    let json_output = args.get(2).map(|a| a == "--json").unwrap_or(false);

    let start = Instant::now();

    match detect_pdf_type(pdf_path) {
        Ok(result) => {
            let elapsed = start.elapsed();

            if json_output {
                println!(
                    r#"{{"pdf_type":"{}","page_count":{},"pages_sampled":{},"pages_with_text":{},"confidence":{:.2},"title":{},"detection_time_ms":{}}}"#,
                    match result.pdf_type {
                        PdfType::TextBased => "text_based",
                        PdfType::Scanned => "scanned",
                        PdfType::ImageBased => "image_based",
                        PdfType::Mixed => "mixed",
                    },
                    result.page_count,
                    result.pages_sampled,
                    result.pages_with_text,
                    result.confidence,
                    result.title.as_ref().map(|t| format!("\"{}\"", t.replace('"', "\\\""))).unwrap_or_else(|| "null".to_string()),
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
                if let Some(title) = &result.title {
                    println!("Title: {}", title);
                }
                println!();
                println!("Detection time: {}ms", elapsed.as_millis());
                println!();

                // Recommendations
                match result.pdf_type {
                    PdfType::TextBased => {
                        println!("Recommendation: Use direct text extraction (fast)");
                    }
                    PdfType::Scanned => {
                        println!("Recommendation: Use OCR (MinerU or similar)");
                    }
                    PdfType::ImageBased => {
                        println!("Recommendation: Use OCR for best results");
                    }
                    PdfType::Mixed => {
                        println!("Recommendation: Try text extraction first, use OCR for image pages");
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
