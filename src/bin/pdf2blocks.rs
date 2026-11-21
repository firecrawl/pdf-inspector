//! Extract a PDF into typed content blocks, one block per line:
//! `page<TAB>kind<TAB>furniture<TAB>level<TAB>text` (text
//! newline-escaped). The tabular form exists for diffing and
//! downstream tooling; `--summary` prints per-kind counts instead.
use std::env;
use std::fs;
use std::process::ExitCode;

use pdf_inspector::{process_pdf_blocks_mem_with_options, PdfOptions};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let mut summary = false;
    let mut path: Option<&str> = None;
    for arg in &args[1..] {
        match arg.as_str() {
            "--summary" => summary = true,
            other => path = Some(other),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: pdf2blocks [--summary] <file.pdf>");
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("reading {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match process_pdf_blocks_mem_with_options(&bytes, PdfOptions::default()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("parsing {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if summary {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for block in &result.blocks {
            *counts.entry(format!("{:?}", block.kind)).or_default() += 1;
            if block.furniture {
                *counts.entry("Furniture".to_string()).or_default() += 1;
            }
        }
        println!(
            "type={:?} pages={} blocks={} encoding_issues={}",
            result.pdf_type,
            result.page_count,
            result.blocks.len(),
            result.has_encoding_issues
        );
        for (kind, count) in counts {
            println!("{kind}: {count}");
        }
    } else {
        for block in &result.blocks {
            println!(
                "{}\t{:?}\t{}\t{}\t{}",
                block.page,
                block.kind,
                block.furniture,
                block.level.map(|l| l.to_string()).unwrap_or_default(),
                block.text.replace('\n', "\\n")
            );
        }
    }
    ExitCode::SUCCESS
}
