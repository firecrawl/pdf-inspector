//! ToUnicode CMap parsing for PDF text extraction
//!
//! This module parses ToUnicode CMaps to convert CID-encoded text to Unicode.

use log::{debug, warn};
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::glyph_names::glyph_to_char;

/// A parsed ToUnicode CMap mapping CIDs to Unicode strings
#[derive(Debug, Default, Clone)]
pub struct ToUnicodeCMap {
    /// Direct character mappings (CID -> Unicode codepoint(s))
    pub char_map: HashMap<u16, String>,
    /// Range mappings (start_cid, end_cid) -> base_unicode
    pub ranges: Vec<(u16, u16, u32)>,
    /// Byte width of source codes (1 or 2), determined from codespace and CMap entries
    pub code_byte_length: u8,
    /// When true, unmapped CIDs are interpreted as Unicode codepoints directly.
    /// Used as a last resort for Identity-H fonts without ToUnicode/cmap/glyph names.
    pub cid_passthrough: bool,
}

pub(crate) fn build_cmap_entry_from_stream(
    data: &[u8],
    font_dict: &lopdf::Dictionary,
    doc: &Document,
    obj_num: u32,
) -> Option<CMapEntry> {
    if let Some(cmap) = ToUnicodeCMap::parse(data) {
        let (mut primary, mut remapped) = try_remap_subset_cmap(cmap, font_dict, doc, obj_num);
        let mut fallback = build_fallback_tounicode_from_encoding(font_dict, doc)
            .or_else(|| build_fallback_cmap_for_type0(font_dict, doc))
            .or_else(|| build_fallback_cmap_for_simple(font_dict, doc));

        let primary_entries = primary.char_map.len() + primary.ranges.len();
        if primary_entries < 10 {
            if let Some(fb) = fallback.take() {
                debug!(
                    "ToUnicode CMap obj={} too sparse ({} entries); using fallback",
                    obj_num, primary_entries
                );
                remapped = Some(primary);
                primary = fb;
            }
        }

        // When a sequential remap was applied and a TrueType fallback has more
        // entries than the primary ToUnicode CMap, prefer the TrueType cmap.
        // Subset fonts number GIDs by document encounter order, so the sorted
        // sequential remap scrambles characters.  The TrueType cmap table maps
        // the real GID→Unicode and is authoritative.
        if remapped.is_some() {
            if let Some(ref fb) = fallback {
                let fb_entries = fb.char_map.len() + fb.ranges.len();
                if fb_entries > primary_entries {
                    debug!(
                        "ToUnicode CMap obj={}: TrueType fallback ({} entries) > primary ({}); promoting over sequential remap",
                        obj_num, fb_entries, primary_entries
                    );
                    let old_remap = remapped.take().unwrap();
                    remapped = fallback.take();
                    fallback = Some(old_remap);
                }
            }
        }

        return Some(CMapEntry {
            primary,
            remapped,
            fallback,
        });
    }

    let fallback = build_fallback_cmap_for_type0(font_dict, doc)
        .or_else(|| build_fallback_cmap_for_simple(font_dict, doc))?;
    debug!(
        "ToUnicode CMap obj={} parse failed; using fallback (entries={})",
        obj_num,
        fallback.char_map.len()
    );
    Some(CMapEntry {
        primary: fallback,
        remapped: None,
        fallback: None,
    })
}

impl ToUnicodeCMap {
    /// Create a new empty CMap
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a ToUnicode CMap from its decompressed content
    pub fn parse(content: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(content);
        let mut cmap = ToUnicodeCMap::new();
        let mut src_hex_lengths: Vec<usize> = Vec::new();
        let mut use_cmap_name: Option<String> = None;

        // Parse begincodespacerange ... endcodespacerange to determine byte width
        let mut codespace_byte_len: Option<u8> = None;
        if let Some(cs_start) = text.find("begincodespacerange") {
            let section_start = cs_start + "begincodespacerange".len();
            if let Some(cs_end) = text[section_start..].find("endcodespacerange") {
                let section = &text[section_start..section_start + cs_end];
                // Parse hex values to determine byte length
                let mut in_hex = false;
                let mut hex_len = 0;
                for c in section.chars() {
                    if c == '<' {
                        in_hex = true;
                        hex_len = 0;
                    } else if c == '>' {
                        if in_hex && hex_len > 0 {
                            let byte_len = (hex_len + 1) / 2; // 2 hex digits = 1 byte
                            codespace_byte_len = Some(byte_len as u8);
                        }
                        in_hex = false;
                    } else if in_hex && c.is_ascii_hexdigit() {
                        hex_len += 1;
                    }
                }
            }
        }

        // Parse "usecmap" if present
        if let Some(name) = find_usecmap_name(&text) {
            use_cmap_name = Some(name);
        }

        // Parse beginbfchar ... endbfchar sections
        let mut pos = 0;
        while let Some(start) = text[pos..].find("beginbfchar") {
            let section_start = pos + start + "beginbfchar".len();
            if let Some(end) = text[section_start..].find("endbfchar") {
                let section = &text[section_start..section_start + end];
                cmap.parse_bfchar_section(section, &mut src_hex_lengths);
                pos = section_start + end;
            } else {
                break;
            }
        }

        // Parse beginbfrange ... endbfrange sections
        pos = 0;
        while let Some(start) = text[pos..].find("beginbfrange") {
            let section_start = pos + start + "beginbfrange".len();
            if let Some(end) = text[section_start..].find("endbfrange") {
                let section = &text[section_start..section_start + end];
                cmap.parse_bfrange_section(section, &mut src_hex_lengths);
                pos = section_start + end;
            } else {
                break;
            }
        }

        if cmap.char_map.is_empty() && cmap.ranges.is_empty() {
            return None;
        }

        // Determine byte width: use codespace if available, otherwise infer from entries
        cmap.code_byte_length = if let Some(cs_len) = codespace_byte_len {
            // If codespace says 2-byte but ALL entries use 1-byte source codes
            // (hex length <= 2), treat as 1-byte. This handles the common case where
            // codespace is <0000><FFFF> but entries are <20>, <41>, etc.
            if cs_len == 2 && !src_hex_lengths.is_empty() && src_hex_lengths.iter().all(|&l| l <= 2)
            {
                1
            } else {
                cs_len
            }
        } else if !src_hex_lengths.is_empty() {
            // No codespace declaration: infer from entry hex lengths
            let max_hex_len = src_hex_lengths.iter().max().copied().unwrap_or(4);
            if max_hex_len <= 2 {
                1
            } else {
                2
            }
        } else {
            2 // Default to 2-byte
        };

        // Sort ranges by start CID for binary search in lookup()
        cmap.ranges.sort_unstable_by_key(|&(start, _, _)| start);

        if let Some(name) = use_cmap_name {
            if let Some(base) = load_builtin_cmap_by_name(&name) {
                cmap = merge_cmaps(base, cmap);
            } else {
                warn!("usecmap={} could not be loaded", name);
            }
        }

        Some(cmap)
    }

    /// Parse a bfchar section: <src> <dst> pairs
    fn parse_bfchar_section(&mut self, section: &str, src_hex_lengths: &mut Vec<usize>) {
        // Match pairs of hex values: <XXXX> <YYYY>
        let mut chars = section.chars().peekable();

        loop {
            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Look for opening <
            if chars.peek() != Some(&'<') {
                break;
            }
            chars.next(); // consume <

            // Read source hex
            let mut src_hex = String::new();
            while chars.peek().is_some_and(|&c| c != '>') {
                if let Some(c) = chars.next() {
                    src_hex.push(c);
                }
            }
            chars.next(); // consume >

            // Track source hex length for byte width detection
            let trimmed_src = src_hex.trim();
            if !trimmed_src.is_empty() {
                src_hex_lengths.push(trimmed_src.len());
            }

            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Look for opening <
            if chars.peek() != Some(&'<') {
                continue;
            }
            chars.next(); // consume <

            // Read destination hex
            let mut dst_hex = String::new();
            while chars.peek().is_some_and(|&c| c != '>') {
                if let Some(c) = chars.next() {
                    dst_hex.push(c);
                }
            }
            chars.next(); // consume >

            // Parse and store mapping
            if let (Some(src), Some(dst)) =
                (parse_hex_u16(&src_hex), hex_to_unicode_string(&dst_hex))
            {
                self.char_map.insert(src, dst);
            }
        }
    }

    /// Parse a bfrange section: <start> <end> <base> or <start> <end> [<u1> <u2> ...] triplets
    fn parse_bfrange_section(&mut self, section: &str, src_hex_lengths: &mut Vec<usize>) {
        let mut chars = section.chars().peekable();

        loop {
            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Look for opening <
            if chars.peek() != Some(&'<') {
                break;
            }
            chars.next(); // consume <

            // Read start hex
            let mut start_hex = String::new();
            while chars.peek().is_some_and(|&c| c != '>') {
                if let Some(c) = chars.next() {
                    start_hex.push(c);
                }
            }
            chars.next(); // consume >

            // Track source hex length
            let trimmed_start = start_hex.trim();
            if !trimmed_start.is_empty() {
                src_hex_lengths.push(trimmed_start.len());
            }

            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Read end hex
            if chars.peek() != Some(&'<') {
                continue;
            }
            chars.next();
            let mut end_hex = String::new();
            while chars.peek().is_some_and(|&c| c != '>') {
                if let Some(c) = chars.next() {
                    end_hex.push(c);
                }
            }
            chars.next();

            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Read base - could be <hex> or [array]
            if chars.peek() == Some(&'<') {
                chars.next();
                let mut base_hex = String::new();
                while chars.peek().is_some_and(|&c| c != '>') {
                    if let Some(c) = chars.next() {
                        base_hex.push(c);
                    }
                }
                chars.next();

                // Store range mapping
                if let (Some(start), Some(end), Some(base)) = (
                    parse_hex_u16(&start_hex),
                    parse_hex_u16(&end_hex),
                    parse_hex_u32(&base_hex),
                ) {
                    self.ranges.push((start, end, base));
                }
            } else if chars.peek() == Some(&'[') {
                // Array format: [<unicode1> <unicode2> ...]
                // Each entry maps to start_cid + index
                chars.next(); // consume [
                if let (Some(start), Some(end)) =
                    (parse_hex_u16(&start_hex), parse_hex_u16(&end_hex))
                {
                    let mut cid = start;
                    loop {
                        // Skip whitespace
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                        if chars.peek() == Some(&']') {
                            chars.next();
                            break;
                        }
                        if chars.peek() != Some(&'<') {
                            break;
                        }
                        chars.next(); // consume <
                        let mut hex = String::new();
                        while chars.peek().is_some_and(|&c| c != '>') {
                            if let Some(c) = chars.next() {
                                hex.push(c);
                            }
                        }
                        chars.next(); // consume >
                        if let Some(unicode_str) = hex_to_unicode_string(&hex) {
                            self.char_map.insert(cid, unicode_str);
                        }
                        if cid >= end {
                            // Skip remaining entries and closing bracket
                            while chars.peek().is_some_and(|&c| c != ']') {
                                chars.next();
                            }
                            if chars.peek() == Some(&']') {
                                chars.next();
                            }
                            break;
                        }
                        cid = cid.saturating_add(1);
                    }
                } else {
                    // Couldn't parse start/end, skip the array
                    while chars.peek().is_some_and(|&c| c != ']') {
                        chars.next();
                    }
                    if chars.peek() == Some(&']') {
                        chars.next();
                    }
                }
            }
        }
    }

