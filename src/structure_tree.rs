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
use std::collections::{HashMap, HashSet};

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
    /// Content roles whose text must never be promoted to a heading by the
    /// visual heuristic. These carry an explicit non-heading meaning in the
    /// struct tree (lists, quotes, notes, references, captions, formulas,
    /// forms, ToC entries), yet their text is often short and visually
    /// isolated — exactly what the heuristic keys on. Heading roles (H, H1–H6)
    /// and generic container/flow roles (P, Div, Sect, Span, …) are excluded
    /// so the heuristic can still fire there.
    ///
    /// `Figure` is deliberately NOT in this set: cover/banner pages routinely
    /// tag the document title inside a Figure (alongside a seal or logo), and
    /// that title is a real heading. `Formula` and `Form` stay — a line
    /// explicitly tagged as an equation or form field is never a heading.
    ///
    /// Table roles (Table/TR/TH/TD/THead/TBody/TFoot) are included so that
    /// when table reconstruction falls back and cells reach the line loop as
    /// plain text, a short isolated cell — a `TH` column header especially —
    /// is not promoted to a heading.
    pub(crate) fn is_non_heading_content(&self) -> bool {
        matches!(
            self,
            Self::L
                | Self::LI
                | Self::Lbl
                | Self::LBody
                | Self::BlockQuote
                | Self::Quote
                | Self::Caption
                | Self::TOC
                | Self::TOCI
                | Self::Index
                | Self::Note
                | Self::Reference
                | Self::BibEntry
                | Self::Code
                | Self::Formula
                | Self::Form
                | Self::Table
                | Self::TR
                | Self::TH
                | Self::TD
                | Self::THead
                | Self::TBody
                | Self::TFoot
        )
    }

    /// The standard structure type name for this role ("H1", "P", "Table", …).
    ///
    /// Inverse of [`StructRole::from_name`]: for [`StructRole::Other`] the
    /// custom tag name is returned verbatim.
    pub fn name(&self) -> &str {
        match self {
            Self::Document => "Document",
            Self::Part => "Part",
            Self::Art => "Art",
            Self::Sect => "Sect",
            Self::Div => "Div",
            Self::BlockQuote => "BlockQuote",
            Self::Caption => "Caption",
            Self::TOC => "TOC",
            Self::TOCI => "TOCI",
            Self::Index => "Index",
            Self::NonStruct => "NonStruct",
            Self::Private => "Private",
            Self::H => "H",
            Self::H1 => "H1",
            Self::H2 => "H2",
            Self::H3 => "H3",
            Self::H4 => "H4",
            Self::H5 => "H5",
            Self::H6 => "H6",
            Self::P => "P",
            Self::L => "L",
            Self::LI => "LI",
            Self::Lbl => "Lbl",
            Self::LBody => "LBody",
            Self::Table => "Table",
            Self::TR => "TR",
            Self::TH => "TH",
            Self::TD => "TD",
            Self::THead => "THead",
            Self::TBody => "TBody",
            Self::TFoot => "TFoot",
            Self::Span => "Span",
            Self::Quote => "Quote",
            Self::Note => "Note",
            Self::Reference => "Reference",
            Self::BibEntry => "BibEntry",
            Self::Code => "Code",
            Self::Link => "Link",
            Self::Annot => "Annot",
            Self::Figure => "Figure",
            Self::Formula => "Formula",
            Self::Form => "Form",
            Self::Ruby => "Ruby",
            Self::RB => "RB",
            Self::RT => "RT",
            Self::RP => "RP",
            Self::Warichu => "Warichu",
            Self::WT => "WT",
            Self::WP => "WP",
            Self::Other(name) => name,
        }
    }

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

        // Seed the cycle guard with the struct-root's own object id so a `/K`
        // that points back at the root is treated as a cycle, and bound total
        // node materialization with a global budget.
        let mut walk = StructWalk::new();
        if let Ok(root_id) = struct_root_obj.as_reference() {
            walk.active.insert(root_id);
        }

        // Parse child elements from /K
        let children = parse_kids(doc, struct_root, &role_map, None, 0, &mut walk);
        debug!("structure tree: {} top-level elements", children.len());

        if walk.truncated {
            log::warn!(
                "structure tree parsing was truncated (node budget of \
                 {MAX_STRUCT_NODES} or traversal budget of {MAX_STRUCT_WORK} \
                 reached, a `/K` reference cycle, or the max nesting depth of \
                 {MAX_DEPTH}); tagged roles/tables may be incomplete (likely a \
                 very large or malformed tagged PDF)"
            );
        }

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

    /// Extract table structures from the tagged PDF tree.
    ///
    /// Walks the tree to find `/Table` elements with `/TR` > `/TD|TH` children,
    /// collecting MCIDs at each cell.  Returns structured descriptors that can
    /// be matched against extracted [`TextItem`]s to build tables without
    /// relying on geometry-based detection.
    pub fn extract_tables(
        &self,
        page_ids: &std::collections::BTreeMap<u32, ObjectId>,
    ) -> Vec<StructTable> {
        let obj_to_page: HashMap<ObjectId, u32> =
            page_ids.iter().map(|(&num, &id)| (id, num)).collect();
        let mut tables = Vec::new();
        collect_tables(&self.children, &obj_to_page, &mut tables);
        tables
    }
}

