//! Tagged PDF structure tree parser.
//!
//! Reads the `/StructTreeRoot` from the document catalog and builds an
//! in-memory tree of [`StructElement`] nodes. Each leaf maps back to
//! content-stream marked content via MCID (Marked Content ID), which lets
//! downstream code attach semantic roles (heading, paragraph, table cell,
//! list item, …) to extracted [`TextItem`]s.

use log::debug;
use lopdf::{Document, Object, ObjectId};
use std::borrow::Cow;
use std::collections::HashMap;

// ─── Standard structure types ────────────────────────────────────────

/// Standard PDF structure element types (ISO 32000-1, Table 333–340).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StructRole {
    Document,
    Part,
    Art,
    Sect,
    Div,
    BlockQuote,
    Caption,
    TOC,
    TOCI,
    Index,
    NonStruct,
    Private,
    // Heading & paragraph
    H,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    P,
    // List
    L,
    LI,
    Lbl,
    LBody,
    // Table
    Table,
    TR,
    TH,
    TD,
    THead,
    TBody,
    TFoot,
    // Inline
    Span,
    Quote,
    Note,
    Reference,
    BibEntry,
    Code,
    Link,
    Annot,
    // Illustration
    Figure,
    Formula,
    Form,
    // Ruby / Warichu (CJK)
    Ruby,
    RB,
    RT,
    RP,
    Warichu,
    WT,
    WP,
    // Fallback
    Other(String),
}

impl StructRole {
    fn from_name(name: &str) -> Self {
        match name {
            "Document" => Self::Document,
            "Part" => Self::Part,
            "Art" => Self::Art,
            "Sect" => Self::Sect,
            "Div" => Self::Div,
            "BlockQuote" => Self::BlockQuote,
            "Caption" => Self::Caption,
            "TOC" => Self::TOC,
            "TOCI" => Self::TOCI,
            "Index" => Self::Index,
            "NonStruct" => Self::NonStruct,
            "Private" => Self::Private,
            "H" => Self::H,
            "H1" => Self::H1,
            "H2" => Self::H2,
            "H3" => Self::H3,
            "H4" => Self::H4,
            "H5" => Self::H5,
            "H6" => Self::H6,
            "P" => Self::P,
            "L" => Self::L,
            "LI" => Self::LI,
            "Lbl" => Self::Lbl,
            "LBody" => Self::LBody,
            "Table" => Self::Table,
            "TR" => Self::TR,
            "TH" => Self::TH,
            "TD" => Self::TD,
            "THead" => Self::THead,
            "TBody" => Self::TBody,
            "TFoot" => Self::TFoot,
            "Span" => Self::Span,
            "Quote" => Self::Quote,
            "Note" => Self::Note,
            "Reference" => Self::Reference,
            "BibEntry" => Self::BibEntry,
            "Code" => Self::Code,
            "Link" => Self::Link,
            "Annot" => Self::Annot,
            "Figure" => Self::Figure,
            "Formula" => Self::Formula,
            "Form" => Self::Form,
            "Ruby" => Self::Ruby,
            "RB" => Self::RB,
            "RT" => Self::RT,
            "RP" => Self::RP,
            "Warichu" => Self::Warichu,
            "WT" => Self::WT,
            "WP" => Self::WP,
            other => Self::Other(other.to_string()),
        }
    }

    /// Resolve a possibly-custom tag name through a role map.
    fn from_name_with_role_map(name: &str, role_map: &HashMap<String, String>) -> Self {
        // Follow role map chain (max 8 hops to avoid cycles)
        let mut current = name.to_string();
        for _ in 0..8 {
            let role = Self::from_name(&current);
            if !matches!(role, Self::Other(_)) {
                return role;
            }
            if let Some(mapped) = role_map.get(current.as_str()) {
                current = mapped.clone();
            } else {
                return role;
            }
        }
        Self::Other(name.to_string())
    }
}

// ─── Marked content reference ────────────────────────────────────────

/// A leaf reference linking a structure element to content-stream content.
#[derive(Debug, Clone)]
pub struct MarkedContentRef {
    /// The Marked Content ID used in the content stream's `BDC`/`BMC`.
    pub mcid: i64,
    /// Page ObjectId this content belongs to (from `/Pg` key).
    pub page_id: Option<ObjectId>,
}

