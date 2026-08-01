//! Font width parsing, encoding, and text decoding.

use crate::glyph_names::glyph_to_char;
use crate::tounicode::FontCMaps;
use crate::types::{FontEncodingMap, FontWidthInfo, PageFontEncodings, PageFontWidths};
use log::debug;
use lopdf::{Document, Encoding, Object, ObjectId};
use std::collections::HashMap;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum CMapChoice {
    Primary,
    Remapped,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct CMapDecisionCache {
    decisions: HashMap<u32, CMapDecision>,
}

#[derive(Debug, Default, Clone)]
struct CMapDecision {
    primary_sample: String,
    remapped_sample: String,
    sample_bytes: usize,
    choice: Option<CMapChoice>,
}

impl CMapDecisionCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get_choice(&self, obj_num: u32) -> Option<CMapChoice> {
        self.decisions.get(&obj_num).and_then(|d| d.choice)
    }

    pub(crate) fn consider(
        &mut self,
        obj_num: u32,
        primary: &str,
        remapped: &str,
        bytes_len: usize,
    ) -> Option<CMapChoice> {
        const SAMPLE_TARGET_BYTES: usize = 240;

        let entry = self.decisions.entry(obj_num).or_default();
        entry.sample_bytes = entry.sample_bytes.saturating_add(bytes_len);
        entry.primary_sample.push_str(primary);
        entry.remapped_sample.push_str(remapped);

        if entry.choice.is_none() && entry.sample_bytes >= SAMPLE_TARGET_BYTES {
            let score_primary = score_text(&entry.primary_sample);
            let score_remap = score_text(&entry.remapped_sample);
            entry.choice = if score_remap > score_primary + 5 {
                Some(CMapChoice::Remapped)
            } else {
                Some(CMapChoice::Primary)
            };
        }

        entry.choice
    }
}

/// Resolve a PDF object reference to an array
pub(crate) fn resolve_array<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Vec<Object>> {
    match obj {
        Object::Array(arr) => Some(arr),
        Object::Reference(r) => {
            if let Ok(Object::Array(arr)) = doc.get_object(*r) {
                Some(arr)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve a PDF object reference to a dictionary
pub(crate) fn resolve_dict<'a>(
    doc: &'a Document,
    obj: &'a Object,
) -> Option<&'a lopdf::Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => doc.get_dictionary(*r).ok(),
        _ => None,
    }
}

/// Build font width info for all fonts on a page
pub(crate) fn build_font_widths(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
) -> PageFontWidths {
    let mut widths = PageFontWidths::new();

    for (font_name, font_dict) in fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();

        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_default();
        let base_font = font_dict
            .get(b"BaseFont")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_default();
        let has_tounicode = font_dict.get(b"ToUnicode").is_ok();
        let has_descendants = font_dict.get(b"DescendantFonts").is_ok();
        let encoding_str = font_dict
            .get(b"Encoding")
            .ok()
            .map(|o| match o {
                Object::Name(n) => String::from_utf8_lossy(n).to_string(),
                Object::Reference(_) => "ref(dict)".to_string(),
                Object::Dictionary(_) => "dict".to_string(),
                _ => format!("{:?}", o),
            })
            .unwrap_or_else(|| "none".to_string());

        debug!(
            "font {:<10} sub={:<12} base={:<45} toUni={:<6} enc={:<20} cid={}",
            resource_name, subtype, base_font, has_tounicode, encoding_str, has_descendants
        );

        if let Some(info) = parse_font_widths(doc, font_dict) {
            widths.insert(resource_name, info);
        }
    }

    widths
}

