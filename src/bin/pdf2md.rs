//! CLI tool for PDF to Markdown conversion

use pdf_inspector::{process_pdf, PdfType};
use std::env;
use std::fs;
use std::process;
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file> [output_file]", args[0]);
        eprintln!("       {} <pdf_file> --json", args[0]);
        eprintln!();
        eprintln!("Converts PDF to Markdown with smart type detection.");
        eprintln!("Returns early if PDF is scanned (OCR needed).");
        process::exit(1);
    }

    let pdf_path = &args[1];
    let json_output = args.get(2).map(|a| a == "--json").unwrap_or(false);
    let output_file = if !json_output { args.get(2) } else { None };

    let start = Instant::now();

    match process_pdf(pdf_path) {
        Ok(result) => {
            let _elapsed = start.elapsed();

            if json_output {
                let md_escaped = result
                    .markdown
                    .as_ref()
                    .map(|m| {
                        m.replace('\\', "\\\\")
                            .replace('"', "\\\"")
                            .replace('\n', "\\n")
                    })
                    .unwrap_or_default();

                println!(
                    r#"{{"pdf_type":"{}","page_count":{},"has_text":{},"processing_time_ms":{},"markdown_length":{},"markdown":"{}"}}"#,
                    match result.pdf_type {
                        PdfType::TextBased => "text_based",
                        PdfType::Scanned => "scanned",
                        PdfType::ImageBased => "image_based",
                        PdfType::Mixed => "mixed",
                    },
                    result.page_count,
                    result.text.is_some(),
                    result.processing_time_ms,
                    result.markdown.as_ref().map(|m| m.len()).unwrap_or(0),
                    md_escaped
                );
            } else {
                println!("PDF to Markdown Conversion");
                println!("==========================");
                println!("File: {}", pdf_path);
                println!();

                match result.pdf_type {
                    PdfType::TextBased => {
                        println!("Type: TEXT-BASED (direct extraction)");
                        println!("Pages: {}", result.page_count);
                        println!("Processing time: {}ms", result.processing_time_ms);

                        if let Some(markdown) = &result.markdown {
                            if let Some(output) = output_file {
                                fs::write(output, markdown).expect("Failed to write output file");
                                println!();
                                println!("Markdown written to: {}", output);
                                println!("Length: {} characters", markdown.len());
                            } else {
                                println!();
                                println!("--- Markdown Output ---");
                                println!();
                                println!("{}", markdown);
                            }
                        }
                    }
                    PdfType::Scanned | PdfType::ImageBased => {
                        println!(
                            "Type: {} (OCR required)",
                            if result.pdf_type == PdfType::Scanned {
                                "SCANNED"
                            } else {
                                "IMAGE-BASED"
                            }
                        );
                        println!("Pages: {}", result.page_count);
                        println!("Processing time: {}ms", result.processing_time_ms);
                        println!();
                        println!("This PDF requires OCR for text extraction.");
                        println!("Consider using MinerU or similar OCR tool.");
                        process::exit(2);
                    }
                    PdfType::Mixed => {
                        println!("Type: MIXED (partial text extraction)");
                        println!("Pages: {}", result.page_count);
                        println!("Processing time: {}ms", result.processing_time_ms);

                        if let Some(markdown) = &result.markdown {
                            println!();
                            println!("Note: Some pages may contain images that require OCR.");
                            println!();

                            if let Some(output) = output_file {
                                fs::write(output, markdown).expect("Failed to write output file");
                                println!("Markdown written to: {}", output);
                                println!("Length: {} characters", markdown.len());
                            } else {
                                println!("--- Markdown Output ---");
                                println!();
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