    /// Look up a CID and return the Unicode string
    pub fn lookup(&self, cid: u16) -> Option<String> {
        // First check direct mappings
        if let Some(s) = self.char_map.get(&cid) {
            return Some(s.clone());
        }

        // Binary search through sorted ranges
        let idx = self
            .ranges
            .binary_search_by(|&(start, _, _)| start.cmp(&cid))
            .unwrap_or_else(|i| i);

        // Check the range at idx (where start == cid)
        if idx < self.ranges.len() {
            let (start, end, base) = self.ranges[idx];
            if cid >= start && cid <= end {
                let unicode = base + (cid - start) as u32;
                if let Some(c) = char::from_u32(unicode) {
                    return Some(c.to_string());
                }
            }
        }

        // Check the range before idx (cid may fall within a range that starts before it)
        if idx > 0 {
            let (start, end, base) = self.ranges[idx - 1];
            if cid >= start && cid <= end {
                let unicode = base + (cid - start) as u32;
                if let Some(c) = char::from_u32(unicode) {
                    return Some(c.to_string());
                }
            }
        }

        None
    }

    /// Per-byte CMap lookup without Latin-1 fallback.
    /// Returns `(raw_byte, Option<cmap_result>)` for each byte.
    /// Only meaningful for single-byte (code_byte_length==1) CMaps.
    pub fn lookup_bytes(&self, bytes: &[u8]) -> Vec<(u8, Option<String>)> {
        bytes
            .iter()
            .map(|&b| {
                let code = b as u16;
                let result = self.lookup(code).filter(|s| !s.contains('\u{FFFD}'));
                (b, result)
            })
            .collect()
    }

    /// Decode a byte slice to a Unicode string, respecting the CMap's code byte width
    pub fn decode_cids(&self, bytes: &[u8]) -> String {
        let mut result = String::new();
        let mut unmapped_count = 0usize;

        if self.code_byte_length == 1 {
            // Single-byte codes: each byte is a code
            for &b in bytes {
                let code = b as u16;
                match self.lookup(code) {
                    Some(s) if !s.contains('\u{FFFD}') => result.push_str(&s),
                    _ => {
                        // For single-byte unmapped codes, try as Latin-1
                        // (the byte IS the character code in most legacy encodings)
                        if b >= 0x20 {
                            result.push(b as char);
                        }
                        unmapped_count += 1;
                    }
                }
            }
        } else {
            // Two-byte codes: CIDs are 2 bytes each (big-endian)
            for chunk in bytes.chunks(2) {
                if chunk.len() == 2 {
                    let cid = u16::from_be_bytes([chunk[0], chunk[1]]);
                    match self.lookup(cid) {
                        Some(s) if !s.contains('\u{FFFD}') => result.push_str(&s),
                        _ => {
                            if self.cid_passthrough {
                                // Last-resort: treat CID as Unicode codepoint.
                                // Valid for Identity-H fonts where the PDF generator
                                // used Unicode values as CIDs but stripped the cmap.
                                if let Some(ch) = char::from_u32(cid as u32) {
                                    if !ch.is_control() || ch == '\t' || ch == '\n' {
                                        result.push(ch);
                                    } else {
                                        unmapped_count += 1;
                                    }
                                } else {
                                    unmapped_count += 1;
                                }
                            } else {
                                // CIDs are font-internal indices, not Unicode values.
                                // Unmapped 2-byte CIDs are skipped to avoid CJK garbage.
                                unmapped_count += 1;
                            }
                        }
                    }
                }
            }
        }

        // If too many codes were unmapped, signal failure by returning empty
        // so the caller can fall through to other decoding methods
        let total = if self.code_byte_length == 1 {
            bytes.len()
        } else {
            bytes.len() / 2
        };
        if total > 0 && unmapped_count > total / 2 {
            return String::new();
        }

        result
    }

    /// Get the minimum source CID across all mappings (char_map + ranges).
    fn min_source_cid(&self) -> Option<u16> {
        let char_min = self.char_map.keys().copied().min();
        let range_min = self.ranges.iter().map(|&(start, _, _)| start).min();
        match (char_min, range_min) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a @ Some(_), None) => a,
            (None, b @ Some(_)) => b,
            (None, None) => None,
        }
    }

    /// Remap a CMap that references pre-subsetting GIDs to sequential post-subsetting GIDs.
    /// Collects all source CIDs, sorts them, and reassigns to 1, 2, 3, ...
    pub fn remap_to_sequential(&self) -> ToUnicodeCMap {
        let mut cid_to_unicode: HashMap<u16, String> = HashMap::new();

        // Expand ranges first
        for &(start, end, base) in &self.ranges {
            for cid in start..=end {
                let unicode_cp = base + (cid - start) as u32;
                if let Some(ch) = char::from_u32(unicode_cp) {
                    cid_to_unicode.insert(cid, ch.to_string());
                }
            }
        }

        // char_map entries override range entries
        for (&cid, unicode) in &self.char_map {
            cid_to_unicode.insert(cid, unicode.clone());
        }

        // Sort old CIDs ascending
        let mut old_cids: Vec<u16> = cid_to_unicode.keys().copied().collect();
        old_cids.sort_unstable();

        // Build new CMap with sequential CIDs starting at 1
        let mut new_cmap = ToUnicodeCMap::new();
        for (i, &old_cid) in old_cids.iter().enumerate() {
            let new_cid = (i + 1) as u16; // GID 0 is .notdef, content CIDs start at 1
            if let Some(unicode) = cid_to_unicode.get(&old_cid) {
                new_cmap.char_map.insert(new_cid, unicode.clone());
            }
        }
        new_cmap.code_byte_length = self.code_byte_length;

        new_cmap
    }
}

/// Parse a hex string to u16
fn parse_hex_u16(hex: &str) -> Option<u16> {
    u16::from_str_radix(hex.trim(), 16).ok()
}

/// Parse a hex string to u32
fn parse_hex_u32(hex: &str) -> Option<u32> {
    u32::from_str_radix(hex.trim(), 16).ok()
}

/// Convert a hex string to a Unicode string
/// Handles both 2-byte (BMP) and 4-byte (supplementary) codepoints
fn hex_to_unicode_string(hex: &str) -> Option<String> {
    let hex = hex.trim();
    let mut result = String::new();

    // Process 4 hex digits at a time
    let mut i = 0;
    while i + 4 <= hex.len() {
        if let Ok(cp) = u32::from_str_radix(&hex[i..i + 4], 16) {
            if let Some(c) = char::from_u32(cp) {
                result.push(c);
            }
        }
        i += 4;
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn find_usecmap_name(text: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains("usecmap") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for i in 0..parts.len() {
                if parts[i] == "usecmap" && i > 0 {
                    let name = parts[i - 1].trim();
                    if let Some(stripped) = name.strip_prefix('/') {
                        return Some(stripped.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Navigate to the first DescendantFont dictionary of a Type0 font.
fn get_descendant_cid_font<'a>(
    font_dict: &'a lopdf::Dictionary,
    doc: &'a Document,
) -> Option<&'a lopdf::Dictionary> {
    let desc_fonts_obj = font_dict.get(b"DescendantFonts").ok()?;
    let arr = match desc_fonts_obj {
        Object::Array(arr) => arr,
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Array(arr)) => arr,
            _ => return None,
        },
        _ => return None,
    };
    if arr.is_empty() {
        return None;
    }
    match &arr[0] {
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// Get the starting CID from a CIDFont's W (widths) array.
fn get_w_array_start_cid(cid_font_dict: &lopdf::Dictionary, doc: &Document) -> Option<u16> {
    let w_obj = cid_font_dict.get(b"W").ok()?;
    let arr = match w_obj {
        Object::Array(arr) => arr,
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Array(arr)) => arr,
            _ => return None,
        },
        _ => return None,
    };
    if arr.is_empty() {
        return None;
    }
    match &arr[0] {
        Object::Integer(n) => Some(*n as u16),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Integer(n)) => Some(*n as u16),
            _ => None,
        },
        _ => None,
    }
}

/// Extract CIDToGIDMap as a vector of GIDs (u16) indexed by CID.
fn get_cid_to_gid_map(cid_font_dict: &lopdf::Dictionary, doc: &Document) -> Option<Vec<u16>> {
    let obj = cid_font_dict.get(b"CIDToGIDMap").ok()?;
    match obj {
        Object::Name(n) if n.as_slice() == b"Identity" => None,
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Stream(s)) => parse_cid_to_gid_stream(&s.decompressed_content().ok()?),
            _ => None,
        },
        Object::Stream(s) => parse_cid_to_gid_stream(&s.decompressed_content().ok()?),
        _ => None,
    }
}

fn parse_cid_to_gid_stream(data: &[u8]) -> Option<Vec<u16>> {
    if data.len() < 2 {
        return None;
    }
    let mut map = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        map.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    Some(map)
}

/// Build a CID→Unicode CMap by applying a CIDToGIDMap to an existing CMap that maps GID→Unicode.
fn build_cmap_with_cid_to_gid_map(
    cmap: &ToUnicodeCMap,
    cid_to_gid: &[u16],
) -> Option<ToUnicodeCMap> {
    let mut new_cmap = ToUnicodeCMap::new();
    for (cid, &gid) in cid_to_gid.iter().enumerate() {
        if let Some(s) = cmap.lookup(gid) {
            new_cmap.char_map.insert(cid as u16, s);
        }
    }
    if new_cmap.char_map.is_empty() {
        None
    } else {
        new_cmap.code_byte_length = 2;
        Some(new_cmap)
    }
}