// ─── Structure element ───────────────────────────────────────────────

/// A node in the PDF structure tree.
#[derive(Debug, Clone)]
pub struct StructElement {
    /// Semantic role (H1, P, Table, TD, …).
    pub role: StructRole,
    /// Alternative text for figures / illustrations.
    pub alt_text: Option<String>,
    /// Actual text override (e.g. for ligatures).
    pub actual_text: Option<String>,
    /// Language override (e.g. "en-US").
    pub lang: Option<String>,
    /// Direct marked-content references (leaf content).
    pub content_refs: Vec<MarkedContentRef>,
    /// Child structure elements.
    pub children: Vec<StructElement>,
}

// ─── Structure tree (top level) ──────────────────────────────────────

/// Parsed PDF structure tree.
///
/// Built from `/StructTreeRoot` in the document catalog. Use
/// [`StructTree::from_doc`] to parse, then [`StructTree::mcid_to_roles`]
/// to get per-page MCID → role lookup tables.
#[derive(Debug, Clone)]
pub struct StructTree {
    /// Root children (the top-level structure elements).
    pub children: Vec<StructElement>,
}

impl StructTree {
    /// Attempt to parse the structure tree from a PDF document.
    ///
    /// Returns `None` if the PDF is not tagged (no `/StructTreeRoot`).
    pub fn from_doc(doc: &Document) -> Option<Self> {
        let catalog = doc.catalog().ok()?;
        let struct_root_obj = catalog.get(b"StructTreeRoot").ok()?;
        let struct_root = resolve_dict(doc, struct_root_obj)?;

        // Parse role map: custom tag → standard tag
        let role_map = parse_role_map(doc, struct_root);
        debug!("structure tree: {} role map entries", role_map.len());

        // Parse child elements from /K
        let children = parse_kids(doc, struct_root, &role_map, None, 0);
        debug!("structure tree: {} top-level elements", children.len());

        if children.is_empty() {
            return None;
        }

        Some(StructTree { children })
    }

    /// Build per-page MCID → StructRole lookup.
    ///
    /// Returns a map: page_number (1-indexed) → (MCID → StructRole).
    /// The `page_ids` map should come from `doc.get_pages()`.
    pub fn mcid_to_roles(
        &self,
        page_ids: &std::collections::BTreeMap<u32, ObjectId>,
    ) -> HashMap<u32, HashMap<i64, StructRole>> {
        // Invert: ObjectId → page number
        let obj_to_page: HashMap<ObjectId, u32> =
            page_ids.iter().map(|(&num, &id)| (id, num)).collect();

        let mut result: HashMap<u32, HashMap<i64, StructRole>> = HashMap::new();
        self.collect_mcid_roles(&self.children, &obj_to_page, &mut result);
        result
    }

    fn collect_mcid_roles(
        &self,
        elements: &[StructElement],
        obj_to_page: &HashMap<ObjectId, u32>,
        result: &mut HashMap<u32, HashMap<i64, StructRole>>,
    ) {
        for elem in elements {
            for mcref in &elem.content_refs {
                if let Some(page_id) = mcref.page_id {
                    if let Some(&page_num) = obj_to_page.get(&page_id) {
                        result
                            .entry(page_num)
                            .or_default()
                            .insert(mcref.mcid, elem.role.clone());
                    }
                }
            }
            self.collect_mcid_roles(&elem.children, obj_to_page, result);
        }
    }

    /// Count total marked-content references across the tree.
    pub fn mcid_count(&self) -> usize {
        fn count(elements: &[StructElement]) -> usize {
            elements
                .iter()
                .map(|e| e.content_refs.len() + count(&e.children))
                .sum()
        }
        count(&self.children)
    }

    /// Build a flat list of structure elements with their roles and MCIDs,
    /// preserving document order. Useful for structure-aware markdown generation.
    pub fn flatten(&self) -> Vec<FlatStructElement> {
        let mut out = Vec::new();
        flatten_recursive(&self.children, &mut out, 0);
        out
    }
}

