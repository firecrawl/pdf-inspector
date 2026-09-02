//! PDF outline (bookmark) extraction.
//!
//! Walks the document catalog's `/Outlines` tree and flattens it into a
//! PyMuPDF-style simple table of contents: one entry per bookmark with a
//! 1-based nesting level, decoded title, and resolved 1-indexed target page.
//!
//! The walk is bounded the same way as the AcroForm field walk in
//! [`super::links`]: a visited set catches `/Next`/`/First` reference cycles,
//! a depth cap bounds child recursion, and a node budget caps total traversal
//! work so a crafted PDF cannot burn unbounded CPU on invalid or duplicate
//! entries.

use crate::text_utils::decode_pdfdoc_text_string;
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};

use super::fonts::{resolve_array, resolve_dict};

/// Upper bound on the number of outline nodes visited during a single
/// `extract_outline_from_doc` pass. Mirrors the AcroForm field walk budget:
/// a crafted PDF can chain enormous `/Next` lists, so total traversal work is
/// capped in addition to detecting cycles.
const MAX_OUTLINE_NODES: usize = 100_000;

/// Upper bound on `/First` (child) recursion depth. Real outlines are only a
/// handful of levels deep; a crafted PDF can nest thousands of levels to
/// overflow the stack long before the node budget is reached. Sibling
/// traversal is iterative, so only child nesting consumes stack.
const MAX_OUTLINE_DEPTH: usize = 32;

/// Traversal budget for the outline walk (FieldWalkBudget-style, see
/// [`super::links::FieldWalkBudget`]). Bounds both the number of distinct
/// nodes visited *and* the total number of node references examined, so
/// duplicate or invalid entries cannot iterate past the cap either.
struct OutlineWalkBudget {
    visited: HashSet<ObjectId>,
    examined: usize,
}

impl OutlineWalkBudget {
    fn new() -> Self {
        Self {
            visited: HashSet::new(),
            examined: 0,
        }
    }

    /// True once the budget is spent; callers must stop iterating and recursing.
    fn exhausted(&self) -> bool {
        self.visited.len() >= MAX_OUTLINE_NODES || self.examined >= MAX_OUTLINE_NODES
    }
}

/// One flattened outline (bookmark) entry.
///
/// Matches the shape of PyMuPDF's `Document.get_toc(simple=True)` rows
/// (`[level, title, page]`) plus the destination kind when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    /// 1-based nesting depth (top-level bookmarks are level 1).
    pub level: u32,
    /// Bookmark title, decoded from BOM-marked UTF-16 or PDFDocEncoding.
    pub title: String,
    /// 1-indexed target page (matches [`crate::TextItem::page`]), or `None`
    /// when the destination is missing or cannot be resolved.
    pub page: Option<u32>,
    /// Destination kind when known: an explicit destination's fit type
    /// ("XYZ", "Fit", "FitH", …) or "named" for named destinations.
    pub dest_kind: Option<String>,
}

/// Walk `trailer /Root -> /Outlines` and flatten the bookmark tree.
///
/// `page_map` maps page `ObjectId`s to 1-indexed page numbers (the same map
/// used by [`super::links::extract_form_fields`]). Returns an empty list when
/// the document has no `/Outlines`; a missing or malformed outline is never
/// an error.
pub(crate) fn extract_outline_from_doc(
    doc: &Document,
    page_map: &HashMap<ObjectId, u32>,
) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();

    let root = match doc.trailer.get(b"Root") {
        Ok(root_ref) => match root_ref.as_reference() {
            Ok(r) => match doc.get_dictionary(r) {
                Ok(d) => d,
                Err(_) => return entries,
            },
            Err(_) => return entries,
        },
        Err(_) => return entries,
    };

    let outlines = match root.get(b"Outlines") {
        Ok(obj) => match resolve_dict(doc, obj) {
            Some(d) => d,
            None => return entries,
        },
        Err(_) => return entries,
    };

    let first = match outlines.get(b"First") {
        Ok(obj) => match obj.as_reference() {
            Ok(r) => r,
            Err(_) => return entries,
        },
        Err(_) => return entries,
    };

    // Named destinations are resolved lazily: most outlines use explicit
    // destinations, so the name table is only built when first needed.
    let mut named_dests = NamedDests::new();
    let mut budget = OutlineWalkBudget::new();
    walk_outline_level(
        doc,
        first,
        1,
        page_map,
        &mut named_dests,
        &mut entries,
        &mut budget,
        0,
    );
    entries
}