/// Detect and fix broken ToUnicode CMaps from subset fonts with GID mismatch.
///
/// Some PDF generators subset-embed fonts by renumbering GIDs sequentially (1, 2, 3...)
/// but fail to update the ToUnicode CMap, which still references original GID values.
/// This detects the mismatch and remaps the CMap to sequential positions.
fn try_remap_subset_cmap(
    cmap: ToUnicodeCMap,
    font_dict: &lopdf::Dictionary,
    doc: &Document,
    obj_num: u32,
) -> (ToUnicodeCMap, Option<ToUnicodeCMap>) {
    // Only applies to Identity-H/V CID fonts
    let encoding = font_dict
        .get(b"Encoding")
        .ok()
        .and_then(|o| o.as_name().ok());
    if encoding != Some(b"Identity-H") && encoding != Some(b"Identity-V") {
        return (cmap, None);
    }

    // CMap's minimum source CID must be > 2 (indicating old, non-sequential GIDs)
    let min_cid = match cmap.min_source_cid() {
        Some(c) if c > 2 => c,
        _ => return (cmap, None),
    };

    // Navigate to DescendantFonts[0]
    let cid_font_dict = match get_descendant_cid_font(font_dict, doc) {
        Some(d) => d,
        None => return (cmap, None),
    };

    // If there's an explicit CIDToGIDMap, build a repaired CMap using it.
    if let Some(cid_to_gid) = get_cid_to_gid_map(cid_font_dict, doc) {
        if let Some(repaired) = build_cmap_with_cid_to_gid_map(&cmap, &cid_to_gid) {
            debug!(
                "CIDToGIDMap repair applied for obj={}: {} entries",
                obj_num,
                repaired.char_map.len()
            );
            return (cmap, Some(repaired));
        }
        // Fall through to sequential remap if repair failed.
    }

    // W array must start at a low CID (≤ 2), indicating sequential post-subset GIDs
    let w_start = match get_w_array_start_cid(cid_font_dict, doc) {
        Some(c) if c <= 2 => c,
        _ => return (cmap, None),
    };

    debug!(
        "Subset GID mismatch detected for obj={}: W starts at CID {}, CMap min CID {}. Remapping to sequential.",
        obj_num, w_start, min_cid
    );

    let remapped = cmap.remap_to_sequential();
    (cmap, Some(remapped))
}

/// Build a ToUnicodeCMap from an embedded TrueType font's cmap table.
///
/// For Identity-H CID fonts, CID == GID. The TrueType cmap maps Unicode→GID,
/// so we reverse it to get GID→Unicode (i.e. CID→Unicode).
pub fn build_cmap_from_truetype(font_data: &[u8]) -> Option<ToUnicodeCMap> {
    let face = ttf_parser::Face::parse(font_data, 0).ok()?;
    let gid_to_unicode = build_gid_to_unicode(&face)?;

    debug!(
        "TrueType cmap: {} GID→Unicode entries",
        gid_to_unicode.len()
    );

    let mut cmap = ToUnicodeCMap::new();
    for (gid, ch) in &gid_to_unicode {
        cmap.char_map.insert(*gid, ch.to_string());
    }
    cmap.code_byte_length = 2; // Identity-H uses 2-byte CIDs

    Some(cmap)
}

/// Build a single-byte CMap for simple fonts by treating the character code
/// as a glyph id (best-effort fallback when no usable ToUnicode exists).
fn build_simple_cmap_from_truetype(font_data: &[u8]) -> Option<ToUnicodeCMap> {
    let face = ttf_parser::Face::parse(font_data, 0).ok()?;
    let gid_to_unicode = build_gid_to_unicode(&face)?;

    let mut cmap = ToUnicodeCMap::new();

    // Use the font's encoding cmap subtable for proper code→GID→Unicode mapping.
    // In subsetted TrueType fonts, GID ≠ character code, so we need the cmap table
    // to translate byte codes (as used in the PDF content stream) to GIDs.
    let mut used_encoding_cmap = false;
    if let Some(cmap_table) = face.tables().cmap {
        // Prefer Mac Roman (1,0): maps byte codes 0–255 directly to GIDs.
        for subtable in cmap_table.subtables {
            if subtable.platform_id == ttf_parser::PlatformId::Macintosh
                && subtable.encoding_id == 0
            {
                for code in 0x20..=0xFF_u32 {
                    if let Some(gid) = subtable.glyph_index(code) {
                        if let Some(&ch) = gid_to_unicode.get(&gid.0) {
                            let ch = strip_pua_char(ch);
                            cmap.char_map.entry(code as u16).or_insert(ch.to_string());
                        }
                    }
                }
                used_encoding_cmap = true;
                break;
            }
        }
        // Fallback: Windows Symbol (3,0) — maps F000+byte to GIDs.
        if !used_encoding_cmap {
            for subtable in cmap_table.subtables {
                if subtable.platform_id == ttf_parser::PlatformId::Windows
                    && subtable.encoding_id == 0
                {
                    for code in 0x20..=0xFF_u32 {
                        if let Some(gid) = subtable.glyph_index(code + 0xF000) {
                            if let Some(&ch) = gid_to_unicode.get(&gid.0) {
                                let ch = strip_pua_char(ch);
                                cmap.char_map.entry(code as u16).or_insert(ch.to_string());
                            }
                        }
                    }
                    used_encoding_cmap = true;
                    break;
                }
            }
        }
        // Fallback: Windows Unicode BMP (3,1) — maps Unicode codepoints to GIDs.
        // For single-byte fonts, try each byte value as a Unicode codepoint.
        // Common in OCR-generated PDFs where byte values correspond to Unicode
        // codepoints but the declared encoding (WinAnsiEncoding) is wrong.
        if !used_encoding_cmap {
            for subtable in cmap_table.subtables {
                if subtable.platform_id == ttf_parser::PlatformId::Windows
                    && subtable.encoding_id == 1
                {
                    for code in 0x20..=0xFF_u32 {
                        if let Some(gid) = subtable.glyph_index(code) {
                            if let Some(&ch) = gid_to_unicode.get(&gid.0) {
                                let ch = strip_pua_char(ch);
                                cmap.char_map.entry(code as u16).or_insert(ch.to_string());
                            }
                        }
                    }
                    used_encoding_cmap = true;
                    break;
                }
            }
        }
    }

    if !used_encoding_cmap {
        // No encoding cmap found — fall back to treating GID as code.
        for (&gid, &ch) in &gid_to_unicode {
            if gid <= 0xFF {
                cmap.char_map.insert(gid, ch.to_string());
            }
        }
        // Fill missing single-byte codes from glyph names (helps with ligatures like "t_i").
        for gid_idx in 0..face.number_of_glyphs() {
            let gid = ttf_parser::GlyphId(gid_idx);
            let gid_val = gid.0;
            if gid_val > 0xFF || cmap.char_map.contains_key(&gid_val) {
                continue;
            }
            if let Some(name) = face.glyph_name(gid) {
                if let Some(s) = glyph_name_to_string(name) {
                    cmap.char_map.insert(gid_val, s);
                }
            }
        }
    }

    if cmap.char_map.is_empty() {
        return None;
    }
    debug!(
        "TrueType simple cmap: {} code→Unicode entries",
        cmap.char_map.len()
    );
    cmap.code_byte_length = 1;
    Some(cmap)
}

/// Strip Private Use Area F000 offset (Windows Symbol encoding convention).
fn strip_pua_char(ch: char) -> char {
    let cp = ch as u32;
    if (0xF000..=0xF0FF).contains(&cp) {
        char::from_u32(cp - 0xF000).unwrap_or(ch)
    } else {
        ch
    }
}

