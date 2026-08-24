//! CLI tool for PDF to Markdown conversion

use pdf_inspector::extractor::ItemType;
#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
use pdf_inspector::vision::{
    process_pdf_with_ocr, ModelDownloadPolicy, OcrMode, OcrOptions, OcrPdfOptions, OcrPdfResult,
    PageContentSource, RenderOptions,
};
use pdf_inspector::{
    extract_text_with_positions_pages_with_password, process_pdf_with_options, LayoutComplexity,
    PdfOptions, PdfType, ProcessMode, TextItem,
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
                r#"{{"text":"{}","page":{},"x":{:.2},"y":{:.2},"width":{:.2},"height":{:.2},"font":"{}","font_tag":"{}","font_size":{:.2},"is_bold":{},"is_italic":{},"is_underline":{},"is_strikeout":{},"item_type":"{}","mcid":{}{}}}"#,
                json_escape(&item.text),
                item.page,
                item.x,
                item.y,
                item.width,
                item.height,
                json_escape(&item.font),
                json_escape(&item.font_tag),
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

#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
fn optional_json_number(value: Option<f32>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "null".to_string())
}

#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
fn format_ocr_json(result: &OcrPdfResult) -> String {
    let routed = result
        .pages_routed_to_ocr
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let recommended = result
        .pages_recommended_for_ocr
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let hosted = result
        .pages_recommending_hosted
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let pages = result
        .pages
        .iter()
        .map(|page| {
            let provenance = &page.provenance;
            let source = match provenance.source {
                PageContentSource::Native => "native",
                PageContentSource::Ocr => "ocr",
                PageContentSource::Fused => "fused",
                _ => "unknown",
            };
            let model = provenance
                .ocr_model
                .as_ref()
                .map(|model| {
                    format!(
                        r#"{{"name":"{}","revision":"{}"}}"#,
                        json_escape(&model.name),
                        json_escape(&model.revision)
                    )
                })
                .unwrap_or_else(|| "null".to_string());
            let warnings = provenance
                .warnings
                .iter()
                .map(|warning| format!(r#""{}""#, json_escape(warning)))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"page":{},"source":"{}","markdown":"{}","ocr_model":{},"render_dpi":{},"ocr_confidence":{},"hosted_recommended":{},"timings":{{"render_ms":{},"ocr_ms":{},"assembly_ms":{}}},"warnings":[{}]}}"#,
                provenance.page_number,
                source,
                json_escape(&page.markdown),
                model,
                optional_json_number(provenance.render_dpi),
                optional_json_number(provenance.ocr_confidence),
                provenance.hosted_recommended,
                provenance.timings.render_ms,
                provenance.timings.ocr_ms,
                provenance.timings.assembly_ms,
                warnings,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let table_pages = result
        .pages_with_tables
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let column_pages = result
        .pages_with_columns
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let ocr_reasons = format_ocr_reasons_by_page(&result.ocr_reasons_by_page);
    format!(
        r#"{{"schema_version":1,"page_count":{},"processing_time_ms":{},"render_time_ms":{},"ocr_time_ms":{},"pages_recommended_for_ocr":[{}],"pages_routed_to_ocr":[{}],"pages_recommending_hosted":[{}],"ocr_reasons_by_page":[{}],"is_complex":{},"pages_with_tables":[{}],"pages_with_columns":[{}],"pages":[{}],"markdown":"{}"}}"#,
        result.page_count,
        result.processing_time_ms,
        result.render_time_ms,
        result.ocr_time_ms,
        recommended,
        routed,
        hosted,
        ocr_reasons,
        result.is_complex,
        table_pages,
        column_pages,
        pages,
        json_escape(&result.markdown),
    )
}

fn argument_value<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    args.iter()
        .position(|argument| argument == name)
        .map(|index| {
            args.get(index + 1)
                .map(String::as_str)
                .ok_or_else(|| format!("{name} requires a value"))
        })
        .transpose()
}

fn format_ocr_error_json(error: &str) -> String {
    format!(r#"{{"schema_version":1,"error":"{}"}}"#, json_escape(error))
}

fn exit_ocr_error(error: &str, json_output: bool) -> ! {
    if json_output {
        println!("{}", format_ocr_error_json(error));
    } else {
        eprintln!("Error: {error}");
    }
    process::exit(1);
}

#[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
fn float_argument(args: &[String], name: &str, default: f32) -> Result<f32, String> {
    argument_value(args, name)?
        .map(|value| {
            value
                .parse::<f32>()
                .map_err(|_| format!("{name} requires a number, got {value:?}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn extract_items_json(
    pdf_path: &str,
    page_filter: Option<&HashSet<u32>>,
    password: Option<&str>,
) -> Result<String, pdf_inspector::PdfError> {
    extract_text_with_positions_pages_with_password(pdf_path, page_filter, password)
        .map(|items| format_items_json(&items))
}

#[cfg(test)]
mod tests {
    use super::{extract_items_json, format_items_json, format_ocr_error_json};
    #[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
    use super::{format_ocr_json, process_pdf_with_ocr, OcrPdfOptions};
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
            font_tag: String::new(),
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

    #[test]
    fn items_json_uses_supplied_pdf_password() {
        let path = "tests/fixtures/encrypted-secret123.pdf";

        let without_password = extract_items_json(path, None, None);
        assert!(
            without_password.is_err(),
            "encrypted fixture unexpectedly extracted without a password"
        );

        let json = extract_items_json(path, None, Some("secret123"))
            .expect("correct password should decrypt positioned text");
        assert!(
            json.contains("Procurement"),
            "decrypted item JSON should contain fixture text, got {json}"
        );
    }

    #[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
    #[test]
    fn ocr_json_has_a_versioned_stable_envelope() {
        let result =
            process_pdf_with_ocr("tests/fixtures/thermo-freon12.pdf", OcrPdfOptions::new())
                .unwrap();
        let json = format_ocr_json(&result);

        assert!(json.starts_with(r#"{"schema_version":1,"page_count":3,"#));
        assert!(json.contains(r#""page":1,"source":"native""#));
        assert!(!json.contains("layout_ms"));
    }

    #[test]
    fn ocr_json_errors_use_the_same_versioned_envelope() {
        assert_eq!(
            format_ocr_error_json("bad \"value\""),
            r#"{"schema_version":1,"error":"bad \"value\""}"#
        );
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
        eprintln!("  --ocr MODE          OCR mode: off, auto, or force (requires feature `ocr`)");
        eprintln!("  --ocr-dpi N         OCR render resolution (default: 150)");
        eprintln!("  --ocr-min-confidence N  Drop OCR spans below N (default: 0)");
        eprintln!("  --ocr-hosted-threshold N  Recommend hosted parsing below N (default: 0.5)");
        eprintln!("  --ocr-model-dir DIR Use a package-managed local model directory");
        eprintln!("  --ocr-offline       Never download missing OCR models");
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
    let ocr_mode_argument = argument_value(&args, "--ocr").unwrap_or_else(|error| {
        eprintln!("Error: {error}");
        process::exit(1);
    });

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

    let output_file = args
        .get(2)
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str());

    let has_ocr_only_option = [
        "--ocr-dpi",
        "--ocr-min-confidence",
        "--ocr-hosted-threshold",
        "--ocr-model-dir",
        "--ocr-offline",
    ]
    .iter()
    .any(|option| args.iter().any(|argument| argument == option));
    if ocr_mode_argument.is_none() && has_ocr_only_option {
        exit_ocr_error(
            "OCR options require --ocr off, --ocr auto, or --ocr force",
            json_output,
        );
    }

    if let Some(mode) = ocr_mode_argument {
        if items_json_output || detect_only || analyze {
            exit_ocr_error(
                "--ocr cannot be combined with --items-json, --detect-only, or --analyze",
                json_output,
            );
        }

        #[cfg(not(all(feature = "ocr", not(target_arch = "wasm32"))))]
        {
            let _ = mode;
            exit_ocr_error(
                "this pdf2md build does not include OCR; rebuild with --features ocr",
                json_output,
            );
        }

        #[cfg(all(feature = "ocr", not(target_arch = "wasm32")))]
        {
            let mode = match mode {
                "off" => OcrMode::Off,
                "auto" => OcrMode::Auto,
                "force" => OcrMode::Force,
                value => {
                    exit_ocr_error(
                        &format!("invalid --ocr mode {value:?}; expected off, auto, or force"),
                        json_output,
                    );
                }
            };
            let dpi = float_argument(&args, "--ocr-dpi", 150.0).unwrap_or_else(|error| {
                exit_ocr_error(&error, json_output);
            });
            let minimum_confidence = float_argument(&args, "--ocr-min-confidence", 0.0)
                .unwrap_or_else(|error| {
                    exit_ocr_error(&error, json_output);
                });
            let hosted_threshold = float_argument(&args, "--ocr-hosted-threshold", 0.5)
                .unwrap_or_else(|error| {
                    exit_ocr_error(&error, json_output);
                });
            let model_directory =
                argument_value(&args, "--ocr-model-dir").unwrap_or_else(|error| {
                    exit_ocr_error(&error, json_output);
                });

            let mut ocr = OcrOptions::new()
                .mode(mode)
                .minimum_confidence(minimum_confidence);
            if let Some(directory) = model_directory {
                ocr = ocr.model_directory(directory);
            }
            if args.iter().any(|argument| argument == "--ocr-offline") {
                ocr = ocr.model_downloads(ModelDownloadPolicy::Offline);
            }
            let mut markdown = pdf_inspector::MarkdownOptions::default();
            if compact_output {
                markdown.profile = pdf_inspector::MarkdownProfile::Compact;
            }
            markdown.include_page_numbers = page_numbers;
            let mut pdf_options = OcrPdfOptions::new()
                .render(RenderOptions::new().dpi(dpi))
                .ocr(ocr)
                .markdown(markdown)
                .hosted_recommendation_confidence(hosted_threshold);
            if let Some(pages) = page_filter.clone() {
                pdf_options = pdf_options.page_numbers(pages);
            }
            if let Some(password) = password.clone() {
                pdf_options = pdf_options.password(password);
            }

            match process_pdf_with_ocr(pdf_path, pdf_options) {
                Ok(result) => {
                    if json_output {
                        println!("{}", format_ocr_json(&result));
                    } else if raw_output {
                        print!("{}", result.markdown);
                    } else {
                        eprintln!("PDF to Markdown Conversion (OCR)");
                        eprintln!("======================================");
                        eprintln!("File: {pdf_path}");
                        eprintln!("Pages: {}", result.page_count);
                        eprintln!("Pages routed to OCR: {:?}", result.pages_routed_to_ocr);
                        if !result.pages_recommending_hosted.is_empty() {
                            eprintln!(
                                "Hosted parsing recommended for pages: {:?}",
                                result.pages_recommending_hosted
                            );
                        }
                        eprintln!("Processing time: {}ms", result.processing_time_ms);
                        if let Some(output) = output_file {
                            fs::write(output, &result.markdown)
                                .expect("Failed to write output file");
                            eprintln!("Markdown written to: {output}");
                        } else {
                            eprintln!();
                            eprintln!("--- Markdown Output ---");
                            eprintln!();
                            print!("{}", result.markdown);
                        }
                    }
                }
                Err(error) => {
                    exit_ocr_error(&error.to_string(), json_output);
                }
            }
            return;
        }
    }

    if items_json_output {
        match extract_items_json(pdf_path, page_filter.as_ref(), password.as_deref()) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                println!(r#"{{"error":"{}"}}"#, json_escape(&e.to_string()));
                process::exit(1);
            }
        }
        return;
    }

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
