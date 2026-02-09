use pdf_inspector::extract_text_with_positions;
use std::env;

fn main() {
    let path = env::args().nth(1).expect("Need PDF path");
    let items = extract_text_with_positions(&path).expect("Failed");

    // Look at consecutive items on same Y line
    let mut prev_item: Option<&pdf_inspector::TextItem> = None;
    for item in items.iter() {
        if let Some(prev) = prev_item {
            // Same line (similar Y)
            if (item.y - prev.y).abs() < 5.0 && item.x > prev.x {
                let gap = item.x - prev.x - prev.width;
                let char_width = if prev.width > 0.0 && !prev.text.is_empty() {
                    prev.width / prev.text.len() as f32
                } else {
                    prev.font_size * 0.5 // Approximate
                };

                println!(
                    "Gap: {:.1} (charW: {:.1}) | '{}' -> '{}'",
                    gap,
                    char_width,
                    prev.text.chars().take(20).collect::<String>(),
                    item.text.chars().take(20).collect::<String>()
                );
            }
        }
        prev_item = Some(item);
    }
}