/// Walk one sibling chain (`/Next` links) iteratively, recursing into
/// `/First` children. Sibling iteration is a loop so only child nesting
/// consumes stack, which the depth cap bounds.
#[allow(clippy::too_many_arguments)]
fn walk_outline_level(
    doc: &Document,
    first_id: ObjectId,
    level: u32,
    page_map: &HashMap<ObjectId, u32>,
    named_dests: &mut NamedDests,
    entries: &mut Vec<OutlineEntry>,
    budget: &mut OutlineWalkBudget,
    depth: usize,
) {
    if depth > MAX_OUTLINE_DEPTH {
        return;
    }

    let mut current = Some(first_id);
    while let Some(node_id) = current {
        if budget.exhausted() {
            return;
        }
        budget.examined += 1;
        // Revisiting an object ID means a `/Next` or `/First` cycle.
        if !budget.visited.insert(node_id) {
            return;
        }

        let node = match doc.get_dictionary(node_id) {
            Ok(d) => d,
            Err(_) => return,
        };

        let title = node
            .get(b"Title")
            .ok()
            .and_then(|obj| resolve_string_bytes(doc, obj))
            .map(decode_pdfdoc_text_string)
            .unwrap_or_default();

        let (page, dest_kind) = resolve_outline_destination(doc, node, page_map, named_dests);

        entries.push(OutlineEntry {
            level,
            title,
            page,
            dest_kind,
        });

        if let Some(child_id) = node
            .get(b"First")
            .ok()
            .and_then(|obj| obj.as_reference().ok())
        {
            walk_outline_level(
                doc,
                child_id,
                level + 1,
                page_map,
                named_dests,
                entries,
                budget,
                depth + 1,
            );
        }

        current = node
            .get(b"Next")
            .ok()
            .and_then(|obj| obj.as_reference().ok());
    }
}

/// Resolve a possibly-referenced PDF string object to its raw bytes.
fn resolve_string_bytes<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a [u8]> {
    match obj {
        Object::String(bytes, _) => Some(bytes),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::String(bytes, _)) => Some(bytes),
            _ => None,
        },
        _ => None,
    }
}

/// Resolve a possibly-referenced PDF name object to its raw bytes.
fn resolve_name_bytes<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a [u8]> {
    match obj {
        Object::Name(name) => Some(name),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Name(name)) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// Resolve an outline node's destination to a 1-indexed page and dest kind.
///
/// Tries `/Dest` first (explicit array or named destination), then a `/A`
/// GoTo action's `/D`. Returns `(None, None)` when the node has no
/// resolvable in-document destination.
fn resolve_outline_destination(
    doc: &Document,
    node: &lopdf::Dictionary,
    page_map: &HashMap<ObjectId, u32>,
    named_dests: &mut NamedDests,
) -> (Option<u32>, Option<String>) {
    if let Ok(dest) = node.get(b"Dest") {
        return resolve_dest_object(doc, dest, page_map, named_dests);
    }

    if let Some(action) = node.get(b"A").ok().and_then(|obj| resolve_dict(doc, obj)) {
        // `/S` may itself be an indirect reference to the action-type name.
        let is_goto = action
            .get(b"S")
            .ok()
            .and_then(|s| resolve_name_bytes(doc, s))
            .is_some_and(|name| name == b"GoTo");
        if is_goto {
            if let Ok(dest) = action.get(b"D") {
                return resolve_dest_object(doc, dest, page_map, named_dests);
            }
        }
    }

    (None, None)
}

/// Resolve a destination object (explicit array, name, or string) to a
/// 1-indexed page and dest kind.
fn resolve_dest_object(
    doc: &Document,
    dest: &Object,
    page_map: &HashMap<ObjectId, u32>,
    named_dests: &mut NamedDests,
) -> (Option<u32>, Option<String>) {
    let dest = match dest {
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(obj) => obj,
            Err(_) => return (None, None),
        },
        other => other,
    };

    match dest {
        Object::Array(arr) => (
            dest_array_page(arr, page_map),
            dest_array_kind(doc, arr).map(str::to_string),
        ),
        Object::Name(name) => {
            let page = named_dests
                .lookup(doc, name)
                .and_then(|arr| dest_array_page(&arr, page_map));
            (page, Some("named".to_string()))
        }
        Object::String(bytes, _) => {
            let page = named_dests
                .lookup(doc, bytes)
                .and_then(|arr| dest_array_page(&arr, page_map));
            (page, Some("named".to_string()))
        }
        _ => (None, None),
    }
}

