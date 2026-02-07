//! CLI tool for PDF to Markdown conversion

use pdf_inspector::{process_pdf, PdfType};
use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file> [output_file]", args[0]);
        eprintln!("       {} <pdf_file> --json", args[0]);
        eprintln!("       {} <pdf_file> --raw", args[0]);
        eprintln!();
        eprintln!("Converts PDF to Markdown with smart type detection.");
        eprintln!("Returns early if PDF is scanned (OCR needed).");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --json    Output result as JSON");
        eprintln!("  --raw     Output only markdown (no headers)");
        process::exit(1);
    }

    let pdf_path = &args[1];
    let json_output = args.iter().any(|a| a == "--json");
    let raw_output = args.iter().any(|a| a == "--raw");
    let output_file = args
        .get(2)
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str());

    match process_pdf(pdf_path) {
        Ok(result) => {
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

                        if let Some(markdown) = &result.markdown {
                            eprintln!();
                            eprintln!("Note: Some pages may contain images that require OCR.");
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
