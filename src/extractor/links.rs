//! Hyperlink and AcroForm field extraction.

use crate::types::{ItemType, TextItem};
use lopdf::{Document, Object, ObjectId};
use std::collections::HashMap;

use super::fonts::{resolve_array, resolve_dict};
use super::get_number;

pub fn extract_page_links(doc: &Document, page_id: ObjectId, page_num: u32) -> Vec<TextItem> {
    let mut links = Vec::new();

    // Try to get the page dictionary
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        // Get Annots array
        let annots = if let Ok(annots_ref) = page_dict.get(b"Annots") {
            if let Ok(obj_ref) = annots_ref.as_reference() {
                doc.get_object(obj_ref)
                    .ok()
                    .and_then(|o| o.as_array().ok().cloned())
            } else {
                annots_ref.as_array().ok().cloned()
            }
        } else {
            None
        };

        if let Some(annots) = annots {
            for annot_ref in annots {
                // Get annotation dictionary
                let annot_dict = if let Ok(obj_ref) = annot_ref.as_reference() {
                    doc.get_dictionary(obj_ref).ok()
                } else {
                    annot_ref.as_dict().ok()
                };

                if let Some(annot_dict) = annot_dict {
                    // Check if this is a Link annotation
                    if let Ok(subtype) = annot_dict.get(b"Subtype") {
                        if let Ok(subtype_name) = subtype.as_name() {
                            if subtype_name != b"Link" {
                                continue;
                            }
                        }
                    }

                    // Get the Rect (position)
                    let rect = if let Ok(rect_obj) = annot_dict.get(b"Rect") {
                        if let Ok(rect_array) = rect_obj.as_array() {
                            if rect_array.len() >= 4 {
                                let x1 = get_number(&rect_array[0]).unwrap_or(0.0);
                                let y1 = get_number(&rect_array[1]).unwrap_or(0.0);
                                let x2 = get_number(&rect_array[2]).unwrap_or(0.0);
                                let y2 = get_number(&rect_array[3]).unwrap_or(0.0);
                                Some((x1, y1, x2 - x1, y2 - y1))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Get the action (A dictionary) or Dest
                    let uri = extract_link_uri(doc, annot_dict);

                    if let (Some((x, y, width, height)), Some(url)) = (rect, uri) {
                        links.push(TextItem {
                            text: url.clone(),
                            x,
                            y,
                            width,
                            height,
                            font: String::new(),
                            font_size: 0.0,
                            page: page_num,
                            is_bold: false,
                            is_italic: false,
                            item_type: ItemType::Link(url),
                            mcid: None,
                        });
                    }
                }
            }
        }
    }

    links
}

/// Extract URI from a link annotation
pub(crate) fn extract_link_uri(doc: &Document, annot_dict: &lopdf::Dictionary) -> Option<String> {
    // Try to get the A (Action) dictionary
    if let Ok(action_ref) = annot_dict.get(b"A") {
        let action_dict = if let Ok(obj_ref) = action_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            action_ref.as_dict().ok()
        };

        if let Some(action_dict) = action_dict {
            // Check for URI action
            if let Ok(uri_obj) = action_dict.get(b"URI") {
                if let Ok(uri_str) = uri_obj.as_str() {
                    return Some(String::from_utf8_lossy(uri_str).to_string());
                }
            }
        }
    }

    // Try Dest (named destination) - less common for external links
    // We'll skip this for now as it requires looking up named destinations

    None
}

/// Extract form field values from AcroForm dictionary.
/// Returns TextItems positioned at each field's Rect so they flow into the markdown pipeline.
pub(crate) fn extract_form_fields(
    doc: &Document,
    page_map: &HashMap<ObjectId, u32>,
) -> Vec<TextItem> {
    let mut items = Vec::new();

    // Navigate: trailer -> /Root -> /AcroForm -> /Fields
    let root = match doc.trailer.get(b"Root") {
        Ok(root_ref) => match root_ref.as_reference() {
            Ok(r) => match doc.get_dictionary(r) {
                Ok(d) => d,
                Err(_) => return items,
            },
            Err(_) => return items,
        },
        Err(_) => return items,
    };

    let acroform = match root.get(b"AcroForm") {
        Ok(obj) => match resolve_dict(doc, obj) {
            Some(d) => d,
            None => return items,
        },
        Err(_) => return items,
    };

    let fields = match acroform.get(b"Fields") {
        Ok(obj) => match resolve_array(doc, obj) {
            Some(arr) => arr.clone(),
            None => return items,
        },
        Err(_) => return items,
    };

    for field_obj in &fields {
        if let Ok(field_ref) = field_obj.as_reference() {
            walk_form_fields(doc, field_ref, None, "", page_map, &mut items);
        }
    }

    items
}

/// Recursively walk the form field tree, extracting leaf field values.
pub(crate) fn walk_form_fields(
    doc: &Document,
    field_id: ObjectId,
    parent_ft: Option<&[u8]>,
    parent_name: &str,
    page_map: &HashMap<ObjectId, u32>,
    items: &mut Vec<TextItem>,
) {
    let field_dict = match doc.get_dictionary(field_id) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Build fully qualified field name
    let local_name = field_dict
        .get(b"T")
        .ok()
        .and_then(|o| o.as_str().ok())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_default();

    let full_name = if parent_name.is_empty() {
        local_name.clone()
    } else if local_name.is_empty() {
        parent_name.to_string()
    } else {
        format!("{}.{}", parent_name, local_name)
    };

    // Determine field type (may be inherited from parent)
    let ft = field_dict
        .get(b"FT")
        .ok()
        .and_then(|o| o.as_name().ok())
        .or(parent_ft);

    // Check for /Kids — if present, recurse into children
    if let Ok(kids_obj) = field_dict.get(b"Kids") {
        if let Some(kids) = resolve_array(doc, kids_obj) {
            let kids = kids.clone();
            for kid in &kids {
                if let Ok(kid_ref) = kid.as_reference() {
                    walk_form_fields(doc, kid_ref, ft, &full_name, page_map, items);
                }
            }
            return;
        }
    }

    // Leaf field — extract value
    let ft = match ft {
        Some(ft) => ft,
        None => return,
    };

    // Skip signature fields
    if ft == b"Sig" {
        return;
    }

    // Get field value
    let value = match field_dict.get(b"V") {
        Ok(v) => v,
        Err(_) => return,
    };

    let value_str = match ft {
        b"Tx" | b"Ch" => {
            // Text or Choice field — value is a string or array of strings
            match value {
                Object::String(s, _) => {
                    let s = String::from_utf8_lossy(s).to_string();
                    if s.is_empty() {
                        return;
                    }
                    s
                }
                Object::Array(arr) => {
                    let parts: Vec<String> = arr
                        .iter()
                        .filter_map(|o| {
                            if let Object::String(s, _) = o {
                                Some(String::from_utf8_lossy(s).to_string())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if parts.is_empty() {
                        return;
                    }
                    parts.join(", ")
                }
                _ => return,
            }
        }
        b"Btn" => {
            // Checkbox/radio — value is a name
            match value.as_name() {
                Ok(name) if name == b"Off" => return,
                Ok(name) => {
                    let name_str = String::from_utf8_lossy(name).to_string();
                    if name_str == "Yes" || name_str == "1" {
                        "Yes".to_string()
                    } else {
                        name_str
                    }
                }
                Err(_) => return,
            }
        }
        _ => return,
    };

    // Get Rect for positioning
    let (x, y, width, height) = match field_dict.get(b"Rect") {
        Ok(rect_obj) => match rect_obj.as_array() {
            Ok(rect_array) if rect_array.len() >= 4 => {
                let x1 = get_number(&rect_array[0]).unwrap_or(0.0);
                let y1 = get_number(&rect_array[1]).unwrap_or(0.0);
                let x2 = get_number(&rect_array[2]).unwrap_or(0.0);
                let y2 = get_number(&rect_array[3]).unwrap_or(0.0);
                (x1, y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs())
            }
            _ => (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => (0.0, 0.0, 0.0, 0.0),
    };

    // Determine page number from /P reference
    let page_num = field_dict
        .get(b"P")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .and_then(|p| page_map.get(&p).copied())
        .unwrap_or(1);

    let text = if full_name.is_empty() {
        value_str
    } else {
        format!("{}: {}", full_name, value_str)
    };

    items.push(TextItem {
        text,
        x,
        y,
        width,
        height,
        font: String::new(),
        font_size: 0.0,
        page: page_num,
        is_bold: false,
        is_italic: false,
        item_type: ItemType::FormField,
        mcid: None,
    });
}