/// A flattened view of a structure element for linear traversal.
#[derive(Debug, Clone)]
pub struct FlatStructElement {
    /// Semantic role.
    pub role: StructRole,
    /// Nesting depth (0 = top-level).
    pub depth: usize,
    /// Alt text (figures).
    pub alt_text: Option<String>,
    /// Direct MCIDs with page ObjectIds.
    pub content_refs: Vec<MarkedContentRef>,
    /// Number of child elements (in the original tree).
    pub child_count: usize,
}

fn flatten_recursive(elements: &[StructElement], out: &mut Vec<FlatStructElement>, depth: usize) {
    for elem in elements {
        out.push(FlatStructElement {
            role: elem.role.clone(),
            depth,
            alt_text: elem.alt_text.clone(),
            content_refs: elem.content_refs.clone(),
            child_count: elem.children.len(),
        });
        flatten_recursive(&elem.children, out, depth + 1);
    }
}

// ─── Parsing helpers ─────────────────────────────────────────────────

/// Parse the `/RoleMap` dictionary (custom tag → standard tag).
fn parse_role_map(doc: &Document, struct_root: &lopdf::Dictionary) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(rm_obj) = struct_root.get(b"RoleMap") else {
        return map;
    };
    let Some(rm_dict) = resolve_dict(doc, rm_obj) else {
        return map;
    };
    for (key, val) in rm_dict.iter() {
        let key_str = String::from_utf8_lossy(key).to_string();
        if let Ok(name) = val.as_name() {
            let val_str = String::from_utf8_lossy(name).to_string();
            map.insert(key_str, val_str);
        }
    }
    map
}

/// Max recursion depth for structure tree parsing (prevents stack overflow on
/// malformed PDFs).
const MAX_DEPTH: usize = 64;

/// Parse child elements from a `/K` entry.
fn parse_kids(
    doc: &Document,
    dict: &lopdf::Dictionary,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
) -> Vec<StructElement> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }

    let Ok(k_obj) = dict.get(b"K") else {
        return Vec::new();
    };

    // /Pg on this element (inherited by children)
    let page_id = get_page_ref(doc, dict).or(inherited_page);

    match k_obj {
        Object::Array(arr) => {
            let mut children = Vec::new();
            for item in arr {
                let resolved = resolve_obj(doc, item);
                parse_kid(doc, resolved, role_map, page_id, depth, &mut children);
            }
            children
        }
        other => {
            let resolved = resolve_obj(doc, other);
            let mut children = Vec::new();
            parse_kid(doc, resolved, role_map, page_id, depth, &mut children);
            children
        }
    }
}

/// Parse a single child (either a struct element dict or an MCID integer).
fn parse_kid(
    doc: &Document,
    obj: &Object,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
    out: &mut Vec<StructElement>,
) {
    match obj {
        // Direct MCID integer — create a leaf wrapper
        Object::Integer(mcid) => {
            // This is a bare MCID at the struct-element level.
            // We attach it to the parent element, so we create a wrapper struct element.
            // Actually, bare MCIDs inside /K are content refs for the parent,
            // not separate child elements. We handle this at the caller level.
            // For now, create a minimal Span wrapper.
            out.push(StructElement {
                role: StructRole::Span,
                alt_text: None,
                actual_text: None,
                lang: None,
                content_refs: vec![MarkedContentRef {
                    mcid: *mcid,
                    page_id: inherited_page,
                }],
                children: Vec::new(),
            });
        }
        Object::Dictionary(d) => {
            parse_struct_element_dict(doc, d, role_map, inherited_page, depth, out);
        }
        Object::Stream(s) => {
            // Some PDFs wrap struct elements in streams (rare)
            parse_struct_element_dict(doc, &s.dict, role_map, inherited_page, depth, out);
        }
        _ => {}
    }
}

