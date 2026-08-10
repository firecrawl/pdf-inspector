//! Hyperlink and AcroForm field extraction.

use crate::types::{ItemType, TextItem};
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};

use super::fonts::{resolve_array, resolve_dict};
use super::get_number;

/// Upper bound on the number of form-field nodes visited during a single
/// `extract_form_fields` pass. A crafted PDF can chain thousands of distinct
/// `/Kids` fields to blow the stack even without an outright reference cycle,
/// so we cap total traversal work in addition to detecting cycles.
const MAX_FORM_FIELD_NODES: usize = 100_000;

/// Upper bound on `/Kids` recursion depth. Real AcroForm hierarchies are only
/// a few levels deep (fields → child fields → widgets); a crafted PDF can chain
/// tens of thousands of distinct fields into a linear `/Kids` list that would
/// overflow the stack via depth-first recursion long before the node budget is
/// reached. This depth cap bounds the stack independently of total node count.
const MAX_FORM_FIELD_DEPTH: usize = 100;

/// Traversal budget for the AcroForm field walk. Bounds both the number of
/// distinct nodes visited *and* the total number of `/Fields`/`/Kids` entries
/// examined.
///
/// Counting `visited` alone is not enough: invalid entries (non-references) and
/// duplicate references never grow `visited`, so an oversized array full of them
/// would iterate to completion no matter how large. Charging every examined
/// entry against the same budget makes it a real cap on traversal work.
pub(crate) struct FieldWalkBudget {
    visited: HashSet<ObjectId>,
    examined: usize,
}

impl FieldWalkBudget {
    fn new() -> Self {
        Self {
            visited: HashSet::new(),
            examined: 0,
        }
    }

    /// True once the budget is spent; callers must stop iterating and recursing.
    fn exhausted(&self) -> bool {
        self.visited.len() >= MAX_FORM_FIELD_NODES || self.examined >= MAX_FORM_FIELD_NODES
    }
}

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
                            is_underline: false,
                            is_strikeout: false,
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

    // Borrow the array rather than cloning it: a crafted `/Fields` can be huge,
    // and cloning would pay an O(n) allocation/copy before the budget check
    // below can stop the work.
    let fields = match acroform.get(b"Fields") {
        Ok(obj) => match resolve_array(doc, obj) {
            Some(arr) => arr,
            None => return items,
        },
        Err(_) => return items,
    };
    if fields.is_empty() {
        return items;
    }
    let annotation_pages = annotation_page_map(doc, page_map);

    // Bound the walk so a crafted PDF cannot send us into unbounded recursion
    // via a `/Kids` cycle, a deep chain, or an oversized array of invalid or
    // duplicate entries.
    let mut budget = FieldWalkBudget::new();

    for field_obj in fields {
        // Stop once the budget is spent so a `/Fields` array wider than the
        // budget can't burn CPU iterating entries whose walk would no-op. Charge
        // every entry (including invalid ones) against the budget.
        if budget.exhausted() {
            break;
        }
        budget.examined += 1;
        if let Ok(field_ref) = field_obj.as_reference() {
            walk_form_fields(
                doc,
                field_ref,
                None,
                "",
                page_map,
                &annotation_pages,
                &mut items,
                &mut budget,
                0,
            );
        }
    }

    items
}

/// Map widget annotation objects back to the page whose `/Annots` array owns
/// them. Some valid widgets omit `/P`, so the page tree is the only reliable
/// ownership signal available for page-filtered extraction.
fn annotation_page_map(
    doc: &Document,
    page_map: &HashMap<ObjectId, u32>,
) -> HashMap<ObjectId, u32> {
    let mut annotation_pages = HashMap::new();
    for (&page_id, &page_num) in page_map {
        let Some(annotations) = doc
            .get_dictionary(page_id)
            .ok()
            .and_then(|page| page.get(b"Annots").ok())
            .and_then(|annotations| resolve_array(doc, annotations))
        else {
            continue;
        };
        for annotation in annotations {
            if let Ok(annotation_id) = annotation.as_reference() {
                annotation_pages.insert(annotation_id, page_num);
            }
        }
    }
    annotation_pages
}