fn glyph_name_to_string(name: &str) -> Option<String> {
    let base = name.split('.').next().unwrap_or(name);
    if let Some(ch) = glyph_to_char(base) {
        return Some(ch.to_string());
    }
    if base.contains('_') {
        let mut out = String::new();
        for part in base.split('_') {
            if part.is_empty() {
                return None;
            }
            if let Some(ch) = glyph_to_char(part) {
                out.push(ch);
            } else if part.len() == 1 {
                out.push(part.chars().next().unwrap());
            } else {
                return None;
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    if matches!(base, "ti" | "tt" | "tz") {
        return Some(base.to_string());
    }
    None
}

/// Build a ToUnicodeCMap from a font's glyph names (post table).
/// Uses Adobe Glyph List to map glyph names to Unicode.
fn build_cmap_from_glyph_names(face: &ttf_parser::Face<'_>) -> Option<ToUnicodeCMap> {
    let mut cmap = ToUnicodeCMap::new();

    for gid in 0..face.number_of_glyphs() {
        let gid = ttf_parser::GlyphId(gid);
        if let Some(name) = face.glyph_name(gid) {
            if let Some(ch) = glyph_to_char(name) {
                cmap.char_map.insert(gid.0, ch.to_string());
            }
        }
    }

    if cmap.char_map.is_empty() {
        return None;
    }

    debug!(
        "TrueType post glyph names: {} GID→Unicode entries",
        cmap.char_map.len()
    );
    cmap.code_byte_length = 2;
    Some(cmap)
}

fn build_gid_to_unicode(face: &ttf_parser::Face<'_>) -> Option<HashMap<u16, char>> {
    let mut gid_to_unicode: HashMap<u16, char> = HashMap::new();

    // Iterate all Unicode codepoints that have a glyph mapping.
    // For each codepoint, the face gives us a GlyphId; reverse that to GID→Unicode.
    // We prefer the first (lowest) codepoint for each GID to handle duplicates.
    for subtable in face.tables().cmap.iter().flat_map(|cmap| cmap.subtables) {
        let is_symbol =
            subtable.platform_id == ttf_parser::PlatformId::Windows && subtable.encoding_id == 0;
        if !subtable.is_unicode() && !is_symbol {
            continue;
        }
        subtable.codepoints(|cp| {
            if let Some(ch) = char::from_u32(cp) {
                if let Some(gid) = subtable.glyph_index(cp) {
                    let gid_val = gid.0;
                    gid_to_unicode.entry(gid_val).or_insert(ch);
                }
            }
        });
    }

    if gid_to_unicode.is_empty() {
        return build_cmap_from_glyph_names(face).map(|cmap| {
            let mut map = HashMap::new();
            for (gid, s) in cmap.char_map {
                if let Some(ch) = s.chars().next() {
                    map.insert(gid, ch);
                }
            }
            map
        });
    }

    Some(gid_to_unicode)
}

/// Build a ToUnicodeCMap from pdf.js built-in binary CMaps (bcmaps).
fn build_cmap_from_builtin_cmap(ordering: &str) -> Option<ToUnicodeCMap> {
    let name = format!("Adobe-{}-UCS2.bcmap", ordering);
    let dir = find_bcmaps_dir()?;
    let path = dir.join(name);
    let data = std::fs::read(&path).ok()?;
    let mut cmap = parse_binary_cmap(&data).ok()?;
    if cmap.char_map.is_empty() && cmap.ranges.is_empty() {
        return None;
    }
    cmap.code_byte_length = 2;
    debug!(
        "Built-in CMap {}: char_map={} ranges={}",
        path.display(),
        cmap.char_map.len(),
        cmap.ranges.len()
    );
    Some(cmap)
}

fn find_bcmaps_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PDF_INSPECTOR_BCMAPS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    let default = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("external")
        .join("bcmaps");
    if default.is_dir() {
        return Some(default);
    }
    None
}

fn parse_binary_cmap(data: &[u8]) -> Result<ToUnicodeCMap, String> {
    let mut stream = BinaryCMapStream::new(data);
    let _header = stream.read_byte().ok_or("unexpected EOF in bcmap header")?;

    let mut cmap = ToUnicodeCMap::new();
    let mut use_cmap: Option<String> = None;

    while let Some(b) = stream.read_byte() {
        let typ = b >> 5;
        if typ == 7 {
            match b & 0x1f {
                0 => {
                    stream.read_string()?;
                }
                1 => {
                    let name = stream.read_string()?;
                    use_cmap = Some(name);
                }
                _ => {}
            }
            continue;
        }
        let sequence = (b & 0x10) != 0;
        let data_size = (b & 0x0f) as usize;
        if data_size + 1 > 16 {
            return Err("invalid dataSize in bcmap".to_string());
        }
        let subitems = stream.read_number()? as usize;
        match typ {
            4 => {
                // bfchar
                for i in 0..subitems {
                    let src = stream.read_hex_number(1)?;
                    let dst = stream.read_hex_bytes(data_size + 1)?;
                    let src_code = hex_to_u32(&src) as u16;
                    if let Some(s) = bytes_to_unicode_string(&dst) {
                        cmap.char_map.insert(src_code, s);
                    }
                    if i + 1 < subitems && sequence {
                        // sequence handled by encoded data, nothing to do
                    }
                }
            }
            5 => {
                // bfrange
                for _ in 0..subitems {
                    let start = stream.read_hex_number(1)?;
                    let end_delta = stream.read_hex_number(1)?;
                    let mut end = start.clone();
                    add_hex(&mut end, &end_delta);
                    let dst = stream.read_hex_bytes(data_size + 1)?;
                    let start_code = hex_to_u32(&start) as u16;
                    let end_code = hex_to_u32(&end) as u16;
                    if let Some(s) = bytes_to_unicode_string(&dst) {
                        if s.chars().count() == 1 {
                            let base = s.chars().next().unwrap() as u32;
                            cmap.ranges.push((start_code, end_code, base));
                        } else {
                            // Expand multi-char sequences
                            let mut cid = start_code;
                            for ch in s.chars() {
                                cmap.char_map.insert(cid, ch.to_string());
                                if cid == end_code {
                                    break;
                                }
                                cid = cid.saturating_add(1);
                            }
                        }
                    }
                }
            }
            _ => {
                // Skip unsupported types by consuming their payload.
                // We only implement bfchar/bfrange for UCS2 maps.
                for _ in 0..subitems {
                    // Best-effort skip: read a few fields based on type.
                    if typ <= 3 {
                        let _ = stream.read_hex_number(data_size)?;
                        let _ = stream.read_hex_number(data_size)?;
                        if typ >= 1 {
                            let _ = stream.read_number()?;
                        }
                    }
                }
            }
        }
    }

    cmap.ranges.sort_unstable_by_key(|&(start, _, _)| start);
    if let Some(name) = use_cmap {
        if let Some(base) = load_builtin_cmap_by_name(&name) {
            cmap = merge_cmaps(base, cmap);
        } else {
            warn!("bcmap usecmap={} could not be loaded", name);
        }
    }
    Ok(cmap)
}

struct BinaryCMapStream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryCMapStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_byte(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            None
        } else {
            let b = self.data[self.pos];
            self.pos += 1;
            Some(b)
        }
    }

    fn read_number(&mut self) -> Result<u32, String> {
        let mut n = 0u32;
        loop {
            let b = self.read_byte().ok_or("unexpected EOF in bcmap")?;
            let last = (b & 0x80) == 0;
            n = (n << 7) | (b & 0x7f) as u32;
            if last {
                break;
            }
        }
        Ok(n)
    }

    fn read_hex_number(&mut self, size: usize) -> Result<Vec<u8>, String> {
        // encoded 7-bit number into size+1 bytes
        let mut stack = Vec::new();
        loop {
            let b = self.read_byte().ok_or("unexpected EOF in bcmap")?;
            let last = (b & 0x80) == 0;
            stack.push(b & 0x7f);
            if last {
                break;
            }
        }
        let mut out = vec![0u8; size + 1];
        let mut buffer = 0u32;
        let mut buffer_size = 0u32;
        let mut i: i32 = size as i32;
        while i >= 0 {
            while buffer_size < 8 && !stack.is_empty() {
                buffer |= (stack.pop().unwrap() as u32) << buffer_size;
                buffer_size += 7;
            }
            out[i as usize] = (buffer & 0xff) as u8;
            buffer >>= 8;
            buffer_size = buffer_size.saturating_sub(8);
            i -= 1;
        }
        Ok(out)
    }

    fn read_hex_bytes(&mut self, len: usize) -> Result<Vec<u8>, String> {
        if self.pos + len > self.data.len() {
            return Err("unexpected EOF in bcmap".to_string());
        }
        let out = self.data[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(out)
    }

    fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_number()? as usize;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            let v = self.read_number()? as u8;
            buf.push(v);
        }
        String::from_utf8(buf).map_err(|e| e.to_string())
    }
}

fn hex_to_u32(bytes: &[u8]) -> u32 {
    let mut n = 0u32;
    for &b in bytes {
        n = (n << 8) | b as u32;
    }
    n
}

fn add_hex(a: &mut [u8], b: &[u8]) {
    let mut c = 0u16;
    for i in (0..a.len()).rev() {
        c += a[i] as u16 + b[i] as u16;
        a[i] = (c & 0xff) as u8;
        c >>= 8;
    }
}