// ─── Tagged table structures ────────────────────────────────────────

/// A table cell extracted from the structure tree.
#[derive(Debug, Clone)]
pub struct StructTableCell {
    /// Whether this cell is a header cell (`/TH`).
    pub is_header: bool,
    /// MCIDs with their resolved page numbers.
    pub mcids: Vec<(i64, u32)>,
}

/// A table row extracted from the structure tree.
#[derive(Debug, Clone)]
pub struct StructTableRow {
    pub cells: Vec<StructTableCell>,
}

/// A complete table extracted from the structure tree.
#[derive(Debug, Clone)]
pub struct StructTable {
    pub rows: Vec<StructTableRow>,
}

fn collect_tables(
    elements: &[StructElement],
    obj_to_page: &HashMap<ObjectId, u32>,
    tables: &mut Vec<StructTable>,
) {
    for elem in elements {
        if elem.role == StructRole::Table {
            let mut rows = Vec::new();
            collect_rows(&elem.children, obj_to_page, &mut rows);
            if rows.len() >= 2 && rows.iter().any(|r| !r.cells.is_empty()) {
                tables.push(StructTable { rows });
            }
        } else {
            collect_tables(&elem.children, obj_to_page, tables);
        }
    }
}

/// Collect rows from Table children, transparently descending through
/// THead/TBody/TFoot grouping elements.
fn collect_rows(
    elements: &[StructElement],
    obj_to_page: &HashMap<ObjectId, u32>,
    rows: &mut Vec<StructTableRow>,
) {
    for elem in elements {
        match elem.role {
            StructRole::TR => {
                let mut cells = Vec::new();
                for child in &elem.children {
                    if child.role == StructRole::TD || child.role == StructRole::TH {
                        let is_header = child.role == StructRole::TH;
                        let mut mcids = Vec::new();
                        collect_mcids_recursive(child, obj_to_page, &mut mcids);
                        cells.push(StructTableCell { is_header, mcids });
                    }
                }
                rows.push(StructTableRow { cells });
            }
            StructRole::THead | StructRole::TBody | StructRole::TFoot => {
                collect_rows(&elem.children, obj_to_page, rows);
            }
            _ => {}
        }
    }
}