/// First element of an explicit destination array resolved to a 1-indexed
/// page number. The element is normally a page reference; an integer is a
/// 0-based page index (seen in some generators).
fn dest_array_page(arr: &[Object], page_map: &HashMap<ObjectId, u32>) -> Option<u32> {
    match arr.first()? {
        Object::Reference(r) => page_map.get(r).copied(),
        Object::Integer(i) => {
            let index = u32::try_from(*i).ok()?;
            // Checked: a malformed u32::MAX index must yield None, not
            // overflow.
            let page = index.checked_add(1)?;
            (page as usize <= page_map.len()).then_some(page)
        }
        _ => None,
    }
}

/// Fit-type name from an explicit destination array's second element
/// ("XYZ", "Fit", "FitH", …), which may be an indirect reference.
fn dest_array_kind<'a>(doc: &'a Document, arr: &'a [Object]) -> Option<&'a str> {
    let name = resolve_name_bytes(doc, arr.get(1)?)?;
    std::str::from_utf8(name).ok()
}

/// Lazily-built map of named destinations.
///
/// Built on first lookup from the catalog's PDF 1.1 `/Dests` dictionary and
/// the `/Names` `/Dests` name tree, walking the tree with the same
/// cycle/depth/node bounds as the outline walk. Destination values that are
/// dictionaries contribute their `/D` array.
struct NamedDests {
    map: Option<HashMap<Vec<u8>, Vec<Object>>>,
}

impl NamedDests {
    fn new() -> Self {
        Self { map: None }
    }

    /// Resolve `name` to its explicit destination array, building the name
    /// table on first use.
    fn lookup(&mut self, doc: &Document, name: &[u8]) -> Option<Vec<Object>> {
        if self.map.is_none() {
            self.map = Some(build_named_dests(doc));
        }
        self.map.as_ref().and_then(|m| m.get(name).cloned())
    }
}

fn build_named_dests(doc: &Document) -> HashMap<Vec<u8>, Vec<Object>> {
    let mut map = HashMap::new();

    let Some(root) = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|obj| resolve_dict(doc, obj))
    else {
        return map;
    };

    // PDF 1.1 style: /Root /Dests is a dictionary of name -> destination.
    // A crafted dictionary wider than the node budget is truncated.
    if let Some(dests) = root.get(b"Dests").ok().and_then(|o| resolve_dict(doc, o)) {
        for (name, value) in dests.iter().take(MAX_OUTLINE_NODES) {
            if let Some(arr) = dest_value_array(doc, value) {
                map.insert(name.to_vec(), arr);
            }
        }
    }

    // PDF 1.2+ style: /Root /Names /Dests is a name tree.
    if let Some(tree_root) = root
        .get(b"Names")
        .ok()
        .and_then(|o| resolve_dict(doc, o))
        .and_then(|names| names.get(b"Dests").ok())
        .and_then(|o| resolve_dict(doc, o))
    {
        let mut budget = OutlineWalkBudget::new();
        collect_name_tree_dests(doc, tree_root, &mut map, &mut budget, 0);
    }

    map
}

/// Recursively collect `name -> destination array` pairs from a name tree
/// node, bounded by the shared cycle set, depth cap, and node budget.
fn collect_name_tree_dests(
    doc: &Document,
    node: &lopdf::Dictionary,
    map: &mut HashMap<Vec<u8>, Vec<Object>>,
    budget: &mut OutlineWalkBudget,
    depth: usize,
) {
    if depth > MAX_OUTLINE_DEPTH || budget.exhausted() {
        return;
    }

    if let Some(names) = node.get(b"Names").ok().and_then(|o| resolve_array(doc, o)) {
        for pair in names.chunks(2) {
            if budget.exhausted() {
                return;
            }
            budget.examined += 1;
            let [key, value] = pair else { continue };
            let Some(key_bytes) = resolve_string_bytes(doc, key) else {
                continue;
            };
            if let Some(arr) = dest_value_array(doc, value) {
                map.insert(key_bytes.to_vec(), arr);
            }
        }
    }

    if let Some(kids) = node.get(b"Kids").ok().and_then(|o| resolve_array(doc, o)) {
        for kid in kids {
            if budget.exhausted() {
                return;
            }
            budget.examined += 1;
            let Ok(kid_ref) = kid.as_reference() else {
                continue;
            };
            // Revisiting a node means a /Kids cycle.
            if !budget.visited.insert(kid_ref) {
                continue;
            }
            if let Ok(kid_dict) = doc.get_dictionary(kid_ref) {
                collect_name_tree_dests(doc, kid_dict, map, budget, depth + 1);
            }
        }
    }
}