/// Recursively walk the form field tree, extracting leaf field values.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_form_fields(
    doc: &Document,
    field_id: ObjectId,
    parent_ft: Option<&[u8]>,
    parent_name: &str,
    page_map: &HashMap<ObjectId, u32>,
    annotation_pages: &HashMap<ObjectId, u32>,
    items: &mut Vec<TextItem>,
    budget: &mut FieldWalkBudget,
    depth: usize,
) {
    // Guard against `/Kids` cycles and pathologically large field trees.
    // Exceeding the depth cap means the chain is too deep to be a legitimate
    // form (and would overflow the stack); an exhausted budget means the tree is
    // too large. Both checks run *before* inserting so the visited set can never
    // grow past the budget.
    if depth > MAX_FORM_FIELD_DEPTH || budget.exhausted() {
        return;
    }
    // Revisiting an object ID means we hit a `/Kids` cycle.
    if !budget.visited.insert(field_id) {
        return;
    }

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
        // Iterate the borrowed array directly — cloning a crafted, oversized
        // `/Kids` would allocate and copy every entry before the budget check
        // below could stop the work.
        if let Some(kids) = resolve_array(doc, kids_obj) {
            for kid in kids {
                // Stop once the budget is spent so a `/Kids` array wider than the
                // budget can't burn CPU iterating entries whose walk would no-op.
                // Charge every entry (including invalid/duplicate ones) against
                // the budget so this is a true traversal-work cap.
                if budget.exhausted() {
                    break;
                }
                budget.examined += 1;
                if let Ok(kid_ref) = kid.as_reference() {
                    walk_form_fields(
                        doc,
                        kid_ref,
                        ft,
                        &full_name,
                        page_map,
                        annotation_pages,
                        items,
                        budget,
                        depth + 1,
                    );
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
        .or_else(|| annotation_pages.get(&field_id).copied())
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
        is_underline: false,
        is_strikeout: false,
        item_type: ItemType::FormField,
        mcid: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object};

    #[test]
    fn widget_without_page_reference_uses_owning_page_annotation() {
        let mut doc = Document::new();
        let widget_id = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Widget",
            "FT" => "Tx",
            "T" => Object::string_literal("customer"),
            "V" => Object::string_literal("Alice"),
            "Rect" => vec![10.into(), 20.into(), 110.into(), 40.into()],
        });
        let page_one_id = doc.add_object(dictionary! {
            "Type" => "Page",
        });
        let page_two_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Annots" => vec![Object::Reference(widget_id)],
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(widget_id)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::from([(page_one_id, 1), (page_two_id, 2)]);
        let items = extract_form_fields(&doc, &page_map);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].page, 2);
        assert_eq!(items[0].text, "customer: Alice");
    }

    #[test]
    fn kids_self_cycle_does_not_overflow_stack() {
        // A crafted AcroForm field that lists itself in `/Kids` must not send
        // the traversal into unbounded recursion.
        let mut doc = Document::new();
        let field_id = doc.new_object_id();
        doc.set_object(
            field_id,
            dictionary! {
                "FT" => "Tx",
                "T" => Object::string_literal("loop"),
                "Kids" => vec![Object::Reference(field_id)],
            },
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(field_id)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        // Completes (rather than overflowing the stack) and yields no items.
        let items = extract_form_fields(&doc, &page_map);
        assert!(items.is_empty());
    }

    #[test]
    fn kids_mutual_cycle_terminates() {
        // Two fields that reference each other via `/Kids` form a cycle that
        // must also terminate.
        let mut doc = Document::new();
        let field_a = doc.new_object_id();
        let field_b = doc.new_object_id();
        doc.set_object(
            field_a,
            dictionary! {
                "T" => Object::string_literal("a"),
                "Kids" => vec![Object::Reference(field_b)],
            },
        );
        doc.set_object(
            field_b,
            dictionary! {
                "T" => Object::string_literal("b"),
                "Kids" => vec![Object::Reference(field_a)],
            },
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(field_a)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        let items = extract_form_fields(&doc, &page_map);
        assert!(items.is_empty());
    }

    #[test]
    fn deep_acyclic_kids_chain_does_not_overflow_stack() {
        // A long chain of *distinct* fields (no cycle) must also terminate:
        // the visited set alone would still recurse to the chain length, so
        // the depth cap is what prevents a stack overflow here.
        let mut doc = Document::new();
        let n = MAX_FORM_FIELD_DEPTH * 500;
        let ids: Vec<ObjectId> = (0..=n).map(|_| doc.new_object_id()).collect();
        for i in 0..n {
            doc.set_object(
                ids[i],
                dictionary! {
                    "FT" => "Tx",
                    "Kids" => vec![Object::Reference(ids[i + 1])],
                },
            );
        }
        // Leaf carries a value; it sits far below the depth cap so it is never
        // reached, proving traversal stops early rather than crashing.
        doc.set_object(
            ids[n],
            dictionary! {
                "FT" => "Tx",
                "T" => Object::string_literal("leaf"),
                "V" => Object::string_literal("x"),
                "Rect" => vec![10.into(), 20.into(), 110.into(), 40.into()],
            },
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(ids[0])],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        let items = extract_form_fields(&doc, &page_map);
        assert!(items.is_empty());
    }

    #[test]
    fn wide_tree_traversal_stops_at_node_budget() {
        // A single field with a `/Kids` array wider than the node budget must
        // stop traversal at the cap rather than growing `visited` (and the work)
        // without bound. Each processed leaf emits one item, so the item count
        // is bounded by the budget and reaches right up to it (a couple of
        // slots go to the root and the boundary node charged against the cap).
        let mut doc = Document::new();
        let fanout = MAX_FORM_FIELD_NODES + 50;
        let leaf_ids: Vec<ObjectId> = (0..fanout).map(|_| doc.new_object_id()).collect();
        for &leaf in &leaf_ids {
            doc.set_object(
                leaf,
                dictionary! {
                    "FT" => "Tx",
                    "V" => Object::string_literal("v"),
                    "Rect" => vec![10.into(), 20.into(), 110.into(), 40.into()],
                },
            );
        }
        let kids: Vec<Object> = leaf_ids.iter().map(|&id| Object::Reference(id)).collect();
        let root_id = doc.add_object(dictionary! {
            "T" => Object::string_literal("root"),
            "Kids" => kids,
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(root_id)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        let items = extract_form_fields(&doc, &page_map);
        // Extraction stops at the budget: bounded above by the cap, and it gets
        // right up to it (allowing a small delta for the root/boundary nodes
        // charged against the budget).
        assert!(items.len() <= MAX_FORM_FIELD_NODES);
        assert!(items.len() >= MAX_FORM_FIELD_NODES - 3);
    }

    #[test]
    fn wide_top_level_fields_stop_at_node_budget() {
        // A top-level `/Fields` array wider than the budget must also stop at
        // the cap: the item count is bounded by the budget and reaches right up
        // to it.
        let mut doc = Document::new();
        let fanout = MAX_FORM_FIELD_NODES + 50;
        let leaf_ids: Vec<ObjectId> = (0..fanout).map(|_| doc.new_object_id()).collect();
        for &leaf in &leaf_ids {
            doc.set_object(
                leaf,
                dictionary! {
                    "FT" => "Tx",
                    "V" => Object::string_literal("v"),
                    "Rect" => vec![10.into(), 20.into(), 110.into(), 40.into()],
                },
            );
        }
        let fields: Vec<Object> = leaf_ids.iter().map(|&id| Object::Reference(id)).collect();
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => fields,
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        let items = extract_form_fields(&doc, &page_map);
        assert!(items.len() <= MAX_FORM_FIELD_NODES);
        assert!(items.len() >= MAX_FORM_FIELD_NODES - 3);
    }

    #[test]
    fn duplicate_and_invalid_kids_entries_stop_at_budget() {
        // Duplicate references and non-reference junk never grow `visited`, so
        // without charging examined entries against the budget an oversized
        // array of them would iterate to completion. The walk must still
        // terminate and extract the single real leaf exactly once.
        let mut doc = Document::new();
        let leaf_id = doc.new_object_id();
        doc.set_object(
            leaf_id,
            dictionary! {
                "FT" => "Tx",
                "V" => Object::string_literal("v"),
                "Rect" => vec![10.into(), 20.into(), 110.into(), 40.into()],
            },
        );
        // A `/Kids` array far wider than the budget: half duplicate references
        // to the same leaf, half invalid (null) entries.
        let mut kids: Vec<Object> = Vec::new();
        for i in 0..(MAX_FORM_FIELD_NODES * 2) {
            if i % 2 == 0 {
                kids.push(Object::Reference(leaf_id));
            } else {
                kids.push(Object::Null);
            }
        }
        let root_id = doc.add_object(dictionary! {
            "T" => Object::string_literal("root"),
            "Kids" => kids,
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "AcroForm" => dictionary! {
                "Fields" => vec![Object::Reference(root_id)],
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_map = HashMap::new();
        let items = extract_form_fields(&doc, &page_map);
        assert_eq!(items.len(), 1);
    }
}
