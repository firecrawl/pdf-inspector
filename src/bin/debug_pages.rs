use pdf_inspector::extract_text_with_positions;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: debug_pages <pdf_path> [max_page | min-max]");
        std::process::exit(1);
    }

    let range = args.get(2).map(|s| s.as_str()).unwrap_or("1-3");
    let (min_page, max_page) = if let Some((a, b)) = range.split_once('-') {
        (a.parse().unwrap_or(1), b.parse().unwrap_or(3))
    } else {
        (1, range.parse().unwrap_or(3))
    };

    let items = extract_text_with_positions(&args[1]).expect("Failed to extract");

    for page in min_page..=max_page {
        let page_items: Vec<_> = items.iter().filter(|i| i.page == page).collect();
        println!("=== PAGE {} ({} items) ===", page, page_items.len());
        for item in &page_items {
            println!(
                "  x={:7.1} y={:7.1} w={:7.1} fs={:5.1} text={:?}",
                item.x, item.y, item.width, item.font_size, item.text
            );
        }
        println!();
    }
}