/// Normalize a named-destination value to its explicit destination array.
/// The value is either the array itself or a dictionary whose `/D` holds it.
fn dest_value_array(doc: &Document, value: &Object) -> Option<Vec<Object>> {
    let value = match value {
        Object::Reference(r) => doc.get_object(*r).ok()?,
        other => other,
    };
    match value {
        Object::Array(arr) => Some(arr.clone()),
        Object::Dictionary(dict) => {
            let d = dict.get(b"D").ok()?;
            resolve_array(doc, d).cloned()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object};

    /// Build a doc with `page_count` pages, returning (doc, page ids).
    /// The pages are standalone objects; tests build the page map directly,
    /// matching how `extract_positioned_text_impl` derives it from
    /// `doc.get_pages()`.
    fn doc_with_pages(page_count: usize) -> (Document, Vec<ObjectId>) {
        let mut doc = Document::new();
        let page_ids: Vec<ObjectId> = (0..page_count)
            .map(|_| doc.add_object(dictionary! { "Type" => "Page" }))
            .collect();
        (doc, page_ids)
    }

    fn page_map(page_ids: &[ObjectId]) -> HashMap<ObjectId, u32> {
        page_ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32 + 1))
            .collect()
    }

    fn set_catalog(doc: &mut Document, outlines_id: ObjectId) {
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Outlines" => Object::Reference(outlines_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
    }

    #[test]
    fn no_outlines_returns_empty() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert!(entries.is_empty());
    }

    #[test]
    fn flat_outline_with_explicit_dests() {
        let (mut doc, page_ids) = doc_with_pages(3);
        let second = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Chapter 2"),
            "Dest" => vec![
                Object::Reference(page_ids[2]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let first = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Chapter 1"),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"XYZ".to_vec()),
                69.into(),
                720.into(),
                0.into(),
            ],
            "Next" => Object::Reference(second),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(first),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(
            entries,
            vec![
                OutlineEntry {
                    level: 1,
                    title: "Chapter 1".to_string(),
                    page: Some(1),
                    dest_kind: Some("XYZ".to_string()),
                },
                // Second entry is a sibling, not a child, so it stays level 1.
                OutlineEntry {
                    level: 1,
                    title: "Chapter 2".to_string(),
                    page: Some(3),
                    dest_kind: Some("Fit".to_string()),
                },
            ]
        );
    }

    #[test]
    fn nested_children_get_deeper_levels() {
        let (mut doc, page_ids) = doc_with_pages(2);
        let grandchild = doc.add_object(dictionary! {
            "Title" => Object::string_literal("1.1.1"),
            "Dest" => vec![
                Object::Reference(page_ids[1]),
                Object::Name(b"FitH".to_vec()),
                796.into(),
            ],
        });
        let child = doc.add_object(dictionary! {
            "Title" => Object::string_literal("1.1"),
            "First" => Object::Reference(grandchild),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let top = doc.add_object(dictionary! {
            "Title" => Object::string_literal("1"),
            "First" => Object::Reference(child),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(top),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        let simple: Vec<(u32, &str, Option<u32>)> = entries
            .iter()
            .map(|e| (e.level, e.title.as_str(), e.page))
            .collect();
        assert_eq!(
            simple,
            vec![
                (1, "1", Some(1)),
                (2, "1.1", Some(1)),
                (3, "1.1.1", Some(2)),
            ]
        );
        assert_eq!(entries[2].dest_kind.as_deref(), Some("FitH"));
    }

    #[test]
    fn goto_action_dest_resolves() {
        let (mut doc, page_ids) = doc_with_pages(2);
        let action = doc.add_object(dictionary! {
            "S" => "GoTo",
            "D" => vec![
                Object::Reference(page_ids[1]),
                Object::Name(b"FitH".to_vec()),
                796.into(),
            ],
        });
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Via action"),
            "A" => Object::Reference(action),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(2));
        assert_eq!(entries[0].dest_kind.as_deref(), Some("FitH"));
    }

    #[test]
    fn goto_action_with_indirect_s_name_resolves() {
        // `/S` stored as an indirect reference to the /GoTo name must still
        // be recognized as a GoTo action.
        let (mut doc, page_ids) = doc_with_pages(2);
        let s_name = doc.add_object(Object::Name(b"GoTo".to_vec()));
        let action = doc.add_object(dictionary! {
            "S" => Object::Reference(s_name),
            "D" => vec![
                Object::Reference(page_ids[1]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Indirect S"),
            "A" => Object::Reference(action),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(2));
        assert_eq!(entries[0].dest_kind.as_deref(), Some("Fit"));
    }

    #[test]
    fn indirect_dest_fit_type_name_resolves() {
        // The fit-type name in an explicit destination array may itself be
        // an indirect reference.
        let (mut doc, page_ids) = doc_with_pages(1);
        let kind_name = doc.add_object(Object::Name(b"FitH".to_vec()));
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Indirect kind"),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Reference(kind_name),
                796.into(),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(1));
        assert_eq!(entries[0].dest_kind.as_deref(), Some("FitH"));
    }

    #[test]
    fn max_u32_integer_page_index_yields_none_without_overflow() {
        // A malformed 0-based page index of u32::MAX must return None, not
        // overflow in `index + 1`.
        let (mut doc, page_ids) = doc_with_pages(1);
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Overflow"),
            "Dest" => vec![
                Object::Integer(u32::MAX as i64),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, None);
    }

    #[test]
    fn non_goto_action_yields_no_page() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("External link"),
            "A" => dictionary! {
                "S" => "URI",
                "URI" => Object::string_literal("https://example.com"),
            },
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, None);
        assert_eq!(entries[0].dest_kind, None);
    }

    #[test]
    fn named_dest_via_names_tree() {
        let (mut doc, page_ids) = doc_with_pages(2);
        let leaf = doc.add_object(dictionary! {
            "Names" => vec![
                Object::string_literal("section.2"),
                Object::Array(vec![
                    Object::Reference(page_ids[1]),
                    Object::Name(b"XYZ".to_vec()),
                    Object::Null,
                    Object::Null,
                    Object::Null,
                ]),
            ],
        });
        let tree_root = doc.add_object(dictionary! {
            "Kids" => vec![Object::Reference(leaf)],
        });
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Section 2"),
            "Dest" => Object::string_literal("section.2"),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Outlines" => Object::Reference(outlines_id),
            "Names" => dictionary! {
                "Dests" => Object::Reference(tree_root),
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(2));
        assert_eq!(entries[0].dest_kind.as_deref(), Some("named"));
    }

    #[test]
    fn named_dest_via_dests_dictionary() {
        // PDF 1.1 style: catalog /Dests dictionary, value is a dictionary
        // whose /D holds the explicit destination.
        let (mut doc, page_ids) = doc_with_pages(2);
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Appendix"),
            "Dest" => Object::Name(b"appendix".to_vec()),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        let dests_id = doc.add_object(dictionary! {
            "appendix" => dictionary! {
                "D" => vec![
                    Object::Reference(page_ids[1]),
                    Object::Name(b"Fit".to_vec()),
                ],
            },
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Outlines" => Object::Reference(outlines_id),
            "Dests" => Object::Reference(dests_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(2));
        assert_eq!(entries[0].dest_kind.as_deref(), Some("named"));
    }

    #[test]
    fn unresolvable_named_dest_keeps_entry_without_page() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Dangling"),
            "Dest" => Object::string_literal("missing-name"),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, None);
        assert_eq!(entries[0].dest_kind.as_deref(), Some("named"));
    }

    #[test]
    fn utf16be_title_decodes() {
        let (mut doc, page_ids) = doc_with_pages(1);
        // "Résumé" as UTF-16BE with BOM.
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "Résumé".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        let node = doc.add_object(dictionary! {
            "Title" => Object::String(bytes, lopdf::StringFormat::Literal),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Résumé");
    }

    #[test]
    fn utf16le_title_decodes() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "Résumé".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let node = doc.add_object(dictionary! {
            "Title" => Object::String(bytes, lopdf::StringFormat::Literal),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Résumé");
    }

    #[test]
    fn pdfdoc_encoding_title_maps_punctuation_and_ligatures() {
        // Titles without a UTF-16 BOM are PDFDocEncoded: bytes 0x80–0x9E
        // are punctuation/ligatures, not the Latin-1 control characters.
        let (mut doc, page_ids) = doc_with_pages(1);
        // "ﬁle — 'quoted'" in PDFDocEncoding.
        let bytes = vec![
            0x93, b'l', b'e', b' ', 0x84, b' ', 0x8F, b'q', b'u', b'o', b't', b'e', b'd', 0x90,
        ];
        let node = doc.add_object(dictionary! {
            "Title" => Object::String(bytes, lopdf::StringFormat::Literal),
            "Dest" => vec![
                Object::Reference(page_ids[0]),
                Object::Name(b"Fit".to_vec()),
            ],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].title,
            "\u{FB01}le \u{2014} \u{2018}quoted\u{2019}"
        );
    }

    #[test]
    fn integer_page_index_resolves_one_indexed() {
        let (mut doc, page_ids) = doc_with_pages(3);
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("By index"),
            "Dest" => vec![Object::Integer(2), Object::Name(b"Fit".to_vec())],
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries[0].page, Some(3));
    }

    #[test]
    fn next_self_cycle_terminates() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let node_id = doc.new_object_id();
        doc.set_object(
            node_id,
            dictionary! {
                "Title" => Object::string_literal("loop"),
                "Next" => Object::Reference(node_id),
            },
        );
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node_id),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn first_next_mutual_cycle_terminates() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let a = doc.new_object_id();
        let b = doc.new_object_id();
        doc.set_object(
            a,
            dictionary! {
                "Title" => Object::string_literal("a"),
                "First" => Object::Reference(b),
            },
        );
        doc.set_object(
            b,
            dictionary! {
                "Title" => Object::string_literal("b"),
                "Next" => Object::Reference(a),
            },
        );
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(a),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn deep_first_chain_stops_at_depth_cap() {
        // A chain of distinct children nested deeper than the cap must stop
        // at the cap rather than recursing to the chain length.
        let (mut doc, page_ids) = doc_with_pages(1);
        let n = MAX_OUTLINE_DEPTH * 100;
        let ids: Vec<ObjectId> = (0..=n).map(|_| doc.new_object_id()).collect();
        for i in 0..n {
            doc.set_object(
                ids[i],
                dictionary! {
                    "Title" => Object::string_literal("nested"),
                    "First" => Object::Reference(ids[i + 1]),
                },
            );
        }
        doc.set_object(
            ids[n],
            dictionary! { "Title" => Object::string_literal("leaf") },
        );
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(ids[0]),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        // Levels 1..=MAX_OUTLINE_DEPTH+1 are emitted (recursion into depth
        // MAX_OUTLINE_DEPTH+1 is cut off), far fewer than the chain length.
        assert_eq!(entries.len(), MAX_OUTLINE_DEPTH + 1);
        assert_eq!(
            entries.last().unwrap().level as usize,
            MAX_OUTLINE_DEPTH + 1
        );
    }

    #[test]
    fn long_sibling_chain_stops_at_node_budget() {
        // A /Next chain longer than the node budget must stop at the cap.
        let (mut doc, page_ids) = doc_with_pages(1);
        let n = MAX_OUTLINE_NODES + 50;
        let ids: Vec<ObjectId> = (0..n).map(|_| doc.new_object_id()).collect();
        for i in 0..n {
            let mut dict = dictionary! {
                "Title" => Object::string_literal("s"),
            };
            if i + 1 < n {
                dict.set("Next", Object::Reference(ids[i + 1]));
            }
            doc.set_object(ids[i], dict);
        }
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(ids[0]),
        });
        set_catalog(&mut doc, outlines_id);

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert!(entries.len() <= MAX_OUTLINE_NODES);
        assert!(entries.len() >= MAX_OUTLINE_NODES - 1);
    }

    #[test]
    fn name_tree_kids_cycle_terminates() {
        let (mut doc, page_ids) = doc_with_pages(1);
        let kid = doc.new_object_id();
        doc.set_object(
            kid,
            dictionary! {
                "Kids" => vec![Object::Reference(kid)],
                "Names" => vec![
                    Object::string_literal("n1"),
                    Object::Array(vec![
                        Object::Reference(page_ids[0]),
                        Object::Name(b"Fit".to_vec()),
                    ]),
                ],
            },
        );
        let node = doc.add_object(dictionary! {
            "Title" => Object::string_literal("cyclic tree"),
            "Dest" => Object::string_literal("n1"),
        });
        let outlines_id = doc.add_object(dictionary! {
            "Type" => "Outlines",
            "First" => Object::Reference(node),
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Outlines" => Object::Reference(outlines_id),
            "Names" => dictionary! {
                "Dests" => Object::Reference(kid),
            },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let entries = extract_outline_from_doc(&doc, &page_map(&page_ids));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, Some(1));
    }
}