/// Parse a dictionary that could be either a struct element or a marked-content
/// reference (MCR) dictionary.
fn parse_struct_element_dict(
    doc: &Document,
    dict: &lopdf::Dictionary,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
    out: &mut Vec<StructElement>,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    // Check if this is a marked-content reference dict (has /Type /MCR)
    if is_mcr_dict(dict) {
        if let Ok(Object::Integer(mcid)) = dict.get(b"MCID") {
            let page_id = get_page_ref(doc, dict).or(inherited_page);
            out.push(StructElement {
                role: StructRole::Span,
                alt_text: None,
                actual_text: None,
                lang: None,
                content_refs: vec![MarkedContentRef {
                    mcid: *mcid,
                    page_id,
                }],
                children: Vec::new(),
            });
        }
        return;
    }

    // Check if this is an object reference dict (has /Type /OBJR) — skip these
    if is_objr_dict(dict) {
        return;
    }

    // It's a struct element — parse its /S (structure type)
    let role_name = match dict.get(b"S") {
        Ok(s_obj) => {
            let resolved = resolve_obj(doc, s_obj);
            match resolved.as_name() {
                Ok(name) => String::from_utf8_lossy(name).to_string(),
                Err(_) => return,
            }
        }
        Err(_) => return,
    };

    let role = StructRole::from_name_with_role_map(&role_name, role_map);
    let page_id = get_page_ref(doc, dict).or(inherited_page);

    // Extract optional attributes
    let alt_text = get_text_string(dict, b"Alt");
    let actual_text = get_text_string(dict, b"ActualText");
    let lang = get_text_string(dict, b"Lang");

    // Parse children from /K
    let mut content_refs = Vec::new();
    let mut children = Vec::new();

    if let Ok(k_obj) = dict.get(b"K") {
        let k_resolved = resolve_obj(doc, k_obj);
        match k_resolved {
            Object::Integer(mcid) => {
                content_refs.push(MarkedContentRef {
                    mcid: *mcid,
                    page_id,
                });
            }
            Object::Array(arr) => {
                for item in arr {
                    let resolved = resolve_obj(doc, item);
                    match resolved {
                        Object::Integer(mcid) => {
                            content_refs.push(MarkedContentRef {
                                mcid: *mcid,
                                page_id,
                            });
                        }
                        Object::Dictionary(d) => {
                            if is_mcr_dict(d) {
                                if let Ok(Object::Integer(mcid)) = d.get(b"MCID") {
                                    let pg = get_page_ref(doc, d).or(page_id);
                                    content_refs.push(MarkedContentRef {
                                        mcid: *mcid,
                                        page_id: pg,
                                    });
                                }
                            } else if is_objr_dict(d) {
                                // Skip object references
                            } else {
                                parse_struct_element_dict(
                                    doc,
                                    d,
                                    role_map,
                                    page_id,
                                    depth + 1,
                                    &mut children,
                                );
                            }
                        }
                        Object::Stream(s) => {
                            parse_struct_element_dict(
                                doc,
                                &s.dict,
                                role_map,
                                page_id,
                                depth + 1,
                                &mut children,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Object::Dictionary(d) => {
                if is_mcr_dict(d) {
                    if let Ok(Object::Integer(mcid)) = d.get(b"MCID") {
                        let pg = get_page_ref(doc, d).or(page_id);
                        content_refs.push(MarkedContentRef {
                            mcid: *mcid,
                            page_id: pg,
                        });
                    }
                } else {
                    parse_struct_element_dict(doc, d, role_map, page_id, depth + 1, &mut children);
                }
            }
            _ => {}
        }
    }

    out.push(StructElement {
        role,
        alt_text,
        actual_text,
        lang,
        content_refs,
        children,
    });
}

/// Check if dict has `/Type /MCR`.
fn is_mcr_dict(dict: &lopdf::Dictionary) -> bool {
    dict.get(b"Type")
        .ok()
        .and_then(|o| o.as_name().ok())
        .is_some_and(|n| n == b"MCR")
}

/// Check if dict has `/Type /OBJR`.
fn is_objr_dict(dict: &lopdf::Dictionary) -> bool {
    dict.get(b"Type")
        .ok()
        .and_then(|o| o.as_name().ok())
        .is_some_and(|n| n == b"OBJR")
}

/// Get the `/Pg` page reference from a dictionary.
fn get_page_ref(doc: &Document, dict: &lopdf::Dictionary) -> Option<ObjectId> {
    let pg = dict.get(b"Pg").ok()?;
    match pg {
        Object::Reference(id) => Some(*id),
        _ => {
            let resolved = resolve_obj(doc, pg);
            if let Object::Reference(id) = resolved {
                Some(*id)
            } else {
                None
            }
        }
    }
}

/// Extract a text string from a dictionary key (handles PDF text encoding).
fn get_text_string(dict: &lopdf::Dictionary, key: &[u8]) -> Option<String> {
    let obj = dict.get(key).ok()?;
    match obj {
        Object::String(bytes, _) => Some(crate::text_utils::decode_text_string(bytes)),
        _ => None,
    }
}

/// Resolve an Object reference, returning the target object.
fn resolve_obj<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(obj),
        _ => obj,
    }
}

/// Resolve an Object to a dictionary (handling references).
fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a lopdf::Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

// ─── PDF byte pre-processing ────────────────────────────────────────

/// Fix malformed structure element `/S` entries in raw PDF bytes.
///
/// Some PDF generators (notably fpdf2) write bare names like `/S Code`
/// instead of the correct `/S /Code`. lopdf cannot parse dictionaries
/// containing bare tokens, so the entire object is silently dropped.
///
/// This function scans for the pattern `/S <bare_word>` inside struct
/// element dictionaries and prepends `/` to make them valid PDF names.
/// Returns `Cow::Borrowed` if no fixes were needed.
pub fn fix_bare_struct_names(buf: &[u8]) -> Cow<'_, [u8]> {
    // Quick check: if no StructTreeRoot, nothing to fix
    if !contains_bytes(buf, b"/StructTreeRoot") {
        return Cow::Borrowed(buf);
    }

    // Known struct type names that may appear as bare tokens.
    // We only fix names that are valid PDF structure types to avoid
    // false positives on arbitrary dictionary values.
    const KNOWN_NAMES: &[&[u8]] = &[
        b"Document",
        b"Part",
        b"Art",
        b"Sect",
        b"Div",
        b"BlockQuote",
        b"Caption",
        b"TOC",
        b"TOCI",
        b"Index",
        b"NonStruct",
        b"Private",
        b"H",
        b"H1",
        b"H2",
        b"H3",
        b"H4",
        b"H5",
        b"H6",
        b"P",
        b"L",
        b"LI",
        b"Lbl",
        b"LBody",
        b"Table",
        b"TR",
        b"TH",
        b"TD",
        b"THead",
        b"TBody",
        b"TFoot",
        b"Span",
        b"Quote",
        b"Note",
        b"Reference",
        b"BibEntry",
        b"Code",
        b"Link",
        b"Annot",
        b"Figure",
        b"Formula",
        b"Form",
        b"Ruby",
        b"RB",
        b"RT",
        b"RP",
        b"Warichu",
        b"WT",
        b"WP",
    ];

    let pattern = b"/S ";
    let mut result: Option<Vec<u8>> = None;
    let mut pos = 0;

    while pos + pattern.len() < buf.len() {
        let Some(idx) = find_bytes(&buf[pos..], pattern).map(|i| i + pos) else {
            break;
        };

        let after = idx + pattern.len();
        // Check if the next char is already '/' (correct name) or not
        if after < buf.len() && buf[after] == b'/' {
            pos = after;
            continue;
        }

        // Try to match a known bare struct name at this position
        let mut matched = false;
        for name in KNOWN_NAMES {
            let end = after + name.len();
            if end <= buf.len()
                && &buf[after..end] == *name
                // Must be followed by a delimiter (whitespace, newline, /, >)
                && (end >= buf.len() || matches!(buf[end], b'\n' | b'\r' | b' ' | b'/' | b'>'))
            {
                // Found a bare name — lazily allocate output buffer
                let out = result.get_or_insert_with(|| buf[..after].to_vec());
                // Append everything from last position up to the bare name
                if out.len() < after {
                    out.extend_from_slice(&buf[out.len()..after]);
                }
                out.push(b'/');
                out.extend_from_slice(name);
                pos = end;
                matched = true;
                debug!(
                    "fix_bare_struct_names: patched /S {} → /S /{}",
                    String::from_utf8_lossy(name),
                    String::from_utf8_lossy(name)
                );
                break;
            }
        }

        if !matched {
            pos = after;
        }
    }

    match result {
        Some(mut out) => {
            // Append remaining bytes
            if out.len() < buf.len() {
                out.extend_from_slice(&buf[out.len()..]);
            }
            Cow::Owned(out)
        }
        None => Cow::Borrowed(buf),
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_struct_role_from_name() {
        assert_eq!(StructRole::from_name("H1"), StructRole::H1);
        assert_eq!(StructRole::from_name("P"), StructRole::P);
        assert_eq!(StructRole::from_name("Table"), StructRole::Table);
        assert_eq!(StructRole::from_name("TD"), StructRole::TD);
        assert_eq!(
            StructRole::from_name("CustomTag"),
            StructRole::Other("CustomTag".to_string())
        );
    }

    #[test]
    fn test_struct_role_with_role_map() {
        let mut role_map = HashMap::new();
        role_map.insert("Heading1".to_string(), "H1".to_string());
        role_map.insert("Body".to_string(), "P".to_string());
        // Chain: MyTag → Heading1 → H1
        role_map.insert("MyTag".to_string(), "Heading1".to_string());

        assert_eq!(
            StructRole::from_name_with_role_map("Heading1", &role_map),
            StructRole::H1
        );
        assert_eq!(
            StructRole::from_name_with_role_map("Body", &role_map),
            StructRole::P
        );
        assert_eq!(
            StructRole::from_name_with_role_map("MyTag", &role_map),
            StructRole::H1
        );
        // Standard names bypass the map
        assert_eq!(
            StructRole::from_name_with_role_map("H2", &role_map),
            StructRole::H2
        );
    }

    #[test]
    fn test_struct_role_role_map_cycle() {
        // A→B→A cycle should not infinite-loop
        let mut role_map = HashMap::new();
        role_map.insert("A".to_string(), "B".to_string());
        role_map.insert("B".to_string(), "A".to_string());

        let role = StructRole::from_name_with_role_map("A", &role_map);
        // Should terminate (as Other) rather than loop forever
        assert!(matches!(role, StructRole::Other(_)));
    }

    #[test]
    fn test_flat_struct_element() {
        let tree = StructTree {
            children: vec![StructElement {
                role: StructRole::Document,
                alt_text: None,
                actual_text: None,
                lang: None,
                content_refs: Vec::new(),
                children: vec![
                    StructElement {
                        role: StructRole::H1,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![MarkedContentRef {
                            mcid: 0,
                            page_id: Some((1, 0)),
                        }],
                        children: Vec::new(),
                    },
                    StructElement {
                        role: StructRole::P,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![MarkedContentRef {
                            mcid: 1,
                            page_id: Some((1, 0)),
                        }],
                        children: Vec::new(),
                    },
                ],
            }],
        };

        let flat = tree.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].role, StructRole::Document);
        assert_eq!(flat[0].depth, 0);
        assert_eq!(flat[1].role, StructRole::H1);
        assert_eq!(flat[1].depth, 1);
        assert_eq!(flat[2].role, StructRole::P);
        assert_eq!(flat[2].depth, 1);
    }

    #[test]
    fn test_mcid_count() {
        let tree = StructTree {
            children: vec![StructElement {
                role: StructRole::Document,
                alt_text: None,
                actual_text: None,
                lang: None,
                content_refs: Vec::new(),
                children: vec![
                    StructElement {
                        role: StructRole::H1,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![
                            MarkedContentRef {
                                mcid: 0,
                                page_id: Some((1, 0)),
                            },
                            MarkedContentRef {
                                mcid: 1,
                                page_id: Some((1, 0)),
                            },
                        ],
                        children: Vec::new(),
                    },
                    StructElement {
                        role: StructRole::P,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![MarkedContentRef {
                            mcid: 2,
                            page_id: Some((1, 0)),
                        }],
                        children: Vec::new(),
                    },
                ],
            }],
        };

        assert_eq!(tree.mcid_count(), 3);
    }

    #[test]
    fn test_mcid_to_roles() {
        use std::collections::BTreeMap;

        let page_id: ObjectId = (5, 0);
        let mut page_ids = BTreeMap::new();
        page_ids.insert(1u32, page_id);

        let tree = StructTree {
            children: vec![StructElement {
                role: StructRole::Document,
                alt_text: None,
                actual_text: None,
                lang: None,
                content_refs: Vec::new(),
                children: vec![
                    StructElement {
                        role: StructRole::H1,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![MarkedContentRef {
                            mcid: 0,
                            page_id: Some(page_id),
                        }],
                        children: Vec::new(),
                    },
                    StructElement {
                        role: StructRole::P,
                        alt_text: None,
                        actual_text: None,
                        lang: None,
                        content_refs: vec![MarkedContentRef {
                            mcid: 1,
                            page_id: Some(page_id),
                        }],
                        children: Vec::new(),
                    },
                ],
            }],
        };

        let roles = tree.mcid_to_roles(&page_ids);
        let page1 = roles.get(&1).unwrap();
        assert_eq!(page1.get(&0), Some(&StructRole::H1));
        assert_eq!(page1.get(&1), Some(&StructRole::P));
    }

    #[test]
    fn test_fix_bare_struct_names() {
        // Verify the byte-level pre-processor fixes bare names.
        // All inputs include /StructTreeRoot to pass the early-return guard.
        let input = b"/StructTreeRoot /S Code\n/Type /StructElem";
        let fixed = fix_bare_struct_names(input);
        assert!(
            fixed.windows(b"/S /Code".len()).any(|w| w == b"/S /Code"),
            "Should fix bare Code: {:?}",
            String::from_utf8_lossy(&fixed)
        );

        // Already correct — should return borrowed
        let input = b"/StructTreeRoot /S /Code\n/Type /StructElem";
        let fixed = fix_bare_struct_names(input);
        assert!(matches!(fixed, std::borrow::Cow::Borrowed(_)));

        // Multiple bare names
        let input = b"/StructTreeRoot /S H1\n/foo\n/S P\n/bar";
        let fixed = fix_bare_struct_names(input);
        let s = String::from_utf8_lossy(&fixed);
        assert!(s.contains("/S /H1"), "Should fix H1: {s}");
        assert!(s.contains("/S /P"), "Should fix P: {s}");

        // Unknown name should not be touched
        let input = b"/StructTreeRoot /S FooBar\n";
        let fixed = fix_bare_struct_names(input);
        let s = String::from_utf8_lossy(&fixed);
        assert!(s.contains("/S FooBar"), "Should not fix unknown: {s}");

        // No StructTreeRoot — skip entirely
        let input = b"/S Code\nno struct tree";
        let fixed = fix_bare_struct_names(input);
        assert!(matches!(fixed, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn test_bare_name_struct_types() {
        // Some PDF generators (e.g. fpdf2) write /S Code instead of /S /Code.
        // lopdf silently drops objects with invalid tokens. Our pre-processor
        // fixes these before loading.
        let raw = std::fs::read("tests/fixtures/bare_name_struct.pdf").unwrap();
        let fixed = fix_bare_struct_names(&raw);
        let doc = Document::load_mem(fixed.as_ref()).unwrap();

        let tree = StructTree::from_doc(&doc);
        assert!(tree.is_some(), "Should parse bare-name struct tree");
        let tree = tree.unwrap();

        let flat = tree.flatten();
        let roles: Vec<&StructRole> = flat.iter().map(|e| &e.role).collect();

        assert!(
            roles.iter().any(|r| matches!(r, StructRole::H1)),
            "Should find H1 from bare name: {:?}",
            roles
        );
        assert!(
            roles.iter().any(|r| matches!(r, StructRole::Code)),
            "Should find Code from bare name: {:?}",
            roles
        );
    }

    #[test]
    fn test_parse_real_tagged_pdf() {
        let doc = Document::load("tests/fixtures/2013-app2.pdf").unwrap();
        let tree = StructTree::from_doc(&doc);
        assert!(tree.is_some(), "2013-app2.pdf should have a structure tree");
        let tree = tree.unwrap();

        // Should have a non-trivial structure
        assert!(!tree.children.is_empty());
        assert!(
            tree.mcid_count() > 0,
            "Should have marked content references"
        );

        // Flatten and verify we get heading/paragraph/table elements
        let flat = tree.flatten();
        let roles: Vec<&StructRole> = flat.iter().map(|e| &e.role).collect();
        assert!(
            roles.iter().any(|r| matches!(r, StructRole::P)),
            "Should contain paragraph elements"
        );

        // Verify mcid_to_roles produces a populated map
        let page_ids = doc.get_pages();
        let role_map = tree.mcid_to_roles(&page_ids);
        assert!(!role_map.is_empty(), "Should have MCID→role mappings");
    }
}