/// Recursively collect all MCIDs from an element and its descendants.
fn collect_mcids_recursive(
    elem: &StructElement,
    obj_to_page: &HashMap<ObjectId, u32>,
    mcids: &mut Vec<(i64, u32)>,
) {
    for mcref in &elem.content_refs {
        if let Some(page_id) = mcref.page_id {
            if let Some(&page_num) = obj_to_page.get(&page_id) {
                mcids.push((mcref.mcid, page_num));
            }
        }
    }
    for child in &elem.children {
        collect_mcids_recursive(child, obj_to_page, mcids);
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

/// Global cap on the number of structure-tree nodes materialized in a single
/// parse. Real tagged trees are far smaller; a crafted PDF can alias one struct
/// element into its own `/K` (e.g. `/K [n 0 R n 0 R]`) so the tree branches
/// exponentially (2^depth) before the depth cap is reached, exhausting memory.
/// This budget bounds total work and allocation regardless of tree shape.
const MAX_STRUCT_NODES: usize = 500_000;

/// Cap on the number of `/K` items *examined* during a single parse, regardless
/// of whether they materialize anything. Bounds CPU for crafted wide `/K` arrays
/// of non-materializing entries (unsupported value types, `/OBJR` dicts, cycle
/// back-edges) that would otherwise be scanned in full without ever touching the
/// node budget. Kept well above the node budget so it never truncates content
/// that already fits within `MAX_STRUCT_NODES`.
const MAX_STRUCT_WORK: usize = 2_000_000;

/// Traversal state shared across the recursive structure-tree parse.
///
/// `budget` is a global allowance charged once per materialized item — each
/// struct-element node and each marked-content reference — so total work is
/// bounded even for aliased/DAG-shaped `/K` graphs of distinct objects or a
/// single element with a very wide `/K` array. `active` holds the object IDs
/// currently on the depth-first path so a struct element that references itself
/// (or an ancestor) is not expanded into an unbounded/exponential subtree.
/// `budget` bounds *materialization* (nodes + content refs). `work` separately
/// bounds *traversal* — every `/K` item examined is charged against it, even
/// ones that materialize nothing (unsupported values, `/OBJR`, cycle back-edges)
/// — so a wide malformed array cannot force an unbounded scan, and those skipped
/// items don't drain the materialization budget and truncate real content.
/// `truncated` records whether any parse work was skipped — the budget was
/// exhausted, a `/K` reference cycle was broken, or the depth cap was hit — so
/// the caller can log it once rather than per skipped item. `stalled` is set
/// when an atomic multi-unit reservation could not fit in the remaining budget;
/// it makes [`exhausted`](Self::exhausted) report done so a wide `/K` array is
/// not scanned to the end once no further leaf can be materialized.
struct StructWalk {
    budget: usize,
    work: usize,
    active: HashSet<ObjectId>,
    truncated: bool,
    stalled: bool,
}

impl StructWalk {
    fn new() -> Self {
        Self {
            budget: MAX_STRUCT_NODES,
            work: MAX_STRUCT_WORK,
            active: HashSet::new(),
            truncated: false,
            stalled: false,
        }
    }

    /// Charge one unit of traversal work for an examined `/K` item, whether or
    /// not it materializes anything. Returns `false` (flagging truncation) once
    /// the traversal budget is spent, so an enclosing loop stops instead of
    /// scanning the rest of a wide array of non-materializing entries.
    fn spend_work(&mut self) -> bool {
        if self.work == 0 {
            self.truncated = true;
            return false;
        }
        self.work -= 1;
        true
    }

    /// Record that some parse work was skipped for a non-budget reason (a `/K`
    /// reference cycle or the depth cap), so the one-shot truncation warning
    /// also covers malformed/over-deep trees, not just budget exhaustion.
    fn note_skipped(&mut self) {
        self.truncated = true;
    }

    /// Charge one unit against the budget for a materialized item (a struct
    /// element node or a marked-content reference). Returns `false` — without
    /// underflowing — once the budget is exhausted, so callers skip the item.
    fn charge(&mut self) -> bool {
        if self.budget == 0 {
            self.truncated = true;
            return false;
        }
        self.budget -= 1;
        true
    }

    /// Atomically charge `n` units for a single item that materializes several
    /// budget-counted parts at once (a leaf wrapper node *plus* its content
    /// reference). Charges nothing when fewer than `n` units remain — so a
    /// partial reservation never wastes capacity — and marks the walk `stalled`
    /// so the enclosing loop stops instead of scanning the rest of a wide `/K`
    /// array that can no longer fit any leaf.
    fn charge_n(&mut self, n: usize) -> bool {
        if self.budget < n {
            self.truncated = true;
            self.stalled = true;
            return false;
        }
        self.budget -= n;
        true
    }

    /// Whether traversal should stop: the budget is spent, or a multi-unit
    /// reservation could not fit (`stalled`) so no further leaf will materialize.
    /// Use this at the guards that break/return to skip remaining items; it
    /// records that truncation occurred (a guard only fires while an item is
    /// still pending), so callers that drop work without going through
    /// [`charge`](Self::charge) still flag the truncation for logging.
    fn exhausted(&mut self) -> bool {
        if self.budget == 0 || self.stalled {
            self.truncated = true;
            true
        } else {
            false
        }
    }
}

/// Parse child elements from a `/K` entry.
fn parse_kids(
    doc: &Document,
    dict: &lopdf::Dictionary,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
    walk: &mut StructWalk,
) -> Vec<StructElement> {
    if depth >= MAX_DEPTH {
        walk.note_skipped();
        return Vec::new();
    }
    if walk.exhausted() {
        return Vec::new();
    }

    let Ok(k_obj) = dict.get(b"K") else {
        return Vec::new();
    };

    // /Pg on this element (inherited by children)
    let page_id = get_page_ref(doc, dict).or(inherited_page);

    let mut children = Vec::new();
    match k_obj {
        Object::Array(arr) => {
            for item in arr {
                if walk.exhausted() || !walk.spend_work() {
                    break;
                }
                process_kid_item(doc, item, role_map, page_id, depth, &mut children, walk);
            }
        }
        other => {
            process_kid_item(doc, other, role_map, page_id, depth, &mut children, walk);
        }
    }
    children
}

/// Resolve one `/K` array item (following at most one level of indirection),
/// guarding against reference cycles and the global node budget, then dispatch
/// it via [`parse_kid`].
fn process_kid_item(
    doc: &Document,
    item: &Object,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
    out: &mut Vec<StructElement>,
    walk: &mut StructWalk,
) {
    if walk.exhausted() {
        return;
    }
    if depth >= MAX_DEPTH {
        walk.note_skipped();
        return;
    }
    // If this child is an indirect reference, track its id on the active path so
    // a self/ancestor reference is not expanded into an exponential subtree.
    let ref_id = match item {
        Object::Reference(id) => Some(*id),
        _ => None,
    };
    if let Some(id) = ref_id {
        if !walk.active.insert(id) {
            walk.note_skipped();
            return; // cycle: this object is already on the current path
        }
    }
    let resolved = resolve_obj(doc, item);
    parse_kid(doc, resolved, role_map, inherited_page, depth, out, walk);
    if let Some(id) = ref_id {
        walk.active.remove(&id);
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
    walk: &mut StructWalk,
) {
    match obj {
        // Direct MCID integer — create a leaf wrapper
        Object::Integer(mcid) => {
            // A wrapper node plus its content reference — two items — reserved
            // atomically so we never consume one unit without emitting both.
            if !walk.charge_n(2) {
                return;
            }
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
            parse_struct_element_dict(doc, d, role_map, inherited_page, depth, out, walk);
        }
        Object::Stream(s) => {
            // Some PDFs wrap struct elements in streams (rare)
            parse_struct_element_dict(doc, &s.dict, role_map, inherited_page, depth, out, walk);
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
    walk: &mut StructWalk,
) {
    if depth >= MAX_DEPTH {
        walk.note_skipped();
        return;
    }

    // A marked-content reference dict materializes a wrapper node + one content
    // reference (two items). Reserve both atomically *before* the node charge so
    // we never consume a unit without emitting the reference — which would also
    // deny that unit to a later element that would have fit. This matches the
    // bare-MCID path.
    if is_mcr_dict(dict) {
        if let Ok(Object::Integer(mcid)) = dict.get(b"MCID") {
            if !walk.charge_n(2) {
                return;
            }
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

    // Skip object-reference dicts (`/Type /OBJR`) — they materialize no node, so
    // recognize and return *before* charging the budget (otherwise a document
    // full of OBJRs would drain the shared budget and truncate real content).
    if is_objr_dict(dict) {
        return;
    }

    // It's a struct element — parse its /S (structure type). A dict without a
    // valid /S also materializes nothing, so validate before charging.
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

    // Charge the node only now that we know it will materialize (bounds
    // aliased/DAG-shaped `/K` graphs the per-path cycle guard alone cannot stop).
    if !walk.charge() {
        return;
    }

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
                if walk.charge() {
                    content_refs.push(MarkedContentRef {
                        mcid: *mcid,
                        page_id,
                    });
                }
            }
            Object::Array(arr) => {
                for item in arr {
                    if walk.exhausted() || !walk.spend_work() {
                        break;
                    }
                    // Only content-ref items (bare MCIDs / MCR dicts) are charged
                    // here — those are the unbounded allocations. Structural
                    // children are charged once at their own node entry in the
                    // recursive call, so charging them here too would double-count
                    // and drain the budget ~2× faster than the per-node semantics.
                    let ref_id = match item {
                        Object::Reference(id) => Some(*id),
                        _ => None,
                    };
                    let resolved = resolve_obj(doc, item);
                    match resolved {
                        Object::Integer(mcid) => {
                            if walk.charge() {
                                content_refs.push(MarkedContentRef {
                                    mcid: *mcid,
                                    page_id,
                                });
                            }
                        }
                        Object::Dictionary(d) => {
                            if is_mcr_dict(d) {
                                if let Ok(Object::Integer(mcid)) = d.get(b"MCID") {
                                    if walk.charge() {
                                        let pg = get_page_ref(doc, d).or(page_id);
                                        content_refs.push(MarkedContentRef {
                                            mcid: *mcid,
                                            page_id: pg,
                                        });
                                    }
                                }
                            } else if is_objr_dict(d) {
                                // Skip object references
                            } else {
                                recurse_struct_child(
                                    doc,
                                    ref_id,
                                    d,
                                    role_map,
                                    page_id,
                                    depth,
                                    &mut children,
                                    walk,
                                );
                            }
                        }
                        Object::Stream(s) => {
                            recurse_struct_child(
                                doc,
                                ref_id,
                                &s.dict,
                                role_map,
                                page_id,
                                depth,
                                &mut children,
                                walk,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Object::Dictionary(d) => {
                if is_mcr_dict(d) {
                    if let Ok(Object::Integer(mcid)) = d.get(b"MCID") {
                        if walk.charge() {
                            let pg = get_page_ref(doc, d).or(page_id);
                            content_refs.push(MarkedContentRef {
                                mcid: *mcid,
                                page_id: pg,
                            });
                        }
                    }
                } else {
                    let ref_id = match k_obj {
                        Object::Reference(id) => Some(*id),
                        _ => None,
                    };
                    recurse_struct_child(
                        doc,
                        ref_id,
                        d,
                        role_map,
                        page_id,
                        depth,
                        &mut children,
                        walk,
                    );
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

/// Recurse into a child struct-element dictionary, guarding against reference
/// cycles (via the active-path object-id set) and the global node budget.
///
/// `ref_id` is the object id of the child when it was reached through an
/// indirect reference (`None` for an inline dictionary, which cannot alias).
#[allow(clippy::too_many_arguments)]
fn recurse_struct_child(
    doc: &Document,
    ref_id: Option<ObjectId>,
    dict: &lopdf::Dictionary,
    role_map: &HashMap<String, String>,
    inherited_page: Option<ObjectId>,
    depth: usize,
    out: &mut Vec<StructElement>,
    walk: &mut StructWalk,
) {
    if walk.exhausted() {
        return;
    }
    if let Some(id) = ref_id {
        if !walk.active.insert(id) {
            walk.note_skipped();
            return; // cycle: this object is already on the current path
        }
    }
    parse_struct_element_dict(doc, dict, role_map, inherited_page, depth + 1, out, walk);
    if let Some(id) = ref_id {
        walk.active.remove(&id);
    }
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
    use lopdf::dictionary;

    #[test]
    fn non_heading_content_roles() {
        for r in [
            StructRole::L,
            StructRole::LI,
            StructRole::BlockQuote,
            StructRole::Quote,
            StructRole::Caption,
            StructRole::TOC,
            StructRole::TOCI,
            StructRole::Index,
            StructRole::Note,
            StructRole::Reference,
            StructRole::BibEntry,
            StructRole::Code,
            StructRole::Formula,
            StructRole::Form,
            StructRole::Table,
            StructRole::TR,
            StructRole::TH,
            StructRole::TD,
            StructRole::THead,
            StructRole::TBody,
            StructRole::TFoot,
        ] {
            assert!(
                r.is_non_heading_content(),
                "{r:?} should block heading promotion"
            );
        }
        // Heading and generic container/flow roles must NOT block promotion
        for r in [
            StructRole::H,
            StructRole::H1,
            StructRole::H3,
            StructRole::P,
            StructRole::Div,
            StructRole::Sect,
            StructRole::Span,
            StructRole::Figure,
        ] {
            assert!(
                !r.is_non_heading_content(),
                "{r:?} should allow heading promotion"
            );
        }
    }

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
    fn test_struct_role_name_roundtrip() {
        // `name()` is the inverse of `from_name` for every standard type.
        for name in [
            "Document",
            "Part",
            "Art",
            "Sect",
            "Div",
            "BlockQuote",
            "Caption",
            "TOC",
            "TOCI",
            "Index",
            "NonStruct",
            "Private",
            "H",
            "H1",
            "H2",
            "H3",
            "H4",
            "H5",
            "H6",
            "P",
            "L",
            "LI",
            "Lbl",
            "LBody",
            "Table",
            "TR",
            "TH",
            "TD",
            "THead",
            "TBody",
            "TFoot",
            "Span",
            "Quote",
            "Note",
            "Reference",
            "BibEntry",
            "Code",
            "Link",
            "Annot",
            "Figure",
            "Formula",
            "Form",
            "Ruby",
            "RB",
            "RT",
            "RP",
            "Warichu",
            "WT",
            "WP",
        ] {
            assert_eq!(StructRole::from_name(name).name(), name);
        }
        // Custom tags pass through verbatim.
        assert_eq!(StructRole::from_name("CustomTag").name(), "CustomTag");
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

    fn count_nodes(elems: &[StructElement]) -> usize {
        elems.iter().map(|e| 1 + count_nodes(&e.children)).sum()
    }

    /// Wrap already-created struct elements under a `/StructTreeRoot` and
    /// `/Catalog`, returning a document ready for [`StructTree::from_doc`].
    /// `root_kid` is the top-level element the root's `/K` points at.
    fn finalize_tagged_doc(mut doc: Document, root_kid: ObjectId) -> Document {
        let root_id = doc.add_object(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => vec![Object::Reference(root_kid)],
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "StructTreeRoot" => Object::Reference(root_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn struct_tree_self_alias_kids_terminates() {
        // A struct element that lists itself twice in `/K` (`/K [n 0 R n 0 R]`)
        // must not expand into an exponential tree.
        let mut doc = Document::new();
        let elem = doc.new_object_id();
        doc.set_object(
            elem,
            dictionary! {
                "Type" => "StructElem",
                "S" => "Div",
                "K" => vec![Object::Reference(elem), Object::Reference(elem)],
            },
        );
        let doc = finalize_tagged_doc(doc, elem);

        let tree = StructTree::from_doc(&doc).expect("tree should parse");
        let n = count_nodes(&tree.children);
        assert!(
            n < 10,
            "self-alias must not explode; materialized {n} nodes"
        );
    }

    #[test]
    fn struct_tree_mutual_alias_kids_terminates() {
        // A → B → A cycle via `/K` must terminate.
        let mut doc = Document::new();
        let a = doc.new_object_id();
        let b = doc.new_object_id();
        doc.set_object(
            a,
            dictionary! {
                "Type" => "StructElem",
                "S" => "Div",
                "K" => vec![Object::Reference(b), Object::Reference(b)],
            },
        );
        doc.set_object(
            b,
            dictionary! {
                "Type" => "StructElem",
                "S" => "Div",
                "K" => vec![Object::Reference(a), Object::Reference(a)],
            },
        );
        let doc = finalize_tagged_doc(doc, a);

        let tree = StructTree::from_doc(&doc).expect("tree should parse");
        let n = count_nodes(&tree.children);
        assert!(
            n < 100,
            "mutual alias must terminate small; materialized {n} nodes"
        );
    }

    #[test]
    fn struct_tree_aliased_dag_respects_node_budget() {
        // Distinct elements, each aliased twice in the next level's `/K`, form a
        // DAG that would expand to 2^depth nodes (the per-path cycle guard does
        // not catch this since every id is on the path only once). The global
        // node budget must cap total materialization.
        let mut doc = Document::new();
        let levels = 22; // 2^22 ≈ 4.2M unbounded, well past the budget
        let ids: Vec<ObjectId> = (0..=levels).map(|_| doc.new_object_id()).collect();
        for i in 0..levels {
            doc.set_object(
                ids[i],
                dictionary! {
                    "Type" => "StructElem",
                    "S" => "Div",
                    "K" => vec![Object::Reference(ids[i + 1]), Object::Reference(ids[i + 1])],
                },
            );
        }
        doc.set_object(
            ids[levels],
            dictionary! { "Type" => "StructElem", "S" => "P" },
        );
        let root_id = doc.add_object(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => vec![Object::Reference(ids[0])],
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "StructTreeRoot" => Object::Reference(root_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let tree = StructTree::from_doc(&doc).expect("tree should parse");
        let n = count_nodes(&tree.children);
        assert!(
            n <= MAX_STRUCT_NODES,
            "node count {n} exceeded budget {MAX_STRUCT_NODES}"
        );
    }

    #[test]
    fn struct_tree_wide_mcid_array_respects_budget() {
        // A single struct element with a `/K` array of bare MCIDs wider than the
        // budget must not allocate `content_refs` without bound — each array item
        // is charged, so materialized marked-content refs stay within the budget.
        let mut doc = Document::new();
        let elem = doc.new_object_id();
        let kids: Vec<Object> = (0..(MAX_STRUCT_NODES as i64 + 100))
            .map(Object::Integer)
            .collect();
        doc.set_object(
            elem,
            dictionary! {
                "Type" => "StructElem",
                "S" => "P",
                "K" => kids,
            },
        );
        let doc = finalize_tagged_doc(doc, elem);

        let tree = StructTree::from_doc(&doc).expect("tree should parse");
        assert!(
            tree.mcid_count() <= MAX_STRUCT_NODES,
            "content_refs unbounded: {} > {MAX_STRUCT_NODES}",
            tree.mcid_count()
        );
    }

    #[test]
    fn budget_charge_flags_truncation_once_exhausted() {
        let mut walk = StructWalk::new();
        walk.budget = 1;
        assert!(walk.charge(), "should spend the last unit");
        assert!(!walk.truncated, "not truncated while budget remained");
        assert!(!walk.charge(), "budget exhausted");
        assert!(walk.truncated, "exhaustion must set the truncation flag");
        // Stays exhausted/flagged on subsequent calls.
        assert!(!walk.charge());
        assert!(walk.truncated);
    }

    #[test]
    fn exhausted_flags_truncation_after_budget_spent_by_charge() {
        // The dominant truncation path: the budget is driven to 0 by a
        // successful `charge()` (which does not set the flag), and remaining
        // items are then dropped by an `exhausted()` guard — which must flag it.
        let mut walk = StructWalk::new();
        walk.budget = 1;
        assert!(walk.charge());
        assert!(
            !walk.truncated,
            "spending the last unit is not truncation yet"
        );
        assert!(walk.exhausted(), "budget is now spent");
        assert!(
            walk.truncated,
            "the guard that skips work must flag truncation"
        );
    }

    #[test]
    fn wide_kids_array_flags_truncation_via_parser() {
        // Reproduce the reviewer's scenario through the real parser: a `/K`
        // array wider than the budget drives the budget to 0 via `charge()`,
        // then the loop guard drops the rest — the truncation flag must be set
        // (so `from_doc` logs it) rather than staying silently false.
        let mut doc = Document::new();
        let elem = doc.new_object_id();
        let kids: Vec<Object> = (0..20i64).map(Object::Integer).collect();
        doc.set_object(
            elem,
            dictionary! { "Type" => "StructElem", "S" => "P", "K" => kids },
        );
        let dict = doc.get_dictionary(elem).unwrap().clone();

        let mut walk = StructWalk::new();
        walk.budget = 5; // smaller than the 20-item `/K` array
        let role_map = HashMap::new();
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &dict, &role_map, None, 0, &mut out, &mut walk);
        assert!(
            walk.truncated,
            "a `/K` array wider than the budget must flag truncation"
        );
    }

    #[test]
    fn cycle_skip_flags_truncation() {
        // A `/K` reference cycle is dropped rather than expanded; that skip must
        // still flag truncation so the one-shot warning fires for malformed
        // trees, not only for budget exhaustion.
        let mut doc = Document::new();
        let elem = doc.new_object_id();
        doc.set_object(
            elem,
            dictionary! {
                "Type" => "StructElem",
                "S" => "Div",
                "K" => vec![Object::Reference(elem), Object::Reference(elem)],
            },
        );
        let dict = doc.get_dictionary(elem).unwrap().clone();

        let mut walk = StructWalk::new();
        walk.active.insert(elem); // simulate `elem` already on the DFS path
        let role_map = HashMap::new();
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &dict, &role_map, None, 0, &mut out, &mut walk);
        assert!(
            walk.truncated,
            "a cycle-skipped `/K` child must flag truncation"
        );
    }

    #[test]
    fn bare_mcid_charges_node_and_reference() {
        // A bare MCID `/K` child becomes a wrapper node carrying one content
        // reference — two materialized items — so it must charge two budget
        // units, not one.
        let doc = Document::new();
        let obj = Object::Integer(7);
        let role_map = HashMap::new();
        let mut out = Vec::new();
        let mut walk = StructWalk::new();
        let before = walk.budget;
        parse_kid(&doc, &obj, &role_map, None, 0, &mut out, &mut walk);
        assert_eq!(
            out.len(),
            1,
            "bare MCID should materialize one wrapper node"
        );
        assert_eq!(
            before - walk.budget,
            2,
            "bare MCID must charge for both the node and its content reference"
        );
    }

    #[test]
    fn mcr_dict_charges_node_and_reference() {
        // A top-level MCR `/K` dict materializes the same wrapper node + content
        // reference as a bare MCID, so it must charge the same two budget units
        // (not one), keeping the per-item budgeting uniform.
        let doc = Document::new();
        let obj = Object::Dictionary(dictionary! { "Type" => "MCR", "MCID" => 3 });
        let role_map = HashMap::new();
        let mut out = Vec::new();
        let mut walk = StructWalk::new();
        let before = walk.budget;
        parse_kid(&doc, &obj, &role_map, None, 0, &mut out, &mut walk);
        assert_eq!(out.len(), 1, "MCR dict should materialize one wrapper node");
        assert_eq!(
            before - walk.budget,
            2,
            "MCR dict must charge for both the node and its content reference"
        );
    }

    #[test]
    fn leaf_wrappers_reserve_both_units_atomically() {
        // With only one unit left, a two-item leaf wrapper (bare MCID or MCR
        // dict) must consume nothing and flag truncation, leaving the unit for a
        // later single-item element instead of half-charging.
        let doc = Document::new();
        let role_map = HashMap::new();

        // Bare MCID via parse_kid.
        let mut walk = StructWalk::new();
        walk.budget = 1;
        let mut out = Vec::new();
        parse_kid(
            &doc,
            &Object::Integer(5),
            &role_map,
            None,
            0,
            &mut out,
            &mut walk,
        );
        assert!(out.is_empty(), "bare MCID must not partially materialize");
        assert_eq!(walk.budget, 1, "the leftover unit must be preserved");
        assert!(walk.truncated);

        // MCR dict via parse_struct_element_dict.
        let mcr = dictionary! { "Type" => "MCR", "MCID" => 1 };
        let mut walk = StructWalk::new();
        walk.budget = 1;
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &mcr, &role_map, None, 0, &mut out, &mut walk);
        assert!(out.is_empty(), "MCR dict must not partially materialize");
        assert_eq!(walk.budget, 1, "the leftover unit must be preserved");
        assert!(walk.truncated);
    }

    #[test]
    fn insufficient_reservation_stops_the_scan() {
        // A one-unit budget is not "exhausted" for a one-unit item, but once a
        // two-unit leaf reservation fails, the walk is stalled so enclosing `/K`
        // loops stop instead of scanning the rest of a wide array.
        let mut walk = StructWalk::new();
        walk.budget = 1;
        assert!(
            !walk.exhausted(),
            "one unit left must still allow a one-unit item"
        );
        assert!(!walk.charge_n(2), "cannot reserve two units from one");
        assert!(
            walk.exhausted(),
            "an insufficient reservation must stop the loop"
        );
        assert!(walk.truncated);
    }

    #[test]
    fn work_budget_bounds_examined_items() {
        let mut walk = StructWalk::new();
        walk.work = 2;
        assert!(walk.spend_work());
        assert!(walk.spend_work());
        assert!(!walk.spend_work(), "traversal budget exhausted");
        assert!(walk.truncated);
    }

    #[test]
    fn wide_unsupported_kids_stop_at_work_budget() {
        // A wide `/K` array of unsupported values (nulls) materializes nothing;
        // it must stop at the traversal budget instead of scanning every entry.
        let mut doc = Document::new();
        let elem = doc.new_object_id();
        let kids: Vec<Object> = (0..1000).map(|_| Object::Null).collect();
        doc.set_object(
            elem,
            dictionary! { "Type" => "StructElem", "S" => "P", "K" => kids },
        );
        let dict = doc.get_dictionary(elem).unwrap().clone();

        let mut walk = StructWalk::new();
        walk.work = 10; // far smaller than the 1000-entry array
        let role_map = HashMap::new();
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &dict, &role_map, None, 0, &mut out, &mut walk);
        assert!(
            walk.truncated,
            "a wide unsupported `/K` array must hit the work budget"
        );
    }

    #[test]
    fn non_materializing_dicts_do_not_charge_node_budget() {
        let doc = Document::new();
        let role_map = HashMap::new();

        // OBJR dict: materializes no node, so it must not spend the node budget.
        let objr = dictionary! { "Type" => "OBJR" };
        let mut walk = StructWalk::new();
        let before = walk.budget;
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &objr, &role_map, None, 0, &mut out, &mut walk);
        assert!(out.is_empty());
        assert_eq!(walk.budget, before, "OBJR must not spend the node budget");

        // A struct dict without a valid /S also materializes nothing.
        let no_s = dictionary! { "Type" => "StructElem" };
        let mut walk = StructWalk::new();
        let before = walk.budget;
        let mut out = Vec::new();
        parse_struct_element_dict(&doc, &no_s, &role_map, None, 0, &mut out, &mut walk);
        assert!(out.is_empty());
        assert_eq!(
            walk.budget, before,
            "a dict without /S must not spend the node budget"
        );
    }
}
