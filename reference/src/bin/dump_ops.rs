use lopdf::Document;

fn main() {
    let path = std::env::args().nth(1).expect("Usage: dump_ops <pdf>");
    let target_page: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let search = std::env::args().nth(3);

    let doc = Document::load(&path).unwrap();
    let pages = doc.get_pages();
    let page_id = pages[&target_page];
    let content_ids = doc.get_page_contents(page_id);

    let mut found_region = search.is_none();
    let mut line_count = 0;

    for content_id in content_ids {
        if let Ok(lopdf::Object::Stream(stream)) = doc.get_object(content_id) {
            let data = stream
                .decompressed_content()
                .unwrap_or(stream.content.clone());
            let content = lopdf::content::Content::decode(&data).unwrap();
            for op in &content.operations {
                let op_str = format!("{:?}", op);
                if let Some(ref s) = search {
                    if op_str.contains(s.as_str()) {
                        found_region = true;
                        line_count = 0;
                    }
                }
                if found_region {
                    println!("{}", op_str);
                    line_count += 1;
                    if search.is_some() && line_count > 60 {
                        return;
                    }
                }
            }
        }
    }
}
