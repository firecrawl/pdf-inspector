//! PDF Outline (Bookmarks) extraction and heading matching.
//!
//! Parses the `/Outlines` hierarchy from the document catalog, resolving
//! target pages and destination coordinates. Outlines provide author-defined
//! semantic headings and their hierarchical levels (H1–H6).

use std::collections::{HashMap, HashSet};

use lopdf::{Document, Object, ObjectId};

use crate::types::TextLine;

/// A single outline item (bookmark) with its destination target.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// Bookmark title.
    pub title: String,
    /// Heading level (1 = H1, 2 = H2, ..., 6 = H6).
    pub level: usize,
    /// Target 1-based page number.
    pub page: u32,
    /// Target Y coordinate on the page (in PDF points), if specified.
    pub dest_y: Option<f32>,
}

/// Decode a PDF string into a Rust String (handling UTF-16BE BOM, UTF-8 BOM, and PDFDocEncoding).
pub fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        char::decode_utf16(u16s)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect()
    } else if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        String::from_utf8_lossy(&bytes[3..]).into_owned()
    } else if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_string()
    } else {
        // PDFDocEncoding / Latin-1 fallback
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Parse all outlines from a PDF document.
pub fn parse_outlines(doc: &Document) -> Vec<OutlineItem> {
    let mut results = Vec::new();

    let page_map = doc.get_pages();
    if page_map.is_empty() {
        return results;
    }

    let obj_to_page: HashMap<ObjectId, u32> =
        page_map.into_iter().map(|(num, id)| (id, num)).collect();

    // Parse named destinations if available
    let named_dests = parse_named_destinations(doc, &obj_to_page);

    // Get Root catalog
    let Ok(root_ref) = doc.trailer.get(b"Root") else {
        return results;
    };
    let root = match root_ref {
        Object::Reference(r) => match doc.get_dictionary(*r) {
            Ok(d) => d,
            Err(_) => return results,
        },
        Object::Dictionary(d) => d,
        _ => return results,
    };

    // Get Outlines dictionary
    let Ok(outlines_ref) = root.get(b"Outlines") else {
        return results;
    };
    let outlines_dict = match outlines_ref {
        Object::Reference(r) => match doc.get_dictionary(*r) {
            Ok(d) => d,
            Err(_) => return results,
        },
        Object::Dictionary(d) => d,
        _ => return results,
    };

    // Start with /First
    let Ok(first_obj) = outlines_dict.get(b"First") else {
        return results;
    };
    let Ok(first_ref) = first_obj.as_reference() else {
        return results;
    };

    let mut visited = HashSet::new();
    walk_outline_items(
        doc,
        first_ref,
        1,
        &obj_to_page,
        &named_dests,
        &mut visited,
        &mut results,
    );

    results
}

/// Parse named destinations from `/Root /Names /Dests` or `/Root /Dests`.
fn parse_named_destinations(
    doc: &Document,
    obj_to_page: &HashMap<ObjectId, u32>,
) -> HashMap<Vec<u8>, (u32, Option<f32>)> {
    let mut map = HashMap::new();

    let Ok(root_ref) = doc.trailer.get(b"Root") else {
        return map;
    };
    let root = match root_ref {
        Object::Reference(r) => match doc.get_dictionary(*r) {
            Ok(d) => d,
            Err(_) => return map,
        },
        Object::Dictionary(d) => d,
        _ => return map,
    };

    // Direct /Dests dictionary
    if let Ok(dests_obj) = root.get(b"Dests") {
        let dests_dict = match dests_obj {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        };
        if let Some(dict) = dests_dict {
            for (key, val) in dict.iter() {
                if let Some(target) = resolve_dest_array_or_dict(doc, val, obj_to_page) {
                    map.insert(key.clone(), target);
                }
            }
        }
    }

    // Name tree in /Names /Dests
    if let Ok(names_obj) = root.get(b"Names") {
        let names_dict = match names_obj {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        };
        if let Some(names) = names_dict {
            if let Ok(dests_name_obj) = names.get(b"Dests") {
                let dests_tree = match dests_name_obj {
                    Object::Reference(r) => doc.get_dictionary(*r).ok(),
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                };
                if let Some(tree) = dests_tree {
                    collect_name_tree_dests(doc, tree, obj_to_page, &mut map);
                }
            }
        }
    }

    map
}

fn collect_name_tree_dests(
    doc: &Document,
    dict: &lopdf::Dictionary,
    obj_to_page: &HashMap<ObjectId, u32>,
    map: &mut HashMap<Vec<u8>, (u32, Option<f32>)>,
) {
    if let Ok(names_arr_obj) = dict.get(b"Names") {
        if let Ok(arr) = names_arr_obj.as_array() {
            for chunk in arr.chunks(2) {
                if chunk.len() == 2 {
                    if let Ok(key) = chunk[0].as_str() {
                        if let Some(target) =
                            resolve_dest_array_or_dict(doc, &chunk[1], obj_to_page)
                        {
                            map.insert(key.to_vec(), target);
                        }
                    }
                }
            }
        }
    }
    if let Ok(kids_obj) = dict.get(b"Kids") {
        if let Ok(kids_arr) = kids_obj.as_array() {
            for kid in kids_arr {
                let kid_dict = match kid {
                    Object::Reference(r) => doc.get_dictionary(*r).ok(),
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                };
                if let Some(kd) = kid_dict {
                    collect_name_tree_dests(doc, kd, obj_to_page, map);
                }
            }
        }
    }
}

fn walk_outline_items(
    doc: &Document,
    first_ref: ObjectId,
    level: usize,
    obj_to_page: &HashMap<ObjectId, u32>,
    named_dests: &HashMap<Vec<u8>, (u32, Option<f32>)>,
    visited: &mut HashSet<ObjectId>,
    out: &mut Vec<OutlineItem>,
) {
    let mut current_id = Some(first_ref);

    while let Some(item_id) = current_id {
        if !visited.insert(item_id) || visited.len() > 20_000 {
            break;
        }

        let Ok(item_dict) = doc.get_dictionary(item_id) else {
            break;
        };

        // Parse Title
        let title = item_dict
            .get(b"Title")
            .ok()
            .and_then(|t| match t {
                Object::String(bytes, _) => Some(decode_pdf_string(bytes)),
                _ => None,
            })
            .unwrap_or_default();

        let (page, dest_y) = resolve_outline_dest(doc, item_dict, obj_to_page, named_dests);

        if let Some(page_num) = page {
            let clean_title = title.trim();
            if !clean_title.is_empty() {
                out.push(OutlineItem {
                    title: clean_title.to_string(),
                    level: level.clamp(1, 6),
                    page: page_num,
                    dest_y,
                });
            }
        }

        // Walk children (/First)
        if level < 6 {
            if let Ok(first_child) = item_dict.get(b"First") {
                if let Ok(child_id) = first_child.as_reference() {
                    walk_outline_items(
                        doc,
                        child_id,
                        level + 1,
                        obj_to_page,
                        named_dests,
                        visited,
                        out,
                    );
                }
            }
        }

        // Advance to /Next sibling
        current_id = item_dict
            .get(b"Next")
            .ok()
            .and_then(|n| n.as_reference().ok());
    }
}

fn resolve_outline_dest(
    doc: &Document,
    dict: &lopdf::Dictionary,
    obj_to_page: &HashMap<ObjectId, u32>,
    named_dests: &HashMap<Vec<u8>, (u32, Option<f32>)>,
) -> (Option<u32>, Option<f32>) {
    // 1. Direct /Dest
    if let Ok(dest_obj) = dict.get(b"Dest") {
        if let Some(res) = resolve_dest_object(doc, dest_obj, obj_to_page, named_dests) {
            return (Some(res.0), res.1);
        }
    }

    // 2. Action dictionary /A
    if let Ok(action_obj) = dict.get(b"A") {
        let action_dict = match action_obj {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        };
        if let Some(ad) = action_dict {
            if let Ok(s_obj) = ad.get(b"S") {
                if s_obj.as_name().is_ok_and(|s| s == b"GoTo") {
                    if let Ok(d_obj) = ad.get(b"D") {
                        if let Some(res) = resolve_dest_object(doc, d_obj, obj_to_page, named_dests)
                        {
                            return (Some(res.0), res.1);
                        }
                    }
                }
            }
        }
    }

    (None, None)
}

fn resolve_dest_object(
    doc: &Document,
    obj: &Object,
    obj_to_page: &HashMap<ObjectId, u32>,
    named_dests: &HashMap<Vec<u8>, (u32, Option<f32>)>,
) -> Option<(u32, Option<f32>)> {
    match obj {
        Object::Array(_) | Object::Dictionary(_) => {
            resolve_dest_array_or_dict(doc, obj, obj_to_page)
        }
        Object::String(bytes, _) => named_dests.get(bytes).copied(),
        Object::Name(name) => named_dests.get(name).copied(),
        Object::Reference(r) => {
            if let Ok(referenced) = doc.get_object(*r) {
                resolve_dest_object(doc, referenced, obj_to_page, named_dests)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn resolve_dest_array_or_dict(
    doc: &Document,
    obj: &Object,
    obj_to_page: &HashMap<ObjectId, u32>,
) -> Option<(u32, Option<f32>)> {
    match obj {
        Object::Array(arr) => parse_dest_array(arr, obj_to_page),
        Object::Dictionary(dict) => {
            if let Ok(d_obj) = dict.get(b"D") {
                if let Ok(arr) = d_obj.as_array() {
                    return parse_dest_array(arr, obj_to_page);
                }
            }
            None
        }
        Object::Reference(r) => {
            if let Ok(referenced) = doc.get_object(*r) {
                resolve_dest_array_or_dict(doc, referenced, obj_to_page)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_dest_array(
    arr: &[Object],
    obj_to_page: &HashMap<ObjectId, u32>,
) -> Option<(u32, Option<f32>)> {
    if arr.is_empty() {
        return None;
    }

    let page_num = match &arr[0] {
        Object::Reference(page_id) => obj_to_page.get(page_id).copied()?,
        Object::Integer(idx) => (*idx as u32) + 1,
        _ => return None,
    };

    let mut dest_y: Option<f32> = None;
    if arr.len() >= 2 {
        if let Ok(kind) = arr[1].as_name() {
            match kind {
                b"XYZ" if arr.len() >= 4 => {
                    dest_y = get_dest_num(&arr[3]);
                }
                b"FitH" | b"FitBH" if arr.len() >= 3 => {
                    dest_y = get_dest_num(&arr[2]);
                }
                b"FitR" if arr.len() >= 6 => {
                    dest_y = get_dest_num(&arr[5]);
                }
                _ => {}
            }
        }
    }

    Some((page_num, dest_y))
}

fn get_dest_num(obj: &Object) -> Option<f32> {
    match obj {
        Object::Real(f) => Some(*f),
        Object::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

// ─── Heading Matching ────────────────────────────────────────────────

fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Matches publisher/InDesign codes like `c01` -> `Chapter 1`, `fpref` -> `Preface`, etc.
fn matches_publisher_code(code: &str, text: &str) -> bool {
    let lower_code = code.to_lowercase();
    let lower_text = text.to_lowercase();

    if lower_code.starts_with('c') && lower_code.len() >= 2 {
        if let Ok(num) = lower_code[1..].parse::<u32>() {
            let patterns = [
                format!("chapter {}", num),
                format!("chapter {:02}", num),
                format!("{}.", num),
                format!("part {}", num),
            ];
            if patterns.iter().any(|p| lower_text.contains(p)) {
                return true;
            }
        }
    }

    match lower_code.as_str() {
        "fpref" => lower_text.contains("preface"),
        "ftoc" => lower_text.contains("contents") || lower_text.contains("table of contents"),
        "ffirs" => lower_text.contains("title") || lower_text.contains("contents"),
        "fapp" | "app" => lower_text.contains("appendix"),
        _ => false,
    }
}

/// Match a `TextLine` against page outlines to determine its heading level (1..=6).
pub fn match_outline_heading(line: &TextLine, page_outlines: &[OutlineItem]) -> Option<usize> {
    let text = line.text();
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 120 {
        return None;
    }

    let line_norm = normalize_for_match(trimmed);
    if line_norm.is_empty() {
        return None;
    }

    for outline in page_outlines {
        let outline_title = outline.title.trim();
        let outline_norm = normalize_for_match(outline_title);

        if outline_norm.is_empty() {
            continue;
        }

        // 1. Exact / normalized match
        if line_norm == outline_norm {
            return Some(outline.level);
        }

        // 2. Substring / Prefix match for substantial titles
        if outline_norm.len() >= 6
            && (line_norm.starts_with(&outline_norm) || outline_norm.starts_with(&line_norm))
        {
            return Some(outline.level);
        }

        // 3. Publisher code match (e.g. `c01` -> Chapter 1)
        if matches_publisher_code(outline_title, trimmed) {
            return Some(outline.level);
        }

        // 4. Spatial match when Y is near dest_y
        if let Some(dest_y) = outline.dest_y {
            if (line.y - dest_y).abs() < 15.0
                && (line_norm.contains(&outline_norm) || outline_norm.contains(&line_norm))
            {
                return Some(outline.level);
            }
        }
    }

    None
}
