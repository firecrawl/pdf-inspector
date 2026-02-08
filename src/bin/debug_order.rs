use pdf_inspector::extract_text_with_positions;

fn main() {
    let items = extract_text_with_positions("samples/tables/doc.pdf").unwrap();
    
    // Find items containing "Description" or section numbers
    println!("Items containing section markers:");
    for item in items.iter().filter(|i| i.page == 1) {
        if item.text.contains("Description") || 
           item.text.starts_with("3 ") ||
           item.text.starts_with("3  ") {
            println!("  x={:6.1} y={:6.1} \"{}\"", item.x, item.y, item.text);
        }
    }
    
    // Look at the Y range for right column
    let right_col: Vec<_> = items.iter()
        .filter(|i| i.page == 1 && i.x > 300.0 && i.x < 400.0)
        .collect();
    
    let y_min = right_col.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let y_max = right_col.iter().map(|i| i.y).fold(f32::NEG_INFINITY, f32::max);
    println!("\nRight column (x=300-400) Y range: {:.1} to {:.1}", y_min, y_max);
    
    // Show items near Y=675 (where "3 Description" appears)
    println!("\nItems near Y=675 (±10):");
    for item in items.iter().filter(|i| i.page == 1 && (i.y - 675.0).abs() < 10.0) {
        println!("  x={:6.1} y={:6.1} \"{}\"", item.x, item.y, &item.text[..item.text.len().min(50)]);
    }
}
