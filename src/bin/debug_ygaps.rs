//! Debug tool: Print Y positions and gaps between consecutive lines
//!
//! Usage: debug_ygaps <pdf_file> [page_number]
//!
//! Shows text lines grouped by page with Y coordinates, gaps from previous line,
//! font sizes, and whether each gap would be treated as a paragraph break.

use pdf_inspector::extract_text_with_positions;
use pdf_inspector::extractor::{group_into_lines, TextLine};
use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <pdf_file> [page_number]", args[0]);
        eprintln!();
        eprintln!("Prints Y positions and gaps between consecutive text lines.");
        eprintln!("If page_number is given, only that page is shown.");
        process::exit(1);
    }

    let pdf_path = &args[1];
    let filter_page: Option<u32> = args.get(2).and_then(|s| s.parse().ok());

    let items = match extract_text_with_positions(pdf_path) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("Error extracting text: {}", e);
            process::exit(1);
        }
    };

    if items.is_empty() {
        eprintln!("No text items found in PDF.");
        process::exit(0);
    }

    // Compute base font size (most common font size >= 9pt)
    let base_size = {
        let mut size_counts: std::collections::HashMap<i32, usize> =
            std::collections::HashMap::new();
        for item in &items {
            if item.font_size >= 9.0 {
                let key = (item.font_size * 10.0) as i32;
                *size_counts.entry(key).or_insert(0) += 1;
            }
        }
        size_counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(size_key, _)| size_key as f32 / 10.0)
            .unwrap_or(10.0)
    };

    eprintln!("Base font size: {:.1}pt", base_size);
    eprintln!(
        "Paragraph break threshold: y_gap > {:.1} (base * 1.8)",
        base_size * 1.8
    );
    eprintln!();

    // Group into lines
    let lines = group_into_lines(items);

    // Get unique pages
    let mut pages: Vec<u32> = lines.iter().map(|l| l.page).collect();
    pages.sort();
    pages.dedup();

    for page in pages {
        if let Some(fp) = filter_page {
            if page != fp {
                continue;
            }
        }

        let page_lines: Vec<&TextLine> = lines.iter().filter(|l| l.page == page).collect();

        println!("===== PAGE {} ({} lines) =====", page, page_lines.len());
        println!(
            "{:>8} {:>8} {:>8} {:>6} {:>5}  {}",
            "Y", "Gap", "GapRatio", "Font", "Bold", "Text (first 80 chars)"
        );
        println!("{}", "-".repeat(120));

        let mut prev_y: Option<f32> = None;

        for line in &page_lines {
            let font_size = line.items.first().map(|i| i.font_size).unwrap_or(0.0);
            let is_bold = line.items.first().map(|i| i.is_bold).unwrap_or(false);
            let text = line.text();
            let display_text: String = text.chars().take(80).collect();

            let (gap_str, ratio_str, marker) = if let Some(py) = prev_y {
                let gap = py - line.y;
                let ratio = gap / base_size;
                let is_para = gap > base_size * 1.8;
                let marker = if is_para { " <<PARA>>" } else { "" };
                (
                    format!("{:8.1}", gap),
                    format!("{:8.2}", ratio),
                    marker.to_string(),
                )
            } else {
                (
                    "     ---".to_string(),
                    "     ---".to_string(),
                    String::new(),
                )
            };

            println!(
                "{:8.1} {} {} {:6.1} {:>5}  {}{}",
                line.y,
                gap_str,
                ratio_str,
                font_size,
                if is_bold { "B" } else { "" },
                display_text,
                marker
            );

            prev_y = Some(line.y);
        }

        println!();

        // Summary statistics for this page
        let mut gaps: Vec<f32> = Vec::new();
        let mut prev_y: Option<f32> = None;
        for line in &page_lines {
            if let Some(py) = prev_y {
                let gap = py - line.y;
                if gap > 0.0 && gap < 200.0 {
                    gaps.push(gap);
                }
            }
            prev_y = Some(line.y);
        }

        if !gaps.is_empty() {
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let min = gaps.first().unwrap();
            let max = gaps.last().unwrap();
            let median = gaps[gaps.len() / 2];
            let mean: f32 = gaps.iter().sum::<f32>() / gaps.len() as f32;

            println!("  Gap statistics for page {}:", page);
            println!("    Count: {}", gaps.len());
            println!("    Min:    {:6.1} (ratio: {:.2})", min, min / base_size);
            println!("    Max:    {:6.1} (ratio: {:.2})", max, max / base_size);
            println!(
                "    Median: {:6.1} (ratio: {:.2})",
                median,
                median / base_size
            );
            println!("    Mean:   {:6.1} (ratio: {:.2})", mean, mean / base_size);

            // Histogram of gap ratios
            println!();
            println!("  Gap ratio histogram (gap / base_size):");
            let buckets: Vec<f32> = vec![
                0.0,
                0.5,
                1.0,
                1.2,
                1.5,
                1.8,
                2.0,
                2.5,
                3.0,
                5.0,
                10.0,
                f32::INFINITY,
            ];
            for i in 0..buckets.len() - 1 {
                let count = gaps
                    .iter()
                    .filter(|&&g| {
                        let r = g / base_size;
                        r >= buckets[i] && r < buckets[i + 1]
                    })
                    .count();
                if count > 0 {
                    let label = if buckets[i + 1] == f32::INFINITY {
                        format!("{:4.1}+    ", buckets[i])
                    } else {
                        format!("{:4.1}-{:<4.1}", buckets[i], buckets[i + 1])
                    };
                    let bar: String = "#".repeat(count.min(60));
                    println!("    {} | {:3} {}", label, count, bar);
                }
            }
            println!();
        }
    }
}
