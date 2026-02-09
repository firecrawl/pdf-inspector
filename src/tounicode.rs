//! ToUnicode CMap parsing for PDF text extraction
//!
//! This module parses ToUnicode CMaps to convert CID-encoded text to Unicode.

use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

/// A parsed ToUnicode CMap mapping CIDs to Unicode strings
#[derive(Debug, Default, Clone)]
pub struct ToUnicodeCMap {
    /// Direct character mappings (CID -> Unicode codepoint(s))
    pub char_map: HashMap<u16, String>,
    /// Range mappings (start_cid, end_cid) -> base_unicode
    pub ranges: Vec<(u16, u16, u32)>,
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

        // Parse beginbfchar ... endbfchar sections
        let mut pos = 0;
        while let Some(start) = text[pos..].find("beginbfchar") {
            let section_start = pos + start + "beginbfchar".len();
            if let Some(end) = text[section_start..].find("endbfchar") {
                let section = &text[section_start..section_start + end];
                cmap.parse_bfchar_section(section);
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
                cmap.parse_bfrange_section(section);
                pos = section_start + end;
            } else {
                break;
            }
        }

        if cmap.char_map.is_empty() && cmap.ranges.is_empty() {
            None
        } else {
            Some(cmap)
        }
    }

    /// Parse a bfchar section: <src> <dst> pairs
    fn parse_bfchar_section(&mut self, section: &str) {
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
            if let (Some(src), Some(dst)) = (parse_hex_u16(&src_hex), hex_to_unicode_string(&dst_hex))
            {
                self.char_map.insert(src, dst);
            }
        }
    }

    /// Parse a bfrange section: <start> <end> <base> triplets
    fn parse_bfrange_section(&mut self, section: &str) {
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
                // Array format - skip for now (less common)
                while chars.peek().is_some_and(|&c| c != ']') {
                    chars.next();
                }
                chars.next();
            }
        }
    }

    /// Look up a CID and return the Unicode string
    pub fn lookup(&self, cid: u16) -> Option<String> {
        // First check direct mappings
        if let Some(s) = self.char_map.get(&cid) {
            return Some(s.clone());
        }

        // Then check ranges
        for &(start, end, base) in &self.ranges {
            if cid >= start && cid <= end {
                let offset = (cid - start) as u32;
                let unicode = base + offset;
                if let Some(c) = char::from_u32(unicode) {
                    return Some(c.to_string());
                }
            }
        }

        None
    }

    /// Decode a byte slice of CIDs (2 bytes each) to a Unicode string
    pub fn decode_cids(&self, bytes: &[u8]) -> String {
        let mut result = String::new();

        // CIDs are 2 bytes each (big-endian)
        for chunk in bytes.chunks(2) {
            if chunk.len() == 2 {
                let cid = u16::from_be_bytes([chunk[0], chunk[1]]);
                if let Some(s) = self.lookup(cid) {
                    result.push_str(&s);
                } else {
                    // Fallback: try as direct Unicode
                    if let Some(c) = char::from_u32(cid as u32) {
                        result.push(c);
                    }
                }
            }
        }

        result
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

/// Extract a stream from raw PDF bytes by object number
/// This handles linearized PDFs where lopdf may not properly load stream content
pub fn extract_stream_from_raw_pdf(pdf_bytes: &[u8], obj_num: u32) -> Option<Vec<u8>> {
    // Search for "N 0 obj" where N is the object number
    let pattern = format!("{} 0 obj", obj_num);
    let pattern_bytes = pattern.as_bytes();

    // Find the object definition
    let obj_start = find_pattern(pdf_bytes, pattern_bytes)?;

    // Find "stream" keyword after the object start
    let search_start = obj_start + pattern_bytes.len();
    let stream_keyword = find_pattern(&pdf_bytes[search_start..], b"stream")?;
    let stream_start = search_start + stream_keyword + 6; // "stream" is 6 chars

    // Skip newline after "stream"
    let mut content_start = stream_start;
    if pdf_bytes.get(content_start) == Some(&b'\r') {
        content_start += 1;
    }
    if pdf_bytes.get(content_start) == Some(&b'\n') {
        content_start += 1;
    }

    // Find "endstream"
    let stream_end = find_pattern(&pdf_bytes[content_start..], b"endstream")?;
    let content_end = content_start + stream_end;

    // Handle trailing newline before endstream
    let mut actual_end = content_end;
    if actual_end > content_start && pdf_bytes.get(actual_end - 1) == Some(&b'\n') {
        actual_end -= 1;
    }
    if actual_end > content_start && pdf_bytes.get(actual_end - 1) == Some(&b'\r') {
        actual_end -= 1;
    }

    let stream_data = &pdf_bytes[content_start..actual_end];

    // Check if we need to decompress (look for /Filter in the object dict)
    let dict_region = &pdf_bytes[obj_start..stream_start];
    let needs_decompress = find_pattern(dict_region, b"FlateDecode").is_some();

    if needs_decompress {
        // Decompress using zlib/flate
        let mut decoder = ZlibDecoder::new(stream_data);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_ok() {
            return Some(decompressed);
        }
        // If decompression fails, return raw data
        Some(stream_data.to_vec())
    } else {
        Some(stream_data.to_vec())
    }
}

/// Find a byte pattern in a slice, returning the offset
fn find_pattern(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Extract all ToUnicode CMaps from a PDF's raw bytes
/// Returns a map of object number -> ToUnicodeCMap
pub fn extract_tounicode_cmaps(pdf_bytes: &[u8]) -> HashMap<u32, ToUnicodeCMap> {
    let mut cmaps = HashMap::new();

    // Find all ToUnicode references
    // Pattern: /ToUnicode N 0 R
    let mut pos = 0;
    while let Some(idx) = find_pattern(&pdf_bytes[pos..], b"/ToUnicode") {
        let ref_start = pos + idx + 10; // "/ToUnicode" is 10 chars

        // Skip whitespace
        let mut p = ref_start;
        while p < pdf_bytes.len() && (pdf_bytes[p] == b' ' || pdf_bytes[p] == b'\n' || pdf_bytes[p] == b'\r') {
            p += 1;
        }

        // Read object number
        let mut num_str = String::new();
        while p < pdf_bytes.len() && pdf_bytes[p].is_ascii_digit() {
            num_str.push(pdf_bytes[p] as char);
            p += 1;
        }

        if let Ok(obj_num) = num_str.parse::<u32>() {
            // Try to extract the stream for this object
            if let Some(stream_data) = extract_stream_from_raw_pdf(pdf_bytes, obj_num) {
                if let Some(cmap) = ToUnicodeCMap::parse(&stream_data) {
                    cmaps.insert(obj_num, cmap);
                }
            }
        }

        pos = ref_start;
    }

    cmaps
}

/// Collection of ToUnicode CMaps indexed by font name
#[derive(Debug, Default)]
pub struct FontCMaps {
    /// Map of font name (e.g., "FNotoSans0") to ToUnicodeCMap
    pub by_name: HashMap<String, ToUnicodeCMap>,
}

impl FontCMaps {
    /// Extract all font CMaps from raw PDF bytes
    pub fn from_pdf_bytes(pdf_bytes: &[u8]) -> Self {
        let mut by_name = HashMap::new();

        // Find font definitions with ToUnicode references
        // Pattern: /F<name> ... /ToUnicode N 0 R
        // This is a simplified approach - find /BaseFont and nearby /ToUnicode

        // First, extract all ToUnicode streams by object number
        let cmaps_by_obj = extract_tounicode_cmaps(pdf_bytes);

        // Now find font name -> ToUnicode object mappings
        // Look for patterns like: << /Type /Font ... /BaseFont /SomeFont ... /ToUnicode N 0 R >>
        let mut pos = 0;
        while pos < pdf_bytes.len() {
            // Find next font dictionary
            if let Some(idx) = find_pattern(&pdf_bytes[pos..], b"/Type /Font") {
                let font_start = pos + idx;

                // Search backwards and forwards for << and >>
                let dict_start = find_dict_start(&pdf_bytes[..font_start]);
                let dict_end = find_pattern(&pdf_bytes[font_start..], b">>")
                    .map(|e| font_start + e + 2);

                if let (Some(start), Some(end)) = (dict_start, dict_end) {
                    let dict_region = &pdf_bytes[start..end];

                    // Find font name (could be /BaseFont /Name or just the resource name)
                    if let Some(font_name) = extract_font_name(dict_region) {
                        // Find ToUnicode reference
                        if let Some(tounicode_idx) = find_pattern(dict_region, b"/ToUnicode") {
                            let ref_part = &dict_region[tounicode_idx + 10..];
                            if let Some(obj_num) = extract_obj_reference(ref_part) {
                                if let Some(cmap) = cmaps_by_obj.get(&obj_num) {
                                    by_name.insert(font_name, cmap.clone());
                                }
                            }
                        }
                    }
                }

                pos = font_start + 10;
            } else {
                break;
            }
        }

        FontCMaps { by_name }
    }

    /// Get a CMap for a font name
    pub fn get(&self, font_name: &str) -> Option<&ToUnicodeCMap> {
        // Try exact match first
        if let Some(cmap) = self.by_name.get(font_name) {
            return Some(cmap);
        }

        // Try without leading 'F' if present (resource names sometimes differ)
        let stripped = font_name.strip_prefix('F').unwrap_or(font_name);
        for (name, cmap) in &self.by_name {
            if name.contains(stripped) || stripped.contains(name.as_str()) {
                return Some(cmap);
            }
        }

        None
    }
}

/// Find the start of a dictionary (<<) searching backwards from a position
fn find_dict_start(data: &[u8]) -> Option<usize> {
    // Search backwards for <<
    for i in (1..data.len()).rev() {
        if data[i - 1] == b'<' && data[i] == b'<' {
            return Some(i - 1);
        }
    }
    None
}

/// Extract font name from a font dictionary region
fn extract_font_name(dict: &[u8]) -> Option<String> {
    // Look for /BaseFont /Name
    if let Some(idx) = find_pattern(dict, b"/BaseFont") {
        let after = &dict[idx + 9..]; // "/BaseFont" is 9 chars
        // Skip whitespace
        let mut p = 0;
        while p < after.len() && (after[p] == b' ' || after[p] == b'\n' || after[p] == b'\r') {
            p += 1;
        }
        // Expect /Name
        if p < after.len() && after[p] == b'/' {
            p += 1;
            let mut name = String::new();
            while p < after.len() && !after[p].is_ascii_whitespace() && after[p] != b'/' && after[p] != b'>' {
                name.push(after[p] as char);
                p += 1;
            }
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Extract object reference number from "N 0 R" pattern
fn extract_obj_reference(data: &[u8]) -> Option<u32> {
    // Skip whitespace
    let mut p = 0;
    while p < data.len() && (data[p] == b' ' || data[p] == b'\n' || data[p] == b'\r') {
        p += 1;
    }

    // Read number
    let mut num_str = String::new();
    while p < data.len() && data[p].is_ascii_digit() {
        num_str.push(data[p] as char);
        p += 1;
    }

    num_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bfchar() {
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

        assert_eq!(cmap.lookup(0x0003), Some(" ".to_string()));
        assert_eq!(cmap.lookup(0x0024), Some("A".to_string()));
        assert_eq!(cmap.lookup(0x0025), Some("B".to_string()));
    }

    #[test]
    fn test_decode_cids() {
        let cmap_content = r#"
3 beginbfchar
<0003> <0020>
<0024> <0041>
<0025> <0042>
endbfchar
"#;
        let cmap = ToUnicodeCMap::parse(cmap_content.as_bytes()).unwrap();

        // "AB " in CID encoding
        let cids = [0x00, 0x24, 0x00, 0x25, 0x00, 0x03];
        assert_eq!(cmap.decode_cids(&cids), "AB ");
    }
}