fn bytes_to_unicode_string(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes.len().is_multiple_of(2) {
        // Treat as latin-1 bytes
        return Some(bytes.iter().map(|&b| b as char).collect());
    }
    let mut out = String::new();
    for chunk in bytes.chunks_exact(2) {
        let cp = u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
        if let Some(ch) = char::from_u32(cp) {
            out.push(ch);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[derive(Debug, Clone)]
struct EncodingCMap {
    map: HashMap<u16, u16>,
    code_byte_length: u8,
    is_identity: bool,
}

fn build_fallback_tounicode_from_encoding(
    font_dict: &lopdf::Dictionary,
    doc: &Document,
) -> Option<ToUnicodeCMap> {
    let encoding = build_encoding_cmap_from_font(font_dict, doc)?;
    let ordering = get_cid_system_info_ordering(font_dict, doc)?;
    let ucs2 = build_cmap_from_builtin_cmap(&ordering)?;

    if encoding.is_identity {
        // Identity mapping: charcode == CID
        return Some(ucs2);
    }

    let mut cmap = ToUnicodeCMap::new();
    for (charcode, cid) in encoding.map {
        if let Some(s) = ucs2.lookup(cid) {
            cmap.char_map.insert(charcode, s);
        }
    }
    if cmap.char_map.is_empty() {
        return None;
    }
    cmap.code_byte_length = encoding.code_byte_length;
    Some(cmap)
}

fn get_cid_system_info_ordering(font_dict: &lopdf::Dictionary, doc: &Document) -> Option<String> {
    let cid_font_dict = get_descendant_cid_font(font_dict, doc)?;
    let csi_obj = cid_font_dict.get(b"CIDSystemInfo").ok()?;
    let csi_dict = match csi_obj {
        Object::Reference(r) => doc.get_dictionary(*r).ok()?,
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let ordering = csi_dict.get(b"Ordering").ok().and_then(|o| {
        if let Object::String(bytes, _) = o {
            Some(String::from_utf8_lossy(bytes).to_string())
        } else {
            None
        }
    })?;
    Some(ordering)
}

fn build_encoding_cmap_from_font(
    font_dict: &lopdf::Dictionary,
    doc: &Document,
) -> Option<EncodingCMap> {
    let encoding_obj = font_dict.get(b"Encoding").ok()?;
    match encoding_obj {
        Object::Name(name) => {
            let enc = name.as_slice();
            if enc == b"Identity-H" || enc == b"Identity-V" {
                return Some(EncodingCMap {
                    map: HashMap::new(),
                    code_byte_length: 2,
                    is_identity: true,
                });
            }
            let enc_name = String::from_utf8_lossy(enc).to_string();
            load_builtin_encoding_cmap(&enc_name)
        }
        Object::Reference(r) => {
            let obj = doc.get_object(*r).ok()?;
            parse_encoding_cmap_object(obj, doc)
        }
        Object::Stream(s) => parse_encoding_cmap_stream(&s.decompressed_content().ok()?),
        Object::Dictionary(_) => None,
        _ => None,
    }
}

fn parse_encoding_cmap_object(obj: &Object, doc: &Document) -> Option<EncodingCMap> {
    match obj {
        Object::Stream(s) => parse_encoding_cmap_stream(&s.decompressed_content().ok()?),
        Object::Reference(r) => {
            let obj = doc.get_object(*r).ok()?;
            parse_encoding_cmap_object(obj, doc)
        }
        _ => None,
    }
}

fn load_builtin_encoding_cmap(name: &str) -> Option<EncodingCMap> {
    let dir = find_bcmaps_dir()?;
    let path = dir.join(format!("{}.bcmap", name));
    let data = std::fs::read(&path).ok()?;
    parse_binary_cmap_encoding(&data).ok()
}

fn parse_encoding_cmap_stream(data: &[u8]) -> Option<EncodingCMap> {
    let text = String::from_utf8_lossy(data);
    let mut src_hex_lengths: Vec<usize> = Vec::new();
    let mut codespace_byte_len: Option<u8> = None;

    if let Some(cs_start) = text.find("begincodespacerange") {
        let section_start = cs_start + "begincodespacerange".len();
        if let Some(cs_end) = text[section_start..].find("endcodespacerange") {
            let section = &text[section_start..section_start + cs_end];
            let mut in_hex = false;
            let mut hex_len = 0;
            for c in section.chars() {
                if c == '<' {
                    in_hex = true;
                    hex_len = 0;
                } else if c == '>' {
                    if in_hex && hex_len > 0 {
                        let byte_len = (hex_len + 1) / 2;
                        codespace_byte_len = Some(byte_len as u8);
                    }
                    in_hex = false;
                } else if in_hex && c.is_ascii_hexdigit() {
                    hex_len += 1;
                }
            }
        }
    }

    let mut map = HashMap::new();
    let mut pos = 0;
    while let Some(start) = text[pos..].find("begincidchar") {
        let section_start = pos + start + "begincidchar".len();
        if let Some(end) = text[section_start..].find("endcidchar") {
            let section = &text[section_start..section_start + end];
            parse_cidchar_section(section, &mut map, &mut src_hex_lengths);
            pos = section_start + end;
        } else {
            break;
        }
    }
    pos = 0;
    while let Some(start) = text[pos..].find("begincidrange") {
        let section_start = pos + start + "begincidrange".len();
        if let Some(end) = text[section_start..].find("endcidrange") {
            let section = &text[section_start..section_start + end];
            parse_cidrange_section(section, &mut map, &mut src_hex_lengths);
            pos = section_start + end;
        } else {
            break;
        }
    }

    if map.is_empty() {
        return None;
    }

    let code_byte_length = if let Some(cs_len) = codespace_byte_len {
        cs_len
    } else if !src_hex_lengths.is_empty() {
        let max_hex_len = src_hex_lengths.iter().max().copied().unwrap_or(4);
        if max_hex_len <= 2 {
            1
        } else {
            2
        }
    } else {
        2
    };

    Some(EncodingCMap {
        map,
        code_byte_length,
        is_identity: false,
    })
}

fn parse_cidchar_section(
    section: &str,
    map: &mut HashMap<u16, u16>,
    src_hex_lengths: &mut Vec<usize>,
) {
    let mut chars = section.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek() != Some(&'<') {
            break;
        }
        chars.next();
        let mut src_hex = String::new();
        while chars.peek().is_some_and(|&c| c != '>') {
            if let Some(c) = chars.next() {
                src_hex.push(c);
            }
        }
        chars.next();
        let trimmed_src = src_hex.trim();
        if !trimmed_src.is_empty() {
            src_hex_lengths.push(trimmed_src.len());
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut cid_str = String::new();
        while chars.peek().is_some_and(|c| !c.is_whitespace()) {
            if let Some(c) = chars.next() {
                cid_str.push(c);
            }
        }
        if let (Some(code), Ok(cid)) = (parse_hex_u16(&src_hex), cid_str.parse::<u16>()) {
            map.insert(code, cid);
        }
    }
}

fn parse_cidrange_section(
    section: &str,
    map: &mut HashMap<u16, u16>,
    src_hex_lengths: &mut Vec<usize>,
) {
    let mut chars = section.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek() != Some(&'<') {
            break;
        }
        chars.next();
        let mut start_hex = String::new();
        while chars.peek().is_some_and(|&c| c != '>') {
            if let Some(c) = chars.next() {
                start_hex.push(c);
            }
        }
        chars.next();
        let trimmed_start = start_hex.trim();
        if !trimmed_start.is_empty() {
            src_hex_lengths.push(trimmed_start.len());
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek() != Some(&'<') {
            continue;
        }
        chars.next();
        let mut end_hex = String::new();
        while chars.peek().is_some_and(|&c| c != '>') {
            if let Some(c) = chars.next() {
                end_hex.push(c);
            }
        }
        chars.next();
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut cid_str = String::new();
        while chars.peek().is_some_and(|c| !c.is_whitespace()) {
            if let Some(c) = chars.next() {
                cid_str.push(c);
            }
        }
        let (Some(start), Some(end), Ok(start_cid)) = (
            parse_hex_u16(&start_hex),
            parse_hex_u16(&end_hex),
            cid_str.parse::<u16>(),
        ) else {
            continue;
        };
        let mut cid = start_cid;
        for code in start..=end {
            map.insert(code, cid);
            cid = cid.saturating_add(1);
        }
    }
}

fn parse_binary_cmap_encoding(data: &[u8]) -> Result<EncodingCMap, String> {
    let mut stream = BinaryCMapStream::new(data);
    let _header = stream.read_byte().ok_or("unexpected EOF in bcmap header")?;
    let mut map: HashMap<u16, u16> = HashMap::new();
    let mut max_code_size: u8 = 1;
    let mut use_cmap: Option<String> = None;

    while let Some(b) = stream.read_byte() {
        let typ = b >> 5;
        if typ == 7 {
            match b & 0x1f {
                0 => {
                    stream.read_string()?;
                }
                1 => {
                    let name = stream.read_string()?;
                    use_cmap = Some(name);
                }
                _ => {}
            }
            continue;
        }
        let _sequence = (b & 0x10) != 0;
        let data_size = (b & 0x0f) as usize;
        if data_size + 1 > 16 {
            return Err("invalid dataSize in bcmap".to_string());
        }
        max_code_size = max_code_size.max((data_size + 1) as u8);
        let subitems = stream.read_number()? as usize;
        match typ {
            2 => {
                // cidchar
                let mut prev_code: u32 = 0;
                for i in 0..subitems {
                    let code_bytes = stream.read_hex_number(data_size)?;
                    let code = hex_to_u32(&code_bytes);
                    let cid = stream.read_number()? as u16;
                    if i == 0 {
                        prev_code = code;
                        map.insert(code as u16, cid);
                        continue;
                    }
                    if _sequence {
                        prev_code = prev_code.saturating_add(1);
                        map.insert(prev_code as u16, cid);
                    } else {
                        map.insert(code as u16, cid);
                        prev_code = code;
                    }
                }
            }
            3 => {
                // cidrange
                for _ in 0..subitems {
                    let start = stream.read_hex_number(data_size)?;
                    let end_delta = stream.read_hex_number(data_size)?;
                    let mut end = start.clone();
                    add_hex(&mut end, &end_delta);
                    let cid_start = stream.read_number()? as u16;
                    let start_code = hex_to_u32(&start) as u16;
                    let end_code = hex_to_u32(&end) as u16;
                    let mut cid = cid_start;
                    for code in start_code..=end_code {
                        map.insert(code, cid);
                        cid = cid.saturating_add(1);
                    }
                }
            }
            _ => {
                // Skip other types
                for _ in 0..subitems {
                    let _ = stream.read_hex_number(data_size)?;
                    let _ = stream.read_hex_number(data_size)?;
                    let _ = stream.read_number()?;
                }
            }
        }
    }

    if let Some(name) = use_cmap {
        if let Some(base) = load_builtin_encoding_cmap(&name) {
            let mut merged = base.map;
            merged.extend(map);
            return Ok(EncodingCMap {
                map: merged,
                code_byte_length: base.code_byte_length.max(max_code_size),
                is_identity: false,
            });
        }
    }

    Ok(EncodingCMap {
        map,
        code_byte_length: max_code_size,
        is_identity: false,
    })
}

fn load_builtin_cmap_by_name(name: &str) -> Option<ToUnicodeCMap> {
    if !name.ends_with("UCS2") {
        return None;
    }
    let dir = find_bcmaps_dir()?;
    let path = dir.join(format!("{}.bcmap", name));
    let data = std::fs::read(&path).ok()?;
    let mut cmap = parse_binary_cmap(&data).ok()?;
    if cmap.char_map.is_empty() && cmap.ranges.is_empty() {
        return None;
    }
    cmap.code_byte_length = 2;
    Some(cmap)
}

fn merge_cmaps(mut base: ToUnicodeCMap, overlay: ToUnicodeCMap) -> ToUnicodeCMap {
    for (cid, s) in overlay.char_map {
        base.char_map.insert(cid, s);
    }
    base.ranges.extend(overlay.ranges);
    base.ranges.sort_unstable_by_key(|&(start, _, _)| start);
    base.code_byte_length = base.code_byte_length.max(overlay.code_byte_length);
    base
}

/// Check if a CIDFont's /W (widths) array contains CID values that look like
/// Unicode codepoints rather than low-value GIDs.
///
/// Returns true if the median CID is >= 0x41 (letter 'A'), indicating
/// the PDF generator likely used Unicode codepoints as CIDs.
pub(crate) fn cid_values_look_like_unicode(cid_font_dict: &lopdf::Dictionary) -> bool {
    let w_arr = match cid_font_dict.get(b"W").ok() {
        Some(Object::Array(arr)) => arr,
        _ => return false,
    };

    // The /W array format: [cid [w1 w2 ...]] or [cid_start cid_end w]
    // We extract all CID values (the first element of each group).
    let mut cids: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < w_arr.len() {
        if let Ok(cid) = w_arr[i].as_i64() {
            cids.push(cid as u16);
            // Skip the width data
            if i + 1 < w_arr.len() {
                match &w_arr[i + 1] {
                    Object::Array(widths) => {
                        // [cid [w1 w2 ...]] — CIDs are cid, cid+1, ..., cid+len-1
                        for j in 1..widths.len() {
                            cids.push((cid as u16).wrapping_add(j as u16));
                        }
                        i += 2;
                    }
                    _ => {
                        // [cid_start cid_end w] — range of CIDs
                        if i + 2 < w_arr.len() {
                            if let Ok(cid_end) = w_arr[i + 1].as_i64() {
                                for c in (cid as u16)..=(cid_end as u16) {
                                    cids.push(c);
                                }
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                }
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    if cids.is_empty() {
        return false;
    }

    cids.sort_unstable();
    let median = cids[cids.len() / 2];
    // Unicode text CIDs are typically >= 0x20 (space) with letters at 0x41+.
    // GID-based subsets typically start at low values (0-based).
    // Use median >= 0x41 as a heuristic for Unicode CIDs.
    median >= 0x41
}

/// Build a ToUnicodeCMap from predefined CID→Unicode mapping based on CIDSystemInfo.
///
/// Supports Adobe-Korea1 (Korean) character collection. Can be extended for
/// Adobe-Japan1, Adobe-GB1, Adobe-CNS1 in the future.
fn build_cmap_from_cid_system_info(
    cid_font_dict: &lopdf::Dictionary,
    doc: &Document,
) -> Option<ToUnicodeCMap> {
    let csi_obj = cid_font_dict.get(b"CIDSystemInfo").ok()?;
    let csi_dict = match csi_obj {
        Object::Reference(r) => doc.get_dictionary(*r).ok()?,
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let ordering = csi_dict.get(b"Ordering").ok().and_then(|o| {
        if let Object::String(bytes, _) = o {
            Some(String::from_utf8_lossy(bytes).to_string())
        } else {
            None
        }
    })?;

    match ordering.as_str() {
        "Korea1" => {
            use crate::adobe_korea1::ADOBE_KOREA1_CID_TO_UNICODE;
            let mut cmap = ToUnicodeCMap::new();
            for &(cid, unicode) in ADOBE_KOREA1_CID_TO_UNICODE.iter() {
                if let Some(ch) = char::from_u32(unicode as u32) {
                    cmap.char_map.insert(cid, ch.to_string());
                }
            }
            cmap.code_byte_length = 2;
            debug!(
                "Adobe-Korea1 predefined CMap: {} entries",
                cmap.char_map.len()
            );
            Some(cmap)
        }
        "Japan1" | "GB1" | "CNS1" => build_cmap_from_builtin_cmap(&ordering),
        _ => None,
    }
}

/// Collection of ToUnicode CMaps indexed by ToUnicode stream object number
#[derive(Debug, Default, Clone)]
pub struct FontCMaps {
    /// Map of ToUnicode object number to CMap
    by_obj_num: HashMap<u32, CMapEntry>,
}

/// Primary CMap plus optional alternative variants.
#[derive(Debug, Clone)]
pub struct CMapEntry {
    pub primary: ToUnicodeCMap,
    pub remapped: Option<ToUnicodeCMap>,
    pub fallback: Option<ToUnicodeCMap>,
}

impl FontCMaps {
    /// Build FontCMaps from a lopdf Document model.
    ///
    /// Iterates every page, collects fonts (including Form XObject fonts),
    /// and parses any `/ToUnicode` streams via lopdf's decompression.
    pub fn from_doc(doc: &Document) -> Self {
        Self::from_doc_pages(doc, None)
    }

    /// Build FontCMaps for specific pages only. Pass `None` for all pages.
    pub fn from_doc_pages(doc: &Document, page_filter: Option<&HashSet<u32>>) -> Self {
        Self::from_doc_pages_inner(doc, page_filter, false)
    }

    /// Build FontCMaps in fast mode: skip expensive TrueType font fallback
    /// parsing. Fonts that can't be decoded from their ToUnicode CMap alone
    /// will be missing, causing text extraction to produce empty/garbage text
    /// which triggers `needs_ocr` fallback. This is ideal for hybrid OCR
    /// pipelines where GPU OCR is always available as a fallback.
    pub fn from_doc_pages_fast(doc: &Document, page_filter: Option<&HashSet<u32>>) -> Self {
        Self::from_doc_pages_inner(doc, page_filter, true)
    }

    fn from_doc_pages_inner(
        doc: &Document,
        page_filter: Option<&HashSet<u32>>,
        skip_truetype_fallback: bool,
    ) -> Self {
        let mut by_obj_num: HashMap<u32, CMapEntry> = HashMap::new();

        for (page_num, &page_id) in doc.get_pages().iter() {
            if let Some(filter) = page_filter {
                if !filter.contains(page_num) {
                    continue;
                }
            }
            // Page-level fonts (includes inherited parent resources)
            let fonts = doc.get_page_fonts(page_id).unwrap_or_default();
            Self::collect_cmaps_from_fonts_inner(
                &fonts,
                doc,
                &mut by_obj_num,
                skip_truetype_fallback,
            );

            if !skip_truetype_fallback {
                // Fonts inside Form XObjects referenced by this page
                Self::collect_cmaps_from_xobjects(doc, page_id, &mut by_obj_num);
            }
        }

        FontCMaps { by_obj_num }
    }

    /// Parse ToUnicode CMaps from a set of font dictionaries.
    /// Also handles Identity-H/V CID fonts without ToUnicode by parsing
    /// the embedded TrueType cmap from FontFile2.
    fn collect_cmaps_from_fonts(
        fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
        doc: &Document,
        by_obj_num: &mut HashMap<u32, CMapEntry>,
    ) {
        Self::collect_cmaps_from_fonts_inner(fonts, doc, by_obj_num, false);
    }

    fn collect_cmaps_from_fonts_inner(
        fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
        doc: &Document,
        by_obj_num: &mut HashMap<u32, CMapEntry>,
        skip_truetype_fallback: bool,
    ) {
        // First pass: collect ToUnicode CMaps
        for font_dict in fonts.values() {
            let obj_ref = match font_dict
                .get(b"ToUnicode")
                .ok()
                .and_then(|o| o.as_reference().ok())
            {
                Some(r) => r,
                None => continue,
            };
            let obj_num = obj_ref.0;
            if by_obj_num.contains_key(&obj_num) {
                continue;
            }
            let stream = match doc.get_object(obj_ref).and_then(Object::as_stream) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let data = match stream.decompressed_content() {
                Ok(d) => d,
                Err(_) => stream.content.clone(),
            };
            if let Some(cmap) = ToUnicodeCMap::parse(&data) {
                debug!(
                    "CMap obj={:<6} code_byte_length={} char_map={} ranges={}",
                    obj_num,
                    cmap.code_byte_length,
                    cmap.char_map.len(),
                    cmap.ranges.len()
                );
                let (mut primary, mut remapped) =
                    try_remap_subset_cmap(cmap, font_dict, doc, obj_num);

                // Only build expensive fallbacks when the primary CMap is sparse.
                // build_fallback_cmap_for_type0 can take seconds on large embedded
                // TrueType fonts (decompressing + parsing 100K+ byte font files).
                // Skip entirely when the primary CMap is sufficient.
                let primary_entries = primary.char_map.len() + primary.ranges.len();
                let mut fallback = if primary_entries < 10 && !skip_truetype_fallback {
                    // Try cheap fallback first; only attempt expensive TrueType
                    // parsing if cheap fallbacks don't yield results.
                    let cheap = build_fallback_tounicode_from_encoding(font_dict, doc)
                        .or_else(|| build_fallback_cmap_for_simple(font_dict, doc));
                    if cheap.is_some() {
                        cheap
                    } else {
                        build_fallback_cmap_for_type0(font_dict, doc)
                    }
                } else if primary_entries < 10 {
                    // Fast mode: only try cheap fallbacks, skip TrueType parsing.
                    // Regions using this font will get needs_ocr=true.
                    build_fallback_tounicode_from_encoding(font_dict, doc)
                        .or_else(|| build_fallback_cmap_for_simple(font_dict, doc))
                } else {
                    // Primary is rich enough; only try the cheap encoding fallback
                    build_fallback_tounicode_from_encoding(font_dict, doc)
                };

                if primary_entries < 10 {
                    if let Some(fb) = fallback.take() {
                        debug!(
                            "ToUnicode CMap obj={} too sparse ({} entries); using fallback",
                            obj_num, primary_entries
                        );
                        remapped = Some(primary);
                        primary = fb;
                    }
                }
                by_obj_num.insert(
                    obj_num,
                    CMapEntry {
                        primary,
                        remapped,
                        fallback,
                    },
                );
            } else {
                // ToUnicode present but parse failed; try fallbacks to avoid empty decoding.
                let fallback = if skip_truetype_fallback {
                    build_fallback_cmap_for_simple(font_dict, doc)
                } else {
                    build_fallback_cmap_for_type0(font_dict, doc)
                        .or_else(|| build_fallback_cmap_for_simple(font_dict, doc))
                };
                if let Some(fb) = fallback {
                    debug!(
                        "ToUnicode CMap obj={} parse failed; using fallback (entries={})",
                        obj_num,
                        fb.char_map.len()
                    );
                    by_obj_num.insert(
                        obj_num,
                        CMapEntry {
                            primary: fb,
                            remapped: None,
                            fallback: None,
                        },
                    );
                }
            }
        }

        // Second pass: Identity-H/V fonts without ToUnicode
        // Try: (1) embedded TrueType/OpenType cmap, (2) predefined CID→Unicode mapping
        // Skip entirely in fast mode — these fonts require expensive TrueType parsing.
        if skip_truetype_fallback {
            return;
        }
        for font_dict in fonts.values() {
            if font_dict.get(b"ToUnicode").is_ok() {
                continue;
            }
            let encoding = match font_dict
                .get(b"Encoding")
                .ok()
                .and_then(|o| o.as_name().ok())
            {
                Some(name) => name,
                None => continue,
            };
            if encoding != b"Identity-H" && encoding != b"Identity-V" {
                continue;
            }
            // Navigate: DescendantFonts[0]
            let desc_fonts_obj = match font_dict.get(b"DescendantFonts").ok() {
                Some(obj) => obj,
                None => continue,
            };
            let desc_fonts = match desc_fonts_obj {
                Object::Array(arr) => arr.clone(),
                Object::Reference(r) => match doc.get_object(*r) {
                    Ok(Object::Array(arr)) => arr.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            if desc_fonts.is_empty() {
                continue;
            }
            let cid_font_dict = match &desc_fonts[0] {
                Object::Reference(r) => match doc.get_dictionary(*r) {
                    Ok(d) => d,
                    _ => continue,
                },
                Object::Dictionary(d) => d,
                _ => continue,
            };

            // Try to build CMap from embedded font (FontFile2 or FontFile3)
            let font_descriptor = cid_font_dict
                .get(b"FontDescriptor")
                .ok()
                .and_then(|o| match o {
                    Object::Reference(r) => doc.get_dictionary(*r).ok(),
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                });

            let mut resolved = false;

            // Determine the font file reference (FontFile2 or FontFile3)
            let font_file_ref = font_descriptor.and_then(|fd| {
                fd.get(b"FontFile2")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
                    .or_else(|| {
                        fd.get(b"FontFile3")
                            .ok()
                            .and_then(|o| o.as_reference().ok())
                    })
            });

            // The lookup key must match what get_font_file2_obj_num() returns:
            // font file obj_num if present, else CIDFont dict obj_num
            let lookup_key = font_file_ref
                .map(|r| r.0)
                .unwrap_or_else(|| match &desc_fonts[0] {
                    Object::Reference(r) => r.0,
                    _ => 0,
                });
            if lookup_key == 0 || by_obj_num.contains_key(&lookup_key) {
                continue;
            }

            // Try parsing embedded TrueType/OpenType cmap
            if let Some(ff_ref) = font_file_ref {
                if let Ok(stream) = doc.get_object(ff_ref).and_then(Object::as_stream) {
                    let data = match stream.decompressed_content() {
                        Ok(d) => d,
                        Err(_) => stream.content.clone(),
                    };
                    if let Some(cmap) = build_cmap_from_truetype(&data) {
                        debug!(
                            "TrueType CMap obj={:<6} (embedded font) char_map={}",
                            lookup_key,
                            cmap.char_map.len()
                        );
                        by_obj_num.insert(
                            lookup_key,
                            CMapEntry {
                                primary: cmap,
                                remapped: None,
                                fallback: None,
                            },
                        );
                        resolved = true;
                    }
                }
            }

            // Fallback: predefined CID→Unicode mapping from CIDSystemInfo
            if !resolved {
                if let Some(cmap) = build_cmap_from_cid_system_info(cid_font_dict, doc) {
                    debug!(
                        "Predefined CMap obj={:<6} (CIDSystemInfo) char_map={}",
                        lookup_key,
                        cmap.char_map.len()
                    );
                    by_obj_num.insert(
                        lookup_key,
                        CMapEntry {
                            primary: cmap,
                            remapped: None,
                            fallback: None,
                        },
                    );
                    resolved = true;
                }
            }

            // Last resort: CID-as-Unicode passthrough.
            // Many PDF generators (Chromium, wkhtmltopdf) use Identity-H encoding where
            // CID values ARE Unicode codepoints, but strip the cmap table and omit
            // ToUnicode. We detect this by checking the /W (widths) array: if CID values
            // fall in typical Unicode letter/digit ranges (0x41+), CIDs are likely Unicode.
            // If CIDs are low values (< 0x41), they're GIDs in a subset font.
            if !resolved {
                if cid_values_look_like_unicode(cid_font_dict) {
                    debug!(
                        "Identity-H font obj={}: W array CIDs look like Unicode — using passthrough",
                        lookup_key
                    );
                    let mut cmap = ToUnicodeCMap::new();
                    cmap.code_byte_length = 2;
                    cmap.cid_passthrough = true;
                    by_obj_num.insert(
                        lookup_key,
                        CMapEntry {
                            primary: cmap,
                            remapped: None,
                            fallback: None,
                        },
                    );
                } else {
                    debug!(
                        "Identity-H font obj={}: no decoding possible (stripped cmap, GID-based CIDs)",
                        lookup_key
                    );
                }
            }
        }

        // Third pass: simple fonts without ToUnicode (use embedded font cmap as fallback)
        for font_dict in fonts.values() {
            if font_dict.get(b"ToUnicode").is_ok() {
                continue;
            }
            // Skip fonts with explicit encoding — they can be decoded by the
            // standard encoding path (lopdf) and don't need a fallback CMap.
            if let Ok(enc) = font_dict.get(b"Encoding") {
                if enc.as_name().is_ok() || enc.as_dict().is_ok() || enc.as_reference().is_ok() {
                    continue;
                }
            }
            let subtype = match font_dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
            {
                Some(name) => name,
                None => continue,
            };
            if subtype == b"Type0" {
                continue;
            }

            let font_descriptor = font_dict.get(b"FontDescriptor").ok().and_then(|o| match o {
                Object::Reference(r) => doc.get_dictionary(*r).ok(),
                Object::Dictionary(d) => Some(d),
                _ => None,
            });
            let font_file_ref = font_descriptor.and_then(|fd| {
                fd.get(b"FontFile2")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
                    .or_else(|| {
                        fd.get(b"FontFile3")
                            .ok()
                            .and_then(|o| o.as_reference().ok())
                    })
            });
            let ff_ref = match font_file_ref {
                Some(r) => r,
                None => continue,
            };
            let lookup_key = ff_ref.0;
            if by_obj_num.contains_key(&lookup_key) {
                continue;
            }
            if let Ok(stream) = doc.get_object(ff_ref).and_then(Object::as_stream) {
                if let Ok(data) = stream.decompressed_content() {
                    if let Some(cmap) = build_simple_cmap_from_truetype(&data) {
                        debug!(
                            "Simple font cmap obj={:<6} (embedded font) char_map={}",
                            lookup_key,
                            cmap.char_map.len()
                        );
                        by_obj_num.insert(
                            lookup_key,
                            CMapEntry {
                                primary: cmap,
                                remapped: None,
                                fallback: None,
                            },
                        );
                    }
                }
            }
        }
    }

    /// Walk Form XObjects in a page's resources and collect their font CMaps.
    fn collect_cmaps_from_xobjects(
        doc: &Document,
        page_id: ObjectId,
        by_obj_num: &mut HashMap<u32, CMapEntry>,
    ) {
        let (resource_dict, resource_ids) = match doc.get_page_resources(page_id) {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut visited = HashSet::new();

        if let Some(resources) = resource_dict {
            Self::walk_xobject_fonts(resources, doc, by_obj_num, &mut visited);
        }
        for resource_id in resource_ids {
            if let Ok(resources) = doc.get_dictionary(resource_id) {
                Self::walk_xobject_fonts(resources, doc, by_obj_num, &mut visited);
            }
        }
    }

    /// Recursively collect font CMaps from XObjects in a resource dictionary.
    fn walk_xobject_fonts(
        resources: &lopdf::Dictionary,
        doc: &Document,
        by_obj_num: &mut HashMap<u32, CMapEntry>,
        visited: &mut HashSet<ObjectId>,
    ) {
        let xobject_dict = match resources.get(b"XObject") {
            Ok(Object::Reference(id)) => doc.get_object(*id).and_then(Object::as_dict).ok(),
            Ok(Object::Dictionary(dict)) => Some(dict),
            _ => None,
        };
        let xobject_dict = match xobject_dict {
            Some(d) => d,
            None => return,
        };

        for (_name, value) in xobject_dict.iter() {
            let id = match value {
                Object::Reference(id) => *id,
                _ => continue,
            };
            if !visited.insert(id) {
                continue;
            }
            let stream = match doc.get_object(id).and_then(Object::as_stream) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let is_form = stream
                .dict
                .get(b"Subtype")
                .and_then(|o| o.as_name())
                .is_ok_and(|n| n == b"Form");
            if !is_form {
                continue;
            }
            // Collect fonts from this Form XObject's Resources
            if let Ok(form_resources) = stream.dict.get(b"Resources").and_then(Object::as_dict) {
                // Extract font dict from the Form's resources
                let font_dict_obj = match form_resources.get(b"Font") {
                    Ok(Object::Reference(id)) => doc.get_object(*id).and_then(Object::as_dict).ok(),
                    Ok(Object::Dictionary(dict)) => Some(dict),
                    _ => None,
                };
                if let Some(font_dict) = font_dict_obj {
                    let mut fonts = std::collections::BTreeMap::new();
                    for (name, value) in font_dict.iter() {
                        let font = match value {
                            Object::Reference(id) => doc.get_dictionary(*id).ok(),
                            Object::Dictionary(dict) => Some(dict),
                            _ => None,
                        };
                        if let Some(font) = font {
                            fonts.insert(name.clone(), font);
                        }
                    }
                    Self::collect_cmaps_from_fonts(&fonts, doc, by_obj_num);
                }
                // Recurse into nested XObjects
                Self::walk_xobject_fonts(form_resources, doc, by_obj_num, visited);
            }
        }
    }

    /// Get a CMap by ToUnicode object number
    pub fn get_by_obj(&self, obj_num: u32) -> Option<&CMapEntry> {
        self.by_obj_num.get(&obj_num)
    }
}

/// For Type0 CID fonts, try to build a fallback CMap from embedded font data
/// or CIDSystemInfo when a ToUnicode CMap is present but incomplete.
fn build_fallback_cmap_for_type0(
    font_dict: &lopdf::Dictionary,
    doc: &Document,
) -> Option<ToUnicodeCMap> {
    let subtype = font_dict.get(b"Subtype").ok()?.as_name().ok()?;
    if subtype != b"Type0" {
        return None;
    }
    let encoding = font_dict
        .get(b"Encoding")
        .ok()
        .and_then(|o| o.as_name().ok())?;
    if encoding != b"Identity-H" && encoding != b"Identity-V" {
        return None;
    }

    let desc_fonts_obj = font_dict.get(b"DescendantFonts").ok()?;
    let desc_fonts = match desc_fonts_obj {
        Object::Array(arr) => arr,
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Array(arr)) => arr,
            _ => return None,
        },
        _ => return None,
    };
    if desc_fonts.is_empty() {
        return None;
    }
    let cid_font_dict = match &desc_fonts[0] {
        Object::Reference(r) => doc.get_dictionary(*r).ok()?,
        Object::Dictionary(d) => d,
        _ => return None,
    };

    let font_descriptor = cid_font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| match o {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        });

    let font_file_ref = font_descriptor.and_then(|fd| {
        fd.get(b"FontFile2")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .or_else(|| {
                fd.get(b"FontFile3")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
            })
    });

    if let Some(ff_ref) = font_file_ref {
        if let Ok(stream) = doc.get_object(ff_ref).and_then(Object::as_stream) {
            if let Ok(data) = stream.decompressed_content() {
                if let Some(cmap) = build_cmap_from_truetype(&data) {
                    if let Some(cid_to_gid) = get_cid_to_gid_map(cid_font_dict, doc) {
                        if let Some(repaired) = build_cmap_with_cid_to_gid_map(&cmap, &cid_to_gid) {
                            debug!(
                                "Fallback TrueType CMap repaired with CIDToGIDMap: {} entries",
                                repaired.char_map.len()
                            );
                            return Some(repaired);
                        }
                    }
                    debug!(
                        "Fallback TrueType CMap (Type0+ToUnicode) char_map={}",
                        cmap.char_map.len()
                    );
                    return Some(cmap);
                }
            }
        }
    }

    if let Some(cmap) = build_cmap_from_cid_system_info(cid_font_dict, doc) {
        debug!(
            "Fallback CIDSystemInfo CMap (Type0+ToUnicode) char_map={}",
            cmap.char_map.len()
        );
        return Some(cmap);
    }

    None
}

fn build_fallback_cmap_for_simple(
    font_dict: &lopdf::Dictionary,
    doc: &Document,
) -> Option<ToUnicodeCMap> {
    let subtype = font_dict.get(b"Subtype").ok()?.as_name().ok()?;
    if subtype == b"Type0" {
        return None;
    }
    let font_descriptor = font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| match o {
            Object::Reference(r) => doc.get_dictionary(*r).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        })?;
    let font_file_ref = font_descriptor
        .get(b"FontFile2")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .or_else(|| {
            font_descriptor
                .get(b"FontFile3")
                .ok()
                .and_then(|o| o.as_reference().ok())
        })?;
    if let Ok(stream) = doc.get_object(font_file_ref).and_then(Object::as_stream) {
        if let Ok(data) = stream.decompressed_content() {
            if let Some(cmap) = build_simple_cmap_from_truetype(&data) {
                debug!(
                    "Fallback simple font cmap (ToUnicode present) char_map={}",
                    cmap.char_map.len()
                );
                return Some(cmap);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bfchar_2byte() {
        let cmap_content = r#"
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 begincodespacerange
<0000><FFFF>
endcodespacerange
3 beginbfchar
<0003> <0020>
<0024> <0041>
<0025> <0042>
endbfchar
endcmap
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        assert_eq!(cmap.code_byte_length, 2);
        assert_eq!(cmap.lookup(0x0003), Some(" ".to_string()));
        assert_eq!(cmap.lookup(0x0024), Some("A".to_string()));
        assert_eq!(cmap.lookup(0x0025), Some("B".to_string()));
    }

    #[test]
    fn test_parse_bfchar_1byte() {
        // This is the pattern that caused the CJK bug: codespace is <0000><FFFF>
        // but all source codes are 1-byte hex (e.g., <20>, <41>)
        let cmap_content = r#"
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
3 beginbfchar
<20> <0020>
<41> <0041>
<42> <0042>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        // Should detect as 1-byte because all source codes are 1-byte hex
        assert_eq!(cmap.code_byte_length, 1);
        assert_eq!(cmap.lookup(0x0020), Some(" ".to_string()));
        assert_eq!(cmap.lookup(0x0041), Some("A".to_string()));
    }

    #[test]
    fn test_decode_cids_2byte() {
        let cmap_content = r#"
1 begincodespacerange
<0000><FFFF>
endcodespacerange
3 beginbfchar
<0003> <0020>
<0024> <0041>
<0025> <0042>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        // "AB " in 2-byte CID encoding
        let cids = [0x00, 0x24, 0x00, 0x25, 0x00, 0x03];
        assert_eq!(cmap.decode_cids(&cids), "AB ");
    }

    #[test]
    fn test_decode_cids_1byte_no_cjk_garbage() {
        // Simulates the bug: CMap with 1-byte source codes
        let cmap_content = r#"
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
5 beginbfchar
<20> <0020>
<42> <0042>
<79> <0079>
<50> <0050>
<52> <0052>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();
        assert_eq!(cmap.code_byte_length, 1);

        // "By" should decode to "By", NOT to CJK character 䉹
        let bytes = [0x42, 0x79];
        let result = cmap.decode_cids(&bytes);
        assert_eq!(result, "By");
        assert!(!result.contains('䉹'), "Should not produce CJK garbage");

        // "PR" should decode to "PR"
        let bytes2 = [0x50, 0x52];
        assert_eq!(cmap.decode_cids(&bytes2), "PR");
    }

    #[test]
    fn test_bfrange_array_format() {
        let cmap_content = r#"
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfrange
<0003> <0005> [<0041> <0042> <0043>]
endbfrange
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        assert_eq!(cmap.lookup(0x0003), Some("A".to_string()));
        assert_eq!(cmap.lookup(0x0004), Some("B".to_string()));
        assert_eq!(cmap.lookup(0x0005), Some("C".to_string()));
    }

    #[test]
    fn test_remap_to_sequential() {
        // Simulate a broken CMap where GIDs are from pre-subsetting:
        // Old GID 3 → space, old GID 36 → 'A', old GID 37 → 'B'
        // The subset font has sequential GIDs: 1=space, 2='A', 3='B'
        let cmap_content = r#"
1 begincodespacerange
<0000><FFFF>
endcodespacerange
3 beginbfchar
<0003> <0020>
<0024> <0041>
<0025> <0042>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        // Original CMap: CID 3 → space, CID 36 → 'A', CID 37 → 'B'
        assert_eq!(cmap.lookup(0x0003), Some(" ".to_string()));
        assert_eq!(cmap.lookup(0x0024), Some("A".to_string()));
        assert_eq!(cmap.lookup(0x0025), Some("B".to_string()));
        assert_eq!(cmap.lookup(0x0001), None);
        assert_eq!(cmap.lookup(0x0002), None);

        // After remapping: CID 1 → space, CID 2 → 'A', CID 3 → 'B'
        let remapped = cmap.remap_to_sequential();
        assert_eq!(remapped.lookup(0x0001), Some(" ".to_string()));
        assert_eq!(remapped.lookup(0x0002), Some("A".to_string()));
        assert_eq!(remapped.lookup(0x0003), Some("B".to_string()));
        assert_eq!(remapped.lookup(0x0024), None);
        assert_eq!(remapped.lookup(0x0025), None);
    }

    #[test]
    fn test_remap_to_sequential_with_ranges() {
        // CMap with a bfrange: old GIDs 100-102 → 'X', 'Y', 'Z'
        // Plus a bfchar: old GID 50 → space
        let cmap_content = r#"
1 begincodespacerange
<0000><FFFF>
endcodespacerange
1 beginbfchar
<0032> <0020>
endbfchar
1 beginbfrange
<0064> <0066> <0058>
endbfrange
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        assert_eq!(cmap.lookup(0x0032), Some(" ".to_string())); // CID 50
        assert_eq!(cmap.lookup(0x0064), Some("X".to_string())); // CID 100
        assert_eq!(cmap.lookup(0x0065), Some("Y".to_string())); // CID 101
        assert_eq!(cmap.lookup(0x0066), Some("Z".to_string())); // CID 102

        let remapped = cmap.remap_to_sequential();
        // Sorted old CIDs: 50, 100, 101, 102 → new CIDs: 1, 2, 3, 4
        assert_eq!(remapped.lookup(0x0001), Some(" ".to_string()));
        assert_eq!(remapped.lookup(0x0002), Some("X".to_string()));
        assert_eq!(remapped.lookup(0x0003), Some("Y".to_string()));
        assert_eq!(remapped.lookup(0x0004), Some("Z".to_string()));
        // Ranges should be cleared (all in char_map now)
        assert!(remapped.ranges.is_empty());
    }

    #[test]
    fn test_min_source_cid() {
        let cmap_content = r#"
1 begincodespacerange
<0000><FFFF>
endcodespacerange
2 beginbfchar
<0003> <0020>
<0024> <0041>
endbfchar
1 beginbfrange
<0030> <0032> <0058>
endbfrange
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();
        assert_eq!(cmap.min_source_cid(), Some(3));
    }

    #[test]
    fn test_unmapped_2byte_cids_skipped() {
        let cmap_content = r#"
1 begincodespacerange
<0000><FFFF>
endcodespacerange
1 beginbfchar
<0041> <0041>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();
        assert_eq!(cmap.code_byte_length, 2);

        // CID 0x4279 is unmapped - should NOT produce CJK character
        let bytes = [0x42, 0x79];
        let result = cmap.decode_cids(&bytes);
        assert!(
            !result.contains('䉹'),
            "Unmapped 2-byte CIDs should not produce CJK"
        );
    }

    #[test]
    fn fallback_promotion_when_larger_than_primary() {
        // Simulate: primary has 5 char_map entries, remapped exists (sequential),
        // fallback has 20 entries (TrueType cmap).  The fallback should be
        // promoted to `remapped` and the old remap demoted to `fallback`.
        let mut primary = ToUnicodeCMap::new();
        for i in 0..5u16 {
            primary
                .char_map
                .insert(100 + i, char::from(b'A' + i as u8).to_string());
        }
        primary.code_byte_length = 2;

        let mut sequential_remap = ToUnicodeCMap::new();
        for i in 0..5u16 {
            sequential_remap
                .char_map
                .insert(i, char::from(b'A' + i as u8).to_string());
        }
        sequential_remap.code_byte_length = 2;

        let mut truetype_fb = ToUnicodeCMap::new();
        for i in 0..20u16 {
            truetype_fb
                .char_map
                .insert(i, format!("U+{:04X}", 0x4E00 + i));
        }
        truetype_fb.code_byte_length = 2;

        let primary_entries = primary.char_map.len() + primary.ranges.len();
        let mut remapped: Option<ToUnicodeCMap> = Some(sequential_remap);
        let mut fallback: Option<ToUnicodeCMap> = Some(truetype_fb);

        // Apply the same promotion logic as build_cmap_entry_from_stream
        if remapped.is_some() {
            if let Some(ref fb) = fallback {
                let fb_entries = fb.char_map.len() + fb.ranges.len();
                if fb_entries > primary_entries {
                    let old_remap = remapped.take().unwrap();
                    remapped = fallback.take();
                    fallback = Some(old_remap);
                }
            }
        }

        // The TrueType fallback (20 entries) should now be in `remapped`
        let r = remapped.unwrap();
        assert_eq!(
            r.char_map.len(),
            20,
            "TrueType cmap should be promoted to remapped"
        );

        // The old sequential remap (5 entries) should now be in `fallback`
        let f = fallback.unwrap();
        assert_eq!(
            f.char_map.len(),
            5,
            "Sequential remap should be demoted to fallback"
        );
    }

    #[test]
    fn no_fallback_promotion_when_smaller() {
        // When fallback has fewer entries than primary, no swap should occur.
        let mut primary = ToUnicodeCMap::new();
        for i in 0..50u16 {
            primary
                .char_map
                .insert(100 + i, format!("U+{:04X}", 0x0041 + i));
        }
        primary.code_byte_length = 2;

        let mut sequential_remap = ToUnicodeCMap::new();
        for i in 0..50u16 {
            sequential_remap
                .char_map
                .insert(i, format!("U+{:04X}", 0x0041 + i));
        }
        sequential_remap.code_byte_length = 2;

        let mut small_fb = ToUnicodeCMap::new();
        for i in 0..10u16 {
            small_fb.char_map.insert(i, format!("U+{:04X}", 0x4E00 + i));
        }
        small_fb.code_byte_length = 2;

        let primary_entries = primary.char_map.len() + primary.ranges.len();
        let mut remapped: Option<ToUnicodeCMap> = Some(sequential_remap);
        let mut fallback: Option<ToUnicodeCMap> = Some(small_fb);

        if remapped.is_some() {
            if let Some(ref fb) = fallback {
                let fb_entries = fb.char_map.len() + fb.ranges.len();
                if fb_entries > primary_entries {
                    let old_remap = remapped.take().unwrap();
                    remapped = fallback.take();
                    fallback = Some(old_remap);
                }
            }
        }

        // No swap: remapped should still have 50 entries
        assert_eq!(remapped.unwrap().char_map.len(), 50);
        assert_eq!(fallback.unwrap().char_map.len(), 10);
    }
}
