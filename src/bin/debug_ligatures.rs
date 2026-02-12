use pdf_inspector::extract_text_with_positions;
use pdf_inspector::tounicode::FontCMaps;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("Usage: debug_ligatures <pdf>");

    // Load PDF and extract CMaps
    let pdf_bytes = std::fs::read(&path).unwrap();
    let font_cmaps = FontCMaps::from_pdf_bytes(&pdf_bytes);

    println!("=== Font CMaps ===");
    if font_cmaps.by_name.is_empty() && font_cmaps.by_obj_num.is_empty() {
        println!("  (none found)");
    }
    for (name, cmap) in &font_cmaps.by_name {
        println!(
            "  font={:30} code_byte_length={} char_map_entries={} ranges={}",
            name,
            cmap.code_byte_length,
            cmap.char_map.len(),
            cmap.ranges.len()
        );
    }

    // Load with lopdf to inspect font Differences arrays
    let doc = lopdf::Document::load_mem(&pdf_bytes).unwrap();
    let pages = doc.get_pages();

    println!("\n=== Font Encoding Differences ===");
    for (page_num, &page_id) in pages.iter() {
        println!("--- Page {} ---", page_num);
        let fonts = match doc.get_page_fonts(page_id) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for (font_name_bytes, font_dict) in &fonts {
            let font_name = String::from_utf8_lossy(font_name_bytes).to_string();

            // Check for Encoding
            if let Ok(encoding_obj) = font_dict.get(b"Encoding") {
                let enc_dict = match encoding_obj {
                    lopdf::Object::Dictionary(d) => Some(d.clone()),
                    lopdf::Object::Reference(r) => doc.get_dictionary(*r).ok().cloned(),
                    lopdf::Object::Name(name) => {
                        println!(
                            "  font={}: Encoding={}",
                            font_name,
                            String::from_utf8_lossy(name)
                        );
                        None
                    }
                    _ => None,
                };

                if let Some(enc_dict) = enc_dict {
                    // Check BaseEncoding
                    if let Ok(base) = enc_dict.get(b"BaseEncoding") {
                        if let lopdf::Object::Name(name) = base {
                            println!(
                                "  font={}: BaseEncoding={}",
                                font_name,
                                String::from_utf8_lossy(name)
                            );
                        }
                    }

                    // Dump Differences
                    if let Ok(diff_obj) = enc_dict.get(b"Differences") {
                        let diff_array = match diff_obj {
                            lopdf::Object::Array(arr) => Some(arr.clone()),
                            lopdf::Object::Reference(r) => {
                                if let Ok(lopdf::Object::Array(arr)) = doc.get_object(*r) {
                                    Some(arr.clone())
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };

                        if let Some(diff_array) = diff_array {
                            let mut current_code: u8 = 0;
                            let mut entries = Vec::new();
                            let mut total_glyphs = 0;

                            for item in &diff_array {
                                match item {
                                    lopdf::Object::Integer(n) => {
                                        current_code = *n as u8;
                                    }
                                    lopdf::Object::Name(name) => {
                                        let glyph = String::from_utf8_lossy(name).to_string();
                                        entries.push((current_code, glyph));
                                        current_code = current_code.wrapping_add(1);
                                        total_glyphs += 1;
                                    }
                                    _ => {}
                                }
                            }

                            println!(
                                "  font={}: Differences has {} glyph entries",
                                font_name, total_glyphs
                            );

                            // Show ligature entries specifically
                            for (code, glyph) in &entries {
                                if glyph == "fi"
                                    || glyph == "fl"
                                    || glyph == "ffi"
                                    || glyph == "ffl"
                                {
                                    println!(
                                        "    code=0x{:02X} ({:3}) glyph={:?} (LIGATURE)",
                                        code, code, glyph
                                    );
                                }
                            }

                            // Check coverage: does it have standard ASCII letters?
                            let has_a = entries.iter().any(|(_, g)| g == "a");
                            let has_space = entries.iter().any(|(_, g)| g == "space");
                            let has_period = entries.iter().any(|(_, g)| g == "period");
                            println!(
                                "    has 'a': {}, has 'space': {}, has 'period': {}",
                                has_a, has_space, has_period
                            );

                            // Show first 10 and last 5 entries
                            println!("    First 10 entries:");
                            for (code, glyph) in entries.iter().take(10) {
                                println!("      0x{:02X} ({:3}) -> {:?}", code, code, glyph);
                            }
                            if entries.len() > 15 {
                                println!("    ...");
                                println!("    Last 5 entries:");
                                for (code, glyph) in entries
                                    .iter()
                                    .rev()
                                    .take(5)
                                    .collect::<Vec<_>>()
                                    .iter()
                                    .rev()
                                {
                                    println!("      0x{:02X} ({:3}) -> {:?}", code, code, glyph);
                                }
                            }
                        }
                    }
                }
            } else {
                println!("  font={}: no Encoding", font_name);
            }
        }
    }

    // Now extract text and look for ligatures
    let items = extract_text_with_positions(&path).unwrap();

    println!("\n=== Items containing ﬁ or ﬂ (first 10) ===");
    let mut count = 0;
    for item in items.iter() {
        if item.text.contains('\u{FB01}') || item.text.contains('\u{FB02}') {
            println!(
                "  page={} font={} text={:?}",
                item.page, item.font, item.text
            );
            count += 1;
            if count >= 10 {
                break;
            }
        }
    }

    let total_lig = items
        .iter()
        .filter(|i| i.text.contains('\u{FB01}') || i.text.contains('\u{FB02}'))
        .count();
    println!("  Total items with ligatures: {}", total_lig);
}