/// Parse font widths from a font dictionary, dispatching by Subtype
pub(crate) fn parse_font_widths(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<FontWidthInfo> {
    // Get the font subtype
    let subtype = font_dict.get(b"Subtype").ok()?;
    let subtype_name = subtype.as_name().ok()?;

    match subtype_name {
        b"Type0" => parse_type0_widths(doc, font_dict),
        b"Type1" | b"TrueType" | b"MMType1" | b"Type3" => parse_simple_font_widths(doc, font_dict),
        _ => None,
    }
}

/// Parse widths for simple fonts (Type1, TrueType, MMType1, Type3)
/// Reads FirstChar, LastChar, and Widths array.
/// For Type3 fonts, reads FontMatrix to determine the correct units_scale.
pub(crate) fn parse_simple_font_widths(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<FontWidthInfo> {
    let first_char = font_dict.get(b"FirstChar").ok().and_then(|o| match o {
        Object::Integer(n) => Some(*n as u16),
        Object::Reference(r) => doc.get_object(*r).ok().and_then(|o| {
            if let Object::Integer(n) = o {
                Some(*n as u16)
            } else {
                None
            }
        }),
        _ => None,
    })?;

    let last_char = font_dict.get(b"LastChar").ok().and_then(|o| match o {
        Object::Integer(n) => Some(*n as u16),
        Object::Reference(r) => doc.get_object(*r).ok().and_then(|o| {
            if let Object::Integer(n) = o {
                Some(*n as u16)
            } else {
                None
            }
        }),
        _ => None,
    })?;

    let widths_obj = font_dict.get(b"Widths").ok()?;
    let widths_array = resolve_array(doc, widths_obj)?;

    let mut widths = HashMap::new();
    let mut space_width: u16 = 0;

    for (i, w_obj) in widths_array.iter().enumerate() {
        let code = first_char + i as u16;
        if code > last_char {
            break;
        }
        let w = match w_obj {
            Object::Integer(n) => *n as u16,
            Object::Real(n) => *n as u16,
            Object::Reference(r) => {
                if let Ok(obj) = doc.get_object(*r) {
                    match obj {
                        Object::Integer(n) => *n as u16,
                        Object::Real(n) => *n as u16,
                        _ => continue,
                    }
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        if code == 32 {
            space_width = w;
        }
        widths.insert(code, w);
    }

    // Determine units_scale: for Type3 fonts, use FontMatrix[0]; for others, use 1/1000
    let units_scale = if let Ok(fm) = font_dict.get(b"FontMatrix") {
        if let Some(arr) = resolve_array(doc, fm) {
            if !arr.is_empty() {
                match &arr[0] {
                    Object::Real(r) => r.abs(),
                    Object::Integer(i) => (*i as f32).abs(),
                    _ => 0.001,
                }
            } else {
                0.001
            }
        } else {
            0.001
        }
    } else {
        0.001 // Standard 1000-unit system
    };

    // If space width wasn't found in the table, estimate from font metrics.
    // The default of 250 is calibrated for standard 1000-unit fonts (units_scale=0.001).
    // For Type3 fonts with different coordinate systems, use average glyph width instead.
    if space_width == 0 {
        if !widths.is_empty() && (units_scale - 0.001).abs() > 0.0005 {
            // Non-standard scale: estimate space as ~45% of average glyph width
            let sum: u32 = widths.values().map(|&w| w as u32).sum();
            let avg = sum as f32 / widths.len() as f32;
            space_width = (avg * 0.45).max(1.0) as u16;
        } else {
            space_width = 250;
        }
    }

    Some(FontWidthInfo {
        widths,
        default_width: 0,
        space_width,
        is_cid: false,
        units_scale,
        wmode: 0,
    })
}

/// Parse widths for Type0 (composite/CID) fonts
/// Reads DescendantFonts → CIDFont → W array and DW value
pub(crate) fn parse_type0_widths(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<FontWidthInfo> {
    let desc_fonts_obj = font_dict.get(b"DescendantFonts").ok()?;
    let desc_fonts = resolve_array(doc, desc_fonts_obj)?;

    if desc_fonts.is_empty() {
        return None;
    }

    // Get the first descendant font dictionary
    let cid_font_dict = resolve_dict(doc, &desc_fonts[0])?;

    // Get DW (default width)
    let default_width = cid_font_dict
        .get(b"DW")
        .ok()
        .and_then(|o| match o {
            Object::Integer(n) => Some(*n as u16),
            Object::Real(n) => Some(*n as u16),
            _ => None,
        })
        .unwrap_or(1000);

    let mut widths = HashMap::new();

    // Parse W array if present
    if let Ok(w_obj) = cid_font_dict.get(b"W") {
        if let Some(w_array) = resolve_array(doc, w_obj) {
            parse_cid_w_array(doc, w_array, &mut widths);
        }
    }

    // Try to determine space width (CID 32 or CID 3 are common for space)
    let space_width = widths
        .get(&32)
        .or_else(|| widths.get(&3))
        .copied()
        .unwrap_or(if default_width > 0 {
            default_width / 4
        } else {
            250
        });

    let wmode = font_dict
        .get(b"WMode")
        .ok()
        .and_then(|o| match o {
            Object::Integer(n) => Some(*n as u8),
            _ => None,
        })
        .unwrap_or(0);

    Some(FontWidthInfo {
        widths,
        default_width,
        space_width,
        is_cid: true,
        units_scale: 0.001, // CID fonts use standard 1000-unit system
        wmode,
    })
}

/// Parse a CID W array into widths map
/// Format: [c [w1 w2 ...]] (consecutive from c) or [c_first c_last w] (range with same width)
pub(crate) fn parse_cid_w_array(
    doc: &Document,
    w_array: &[Object],
    widths: &mut HashMap<u16, u16>,
) {
    let mut i = 0;
    while i < w_array.len() {
        let start_cid = match &w_array[i] {
            Object::Integer(n) => *n as u16,
            Object::Real(n) => *n as u16,
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        if i >= w_array.len() {
            break;
        }

        // Check if next element is an array (consecutive widths) or integer (range)
        match &w_array[i] {
            Object::Array(arr) => {
                // [c [w1 w2 ...]] — consecutive widths starting at c
                for (j, w_obj) in arr.iter().enumerate() {
                    let w = match w_obj {
                        Object::Integer(n) => *n as u16,
                        Object::Real(n) => *n as u16,
                        _ => continue,
                    };
                    widths.insert(start_cid + j as u16, w);
                }
                i += 1;
            }
            Object::Reference(r) => {
                // Could be a reference to an array
                if let Ok(Object::Array(arr)) = doc.get_object(*r) {
                    for (j, w_obj) in arr.iter().enumerate() {
                        let w = match w_obj {
                            Object::Integer(n) => *n as u16,
                            Object::Real(n) => *n as u16,
                            _ => continue,
                        };
                        widths.insert(start_cid + j as u16, w);
                    }
                    i += 1;
                } else {
                    // Treat as c_first c_last w
                    i += 1; // skip this
                }
            }
            Object::Integer(end_cid) => {
                // [c_first c_last w] — range with uniform width
                let end = *end_cid as u16;
                i += 1;
                if i >= w_array.len() {
                    break;
                }
                let w = match &w_array[i] {
                    Object::Integer(n) => *n as u16,
                    Object::Real(n) => *n as u16,
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                for cid in start_cid..=end {
                    widths.insert(cid, w);
                }
                i += 1;
            }
            Object::Real(end_cid) => {
                let end = *end_cid as u16;
                i += 1;
                if i >= w_array.len() {
                    break;
                }
                let w = match &w_array[i] {
                    Object::Integer(n) => *n as u16,
                    Object::Real(n) => *n as u16,
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                for cid in start_cid..=end {
                    widths.insert(cid, w);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
}

/// Compute the width of a string in text space units,
/// given raw bytes and font width info.
/// Returns width in text space units (font_units * units_scale * font_size).
///
/// `char_spacing` (Tc) is added per character and `word_spacing` (Tw) is added
/// per space character (byte 0x20), both in unscaled text-space units.
/// Per the PDF spec: tx = (w0 × Tfs + Tc + Tw_if_space) per glyph.
pub(crate) fn compute_string_width_ts(
    bytes: &[u8],
    font_info: &FontWidthInfo,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
) -> f32 {
    let mut total: f32 = 0.0;
    let mut num_spaces: usize = 0;
    let num_chars = if font_info.is_cid {
        // 2-byte (big-endian) character codes
        let mut j = 0;
        let mut count = 0usize;
        while j + 1 < bytes.len() {
            let cid = u16::from_be_bytes([bytes[j], bytes[j + 1]]);
            let w = font_info
                .widths
                .get(&cid)
                .copied()
                .unwrap_or(font_info.default_width);
            total += w as f32;
            // CID 32 = space in most CID fonts
            if cid == 32 {
                num_spaces += 1;
            }
            count += 1;
            j += 2;
        }
        count
    } else {
        // 1-byte character codes
        for &b in bytes {
            let code = b as u16;
            let w = font_info
                .widths
                .get(&code)
                .copied()
                .unwrap_or(font_info.default_width);
            total += w as f32;
            if b == 0x20 {
                num_spaces += 1;
            }
        }
        bytes.len()
    };
    // Convert from font units to text space using the font's scale factor
    // Then add Tc per character and Tw per space character
    total * font_info.units_scale * font_size
        + num_chars as f32 * char_spacing
        + num_spaces as f32 * word_spacing
}

/// Extract raw bytes from a PDF operand (String object)
pub(crate) fn get_operand_bytes(obj: &Object) -> Option<&[u8]> {
    if let Object::String(bytes, _) = obj {
        Some(bytes)
    } else {
        None
    }
}

/// Build encoding maps for all fonts on a page.
/// Returns `(encodings, has_gid_fonts)` where `has_gid_fonts` is true when
/// any font uses raw glyph ID names (gidNNNNN) that can't be decoded.
/// Gid names whose codes the font's own ToUnicode CMap maps are decodable
/// and do not set the flag (LibreOffice subsets write /gidNNNN Differences
/// names alongside a complete ToUnicode CMap).
pub(crate) fn build_font_encodings(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
    cmaps: &FontCMaps,
) -> (PageFontEncodings, bool) {
    let mut encodings = PageFontEncodings::new();
    let mut has_gid_fonts = false;

    for (font_name, font_dict) in fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();

        if let Some(result) = parse_font_encoding(doc, font_dict) {
            if !result.gid_codes.is_empty()
                && !tounicode_maps_codes(font_dict, cmaps, &result.gid_codes)
            {
                has_gid_fonts = true;
            }
            if !result.map.is_empty() {
                encodings.insert(resource_name, result.map);
            }
        }
    }

    (encodings, has_gid_fonts)
}

/// True when the font's ToUnicode CMap maps the gid-named character codes,
/// so the Differences entries still decode through the CMap.
fn tounicode_maps_codes(font_dict: &lopdf::Dictionary, cmaps: &FontCMaps, codes: &[u8]) -> bool {
    let Some(obj_ref) = font_dict
        .get(b"ToUnicode")
        .ok()
        .and_then(|o| o.as_reference().ok())
    else {
        return false;
    };
    let Some(entry) = cmaps.get_by_obj(obj_ref.0) else {
        return false;
    };
    // At least one gid code usably mapped means the CMap addresses these
    // codes; remaining unmapped codes are subset leftovers (e.g. the
    // component glyphs of an emoji ZWJ sequence mapped whole on its first
    // code). A mapping is usable only when extraction would accept it —
    // empty or U+FFFD results are rejected there as invalid. Fonts whose
    // CMap ignores the gid codes entirely stay flagged, and the downstream
    // garbage/encoding checks still catch partial damage.
    codes.iter().any(|&code| {
        entry
            .primary
            .lookup(code as u16)
            .is_some_and(|s| !s.is_empty() && !s.contains('\u{FFFD}'))
    })
}

/// Parse font encoding from a font dictionary
pub(crate) fn parse_font_encoding(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<EncodingResult> {
    let encoding_obj = font_dict.get(b"Encoding").ok()?;
    let base_font_name = font_dict
        .get(b"BaseFont")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).to_string());

    // Encoding can be a name or a dictionary
    match encoding_obj {
        Object::Name(_name) => {
            // Standard encoding name (e.g., MacRomanEncoding, WinAnsiEncoding)
            // For standard encodings, we can use the standard tables
            // But we still need to check for Differences
            None // Let lopdf handle standard encodings
        }
        Object::Reference(obj_ref) => {
            // Reference to encoding dictionary
            if let Ok(enc_dict) = doc.get_dictionary(*obj_ref) {
                parse_encoding_dictionary(doc, enc_dict, base_font_name.as_deref())
            } else {
                None
            }
        }
        Object::Dictionary(enc_dict) => {
            parse_encoding_dictionary(doc, enc_dict, base_font_name.as_deref())
        }
        _ => None,
    }
}

/// Result of parsing an encoding dictionary's Differences array.
pub(crate) struct EncodingResult {
    pub map: FontEncodingMap,
    /// Character codes whose glyph names match the `gidNNNNN` pattern (raw
    /// glyph IDs). These reference the original font's glyph table and are
    /// only decodable when the font's ToUnicode CMap maps the code.
    pub gid_codes: Vec<u8>,
}

/// Parse an encoding dictionary with Differences array
pub(crate) fn parse_encoding_dictionary(
    doc: &Document,
    enc_dict: &lopdf::Dictionary,
    base_font_name: Option<&str>,
) -> Option<EncodingResult> {
    let differences = enc_dict.get(b"Differences").ok()?;

    let diff_array = match differences {
        Object::Array(arr) => arr.clone(),
        Object::Reference(obj_ref) => {
            if let Ok(Object::Array(arr)) = doc.get_object(*obj_ref) {
                arr.clone()
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let mut encoding_map = FontEncodingMap::new();
    let mut current_code: u8 = 0;
    let mut ligature_count = 0u32;
    let mut gid_codes: Vec<u8> = Vec::new();

    for item in diff_array {
        match item {
            Object::Integer(n) => {
                // This sets the starting code for subsequent glyph names
                current_code = n as u8;
            }
            Object::Name(name) => {
                // Map current code to glyph name -> Unicode
                let glyph_name = String::from_utf8_lossy(&name).to_string();
                let mapped_char = glyph_to_char(&glyph_name)
                    .or_else(|| private_glyph_to_char(&glyph_name, base_font_name));
                if mapped_char.is_some_and(is_ligature_char) {
                    debug!(
                        "  Differences: code=0x{:02X} glyph={:?} (ligature)",
                        current_code, glyph_name
                    );
                    ligature_count += 1;
                }
                // Detect raw glyph ID names (e.g. "gid00053") that can't be
                // mapped to Unicode without the original font's cmap table.
                if glyph_name.starts_with("gid")
                    && glyph_name.len() >= 4
                    && glyph_name[3..].chars().all(|c| c.is_ascii_digit())
                {
                    gid_codes.push(current_code);
                }
                if let Some(ch) = mapped_char {
                    encoding_map.insert(current_code, ch);
                } else {
                    debug!(
                        "  Differences: code=0x{:02X} glyph={:?} (unmapped)",
                        current_code, glyph_name
                    );
                }
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if ligature_count > 0 {
        debug!(
            "  Differences: {} total entries, {} ligatures",
            encoding_map.len(),
            ligature_count
        );
    }

    if !gid_codes.is_empty() {
        debug!(
            "  Differences: {} gid-encoded glyphs (decodable only via ToUnicode)",
            gid_codes.len()
        );
    }

    Some(EncodingResult {
        map: encoding_map,
        gid_codes,
    })
}

fn private_glyph_to_char(glyph_name: &str, base_font_name: Option<&str>) -> Option<char> {
    let base_font_name = strip_subset_prefix(base_font_name?);

    // Aptos CFF subsets from Office PDFs can expose the ff ligature as /g431
    // without a ToUnicode map. Keep this font-scoped because /gNNN names are private.
    if base_font_name.eq_ignore_ascii_case("Aptos") && glyph_name == "g431" {
        Some('\u{FB00}')
    } else {
        None
    }
}

fn strip_subset_prefix(font_name: &str) -> &str {
    font_name
        .split_once('+')
        .map_or(font_name, |(_, stripped)| stripped)
}

fn is_ligature_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{FB00}' | '\u{FB01}' | '\u{FB02}' | '\u{FB03}' | '\u{FB04}'
    )
}

/// Get the CMap lookup key for an Identity-H/V CID font without ToUnicode.
/// Returns the object number used by `collect_cmaps_from_fonts` to store the CMap:
/// - FontFile2 or FontFile3 obj_num (for embedded font cmap)
/// - CIDFont dict obj_num (for predefined CIDSystemInfo-based mapping)
pub(crate) fn get_font_file2_obj_num(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<u32> {
    let subtype = font_dict
        .get(b"Subtype")
        .ok()
        .and_then(|o| o.as_name().ok());

    // Type0 (CID) fonts
    if subtype == Some(b"Type0") {
        let encoding = font_dict.get(b"Encoding").ok()?.as_name().ok()?;
        if encoding != b"Identity-H" && encoding != b"Identity-V" {
            return None;
        }
        let desc_fonts_obj = font_dict.get(b"DescendantFonts").ok()?;
        let desc_fonts = resolve_array(doc, desc_fonts_obj)?;
        if desc_fonts.is_empty() {
            return None;
        }
        let cid_font_dict = resolve_dict(doc, &desc_fonts[0])?;
        let font_descriptor_obj = cid_font_dict.get(b"FontDescriptor").ok()?;
        let font_descriptor = resolve_dict(doc, font_descriptor_obj)?;

        // Try FontFile2 (TrueType), then FontFile3 (OpenType/CFF)
        if let Some(ff_ref) = font_descriptor
            .get(b"FontFile2")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .or_else(|| {
                font_descriptor
                    .get(b"FontFile3")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
            })
        {
            return Some(ff_ref.0);
        }

        // Fallback: use DescendantFonts[0] obj_num (for predefined CIDSystemInfo mapping)
        if let Object::Reference(r) = &desc_fonts[0] {
            return Some(r.0);
        }
        return None;
    }

    // Simple fonts: use embedded font file if available
    let font_descriptor_obj = font_dict.get(b"FontDescriptor").ok()?;
    let font_descriptor = resolve_dict(doc, font_descriptor_obj)?;
    font_descriptor
        .get(b"FontFile2")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .or_else(|| {
            font_descriptor
                .get(b"FontFile3")
                .ok()
                .and_then(|o| o.as_reference().ok())
        })
        .map(|r| r.0)
}

/// Document-scoped memo of embedded-font style flags, keyed by the
/// FontFile2/FontFile3 stream's object id. The same font program is
/// referenced from every page that uses the font, and decompressing +
/// parsing it dominates `descriptor_style_flags` — without the memo that
/// cost repeats per page whenever the descriptor leaves a flag unset
/// (the common case: regular fonts report neither italic nor bold).
#[derive(Debug, Default)]
pub(crate) struct FontStyleCache {
    by_font_file: HashMap<ObjectId, (bool, bool)>,
}

impl FontStyleCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Style flags from the FontDescriptor, which survive subset fonts whose
/// BaseFont names are opaque tags ("Tc1", "ABCDEF+F1") that defeat the
/// name-based bold/italic heuristics.
///
/// Italic: `ItalicAngle` beyond a few degrees, or Flags bit 7 (Italic,
/// value 64). Bold: Flags bit 19 (ForceBold, value 1<<18). The small
/// ItalicAngle threshold skips fonts that declare a token slant.
pub(crate) fn descriptor_style_flags(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
    style_cache: &mut FontStyleCache,
) -> (bool, bool) {
    let descriptor = font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|obj| resolve_dict(doc, obj))
        .or_else(|| {
            // Type0 fonts hang the descriptor off DescendantFonts[0].
            let desc_fonts = font_dict.get(b"DescendantFonts").ok()?;
            let desc_fonts = resolve_array(doc, desc_fonts)?;
            let cid_font_dict = resolve_dict(doc, desc_fonts.first()?)?;
            resolve_dict(doc, cid_font_dict.get(b"FontDescriptor").ok()?)
        });
    let Some(descriptor) = descriptor else {
        return (false, false);
    };

    let italic_angle = descriptor
        .get(b"ItalicAngle")
        .ok()
        .and_then(|obj| match obj {
            Object::Integer(i) => Some(*i as f32),
            Object::Real(r) => Some(*r),
            _ => None,
        })
        .unwrap_or(0.0);
    let flags = descriptor
        .get(b"Flags")
        .ok()
        .and_then(|obj| obj.as_i64().ok())
        .unwrap_or(0);

    let mut italic = italic_angle.abs() >= 4.0 || flags & (1 << 6) != 0;
    let mut bold = flags & (1 << 18) != 0;

    // Descriptors lie: subset generators write ItalicAngle 0 for genuinely
    // italic faces. The embedded font file keeps the truth — OS/2
    // fsSelection (via `Face::is_italic`) and the post table's italicAngle.
    if !italic || !bold {
        if let Some(ff_ref) = font_file_ref(descriptor) {
            let (emb_italic, emb_bold) = *style_cache
                .by_font_file
                .entry(ff_ref)
                .or_insert_with(|| embedded_style_flags(doc, ff_ref));
            italic = italic || emb_italic;
            bold = bold || emb_bold;
        }
    }
    (italic, bold)
}

/// Style flags parsed from an embedded font program stream.
fn embedded_style_flags(doc: &Document, ff_ref: ObjectId) -> (bool, bool) {
    let Some(data) = font_file_data(doc, ff_ref) else {
        return (false, false);
    };
    if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
        (
            face.is_italic() || face.italic_angle().abs() >= 4.0,
            face.is_bold(),
        )
    } else if let Some(name) = cff_font_name(&data) {
        // FontFile3 is bare CFF (no sfnt container) — ttf_parser
        // can't open it, but the CFF Name INDEX keeps the real
        // PostScript name ("XXXXXX+Amplitude-LightItalic") even
        // when the descriptor was rewritten to claim upright.
        (
            crate::text_utils::is_italic_font(&name),
            crate::text_utils::is_bold_font(&name),
        )
    } else {
        (false, false)
    }
}

/// First PostScript name from a bare CFF font's Name INDEX (CFF spec §7).
fn cff_font_name(data: &[u8]) -> Option<String> {
    // Header: major(1) minor(1) hdrSize(1) offSize(1); major must be 1.
    if data.len() < 4 || data[0] != 1 {
        return None;
    }
    let hdr_size = data[2] as usize;
    // Name INDEX: count(u16) offSize(u8) offsets[count+1] data
    let count = u16::from_be_bytes([*data.get(hdr_size)?, *data.get(hdr_size + 1)?]) as usize;
    if count == 0 {
        return None;
    }
    let off_size = *data.get(hdr_size + 2)? as usize;
    if !(1..=4).contains(&off_size) {
        return None;
    }
    let read_offset = |idx: usize| -> Option<usize> {
        let at = hdr_size + 3 + idx * off_size;
        let bytes = data.get(at..at + off_size)?;
        let mut v = 0usize;
        for b in bytes {
            v = (v << 8) | *b as usize;
        }
        Some(v)
    };
    let start = read_offset(0)?;
    let end = read_offset(1)?;
    if start == 0 || end < start {
        return None;
    }
    // Offsets are 1-based from the byte before the object data.
    let objects_base = hdr_size + 3 + (count + 1) * off_size - 1;
    let name = data.get(objects_base + start..objects_base + end)?;
    Some(String::from_utf8_lossy(name).to_string())
}

/// FontFile2/FontFile3 stream reference from a FontDescriptor.
fn font_file_ref(descriptor: &lopdf::Dictionary) -> Option<ObjectId> {
    descriptor
        .get(b"FontFile2")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .or_else(|| {
            descriptor
                .get(b"FontFile3")
                .ok()
                .and_then(|o| o.as_reference().ok())
        })
}

/// Decompressed embedded font program bytes.
fn font_file_data(doc: &Document, ff_ref: ObjectId) -> Option<Vec<u8>> {
    let stream = doc
        .get_object(ff_ref)
        .and_then(lopdf::Object::as_stream)
        .ok()?;
    Some(
        stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone()),
    )
}

/// Decode text from a PDF string operand using font CMaps, encodings, and fallbacks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_text_from_operand(
    obj: &Object,
    current_font: &str,
    base_font_name: Option<&str>,
    font_cmaps: &FontCMaps,
    font_tounicode_refs: &std::collections::HashMap<String, u32>,
    inline_cmaps: &std::collections::HashMap<String, crate::tounicode::CMapEntry>,
    font_encodings: &PageFontEncodings,
    encoding_cache: &HashMap<String, Encoding<'_>>,
    cmap_decisions: &mut CMapDecisionCache,
    font_widths: &PageFontWidths,
) -> Option<String> {
    let is_type0_cid_font = font_widths
        .get(current_font)
        .is_some_and(|info| info.is_cid);
    let use_cp1252_fallback =
        should_use_cp1252_single_byte_fallback(base_font_name, is_type0_cid_font);
    let result = (|| -> Option<String> {
        if let Object::String(bytes, _) = obj {
            let mut decode_with_entry = |entry: &crate::tounicode::CMapEntry| -> Option<String> {
                // For single-byte CMaps, merge CMap + Differences at the byte level:
                // try CMap first, then Differences, then Latin-1 fallback per byte.
                // This prevents partial CMap results from blocking the Differences path.
                if entry.primary.code_byte_length == 1 {
                    let encoding_map = font_encodings.get(current_font);
                    let decoded: String = bytes
                        .iter()
                        .filter_map(|&b| {
                            let code = b as u16;
                            // 1. Primary CMap
                            if let Some(s) = entry.primary.lookup(code) {
                                if !s.contains('\u{FFFD}') {
                                    return Some(s);
                                }
                            }
                            // 2. Fallback CMap (embedded font cmap)
                            if let Some(fb) = entry.fallback.as_ref().and_then(|c| c.lookup(code)) {
                                if !fb.contains('\u{FFFD}') {
                                    return Some(fb);
                                }
                            }
                            // 3. Differences mapped it? Use Differences result
                            if let Some(map) = encoding_map {
                                if let Some(&ch) = map.get(&b) {
                                    return Some(ch.to_string());
                                }
                            }
                            // 4. Printable single-byte fallback
                            if b >= 0x20 {
                                return Some(
                                    decode_single_byte_fallback_char(b, use_cp1252_fallback)
                                        .to_string(),
                                );
                            }
                            None
                        })
                        .collect();
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                    return None;
                }

                // 2-byte CMap: use standard decode_cids path
                if bytes.len() % 2 == 1 {
                    // Some PDFs emit 1-byte codes even for Type0 fonts; try per-byte lookup
                    let lookups = entry.primary.lookup_bytes(bytes);
                    let decoded: String = lookups
                        .iter()
                        .filter_map(|&(_b, ref cmap_result)| cmap_result.clone())
                        .collect();
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
                let decoded_primary = entry.primary.decode_cids(bytes);
                if let Some(remapped) = entry.remapped.as_ref() {
                    let decoded_remap = remapped.decode_cids(bytes);
                    let decoded_fallback = entry.fallback.as_ref().map(|c| c.decode_cids(bytes));

                    if let Some(choice) = cmap_decisions
                        .get_choice(font_tounicode_refs.get(current_font).copied().unwrap_or(0))
                    {
                        let decoded = match choice {
                            CMapChoice::Primary => decoded_primary.clone(),
                            CMapChoice::Remapped => decoded_remap.clone(),
                        };
                        if !decoded.is_empty() {
                            return Some(decoded);
                        }
                    }

                    let choice = cmap_decisions.consider(
                        font_tounicode_refs.get(current_font).copied().unwrap_or(0),
                        &decoded_primary,
                        &decoded_remap,
                        bytes.len(),
                    );
                    let mut decoded = match choice {
                        Some(CMapChoice::Primary) => decoded_primary,
                        Some(CMapChoice::Remapped) => decoded_remap,
                        None => choose_best_cmap_decode(decoded_primary, decoded_remap),
                    };
                    if let Some(fb) = decoded_fallback {
                        let expected = bytes.len() / 2;
                        let decoded_len = decoded.chars().count();
                        let prefer_fallback = (!fb.is_empty() && decoded.is_empty())
                            || (!fb.is_empty() && expected > 0 && decoded_len * 2 < expected);
                        if prefer_fallback || score_text(&fb) > score_text(&decoded) + 3 {
                            decoded = fb;
                        }
                    }
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                } else if !decoded_primary.is_empty() {
                    if let Some(fb) = entry.fallback.as_ref().map(|c| c.decode_cids(bytes)) {
                        let expected = bytes.len() / 2;
                        let decoded_len = decoded_primary.chars().count();
                        let prefer_fallback = (!fb.is_empty() && decoded_primary.is_empty())
                            || (!fb.is_empty() && expected > 0 && decoded_len * 2 < expected);
                        if prefer_fallback || score_text(&fb) > score_text(&decoded_primary) + 3 {
                            return Some(fb);
                        }
                    }
                    return Some(decoded_primary);
                }

                None
            };

            let mut has_cmap = false;
            if let Some(entry) = inline_cmaps.get(current_font) {
                has_cmap = true;
                if let Some(decoded) = decode_with_entry(entry) {
                    return Some(decoded);
                }
            }

            // Look up CMap by ToUnicode object reference
            if let Some(&obj_num) = font_tounicode_refs.get(current_font) {
                if let Some(entry) = font_cmaps.get_by_obj(obj_num) {
                    has_cmap = true;
                    if let Some(decoded) = decode_with_entry(entry) {
                        return Some(decoded);
                    }
                }
            }

            // CID fonts with a CMap that couldn't decode: the CID is genuinely
            // unmapped. Don't fall through to text-interpretation fallbacks
            // (Latin-1, UTF-16, etc.) which would misinterpret CID bytes as
            // character codes (e.g. CID 0x01A9 → Latin-1 "©").
            if is_type0_cid_font && bytes.iter().any(|&b| b > 0x7F) {
                // 2-byte CIDs (Identity-H) are by far the common case; for
                // an odd byte count we still emit at least one marker so
                // detection downstream fires.
                let cid_count = (bytes.len() / 2).max(1);
                return Some("\u{FFFD}".repeat(cid_count));
            }

            // Try our custom encoding map from Differences arrays.
            // The Differences array overrides specific codes in a base encoding (typically
            // WinAnsiEncoding). We must combine Differences entries with the base encoding
            // rather than using filter_map which silently drops unmapped bytes.
            if let Some(encoding_map) = font_encodings.get(current_font) {
                let has_diff_match = bytes.iter().any(|b| encoding_map.contains_key(b));
                if has_diff_match {
                    let decoded: String = bytes
                        .iter()
                        .filter_map(|&b| {
                            if let Some(&ch) = encoding_map.get(&b) {
                                Some(ch)
                            } else if b >= 0x20 {
                                // Base encoding fallback for printable bytes.
                                // Most PDFs with simple fonts use WinAnsi/PDFDocEncoding
                                // semantics, not ISO-8859-1 C1 controls.
                                Some(decode_single_byte_fallback_char(b, use_cp1252_fallback))
                            } else {
                                None // Skip unmapped control characters
                            }
                        })
                        .collect();
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
            }

            // Fallback: try UTF-16BE then Latin-1
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let utf16: Vec<u16> = bytes[2..]
                    .chunks_exact(2)
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect();
                let text = String::from_utf16_lossy(&utf16);
                if text.contains('\u{FFFD}') {
                    debug!(
                        "utf16 loss produced replacement for font={} bytes_len={}",
                        current_font,
                        bytes.len()
                    );
                }
                return Some(text);
            }

            // Heuristic UTF-16BE decode when bytes look like UTF-16 (even length, null-heavy)
            if bytes.len() >= 4 && bytes.len() % 2 == 0 {
                let nulls = bytes.iter().filter(|&&b| b == 0).count();
                if nulls * 4 > bytes.len() {
                    let utf16: Vec<u16> = bytes
                        .chunks_exact(2)
                        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                        .collect();
                    let text = String::from_utf16_lossy(&utf16);
                    if score_text(&text) > 0 {
                        return Some(text);
                    }
                }
            }

            // Check for UTF-8 encoded strings before single-byte encoding decoding.
            // Some PDFs incorrectly embed UTF-8 bytes in single-byte encoded fonts
            // (e.g. "José" as UTF-8 [C3 A9] instead of WinAnsi [E9]).
            if bytes.iter().any(|&b| b > 0x7F) {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    return Some(text.to_string());
                }
            }

            // Try to decode using cached font encoding from lopdf
            if let Some(encoding) = encoding_cache.get(current_font) {
                if let Ok(text) = Document::decode_text(encoding, bytes) {
                    let text = normalize_cp1252_controls(text, use_cp1252_fallback);
                    if text.contains('\u{FFFD}') {
                        debug!(
                            "decode_text produced replacement for font={} bytes_len={}",
                            current_font,
                            bytes.len()
                        );
                        if bytes.len() <= 8 {
                            let hex: String = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                            debug!(
                                "decode_text replacement bytes font={} base={:?} hex={}",
                                current_font, base_font_name, hex
                            );
                        }
                        if bytes.iter().all(|&b| (0x20..=0x7E).contains(&b)) {
                            return Some(bytes.iter().map(|&b| b as char).collect());
                        }
                        if let Some(symbol_text) = decode_symbol_fallback(bytes, base_font_name) {
                            return Some(symbol_text);
                        }
                        // For CID fonts (have ToUnicode CMap), the CID is
                        // genuinely unmapped — return None to avoid Latin-1
                        // fallback misinterpreting CID bytes as characters.
                        if has_cmap || font_tounicode_refs.contains_key(current_font) {
                            return None;
                        }
                        // Non-CID fonts: fall through to other methods
                    } else {
                        return Some(text);
                    }
                }
            }

            if let Some(symbol_text) = decode_symbol_fallback(bytes, base_font_name) {
                return Some(symbol_text);
            }

            // Non-CID (Type1 / TrueType / Type3) fonts use single-byte
            // encodings. In practice the fallback should follow WinAnsi for
            // 0x80..=0x9F so bytes like 0x92 become smart punctuation instead
            // of C1 controls that look like CID mojibake.
            Some(decode_single_byte_fallback(bytes, use_cp1252_fallback))
        } else {
            None
        }
    })();
    result.map(|text| {
        let text = clean_symbol_pua(text);
        let text = remap_texcm_math_symbols(text, base_font_name);
        normalize_cp1252_controls(text, use_cp1252_fallback)
    })
}

/// Fix a known producer bug in "TeXCMMathsSymbols" subset fonts (IntechOpen
/// and sibling academic pipelines): the Computer Modern symbol glyphs are
/// misnamed after Latin lookalikes (equal → /onequarter, plus → /thorn, …)
/// and the generated ToUnicode faithfully propagates the wrong names. The
/// remap applies only to text decoded from that font, keyed on the glyphs'
/// observed misnames.
fn remap_texcm_math_symbols(text: String, base_font_name: Option<&str>) -> String {
    let is_texcm = base_font_name.is_some_and(|n| {
        let n = n.rsplit_once('+').map_or(n, |(_, s)| s);
        n.eq_ignore_ascii_case("TeXCMMathsSymbols")
    });
    if !is_texcm {
        return text;
    }
    text.chars()
        .map(|c| match c {
            '¼' => '=',
            '½' => '-',
            'þ' => '+',
            'ð' => '(',
            'Þ' => ')',
            _ => c,
        })
        .collect()
}

fn decode_single_byte_fallback(bytes: &[u8], use_cp1252_fallback: bool) -> String {
    bytes
        .iter()
        .map(|&b| decode_single_byte_fallback_char(b, use_cp1252_fallback))
        .collect()
}

fn decode_single_byte_fallback_char(byte: u8, use_cp1252_fallback: bool) -> char {
    if !use_cp1252_fallback {
        return byte as char;
    }

    match byte {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => byte as char,
    }
}

fn normalize_cp1252_controls(text: String, use_cp1252_fallback: bool) -> String {
    if !use_cp1252_fallback {
        return text;
    }
    if !text
        .chars()
        .any(|ch| ('\u{0080}'..='\u{009F}').contains(&ch))
    {
        return text;
    }

    text.chars()
        .map(|ch| {
            if ('\u{0080}'..='\u{009F}').contains(&ch) {
                decode_single_byte_fallback_char(ch as u8, true)
            } else {
                ch
            }
        })
        .collect()
}

fn should_use_cp1252_single_byte_fallback(
    base_font_name: Option<&str>,
    is_type0_cid_font: bool,
) -> bool {
    if is_type0_cid_font {
        return false;
    }

    let Some(base_font_name) = base_font_name else {
        return true;
    };
    let font_name = base_font_name
        .rsplit_once('+')
        .map_or(base_font_name, |(_, stripped)| stripped)
        .to_ascii_lowercase();

    // TeX/Computer Modern and math/symbol fonts often place ligatures or
    // symbols in the C1 byte range. Treating those bytes as Windows-1252 makes
    // words like "deficiente" become "de…ciente" and "fluid" become "‡uid".
    let non_cp1252_prefixes = [
        "cmr", "cmb", "cmmi", "cmsy", "cmex", "cmtt", "cmss", "cmti", "ecrm", "ecbx", "ecti",
        "tcrm", "tctt", "msam", "msbm", "ttdc",
    ];
    if non_cp1252_prefixes
        .iter()
        .any(|prefix| font_name.starts_with(prefix))
    {
        return false;
    }

    let non_cp1252_names = ["math", "symbol", "dingbat", "emoji"];
    !non_cp1252_names.iter().any(|name| font_name.contains(name))
}

/// Replace PUA characters in the F000-F0FF range with standard Unicode equivalents.
/// These come from Symbol/Wingdings fonts whose ToUnicode CMaps map to PUA.
fn clean_symbol_pua(text: String) -> String {
    if !text.chars().any(|c| ('\u{F000}'..='\u{F0FF}').contains(&c)) {
        return text;
    }
    text.chars()
        .map(|c| {
            let code = c as u32;
            if !(0xF000..=0xF0FF).contains(&code) {
                return c;
            }
            let low = code - 0xF000;
            match low {
                // Common bullets
                0xA1 | 0xA7 | 0xB7 => '\u{2022}',
                // Checkmark
                0xFC => '\u{2713}',
                // Printable ASCII range and Latin-1 above: strip F000 offset
                0x20..=0xFF => char::from_u32(low).unwrap_or(c),
                _ => c,
            }
        })
        .collect()
}

fn decode_symbol_fallback(bytes: &[u8], base_font_name: Option<&str>) -> Option<String> {
    let name = base_font_name?.to_ascii_lowercase();
    if !name.contains("symbol") && !name.contains("wingdings") && !name.contains("zapfdingbats") {
        return None;
    }
    let mut out = String::new();
    for &b in bytes {
        if b < 0x20 {
            continue;
        }
        if let Some(ch) = char::from_u32(0xF000 + b as u32) {
            out.push(ch);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn choose_best_cmap_decode(primary: String, remapped: String) -> String {
    if primary.is_empty() {
        return remapped;
    }
    if remapped.is_empty() {
        return primary;
    }
    let score_primary = score_text(&primary);
    let score_remap = score_text(&remapped);
    if score_remap > score_primary + 3 {
        remapped
    } else {
        primary
    }
}

fn score_text(text: &str) -> i32 {
    const COMMON_WORDS: [&str; 22] = [
        "the", "and", "of", "to", "in", "a", "is", "that", "for", "with", "on", "as", "by", "from",
        "this", "be", "are", "at", "or", "not", "it", "our",
    ];

    let mut letters = 0i32;
    let mut spaces = 0i32;
    let mut digits = 0i32;
    let mut other = 0i32;
    let mut word_hits = 0i32;

    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            letters += 1;
            current.push(ch.to_ascii_lowercase());
        } else {
            if !current.is_empty() {
                if COMMON_WORDS.iter().any(|w| *w == current) {
                    word_hits += 1;
                }
                current.clear();
            }
            if ch == ' ' {
                spaces += 1;
            } else if ch.is_ascii_digit() {
                digits += 1;
            } else if ch.is_control() || ch == '\u{FFFD}' {
                other += 3;
            } else if ('\u{4E00}'..='\u{9FFF}').contains(&ch)
                || ('\u{3040}'..='\u{309F}').contains(&ch)
                || ('\u{30A0}'..='\u{30FF}').contains(&ch)
                || ('\u{3400}'..='\u{4DBF}').contains(&ch)
                || ('\u{F900}'..='\u{FAFF}').contains(&ch)
            {
                letters += 1; // CJK ideographs / kana count as valid text
            } else {
                other += 1;
            }
        }
    }
    if !current.is_empty() && COMMON_WORDS.iter().any(|w| *w == current) {
        word_hits += 1;
    }

    let mut score = word_hits * 10 + letters + spaces * 2 + digits - other * 2;
    if letters > 15 && word_hits == 0 {
        score -= 15;
    }
    score
}

#[cfg(test)]
mod tests {

    #[test]
    fn texcm_math_symbols_remap() {
        assert_eq!(
            super::remap_texcm_math_symbols("S ¼ kB þ 1".into(), Some("EEKVNO+TeXCMMathsSymbols")),
            "S = kB + 1"
        );
        // Other fonts keep their genuine fractions/thorns.
        assert_eq!(
            super::remap_texcm_math_symbols("¼ cup þorn".into(), Some("Times-Roman")),
            "¼ cup þorn"
        );
        assert_eq!(super::remap_texcm_math_symbols("¼".into(), None), "¼");
    }

    use super::*;
    use lopdf::dictionary;

    fn make_font_info(widths: &[(u16, u16)], default_width: u16, is_cid: bool) -> FontWidthInfo {
        FontWidthInfo {
            widths: widths.iter().copied().collect(),
            default_width,
            space_width: widths
                .iter()
                .find(|(k, _)| *k == 32)
                .map(|(_, v)| *v)
                .unwrap_or(default_width),
            is_cid,
            units_scale: 0.001,
            wmode: 0,
        }
    }

    fn doc_with_descriptor(descriptor: lopdf::Dictionary) -> (Document, lopdf::Dictionary) {
        let mut doc = Document::with_version("1.4");
        let desc_id = doc.add_object(descriptor);
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Tc1",
            "FontDescriptor" => desc_id,
        };
        (doc, font_dict)
    }

    #[test]
    fn descriptor_italic_angle_sets_italic() {
        // Subset font with an opaque BaseFont name ("Tc1") — the name
        // heuristic sees nothing, the descriptor carries the truth.
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => -12,
            "Flags" => 32,
        });
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (true, false)
        );
    }

    #[test]
    fn descriptor_italic_flag_bit_sets_italic() {
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => 64, // bit 7: Italic
        });
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (true, false)
        );
    }

    #[test]
    fn descriptor_force_bold_flag_sets_bold() {
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => 1 << 18, // ForceBold
        });
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (false, true)
        );
    }

    #[test]
    fn tiny_italic_angle_is_not_italic() {
        // A token 1-degree slant is optical correction, not italic.
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => lopdf::Object::Real(-1.0),
            "Flags" => 32,
        });
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (false, false)
        );
    }

    #[test]
    fn missing_descriptor_yields_no_flags() {
        let doc = Document::with_version("1.4");
        let font_dict = dictionary! { "Type" => "Font", "BaseFont" => "Tc1" };
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (false, false)
        );
    }

    #[test]
    fn type0_descendant_descriptor_is_resolved() {
        let mut doc = Document::with_version("1.4");
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+F1",
            "ItalicAngle" => -15,
        });
        let cid_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "FontDescriptor" => desc_id,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "ABCDEF+F1",
            "DescendantFonts" => vec![lopdf::Object::Reference(cid_id)],
        };
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (true, false)
        );
    }

    /// Bare CFF: header + Name INDEX only — enough for `cff_font_name`.
    fn bare_cff_with_name(name: &str) -> Vec<u8> {
        let mut data = vec![1, 0, 4, 1]; // major, minor, hdrSize, offSize
        data.extend_from_slice(&1u16.to_be_bytes()); // Name INDEX count
        data.push(1); // offSize
        data.push(1); // offset of first name
        data.push(1 + name.len() as u8); // offset past last name
        data.extend_from_slice(name.as_bytes());
        data
    }

    #[test]
    fn embedded_font_style_is_cached_by_font_file_object() {
        use lopdf::{Object, Stream};

        let mut doc = Document::with_version("1.4");
        let ff_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            bare_cff_with_name("ABCDEF+Test-BoldItalic"),
        )));
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+Test-BoldItalic",
            "ItalicAngle" => 0,
            "Flags" => 32,
            "FontFile3" => ff_id,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Tc1",
            "FontDescriptor" => desc_id,
        };

        let mut cache = FontStyleCache::new();
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut cache),
            (true, true)
        );
        assert_eq!(cache.by_font_file.len(), 1);

        // Replace the font program with garbage: a repeat call must serve
        // the memo instead of re-reading the stream — repeated per-page
        // decompression is exactly what the cache exists to avoid.
        doc.objects.insert(
            ff_id,
            Object::Stream(Stream::new(dictionary! {}, vec![0u8; 4])),
        );
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut cache),
            (true, true)
        );
        // A cold cache parses the (now garbage) stream, proving the warm
        // call above answered from the memo.
        assert_eq!(
            descriptor_style_flags(&doc, &font_dict, &mut FontStyleCache::new()),
            (false, false)
        );
    }

    #[test]
    fn compute_string_width_ts_no_tc_tw() {
        // Without Tc/Tw (both 0), width = glyph widths only
        let fi = make_font_info(&[(72, 500), (101, 400), (108, 300)], 600, false);
        let bytes = b"Hello"; // H=500, e=400, l=300, l=300, o=600(default)
        let w = compute_string_width_ts(bytes, &fi, 10.0, 0.0, 0.0);
        // (500+400+300+300+600) * 0.001 * 10 = 21.0
        assert!((w - 21.0).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_with_positive_tc() {
        // Positive Tc adds char_spacing per character
        let fi = make_font_info(&[], 500, false);
        let bytes = b"ab"; // 2 chars, each 500 default
        let w = compute_string_width_ts(bytes, &fi, 10.0, 0.5, 0.0);
        // glyph: (500+500)*0.001*10 = 10.0, Tc: 2*0.5 = 1.0, total = 11.0
        assert!((w - 11.0).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_with_negative_tc() {
        // Negative Tc (tight tracking) reduces width
        let fi = make_font_info(&[], 500, false);
        let bytes = b"ab";
        let w = compute_string_width_ts(bytes, &fi, 10.0, -0.3, 0.0);
        // glyph: 10.0, Tc: 2*(-0.3) = -0.6, total = 9.4
        assert!((w - 9.4).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_with_tw() {
        // Tw applies only to space characters (byte 0x20)
        let fi = make_font_info(&[(32, 250)], 500, false);
        let bytes = b"a b"; // 'a'=500, ' '=250, 'b'=500
        let w = compute_string_width_ts(bytes, &fi, 10.0, 0.0, 0.8);
        // glyph: (500+250+500)*0.001*10 = 12.5, Tw: 1*0.8 = 0.8, total = 13.3
        assert!((w - 13.3).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_with_tc_and_tw() {
        // Both Tc and Tw
        let fi = make_font_info(&[(32, 250)], 500, false);
        let bytes = b"a b"; // 3 chars, 1 space
        let w = compute_string_width_ts(bytes, &fi, 10.0, 0.1, 0.5);
        // glyph: 12.5, Tc: 3*0.1 = 0.3, Tw: 1*0.5 = 0.5, total = 13.3
        assert!((w - 13.3).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_cid_font() {
        // CID font: 2-byte codes, space is CID 32
        let fi = make_font_info(&[(65, 500), (32, 250)], 600, true);
        // "A " in CID: [0,65, 0,32]
        let bytes = &[0u8, 65, 0, 32];
        let w = compute_string_width_ts(bytes, &fi, 12.0, 0.2, 0.3);
        // glyph: (500+250)*0.001*12 = 9.0, Tc: 2*0.2 = 0.4, Tw: 1*0.3 = 0.3
        assert!((w - 9.7).abs() < 0.01);
    }

    #[test]
    fn compute_string_width_ts_large_tc() {
        // Large Tc (character-spreading) is applied in full
        let fi = make_font_info(&[], 500, false);
        let bytes = b"abc"; // 3 chars
        let w = compute_string_width_ts(bytes, &fi, 10.0, 5.0, 0.0);
        // glyph: (500*3)*0.001*10 = 15.0, Tc: 3*5.0 = 15.0, total = 30.0
        assert!((w - 30.0).abs() < 0.01);
    }

    #[test]
    fn score_text_cjk() {
        // Correct Japanese text should score well
        let japanese = "2026年9月期 1Q 業績報告";
        // Garbled output (random CJK from wrong remap)
        let garbled = "\u{FFFD}\u{FFFD}\u{FFFD}";

        let s_jp = score_text(japanese);
        let s_garbled = score_text(garbled);
        assert!(
            s_jp > s_garbled,
            "Japanese text ({s_jp}) should score higher than garbled ({s_garbled})"
        );
    }

    #[test]
    fn score_text_cjk_vs_ascii_garbage() {
        // Real CJK text
        let cjk = "株式会社の業績についてご報告いたします";
        // Ascii garbage of similar length
        let garbage = "}{|~`^@#$%&*()!<>[];:',./";

        let s_cjk = score_text(cjk);
        let s_garbage = score_text(garbage);
        assert!(
            s_cjk > s_garbage,
            "CJK text ({s_cjk}) should score higher than garbage ({s_garbage})"
        );
    }

    #[test]
    fn score_text_english_still_works() {
        let good = "the quick brown fox and the lazy dog";
        let bad = "###!!!@@@$$$";
        assert!(score_text(good) > score_text(bad));
    }

    fn doc_with_private_differences() -> (Document, lopdf::ObjectId) {
        let mut doc = Document::with_version("1.7");
        let encoding_id = doc.add_object(dictionary! {
            "Differences" => Object::Array(vec![
                Object::Integer(0x88),
                Object::Name(b"g431".to_vec()),
                Object::Name(b"fi".to_vec()),
                Object::Integer(0xAD),
                Object::Name(b"fl".to_vec()),
            ]),
        });

        (doc, encoding_id)
    }

    #[test]
    fn aptos_private_g431_maps_to_ff_ligature() {
        let (doc, encoding_id) = doc_with_private_differences();
        let font_dict = dictionary! {
            "BaseFont" => Object::Name(b"NJEQOD+Aptos".to_vec()),
            "Encoding" => Object::Reference(encoding_id),
        };

        let result = parse_font_encoding(&doc, &font_dict).expect("encoding should parse");

        assert_eq!(result.map.get(&0x88u8), Some(&'\u{FB00}'));
        assert_eq!(result.map.get(&0x89u8), Some(&'\u{FB01}'));
        assert_eq!(result.map.get(&0xADu8), Some(&'\u{FB02}'));
    }

    #[test]
    fn private_g431_does_not_map_for_unrelated_fonts() {
        let (doc, encoding_id) = doc_with_private_differences();
        let font_dict = dictionary! {
            "BaseFont" => Object::Name(b"ABCDEF+OtherFont".to_vec()),
            "Encoding" => Object::Reference(encoding_id),
        };

        let result = parse_font_encoding(&doc, &font_dict).expect("encoding should parse");

        assert!(!result.map.contains_key(&0x88u8));
        assert_eq!(result.map.get(&0x89u8), Some(&'\u{FB01}'));
        assert_eq!(result.map.get(&0xADu8), Some(&'\u{FB02}'));
    }

    #[test]
    fn cid_font_with_unparseable_cmap_does_not_emit_latin1_mojibake() {
        // Type0/CID font (font_widths reports `is_cid=true`) where the
        // ToUnicode CMap couldn't be parsed (FontCMaps doesn't have the
        // obj_num). Bytes are a 2-byte CID stream containing high bytes
        // that aren't valid UTF-8 — exactly the case in the production
        // samples (Identity-H text where the ToUnicode CMap was missing
        // or malformed, scrape_id 019de78c-..., e.g. "Í Ù Z)¿").
        //
        // Without the guard, the function falls through to the byte-by-byte
        // Latin-1 fallback and produces "ÍÙ" (U+00CD U+00D9). The correct
        // behavior is to emit U+FFFD per CID so downstream
        // `detect_encoding_issues` flags the page for OCR.
        let bytes = vec![0xCD_u8, 0xD9, 0xCD, 0xD9];
        let obj = Object::String(bytes, lopdf::StringFormat::Hexadecimal);

        let font_cmaps = FontCMaps::default();
        let mut font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        font_tounicode_refs.insert("F0".to_string(), 999);
        let inline_cmaps = HashMap::new();
        let font_encodings: PageFontEncodings = HashMap::new();
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F0".to_string(), make_font_info(&[], 1000, true));

        let result = extract_text_from_operand(
            &obj,
            "F0",
            None,
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        );

        let text = result.expect("CID font fallback should still emit a marker");
        assert!(
            !text.contains('\u{00CD}') && !text.contains('\u{00D9}'),
            "CID font with unparseable CMap leaked Latin-1 mojibake: {text:?}"
        );
        assert!(
            text.contains('\u{FFFD}'),
            "CID font with unparseable CMap should emit U+FFFD so detect_encoding_issues fires: {text:?}"
        );
    }

    #[test]
    fn simple_font_single_byte_fallback_passes_high_bytes_through() {
        // A Type1/TrueType simple font (is_cid=false) with a `/ToUnicode`
        // reference but no usable CMap and no `/Differences` map.
        // Per-byte fallback is the canonical interpretation here — these
        // bytes are character codes, not CIDs. The CID guard must NOT strip
        // them. Reproduces the false positive that an earlier version of the
        // guard introduced for fonts in PDFs like pdf-evals/Navigating-
        // Artificial-Intelligence-..., where bytes like 0xB6 are legitimate
        // single-byte character codes.
        let bytes = vec![0x24_u8, 0x47, 0xB6, 0x56]; // "$G¶V"
        let obj = Object::String(bytes, lopdf::StringFormat::Hexadecimal);

        let font_cmaps = FontCMaps::default();
        let mut font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        font_tounicode_refs.insert("F1".to_string(), 999);
        let inline_cmaps = HashMap::new();
        let font_encodings: PageFontEncodings = HashMap::new();
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F1".to_string(), make_font_info(&[], 1000, false));

        let text = extract_text_from_operand(
            &obj,
            "F1",
            None,
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("simple font should round-trip Latin-1 bytes");
        assert_eq!(text, "$G\u{00B6}V");
        assert!(
            !text.contains('\u{FFFD}'),
            "simple font fallback must not stamp FFFD over legitimate bytes: {text:?}"
        );
    }

    #[test]
    fn simple_font_single_byte_fallback_maps_cp1252_punctuation() {
        let bytes = vec![b'l', 0x92_u8, b'a', b'c', b'a', b'd'];
        let obj = Object::String(bytes, lopdf::StringFormat::Hexadecimal);

        let font_cmaps = FontCMaps::default();
        let font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        let inline_cmaps = HashMap::new();
        let font_encodings: PageFontEncodings = HashMap::new();
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let font_widths: PageFontWidths = HashMap::new();

        let text = extract_text_from_operand(
            &obj,
            "F1",
            None,
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("simple font should decode CP1252 punctuation");

        assert_eq!(text, "l’acad");
    }

    #[test]
    fn cached_encoding_decode_normalizes_cp1252_controls() {
        let text = normalize_cp1252_controls("d\u{92}un \u{96} test".to_string(), true);
        assert_eq!(text, "d’un – test");
    }

    #[test]
    fn tex_font_decode_keeps_c1_ligature_bytes_unmodified() {
        let text = normalize_cp1252_controls("de\u{85}ciente \u{87}uid".to_string(), false);
        assert_eq!(text, "de\u{85}ciente \u{87}uid");
        assert!(!should_use_cp1252_single_byte_fallback(
            Some("TTdcr10"),
            false
        ));
        assert!(!should_use_cp1252_single_byte_fallback(
            Some("cmr10"),
            false
        ));
    }

    #[test]
    fn winansi_text_font_uses_cp1252_fallback() {
        assert!(should_use_cp1252_single_byte_fallback(
            Some("BJPQNQ+Times-Roman"),
            false
        ));
    }

    fn gid_font_doc(bfchar: Option<&str>) -> (Document, lopdf::ObjectId) {
        use lopdf::Stream;
        let mut doc = Document::with_version("1.4");
        let cmap = format!(
            "/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
{}
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end",
            bfchar.unwrap_or_default()
        );
        let tounicode_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            cmap.into_bytes(),
        )));
        let enc_id = doc.add_object(dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![
                1.into(),
                Object::Name(b"gid1283".to_vec()),
                Object::Name(b"gid1464".to_vec()),
            ],
        });
        let mut font = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "ABCDEF+OpenSymbol",
            "Encoding" => Object::Reference(enc_id),
        };
        if bfchar.is_some() {
            font.set("ToUnicode", Object::Reference(tounicode_id));
        }
        let font_id = doc.add_object(font);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages",
            "Count" => Object::Integer(1),
            "Kids" => vec![Object::Reference(page_id)],
        });
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    fn gid_flagged(bfchar: Option<&str>) -> bool {
        let (doc, page_id) = gid_font_doc(bfchar);
        let cmaps = FontCMaps::from_doc(&doc);
        let fonts = doc.get_page_fonts(page_id).unwrap();
        let (_, has_gid_fonts) = build_font_encodings(&doc, &fonts, &cmaps);
        has_gid_fonts
    }

    #[test]
    fn gid_differences_with_covering_tounicode_are_not_flagged() {
        // LibreOffice subsets write /gidNNNN Differences names alongside a
        // ToUnicode CMap that decodes those codes; the page must not be
        // flagged as unresolvable (which would suppress the whole document's
        // markdown when every page carries such a font).
        assert!(!gid_flagged(Some("<01> <2022>\n<02> <25E6>")));
    }

    #[test]
    fn gid_differences_with_partial_tounicode_are_not_flagged() {
        // An emoji ZWJ sequence maps whole on its first code; the remaining
        // component-glyph codes are subset leftovers, not damage.
        assert!(!gid_flagged(Some(
            "<01> <D83DDC68200DD83DDC69200DD83DDC67>"
        )));
    }

    #[test]
    fn gid_differences_without_tounicode_are_flagged() {
        assert!(
            gid_flagged(None),
            "gid glyphs without ToUnicode are unresolvable"
        );
    }

    #[test]
    fn gid_differences_with_disjoint_tounicode_are_flagged() {
        // A ToUnicode that never addresses the gid codes leaves them
        // unresolvable.
        assert!(gid_flagged(Some("<10> <0041>")));
    }

    #[test]
    fn gid_differences_with_replacement_char_tounicode_are_flagged() {
        // A mapping to U+FFFD is not usable — extraction rejects it as an
        // invalid CMap result — so it must not clear the gid flag.
        assert!(gid_flagged(Some("<01> <FFFD>\n<02> <FFFD>")));
    }
}
