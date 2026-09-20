//! Font width parsing, encoding, and text decoding.

use super::get_number;
use crate::glyph_names::{glyph_name_to_string, glyph_to_char};
use crate::tounicode::FontCMaps;
use crate::types::{
    BaseEncoding, BoldSource, FontEncoding, FontEncodingMap, FontWidthInfo, PageFontEncodings,
    PageFontWidths,
};
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

/// Visual-size scale factors for Type3 fonts, keyed by resource name.
///
/// A Type3 font's glyph space maps to text space through FontMatrix, so the
/// visual height of its glyphs is `nominal_size × |matrix_y| × FontBBox
/// height`. For a well-behaved font (matrix 0.001, bbox ≈ 1000 units) that
/// factor is ≈ 1.0 and the nominal size is already right. TeX PK bitmap
/// fonts (dvips → Distiller) instead use FontMatrix [1 0 0 -1 0 0] with
/// nominal sizes like 0.12, which makes every downstream font-size heuristic
/// (drop caps, sub/superscripts, small-font tables, line heights) see
/// nonsense. Fonts without a usable FontBBox are omitted (treated as 1.0).
pub(crate) fn build_type3_scales(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
) -> HashMap<String, f32> {
    let mut scales = HashMap::new();
    for (font_name, font_dict) in fonts {
        let is_type3 = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| n == b"Type3");
        if !is_type3 {
            continue;
        }
        // Array elements may themselves be indirect references per PDF
        // syntax — resolve before reading the numeric value.
        let num = |o: &Object| {
            let resolved = match o {
                Object::Reference(r) => match doc.get_object(*r) {
                    Ok(inner) => inner,
                    Err(_) => return 0.0,
                },
                other => other,
            };
            match resolved {
                Object::Integer(i) => *i as f32,
                Object::Real(r) => *r,
                _ => 0.0,
            }
        };
        let Some(matrix) = font_dict
            .get(b"FontMatrix")
            .ok()
            .and_then(|o| resolve_array(doc, o))
        else {
            continue;
        };
        let Some(bbox) = font_dict
            .get(b"FontBBox")
            .ok()
            .and_then(|o| resolve_array(doc, o))
        else {
            continue;
        };
        if matrix.len() < 4 || bbox.len() < 4 {
            continue;
        }
        let scale_y = (num(&matrix[2]).powi(2) + num(&matrix[3]).powi(2)).sqrt();
        let bbox_h = (num(&bbox[3]) - num(&bbox[1])).abs();
        let scale = bbox_h * scale_y;

        // `scale` is the glyph box measured in text-space units. For a
        // self-consistent font it lands near 1.0 — the FontMatrix is the
        // reciprocal of the glyph-space em by construction — so the Tf
        // operand is already the rendered size and must be left alone.
        // A modest deviation is normal and must NOT trigger rescaling:
        // FontBBox is the glyph bounding box, not the em box, so it is
        // routinely somewhat smaller (descender..ascender ≈ 0.7) or larger
        // (tall accents > 1.0).
        //
        // Only a wildly inconsistent font gets renormalized. dvips/PK
        // bitmap fonts declare [1 0 0 -1 0 0] with glyphs spanning
        // hundreds of units, giving scale ≈ 159 against a nominal size of
        // 0.12pt — there the declared size carries no information. The
        // band is deliberately wide so that only that class qualifies,
        // while any matrix scale (including non-standard ones like 0.005
        // with a full-em bbox, scale = 5.0) is judged on the product
        // rather than on the matrix alone.
        const CONSISTENT_LO: f32 = 0.25;
        const CONSISTENT_HI: f32 = 4.0;
        if scale.is_finite() && scale > 0.0 && !(CONSISTENT_LO..=CONSISTENT_HI).contains(&scale) {
            scales.insert(String::from_utf8_lossy(font_name).to_string(), scale);
        }
    }
    scales
}

/// Resource names of Type3 fonts whose `FontMatrix` mirrors the y axis
/// (`d < 0`). dvips/PK bitmap fonts declare `[1 0 0 -1 0 0]` and pair it with
/// a y-flipped text matrix so the glyphs render upright; the run geometry
/// must undo the flip when deciding which side of the baseline the glyph box
/// lies on (see `geometry::run_geometry`).
pub(crate) fn build_type3_y_flips(
    doc: &Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &lopdf::Dictionary>,
) -> std::collections::HashSet<String> {
    let mut flipped = std::collections::HashSet::new();
    for (font_name, font_dict) in fonts {
        let is_type3 = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| n == b"Type3");
        if !is_type3 {
            continue;
        }
        let Some(matrix) = font_dict
            .get(b"FontMatrix")
            .ok()
            .and_then(|o| resolve_array(doc, o))
        else {
            continue;
        };
        let Some(d) = matrix.get(3) else {
            continue;
        };
        let d = match d {
            Object::Reference(r) => doc.get_object(*r).ok().and_then(|o| o.as_float().ok()),
            other => other.as_float().ok(),
        };
        if d.is_some_and(|d| d < 0.0) {
            flipped.insert(String::from_utf8_lossy(font_name).to_string());
        }
    }
    flipped
}

/// The name a `TextItem` carries for its font: the `/BaseFont` family name
/// ("ABCDEF+CMMI10"), which identifies the actual face, rather than the
/// arbitrary per-page resource tag ("F2").
///
/// Exception: resource names using Distiller's CID convention (`C2_0`,
/// `C0_1`) are kept as-is — `text_utils::is_cid_font` keys on that prefix
/// for micro-gap joining, and the family name carries no CID marker to
/// replace it. This is a known, deliberate wart: `TextItem::font` is the
/// face name except for this one producer convention. The clean fix is an
/// explicit CID flag on `TextItem`, which touches its ~29 construction
/// sites; do that migration when `TextItem` next changes shape, and delete
/// this carve-out with it.
pub(crate) fn item_font_name<'a>(resource_name: &'a str, base_font: &'a str) -> &'a str {
    if crate::text_utils::is_cid_font(resource_name) {
        resource_name
    } else {
        base_font
    }
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
        b"Type1" | b"TrueType" | b"MMType1" => parse_simple_font_widths(doc, font_dict)
            .or_else(|| base14_fallback_widths(doc, font_dict)),
        b"Type3" => parse_simple_font_widths(doc, font_dict),
        _ => None,
    }
}

/// Fallback metrics for non-embedded base-14 fonts whose dictionary omits
/// `/FirstChar`/`/Widths` (legal per the PDF spec — the reader must supply
/// standard-font metrics). Without this, every glyph advances 0 and all
/// downstream gap-based logic (space synthesis, script detection, table
/// columns) collapses — common in 1990s dvips/Distiller PDFs.
///
/// Widths are resolved per code through the font's Differences encoding when
/// present, falling back to the same single-byte decode the text extractor
/// uses (cp1252-style smart punctuation for 0x80..=0x9F, Latin-1 elsewhere) —
/// so the width of a code always matches the char we extract for it.
fn base14_fallback_widths(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<FontWidthInfo> {
    let base_font = font_dict
        .get(b"BaseFont")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).to_string())?;
    if !crate::extractor::base14::is_base14_font(&base_font) {
        return None;
    }

    let encoding = parse_font_encoding(doc, font_dict);
    let base = encoding
        .as_ref()
        .and_then(|r| r.base)
        .or_else(|| builtin_base_encoding(doc, font_dict));
    let named_codes = encoding
        .as_ref()
        .map(|r| r.named_codes.clone())
        .unwrap_or_default();
    let sequences = encoding
        .as_ref()
        .map(|r| r.sequences.clone())
        .unwrap_or_default();
    let enc_map = encoding.map(|r| r.map).unwrap_or_default();

    let mut widths = HashMap::new();
    for code in 0u16..=255 {
        // A ligature named by its components is as wide as its letters.
        if let Some(text) = sequences.get(&(code as u8)) {
            if let Some(total) = text
                .chars()
                .map(|ch| crate::extractor::base14::base14_char_width(&base_font, ch))
                .sum::<Option<u16>>()
            {
                widths.insert(code, total);
            }
            continue;
        }
        // Resolution order: Differences override, then the font's base
        // encoding — the dictionary's `/BaseEncoding`, or the BUILT-IN
        // encoding of Symbol/ZapfDingbats when no named encoding replaces
        // it (`builtin_base_encoding`, the choice `build_font_encodings`
        // makes), whose glyphs live at positions unrelated to cp1252 (the
        // renderer draws α for Symbol 0x61 no matter how the text decoder
        // transliterates it, so the advance must be α's) — then the
        // cp1252-style fallback used by the text decoder. The same order
        // the decoder follows, so the width of a code always matches the
        // char extracted for it — and, like the decoder, a control byte
        // reads through the Differences alone, and a code the Differences
        // name but cannot map reads as nothing.
        let Some(ch) = enc_map.get(&(code as u8)).copied().or_else(|| {
            (code >= 0x20 && !named_codes.contains(&(code as u8))).then(|| {
                base.and_then(|base| base.char_for(code as u8))
                    .unwrap_or_else(|| decode_single_byte_fallback_char(code as u8, true))
            })
        }) else {
            continue;
        };
        if let Some(w) = crate::extractor::base14::base14_char_width(&base_font, ch) {
            widths.insert(code, w);
        }
    }
    let space_width = widths.get(&32).copied().unwrap_or(250);

    debug!(
        "  base14 fallback widths for {} ({} codes mapped)",
        base_font,
        widths.len()
    );

    Some(FontWidthInfo {
        widths,
        default_width: 500,
        space_width,
        is_cid: false,
        units_scale: 0.001,
        wmode: 0,
    })
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
    let mut assigned = 0usize;
    while i < w_array.len() {
        if assigned >= crate::tounicode::MAX_CID_W_EXPANSION {
            return;
        }
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
                    if !assign_cid_width(
                        widths,
                        start_cid.wrapping_add(j as u16),
                        w_obj,
                        &mut assigned,
                    ) {
                        return;
                    }
                }
                i += 1;
            }
            Object::Reference(r) => {
                // Could be a reference to an array
                if let Ok(Object::Array(arr)) = doc.get_object(*r) {
                    for (j, w_obj) in arr.iter().enumerate() {
                        if !assign_cid_width(
                            widths,
                            start_cid.wrapping_add(j as u16),
                            w_obj,
                            &mut assigned,
                        ) {
                            return;
                        }
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
                if !assign_cid_width_range(widths, start_cid, end, w, &mut assigned) {
                    return;
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
                if !assign_cid_width_range(widths, start_cid, end, w, &mut assigned) {
                    return;
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
}

fn assign_cid_width(
    widths: &mut HashMap<u16, u16>,
    cid: u16,
    w_obj: &Object,
    assigned: &mut usize,
) -> bool {
    let w = match w_obj {
        Object::Integer(n) => *n as u16,
        Object::Real(n) => *n as u16,
        _ => return true,
    };
    if *assigned >= crate::tounicode::MAX_CID_W_EXPANSION {
        return false;
    }
    widths.insert(cid, w);
    *assigned += 1;
    true
}

fn assign_cid_width_range(
    widths: &mut HashMap<u16, u16>,
    start: u16,
    end: u16,
    w: u16,
    assigned: &mut usize,
) -> bool {
    if start > end {
        return true;
    }
    for cid in start..=end {
        if *assigned >= crate::tounicode::MAX_CID_W_EXPANSION {
            return false;
        }
        widths.insert(cid, w);
        *assigned += 1;
    }
    true
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
    font_cache: &mut FontStyleCache,
) -> (PageFontEncodings, bool) {
    let mut encodings = PageFontEncodings::new();
    let mut has_gid_fonts = false;

    for (font_name, font_dict) in fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();

        let mut differences = FontEncodingMap::new();
        let mut identity_overrides = FontEncodingMap::new();
        let mut base: Option<BaseEncoding> = None;
        let mut named_codes = std::collections::HashSet::new();
        let mut sequences: HashMap<u8, String> = HashMap::new();
        // A Type3 font's Differences name its glyph procedures: a numbered
        // name there (`g10`) labels a procedure and indexes nothing, so the
        // glyph-index reading below is for fonts with a glyph table only.
        let type3 = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| n == b"Type3");
        if let Some(result) = parse_font_encoding(doc, font_dict) {
            base = result.base;
            named_codes = result.named_codes.clone();
            sequences = result.sequences.clone();
            // Names that are glyph indexes (`g12`, `glyph12`, `index12`)
            // say nothing by themselves; the embedded font program says
            // what those glyphs are.
            let by_index = if type3 {
                FontEncodingMap::new()
            } else {
                glyph_index_chars(doc, font_dict, &result.gid_names, font_cache)
            };
            let unresolved: Vec<u8> = if type3 {
                Vec::new()
            } else {
                result
                    .gid_codes
                    .iter()
                    .copied()
                    .filter(|code| !by_index.contains_key(code))
                    .collect()
            };
            if !unresolved.is_empty() && !tounicode_maps_codes(font_dict, cmaps, &unresolved) {
                has_gid_fonts = true;
            }
            if !result.map.is_empty() {
                identity_overrides = stale_identity_cmap_overrides(doc, font_dict, cmaps, &result);
                differences = result.map;
            }
            for (code, ch) in by_index {
                differences.entry(code).or_insert(ch);
            }
        }
        // Symbol and ZapfDingbats read through their built-in encodings
        // unless the font names another encoding outright.
        if base.is_none() {
            base = builtin_base_encoding(doc, font_dict);
        }
        let blank_codes = blank_glyph_codes(doc, font_dict, font_cache);
        if !differences.is_empty()
            || !sequences.is_empty()
            || !blank_codes.is_empty()
            || base.is_some()
        {
            encodings.insert(
                resource_name,
                FontEncoding {
                    differences,
                    identity_overrides,
                    blank_codes,
                    base,
                    named_codes,
                    sequences,
                },
            );
        }
    }

    (encodings, has_gid_fonts)
}

/// The encoding a font's `/Encoding` entry names outright — written as a
/// name, or as a reference to a name object — rather than describing in
/// a dictionary.
fn named_encoding(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<Vec<u8>> {
    let encoding = match font_dict.get(b"Encoding").ok()? {
        Object::Reference(id) => doc.get_object(*id).ok()?,
        other => other,
    };
    encoding.as_name().ok().map(<[u8]>::to_vec)
}

/// The built-in encoding a Symbol or ZapfDingbats font reads through: its
/// own when nothing overrides it. An `/Encoding` that names an encoding
/// replaces the built-in one, except that `SymbolEncoding` and
/// `ZapfDingbatsEncoding` — names some producers write, though the
/// specification predefines neither — name the font's own built-in
/// encoding. `None` for other fonts. The text decoder and the base-14
/// width fallback both take this choice, so a code's width is the advance
/// of the glyph the text reads.
fn builtin_base_encoding(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<BaseEncoding> {
    let base_font = font_dict.get(b"BaseFont").ok()?.as_name().ok()?;
    let builtin =
        crate::extractor::base14::builtin_symbol_encoding(&String::from_utf8_lossy(base_font))?;
    let own_name: &[u8] = match builtin {
        BaseEncoding::Symbol => b"SymbolEncoding",
        _ => b"ZapfDingbatsEncoding",
    };
    match named_encoding(doc, font_dict) {
        None => Some(builtin),
        Some(name) => (name == own_name).then_some(builtin),
    }
}

/// The characters of the glyphs that `/Differences` names by number, read
/// from the embedded font program. A name the program itself gives one of
/// its glyphs wins — subsetters name glyphs `g431` with no regard to their
/// index — then a `cidNN` name is the CID a CID-keyed CFF program maps to
/// a glyph, and otherwise the number is the glyph's index, which is what
/// producers without names for their glyphs mean by it. The glyph's
/// character comes from the program's cmap and glyph names (TrueType or
/// OpenType) or from its glyph names (bare CFF). Codes whose glyph the
/// program does not identify are left out. What each name resolves to is
/// kept in `font_cache` per program, so a font shared across pages is
/// parsed once.
fn glyph_index_chars(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
    names: &[(u8, String)],
    font_cache: &mut FontStyleCache,
) -> FontEncodingMap {
    if names.is_empty() {
        return FontEncodingMap::new();
    }
    let font_file = || -> Option<ObjectId> {
        let descriptor = resolve_dict(doc, font_dict.get(b"FontDescriptor").ok()?)?;
        [b"FontFile2".as_slice(), b"FontFile3".as_slice()]
            .into_iter()
            .find_map(|key| descriptor.get(key).ok()?.as_reference().ok())
    };
    let Some(ff_ref) = font_file() else {
        return FontEncodingMap::new();
    };
    let cached = font_cache
        .numbered_glyphs_by_font_file
        .entry(ff_ref)
        .or_default();
    let unresolved: Vec<&str> = names
        .iter()
        .map(|(_, name)| name.as_str())
        .filter(|name| !cached.contains_key(*name))
        .collect();
    if !unresolved.is_empty() {
        let resolved = font_file_data(doc, ff_ref)
            .map(|data| resolve_numbered_glyph_names(&data, &unresolved))
            .unwrap_or_default();
        for name in unresolved {
            cached.insert(name.to_string(), resolved.get(name).copied().flatten());
        }
    }
    names
        .iter()
        .filter_map(|(code, name)| Some((*code, (*cached.get(name)?)?)))
        .collect()
}

/// Resolve numbered names against one font program stream (see
/// [`glyph_index_chars`]): the character of each name's glyph, `None` for
/// a glyph the program does not identify. A stream holding a TrueType
/// collection is read face by face: a glyph the program names, or a CID
/// it maps, may sit in any member, while a bare index reads in the first.
fn resolve_numbered_glyph_names(data: &[u8], names: &[&str]) -> HashMap<String, Option<char>> {
    /// One face of the stream with its glyph → character map.
    struct Program<'a> {
        face: Option<ttf_parser::Face<'a>>,
        bare_cff: Option<ttf_parser::cff::Table<'a>>,
        by_glyph: HashMap<u16, char>,
    }
    impl Program<'_> {
        fn cff(&self) -> Option<&ttf_parser::cff::Table<'_>> {
            self.face
                .as_ref()
                .and_then(|face| face.tables().cff.as_ref())
                .or(self.bare_cff.as_ref())
        }
        fn glyph_by_name(&self, name: &str) -> Option<u16> {
            match (&self.face, self.cff()) {
                (Some(face), _) => face.glyph_index_by_name(name),
                (None, Some(cff)) => cff.glyph_index_by_name(name),
                (None, None) => None,
            }
            .map(|gid| gid.0)
        }
        fn glyph_by_cid(&self, cid: u16) -> Option<u16> {
            let cff = self.cff()?;
            (0..cff.number_of_glyphs())
                .find(|&gid| cff.glyph_cid(ttf_parser::GlyphId(gid)) == Some(cid))
        }
    }
    let mut programs: Vec<Program> = (0..ttf_parser::fonts_in_collection(data).unwrap_or(1))
        .filter_map(|index| ttf_parser::Face::parse(data, index).ok())
        .map(|face| Program {
            by_glyph: crate::tounicode::build_gid_to_unicode(&face).unwrap_or_default(),
            face: Some(face),
            bare_cff: None,
        })
        .collect();
    if programs.is_empty() {
        if let Some(cff) = ttf_parser::cff::Table::parse(data) {
            programs.push(Program {
                by_glyph: (0..cff.number_of_glyphs())
                    .filter_map(|gid| {
                        let name = cff.glyph_name(ttf_parser::GlyphId(gid))?;
                        glyph_to_char(name).map(|ch| (gid, ch))
                    })
                    .collect(),
                face: None,
                bare_cff: Some(cff),
            });
        }
    }
    let Some(first) = programs.first() else {
        return HashMap::new();
    };
    let resolve = |name: &str| -> Option<char> {
        // A glyph the program names that way is the glyph meant, whatever
        // character it has.
        if let Some((program, gid)) = programs
            .iter()
            .find_map(|program| program.glyph_by_name(name).map(|gid| (program, gid)))
        {
            return program.by_glyph.get(&gid).copied();
        }
        match numbered_glyph_name(name)? {
            NumberedGlyph::Index(index) => first.by_glyph.get(&index).copied(),
            NumberedGlyph::Cid(cid) => {
                if let Some((program, gid)) = programs
                    .iter()
                    .find_map(|program| program.glyph_by_cid(cid).map(|gid| (program, gid)))
                {
                    return program.by_glyph.get(&gid).copied();
                }
                first.by_glyph.get(&cid).copied()
            }
        }
    };
    names
        .iter()
        .map(|&name| (name.to_string(), resolve(name)))
        .collect()
}

/// The predefined single-byte encodings as 256-entry tables, read once
/// through lopdf's font-encoding resolution (its tables are not public):
/// a font dictionary naming the encoding decodes each code on its own.
type PredefinedTable = [Option<char>; 256];

fn predefined_table(name: &[u8]) -> PredefinedTable {
    let doc = Document::new();
    let font = lopdf::dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => Object::Name(name.to_vec())
    };
    let mut table = [None; 256];
    if let Ok(encoding) = font.get_font_encoding(&doc) {
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = Document::decode_text(&encoding, &[code as u8])
                .ok()
                .and_then(|text| {
                    let mut chars = text.chars();
                    match (chars.next(), chars.next()) {
                        (Some(ch), None) => Some(ch),
                        _ => None,
                    }
                });
        }
    }
    table
}

static STANDARD_TABLE: std::sync::LazyLock<PredefinedTable> =
    std::sync::LazyLock::new(|| predefined_table(b"StandardEncoding"));
static WIN_ANSI_TABLE: std::sync::LazyLock<PredefinedTable> =
    std::sync::LazyLock::new(|| predefined_table(b"WinAnsiEncoding"));
static MAC_ROMAN_TABLE: std::sync::LazyLock<PredefinedTable> =
    std::sync::LazyLock::new(|| predefined_table(b"MacRomanEncoding"));
static MAC_EXPERT_TABLE: std::sync::LazyLock<PredefinedTable> =
    std::sync::LazyLock::new(|| predefined_table(b"MacExpertEncoding"));

impl BaseEncoding {
    /// The predefined encoding a `/BaseEncoding` (or `/Encoding`) name
    /// stands for.
    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"StandardEncoding" => Self::Standard,
            b"WinAnsiEncoding" => Self::WinAnsi,
            b"MacRomanEncoding" => Self::MacRoman,
            b"MacExpertEncoding" => Self::MacExpert,
            _ => return None,
        })
    }

    /// The character at `code`, or `None` where the encoding has no glyph.
    pub(crate) fn char_for(self, code: u8) -> Option<char> {
        let table: &PredefinedTable = match self {
            Self::Standard => &STANDARD_TABLE,
            Self::WinAnsi => &WIN_ANSI_TABLE,
            Self::MacRoman => &MAC_ROMAN_TABLE,
            Self::MacExpert => &MAC_EXPERT_TABLE,
            Self::Symbol | Self::ZapfDingbats => {
                return crate::extractor::base14::symbol_encoding_char(self, code)
            }
        };
        table[usize::from(code)]
    }
}

/// Whether `code`, decoded as `label`, is a blank glyph standing in for a
/// word space: the glyph paints nothing but advances. A tab or no-break
/// space label is spacing already and reads as a plain space too; labels
/// that are invisible formatting (soft hyphen, zero-width joiners, byte
/// order mark) keep their meaning, since a blank glyph is exactly what they
/// render as.
fn blank_glyph_reads_as_space(encoding: Option<&FontEncoding>, code: u8, label: &str) -> bool {
    encoding.is_some_and(|map| map.blank_codes.contains(&code))
        && label.chars().any(|c| !is_invisible_format(c))
}

/// Characters that render as nothing by design: soft hyphen, zero-width
/// spaces and joiners, bidi marks, embeddings and isolates, invisible math
/// operators, byte order mark. A blank glyph labelled with one of these is
/// the label's own rendering, not a stale space.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Codes of a symbolic TrueType font whose glyph has no outline but a
/// positive advance. Painted, such a glyph leaves a gap and nothing else, so
/// the code reads as a space whatever the font's ToUnicode says. Word 2011
/// for Mac writes subsets whose ToUnicode labels the space glyph with the
/// code it landed on ("$", "!", "&"), turning every word space into
/// punctuation. Only symbolic fonts without an `/Encoding` are considered:
/// they map codes through their own `cmap`, which is the evidence used here.
/// A font with no outlined glyph at all (an invisible text layer) is left
/// alone. Results are cached per embedded font program.
fn blank_glyph_codes(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
    font_cache: &mut FontStyleCache,
) -> std::collections::HashSet<u8> {
    use ttf_parser::PlatformId;

    let font_file = || -> Option<ObjectId> {
        if font_dict.get(b"Subtype").ok()?.as_name().ok()? != b"TrueType"
            || font_dict.get(b"Encoding").is_ok()
        {
            return None;
        }
        let descriptor = resolve_dict(doc, font_dict.get(b"FontDescriptor").ok()?)?;
        // Flags bit 3: symbolic. Non-symbolic fonts route codes through a
        // standard encoding, not their own cmap.
        if descriptor.get(b"Flags").ok()?.as_i64().ok()? & 4 == 0 {
            return None;
        }
        descriptor.get(b"FontFile2").ok()?.as_reference().ok()
    };
    let Some(ff_ref) = font_file() else {
        return Default::default();
    };
    if let Some(cached) = font_cache.blank_codes_by_font_file.get(&ff_ref) {
        return cached.clone();
    }
    let compute = || -> Option<std::collections::HashSet<u8>> {
        let data = font_file_data(doc, ff_ref)?;
        let face = ttf_parser::Face::parse(&data, 0).ok()?;
        let cmap = face.tables().cmap?;
        // Symbolic fonts address their glyphs through a (1,0) table by
        // code, or a (3,0) table by code in one of the private ranges
        // F000-F2FF, with the bare code as the last resort (PDF 32000-1,
        // 9.6.6.4).
        let glyph_for = |code: u8| -> Option<ttf_parser::GlyphId> {
            let code = u32::from(code);
            for subtable in cmap.subtables {
                let candidates: &[u32] = match (subtable.platform_id, subtable.encoding_id) {
                    (PlatformId::Macintosh, 0) => &[code],
                    (PlatformId::Windows, 0) => {
                        &[0xF000 + code, 0xF100 + code, 0xF200 + code, code]
                    }
                    _ => continue,
                };
                for &candidate in candidates {
                    if let Some(gid) = subtable.glyph_index(candidate) {
                        if gid.0 != 0 {
                            return Some(gid);
                        }
                    }
                }
            }
            None
        };
        let mut blank = std::collections::HashSet::new();
        let mut outlined = 0usize;
        let mut mapped = 0usize;
        // Control codes never carry a word space; Word's subsets start at
        // 0x21 and other producers keep 0x00-0x1F for genuinely blank
        // control glyphs.
        for code in 0x20u8..=255 {
            let Some(gid) = glyph_for(code) else {
                continue;
            };
            mapped += 1;
            if face.glyph_bounding_box(gid).is_some() {
                outlined += 1;
                continue;
            }
            // The glyph program's own advance keeps the result a property
            // of the font file, which is what the cache is keyed by.
            if face.glyph_hor_advance(gid).is_some_and(|w| w > 0) {
                blank.insert(code);
            }
        }
        // Word for Mac also writes a subset per run, so a space painted on
        // its own arrives as a font holding nothing but `.notdef` and that
        // blank glyph, with one code mapped. With no outline anywhere, at
        // most two mapped codes still read as such a space subset; more is
        // an invisible text layer, which keeps its text.
        if blank.is_empty() || (outlined == 0 && mapped > 2) {
            return None;
        }
        debug!(
            "blank glyph codes for {}: {:?}",
            font_dict
                .get(b"BaseFont")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_default(),
            {
                let mut codes: Vec<u8> = blank.iter().copied().collect();
                codes.sort_unstable();
                codes
            }
        );
        Some(blank)
    };
    let blank = compute().unwrap_or_default();
    font_cache
        .blank_codes_by_font_file
        .insert(ff_ref, blank.clone());
    blank
}

/// Some subset producers change the simple font's glyph encoding but retain
/// its original ToUnicode. Repair only corroborated ASCII identity entries:
/// at least three distinct letters move to ASCII slots, their Unicode values remain
/// elsewhere in the old CMap, and the embedded CFF contains those exact glyphs.
/// Other repairs need the same evidence, allowing a single-character case
/// counterpart in the old CMap once the exact matches establish staleness.
/// Keep repairs per font, since different encodings can share one CMap stream.
fn stale_identity_cmap_overrides(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
    cmaps: &FontCMaps,
    encoding: &EncodingResult,
) -> FontEncodingMap {
    let verified = || -> Option<FontEncodingMap> {
        if font_dict.get(b"Subtype").ok()?.as_name().ok()? != b"Type1" {
            return None;
        }
        let cmap_ref = font_dict.get(b"ToUnicode").ok()?.as_reference().ok()?;
        let entry = cmaps.get_by_obj(cmap_ref.0)?;
        if entry.primary.code_byte_length != 1 || entry.remapped.is_some() {
            return None;
        }
        let elsewhere: std::collections::HashSet<String> = (0..=255)
            .filter_map(|code| entry.primary.lookup(code))
            .collect();
        let single_case_match =
            |case: String| case.chars().count() == 1 && elsewhere.contains(&case);
        let candidates: FontEncodingMap = encoding
            .map
            .iter()
            .filter_map(|(&code, &ch)| {
                (code.is_ascii_graphic()
                    && !ch.is_ascii()
                    && (ch.is_alphabetic() || ch == '\u{00a0}')
                    && entry.primary.lookup(code as u16).as_deref()
                        == Some(&(code as char).to_string())
                    && (elsewhere.contains(&ch.to_string())
                        || (ch.is_alphabetic()
                            && (single_case_match(ch.to_lowercase().to_string())
                                || single_case_match(ch.to_uppercase().to_string())))))
                .then_some((code, ch))
            })
            .collect();
        let exact_anchor_count = |map: &FontEncodingMap| {
            map.values()
                .copied()
                .filter(|ch| ch.is_alphabetic() && elsewhere.contains(&ch.to_string()))
                .collect::<std::collections::HashSet<char>>()
                .len()
        };
        if exact_anchor_count(&candidates) < 3 {
            return None;
        }
        let descriptor = resolve_dict(doc, font_dict.get(b"FontDescriptor").ok()?)?;
        let font_ref = descriptor.get(b"FontFile3").ok()?.as_reference().ok()?;
        let stream = doc.get_object(font_ref).ok()?.as_stream().ok()?;
        if stream.dict.get(b"Subtype").ok()?.as_name().ok()? != b"Type1C" {
            return None;
        }
        let data = font_file_data(doc, font_ref)?;
        let cff = ttf_parser::cff::Table::parse(&data)?;
        let overrides: FontEncodingMap = candidates
            .into_iter()
            .filter(|(code, _)| {
                encoding
                    .glyph_names
                    .get(code)
                    .is_some_and(|name| cff.glyph_index_by_name(name).is_some())
            })
            .collect();
        (exact_anchor_count(&overrides) >= 3).then_some(overrides)
    };
    verified().unwrap_or_default()
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

/// Result of parsing an encoding dictionary: its `/BaseEncoding` and its
/// `/Differences` array, either of which may be absent.
pub(crate) struct EncodingResult {
    pub map: FontEncodingMap,
    glyph_names: HashMap<u8, String>,
    /// Character codes whose glyph names are glyph indexes (`gid53`, `g53`,
    /// `glyph53`, `index53`) rather than names. These reference the font
    /// program's glyph table and are decodable only through it or through
    /// the font's ToUnicode CMap.
    pub gid_codes: Vec<u8>,
    /// The name each of `gid_codes` carries.
    pub gid_names: Vec<(u8, String)>,
    /// Every code the `/Differences` array names, mapped or not.
    pub named_codes: std::collections::HashSet<u8>,
    /// Codes whose glyph stands for several characters (see
    /// [`FontEncoding::sequences`]).
    pub sequences: HashMap<u8, String>,
    /// The `/BaseEncoding`, when the dictionary names one.
    pub base: Option<BaseEncoding>,
}

/// A `/Differences` name that spells a number instead of naming a glyph,
/// the forms producers write for glyphs they have no name for: `g53`,
/// `G53`, `glyph53`, `index53` and `gid53` give a glyph index; `cid53`
/// gives a CID, which a CID-keyed program maps to its glyph. Whether such
/// a name is read as a number at all is the font program's to say (see
/// [`glyph_index_chars`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberedGlyph {
    Index(u16),
    Cid(u16),
}

fn numbered_glyph_name(name: &str) -> Option<NumberedGlyph> {
    let (prefix, digits) = ["glyph", "index", "gid", "cid", "g", "G"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix).map(|digits| (*prefix, digits)))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse().ok()?;
    Some(if prefix == "cid" {
        NumberedGlyph::Cid(number)
    } else {
        NumberedGlyph::Index(number)
    })
}

/// Parse an encoding dictionary: `/BaseEncoding`, `/Differences`, or both.
/// `None` when it carries neither.
pub(crate) fn parse_encoding_dictionary(
    doc: &Document,
    enc_dict: &lopdf::Dictionary,
    base_font_name: Option<&str>,
) -> Option<EncodingResult> {
    let base = enc_dict
        .get(b"BaseEncoding")
        .ok()
        .and_then(|o| match o {
            Object::Reference(id) => doc.get_object(*id).ok()?.as_name().ok(),
            other => other.as_name().ok(),
        })
        .and_then(BaseEncoding::from_name);

    let diff_array = match enc_dict.get(b"Differences") {
        Ok(Object::Array(arr)) => arr.clone(),
        Ok(Object::Reference(obj_ref)) => match doc.get_object(*obj_ref) {
            Ok(Object::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    if diff_array.is_empty() && base.is_none() {
        return None;
    }

    let mut encoding_map = FontEncodingMap::new();
    let mut glyph_names = HashMap::new();
    let mut current_code: u8 = 0;
    let mut ligature_count = 0u32;
    let mut gid_codes: Vec<u8> = Vec::new();
    let mut gid_names: Vec<(u8, String)> = Vec::new();
    let mut named_codes = std::collections::HashSet::new();
    let mut sequences: HashMap<u8, String> = HashMap::new();

    for item in diff_array {
        match item {
            Object::Integer(n) => {
                // This sets the starting code for subsequent glyph names
                current_code = n as u8;
            }
            Object::Name(name) => {
                // Map current code to glyph name -> Unicode
                let glyph_name = String::from_utf8_lossy(&name).to_string();
                named_codes.insert(current_code);
                // What the name stands for: one character, or the letters
                // of a ligature named by its components (`f_t`) or by a
                // `uni` sequence.
                let mapped = glyph_name_to_string(&glyph_name).or_else(|| {
                    private_glyph_to_char(&glyph_name, base_font_name).map(String::from)
                });
                let mut chars = mapped.as_deref().unwrap_or_default().chars();
                let mapped_char = match (chars.next(), chars.next()) {
                    (Some(ch), None) => Some(ch),
                    _ => None,
                };
                if mapped_char.is_some_and(is_ligature_char)
                    || mapped.as_ref().is_some_and(|text| text.chars().count() > 1)
                {
                    debug!(
                        "  Differences: code=0x{:02X} glyph={:?} (ligature)",
                        current_code, glyph_name
                    );
                    ligature_count += 1;
                }
                // Numbered names (e.g. "gid00053", "g53", "cid53") say
                // nothing without the font program's glyph table.
                if mapped.is_none() && numbered_glyph_name(&glyph_name).is_some() {
                    gid_codes.push(current_code);
                    gid_names.push((current_code, glyph_name.clone()));
                }
                if let Some(ch) = mapped_char {
                    encoding_map.insert(current_code, ch);
                    glyph_names.insert(current_code, glyph_name);
                } else if let Some(text) = mapped {
                    sequences.insert(current_code, text);
                    glyph_names.insert(current_code, glyph_name);
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
        glyph_names,
        gid_codes,
        gid_names,
        named_codes,
        sequences,
        base,
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

/// Document-scoped memo of facts read from embedded font programs, keyed
/// by the FontFile2/FontFile3 stream's object id: style flags, and the
/// blank-glyph codes of `blank_glyph_codes`. The same font program is
/// referenced from every page that uses the font, and decompressing +
/// parsing it dominates `font_style` — without the memo that
/// cost repeats per page whenever the descriptor leaves a flag unset
/// (the common case: regular fonts report neither italic nor bold).
#[derive(Debug, Default)]
pub(crate) struct FontStyleCache {
    by_font_file: HashMap<ObjectId, FontStyle>,
    /// Blank-glyph codes per embedded font program (see `blank_glyph_codes`),
    /// so a font shared across pages is scanned once.
    blank_codes_by_font_file: HashMap<ObjectId, std::collections::HashSet<u8>>,
    /// The character each numbered `/Differences` name resolves to per
    /// embedded font program (see `glyph_index_chars`), `None` when the
    /// program does not identify it, so a font shared across pages is
    /// parsed once.
    numbered_glyphs_by_font_file: HashMap<ObjectId, HashMap<String, Option<char>>>,
}

impl FontStyleCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Style of a font resource: italic and bold, the weight class, and what
/// the descriptor and the embedded program say about fixed pitch.
///
/// The flags survive subset fonts whose BaseFont names are opaque tags
/// ("Tc1", "ABCDEF+F1") that defeat the name-based bold/italic heuristics.
/// Italic: `ItalicAngle` beyond a few degrees, or Flags bit 7 (Italic,
/// value 64). Bold: Flags bit 19 (ForceBold, value 1<<18). The small
/// ItalicAngle threshold skips fonts that declare a token slant.
///
/// `bold_source` is where `bold` came from: the descriptor's flag or the
/// program's own selection (`FontFlags`), or the PostScript name of a bare
/// CFF program (`FontName`). The `/BaseFont` name is read afterwards by
/// [`FontStyle::with_name`], and the width table by
/// [`FontStyle::with_measured_pitch`], once per page rather than per run.
///
/// `fixed_pitch` is `Some(true)` when the descriptor's FixedPitch flag or
/// the embedded program's `post` table says the face is monospaced, else
/// `None`: an unset flag is no evidence, since producers write `/Flags 4`
/// whatever the face, and the width table is measured later.
///
/// `weight` is the 100..=900 weight class, read in this order: the embedded
/// font program's OS/2 `usWeightClass` (the weight word of the PostScript
/// name for a bare CFF program, which has no OS/2 table), the descriptor's
/// `/FontWeight`, then the weight word of the `/BaseFont` name (see
/// `text_utils::font_weight_from_name`). `None` when none of them says, and
/// for anything but an ordinary text font (Type0, Type1, MMType1,
/// TrueType): a Type3 font is a set of glyph procedures whose name and
/// descriptor say nothing about the ink they draw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FontStyle {
    pub(crate) italic: bool,
    pub(crate) bold: bool,
    pub(crate) bold_source: Option<BoldSource>,
    pub(crate) weight: Option<u16>,
    pub(crate) fixed_pitch: Option<bool>,
}

impl FontStyle {
    /// `(italic, bold)`.
    #[cfg(test)]
    pub(crate) fn flags(self) -> (bool, bool) {
        (self.italic, self.bold)
    }

    /// The style with what the font's name says folded in: a bold word or
    /// style abbreviation makes it bold, and is reported as the source
    /// ahead of the flags; an italic word makes it italic. `name` is the
    /// `/BaseFont`, or the resource tag of a font dictionary without one.
    pub(crate) fn with_name(mut self, name: &str) -> Self {
        if crate::text_utils::is_bold_font(name) {
            self.bold = true;
            self.bold_source = BoldSource::first(self.bold_source, Some(BoldSource::FontName));
        }
        if crate::text_utils::is_italic_font(name) {
            self.italic = true;
        }
        self
    }

    /// The style with the width table's verdict on fixed pitch (see
    /// [`FontWidthInfo::fixed_pitch_by_advance`]) folded in, for a font
    /// whose descriptor and program say nothing about it.
    pub(crate) fn with_measured_pitch(mut self, widths: Option<&FontWidthInfo>) -> Self {
        if self.fixed_pitch.is_none() {
            self.fixed_pitch = widths.and_then(FontWidthInfo::fixed_pitch_by_advance);
        }
        self
    }
}

/// The [`FontStyle`] of a font resource, from its descriptor, its embedded
/// program and its name.
pub(crate) fn font_style(
    doc: &Document,
    font_dict: &lopdf::Dictionary,
    style_cache: &mut FontStyleCache,
) -> FontStyle {
    let has_weight_class = is_ordinary_text_font(font_dict);
    let name_weight = font_dict
        .get(b"BaseFont")
        .ok()
        .filter(|_| has_weight_class)
        .and_then(|obj| obj.as_name().ok())
        .and_then(|name| crate::text_utils::font_weight_from_name(&String::from_utf8_lossy(name)));
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
        return FontStyle {
            weight: name_weight,
            ..FontStyle::default()
        };
    };

    let italic_angle = descriptor
        .get(b"ItalicAngle")
        .ok()
        .and_then(get_number)
        .unwrap_or(0.0);
    // The flags may be written as an indirect object.
    let flags = descriptor
        .get(b"Flags")
        .ok()
        .and_then(|obj| match obj {
            Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_i64().ok()),
            direct => direct.as_i64().ok(),
        })
        .unwrap_or(0);

    let force_bold = flags & (1 << 18) != 0;
    let mut style = FontStyle {
        italic: italic_angle.abs() >= 4.0 || flags & (1 << 6) != 0,
        bold: force_bold,
        bold_source: force_bold.then_some(BoldSource::FontFlags),
        weight: None,
        // Flags bit 1: FixedPitch. Only a set bit is evidence.
        fixed_pitch: (flags & 1 != 0).then_some(true),
    };

    // Descriptors lie: subset generators write ItalicAngle 0 for genuinely
    // italic faces. The embedded font file keeps the truth — OS/2
    // fsSelection (via `Face::is_italic`) and the post table's italicAngle —
    // and it alone carries the weight class.
    if let Some(ff_ref) = font_file_ref(descriptor) {
        let embedded = *style_cache
            .by_font_file
            .entry(ff_ref)
            .or_insert_with(|| embedded_style(doc, ff_ref));
        style.italic |= embedded.italic;
        style.bold |= embedded.bold;
        style.bold_source = BoldSource::first(style.bold_source, embedded.bold_source);
        style.weight = embedded.weight;
        style.fixed_pitch = style.fixed_pitch.or(embedded.fixed_pitch);
    }
    style.weight = if has_weight_class {
        style
            .weight
            .or_else(|| {
                descriptor
                    .get(b"FontWeight")
                    .ok()
                    .and_then(|obj| resolve_number(doc, obj))
                    .and_then(weight_class)
            })
            .or(name_weight)
    } else {
        None
    };
    style
}

/// Whether a font dictionary is an ordinary text font — Type0, Type1,
/// MMType1 or TrueType — as opposed to a Type3 font or an unknown subtype.
fn is_ordinary_text_font(font_dict: &lopdf::Dictionary) -> bool {
    font_dict
        .get(b"Subtype")
        .ok()
        .and_then(|obj| obj.as_name().ok())
        .is_some_and(|subtype| matches!(subtype, b"Type0" | b"Type1" | b"MMType1" | b"TrueType"))
}

/// A number from an integer or real object, following an indirect reference.
fn resolve_number(doc: &Document, obj: &Object) -> Option<f32> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).ok().and_then(get_number),
        direct => get_number(direct),
    }
}

/// A weight class value from `usWeightClass` or `/FontWeight`, clamped into
/// the 100..=900 scale; `None` for zero, negative or non-numeric values,
/// which both fields use for "unset".
fn weight_class(value: f32) -> Option<u16> {
    if !value.is_finite() || value < 1.0 {
        return None;
    }
    Some(value.round().clamp(100.0, 900.0) as u16)
}

/// Style parsed from an embedded font program stream.
fn embedded_style(doc: &Document, ff_ref: ObjectId) -> FontStyle {
    let Some(data) = font_file_data(doc, ff_ref) else {
        return FontStyle::default();
    };
    if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
        // PDF subsetters can remove OS/2 while retaining the bold bit in
        // head.macStyle. Face::is_bold only reads OS/2; use the legacy flag
        // when that table is unavailable, without overriding an explicit
        // regular OS/2 face. The parsed face has already validated head.
        let os2 = face.tables().os2;
        let mac_bold = os2.is_none()
            && face
                .raw_face()
                .table(ttf_parser::Tag::from_bytes(b"head"))
                .and_then(|head| head.get(44..46))
                .is_some_and(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]) & 1 != 0);
        let bold = face.is_bold() || mac_bold;
        FontStyle {
            italic: face.is_italic() || face.italic_angle().abs() >= 4.0,
            bold,
            bold_source: bold.then_some(BoldSource::FontFlags),
            weight: os2.and_then(|os2| weight_class(f32::from(os2.weight().to_number()))),
            // The post table's isFixedPitch, which font tools set for a
            // monospaced face and subsetters carry over.
            fixed_pitch: face.is_monospaced().then_some(true),
        }
    } else if let Some(name) = cff_font_name(&data) {
        // FontFile3 is bare CFF (no sfnt container) — ttf_parser
        // can't open it, but the CFF Name INDEX keeps the real
        // PostScript name ("XXXXXX+Amplitude-LightItalic") even
        // when the descriptor was rewritten to claim upright.
        let bold = crate::text_utils::is_bold_font(&name);
        FontStyle {
            italic: crate::text_utils::is_italic_font(&name),
            bold,
            bold_source: bold.then_some(BoldSource::FontName),
            weight: crate::text_utils::font_weight_from_name(&name),
            fixed_pitch: None,
        }
    } else {
        FontStyle::default()
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

/// Decode a PDF string and record whether legacy symbol cleanup changed a character.
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
) -> Option<(String, bool)> {
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
                    let decode_byte = |b: u8| -> Option<String> {
                        let code = b as u16;
                        // 1. Primary CMap
                        if let Some(s) = entry.primary.lookup(code) {
                            if !s.contains('\u{FFFD}') {
                                if s == (b as char).to_string() {
                                    if let Some(&ch) =
                                        encoding_map.and_then(|map| map.identity_overrides.get(&b))
                                    {
                                        return Some(ch.to_string());
                                    }
                                }
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
                            if let Some(&ch) = map.differences.get(&b) {
                                return Some(ch.to_string());
                            }
                            if let Some(text) = map.sequences.get(&b) {
                                return Some(text.clone());
                            }
                        }
                        // A code the Differences name but could not map is
                        // that glyph and no other: neither the base
                        // encoding nor the fallback below has a say.
                        if encoding_map.is_some_and(|map| map.named_codes.contains(&b)) {
                            return None;
                        }
                        // 4. The font's base encoding, for printable bytes
                        // (the predefined tables spell out the control
                        // codes too, and those are dropped like they are
                        // by the fallback below)
                        if b >= 0x20 {
                            if let Some(ch) = encoding_map
                                .and_then(|map| map.base)
                                .and_then(|base| base.char_for(b))
                            {
                                return Some(ch.to_string());
                            }
                        }
                        // 5. Printable single-byte fallback
                        if b >= 0x20 {
                            return Some(
                                decode_single_byte_fallback_char(b, use_cp1252_fallback)
                                    .to_string(),
                            );
                        }
                        None
                    };
                    let decoded: String = bytes
                        .iter()
                        .filter_map(|&b| {
                            let label = decode_byte(b)?;
                            // A glyph with no outline paints a gap, whatever
                            // its label says.
                            if blank_glyph_reads_as_space(encoding_map, b, &label) {
                                return Some(" ".to_string());
                            }
                            Some(label)
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
            // rather than using filter_map which silently drops unmapped bytes. A font
            // with a base encoding of its own — a `/BaseEncoding`, or the built-in
            // encoding of Symbol and ZapfDingbats — reads every code through it.
            if let Some(encoding) = font_encodings.get(current_font) {
                let encoding_map = &encoding.differences;
                // A string is read code by code when the Differences, the
                // blank glyphs or the base encoding have a say on any of its
                // bytes — a named code the Differences could not map among
                // them, so that it reads as nothing rather than falling to
                // the single-byte fallback below.
                let has_diff_match = encoding.base.is_some()
                    || bytes.iter().any(|b| {
                        encoding_map.contains_key(b)
                            || encoding.sequences.contains_key(b)
                            || encoding.blank_codes.contains(b)
                            || encoding.named_codes.contains(b)
                    });
                if has_diff_match {
                    let decoded: String = bytes
                        .iter()
                        .filter_map(|&b| {
                            let label: String = if let Some(&ch) = encoding_map.get(&b) {
                                ch.to_string()
                            } else if let Some(text) = encoding.sequences.get(&b) {
                                text.clone()
                            } else if encoding.named_codes.contains(&b) {
                                // A named glyph that could not be mapped:
                                // nothing else stands in for it.
                                return None;
                            } else if let Some(ch) = encoding
                                .base
                                .filter(|_| b >= 0x20)
                                .and_then(|base| base.char_for(b))
                            {
                                ch.to_string()
                            } else if b >= 0x20 {
                                // Base encoding fallback for printable bytes.
                                // Most PDFs with simple fonts use WinAnsi/PDFDocEncoding
                                // semantics, not ISO-8859-1 C1 controls.
                                decode_single_byte_fallback_char(b, use_cp1252_fallback).to_string()
                            } else {
                                return None; // Skip unmapped control characters
                            };
                            // A glyph with no outline paints a gap, whatever
                            // its label says.
                            if blank_glyph_reads_as_space(Some(encoding), b, &label) {
                                return Some(" ".to_string());
                            }
                            Some(label)
                        })
                        .collect();
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                    // Every byte was a named glyph the program could not
                    // identify, or a control code: the string reads as
                    // nothing, and the fallbacks below have no more to say.
                    if bytes
                        .iter()
                        .all(|b| encoding.named_codes.contains(b) || *b < 0x20)
                    {
                        return Some(String::new());
                    }
                }
            }

            // Fallback: try UTF-16BE then Latin-1
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let utf16: Vec<u16> = bytes[2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_be_bytes(*chunk))
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
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|chunk| u16::from_be_bytes(*chunk))
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
        let (text, legacy_symbol_rewrite) = clean_symbol_pua(text);
        let text = remap_texcm_math_symbols(text, base_font_name);
        (
            normalize_cp1252_controls(text, use_cp1252_fallback),
            legacy_symbol_rewrite,
        )
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

/// Apply the existing private-use cleanup without changing its output.
/// The boolean records an actual heuristic rewrite, not a proven Unicode alias.
fn clean_symbol_pua(text: String) -> (String, bool) {
    if !text.chars().any(|c| ('\u{F000}'..='\u{F0FF}').contains(&c)) {
        return (text, false);
    }
    let mut rewritten = false;
    let text = text
        .chars()
        .map(|c| {
            let code = c as u32;
            if !(0xF000..=0xF0FF).contains(&code) {
                return c;
            }
            let low = code - 0xF000;
            let replacement = match low {
                // Common bullets
                0xA1 | 0xA7 | 0xB7 => '\u{2022}',
                // Checkmark
                0xFC => '\u{2713}',
                // Printable ASCII range and Latin-1 above: strip F000 offset
                0x20..=0xFF => char::from_u32(low).unwrap_or(c),
                _ => c,
            };
            rewritten |= replacement != c;
            replacement
        })
        .collect();
    (text, rewritten)
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
    fn encoding_dictionary_with_a_base_encoding_and_no_differences_is_read() {
        let doc = Document::new();
        let enc = lopdf::dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding"
        };
        let result = parse_encoding_dictionary(&doc, &enc, None).expect("base encoding parsed");
        assert_eq!(result.base, Some(BaseEncoding::WinAnsi));
        assert!(result.map.is_empty());
        // Neither key: nothing to read.
        let empty = lopdf::dictionary! { "Type" => "Encoding" };
        assert!(parse_encoding_dictionary(&doc, &empty, None).is_none());
        // Differences on top of a base encoding keep both.
        let both = lopdf::dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "MacRomanEncoding",
            "Differences" => Object::Array(vec![Object::Integer(0x41), Object::Name(b"Alpha".to_vec())])
        };
        let result = parse_encoding_dictionary(&doc, &both, None).unwrap();
        assert_eq!(result.base, Some(BaseEncoding::MacRoman));
        assert_eq!(result.map.get(&0x41), Some(&'\u{0391}'));
        // The name written as an indirect object reads the same.
        let mut doc = Document::new();
        let name_id = doc.add_object(Object::Name(b"WinAnsiEncoding".to_vec()));
        let indirect = lopdf::dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => Object::Reference(name_id)
        };
        let result =
            parse_encoding_dictionary(&doc, &indirect, None).expect("base encoding parsed");
        assert_eq!(result.base, Some(BaseEncoding::WinAnsi));
    }

    #[test]
    fn base_encoding_leaves_control_bytes_out() {
        // A font reading through a base encoding drops the control bytes
        // its text strings carry, as the printable fallback does; the
        // predefined tables spell those codes out, so the guard is needed.
        let bytes = vec![0x41_u8, 0x0D, 0x09, 0x42];
        let obj = Object::String(bytes, lopdf::StringFormat::Literal);
        let font_cmaps = FontCMaps::default();
        let font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        let inline_cmaps = HashMap::new();
        let mut font_encodings: PageFontEncodings = HashMap::new();
        font_encodings.insert(
            "F0".to_string(),
            FontEncoding {
                differences: FontEncodingMap::new(),
                identity_overrides: FontEncodingMap::new(),
                blank_codes: Default::default(),
                base: Some(BaseEncoding::WinAnsi),
                named_codes: Default::default(),
                sequences: Default::default(),
            },
        );
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F0".to_string(), make_font_info(&[], 1000, false));
        let (text, _) = extract_text_from_operand(
            &obj,
            "F0",
            Some("Helvetica"),
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("text decoded");
        assert_eq!(text, "AB");
    }

    #[test]
    fn numbered_names_resolve_through_fontfile3_when_fontfile2_is_not_a_reference() {
        // The glyph-names fixture's second font names glyphs by index and
        // embeds its program as FontFile2. A descriptor whose FontFile2 is
        // not a reference must not stop the lookup: the program under
        // FontFile3 still resolves the names.
        let doc = Document::load("tests/fixtures/glyph_names_in_embedded_fonts.pdf").unwrap();
        let mut doc = doc;
        let page_id = *doc.get_pages().get(&1).unwrap();
        let fonts = doc.get_page_fonts(page_id).unwrap();
        let font = (*fonts.get(&b"F2".to_vec()).expect("F2")).clone();
        let names = vec![
            (0x41_u8, "g1".to_string()),
            (0x42, "g2".to_string()),
            (0x43, "glyph3".to_string()),
        ];
        let expected: FontEncodingMap =
            [(0x41, '\u{03B4}'), (0x42, '\u{03B5}'), (0x43, '\u{03B6}')]
                .into_iter()
                .collect();
        assert_eq!(
            glyph_index_chars(&doc, &font, &names, &mut FontStyleCache::new()),
            expected
        );
        // Move the program to FontFile3 and leave a non-reference FontFile2.
        let descriptor_ref = font.get(b"FontDescriptor").unwrap().as_reference().unwrap();
        let mut descriptor = doc.get_dictionary(descriptor_ref).unwrap().clone();
        let program = descriptor.get(b"FontFile2").unwrap().clone();
        descriptor.set("FontFile3", program);
        descriptor.set("FontFile2", Object::Integer(0));
        let new_descriptor = doc.add_object(descriptor);
        let mut moved = font.clone();
        moved.set("FontDescriptor", Object::Reference(new_descriptor));
        assert_eq!(
            glyph_index_chars(&doc, &moved, &names, &mut FontStyleCache::new()),
            expected
        );
    }

    #[test]
    fn a_string_of_named_but_unmapped_codes_reads_as_nothing_without_a_base() {
        // A subset whose Differences name every code it uses (`gid00016`…)
        // without a program that resolves them, no base encoding and no
        // ToUnicode: the string holds nothing the decoder can read, and it
        // must not fall to the single-byte fallback and print Latin-1
        // characters for the codes.
        let bytes = vec![0x81_u8, 0x82, 0x9B];
        let obj = Object::String(bytes, lopdf::StringFormat::Literal);
        let font_cmaps = FontCMaps::default();
        let font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        let inline_cmaps = HashMap::new();
        let mut font_encodings: PageFontEncodings = HashMap::new();
        font_encodings.insert(
            "F0".to_string(),
            FontEncoding {
                differences: FontEncodingMap::new(),
                identity_overrides: FontEncodingMap::new(),
                blank_codes: Default::default(),
                base: None,
                named_codes: [0x81, 0x82, 0x9B].into_iter().collect(),
                sequences: Default::default(),
            },
        );
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F0".to_string(), make_font_info(&[], 1000, false));
        let decoded = extract_text_from_operand(
            &obj,
            "F0",
            Some("SyntheticSubset"),
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        );
        let text = decoded.map(|(text, _)| text).unwrap_or_default();
        assert!(
            !text.contains('\u{201A}') && !text.contains('\u{203A}') && !text.contains('\u{0081}'),
            "{text:?}"
        );
        assert!(text.trim().is_empty(), "{text:?}");
    }

    #[test]
    fn component_ligature_names_read_as_their_letters() {
        // /Differences naming glyphs by their components — `f_t`, `f_f_i`,
        // with a suffix, as a uni sequence — read as the letters they join,
        // in the Differences map's sequences and in the decoded text.
        let doc = Document::new();
        let enc = lopdf::dictionary! {
            "Type" => "Encoding",
            "Differences" => Object::Array(vec![
                Object::Integer(0x41),
                Object::Name(b"f_t".to_vec()),
                Object::Name(b"f_f_i".to_vec()),
                Object::Name(b"f_i.liga".to_vec()),
                Object::Name(b"uni00660069".to_vec()),
                Object::Name(b"a.sc".to_vec()),
                Object::Name(b"f_zzz".to_vec()),
            ])
        };
        let result = parse_encoding_dictionary(&doc, &enc, None).expect("parsed");
        assert_eq!(result.sequences.get(&0x41).map(String::as_str), Some("ft"));
        assert_eq!(result.sequences.get(&0x42).map(String::as_str), Some("ffi"));
        assert_eq!(result.sequences.get(&0x43).map(String::as_str), Some("fi"));
        assert_eq!(result.sequences.get(&0x44).map(String::as_str), Some("fi"));
        assert_eq!(result.map.get(&0x45), Some(&'a'));
        assert!(!result.map.contains_key(&0x46) && !result.sequences.contains_key(&0x46));
        assert!(result.named_codes.contains(&0x46));

        let obj = Object::String(
            vec![0x41, 0x20, 0x42, 0x45, 0x46],
            lopdf::StringFormat::Literal,
        );
        let font_cmaps = FontCMaps::default();
        let font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        let inline_cmaps = HashMap::new();
        let mut font_encodings: PageFontEncodings = HashMap::new();
        font_encodings.insert(
            "F0".to_string(),
            FontEncoding {
                differences: result.map.clone(),
                identity_overrides: FontEncodingMap::new(),
                blank_codes: Default::default(),
                base: Some(BaseEncoding::WinAnsi),
                named_codes: result.named_codes.clone(),
                sequences: result.sequences.clone(),
            },
        );
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F0".to_string(), make_font_info(&[], 1000, false));
        let (text, _) = extract_text_from_operand(
            &obj,
            "F0",
            Some("Helvetica"),
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("text decoded");
        // "ft", a space, "ffi", "a"; the unreadable `f_zzz` code reads as nothing.
        assert_eq!(text, "ft ffia");
    }

    #[test]
    fn a_named_but_unmapped_code_reads_as_nothing() {
        // A subset names code 0x81 `gid00136` over a WinAnsi base: the
        // name could not be mapped, and neither WinAnsi's bullet at 0x81
        // nor the cp1252 fallback is that glyph — the code reads as
        // nothing.
        let bytes = vec![0x41_u8, 0x81, 0x42];
        let obj = Object::String(bytes, lopdf::StringFormat::Literal);
        let font_cmaps = FontCMaps::default();
        let font_tounicode_refs: HashMap<String, u32> = HashMap::new();
        let inline_cmaps = HashMap::new();
        let mut font_encodings: PageFontEncodings = HashMap::new();
        font_encodings.insert(
            "F0".to_string(),
            FontEncoding {
                differences: FontEncodingMap::new(),
                identity_overrides: FontEncodingMap::new(),
                blank_codes: Default::default(),
                base: Some(BaseEncoding::WinAnsi),
                named_codes: [0x81].into_iter().collect(),
                sequences: Default::default(),
            },
        );
        let encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
        let mut decisions = CMapDecisionCache::new();
        let mut font_widths: PageFontWidths = HashMap::new();
        font_widths.insert("F0".to_string(), make_font_info(&[], 1000, false));
        let (text, _) = extract_text_from_operand(
            &obj,
            "F0",
            Some("Helvetica"),
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("text decoded");
        assert_eq!(text, "AB");
        // The same string with the code unnamed reads the bullet WinAnsi
        // shows at an unused code.
        font_encodings.get_mut("F0").unwrap().named_codes.clear();
        let (text, _) = extract_text_from_operand(
            &obj,
            "F0",
            Some("Helvetica"),
            &font_cmaps,
            &font_tounicode_refs,
            &inline_cmaps,
            &font_encodings,
            &encoding_cache,
            &mut decisions,
            &font_widths,
        )
        .expect("text decoded");
        assert_eq!(text, "A\u{2022}B");
    }

    #[test]
    fn builtin_symbol_encoding_yields_to_a_named_encoding_however_it_is_written() {
        let mut doc = Document::new();
        let symbol = lopdf::dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Symbol"
        };
        assert_eq!(
            builtin_base_encoding(&doc, &symbol),
            Some(BaseEncoding::Symbol)
        );
        // A named encoding replaces the built-in one, as a name or as a
        // reference to one.
        let mut direct = symbol.clone();
        direct.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        assert_eq!(builtin_base_encoding(&doc, &direct), None);
        let name_id = doc.add_object(Object::Name(b"WinAnsiEncoding".to_vec()));
        let mut indirect = symbol.clone();
        indirect.set("Encoding", Object::Reference(name_id));
        assert_eq!(builtin_base_encoding(&doc, &indirect), None);
        // The font's own built-in encoding, named outright.
        let mut own = symbol.clone();
        own.set("Encoding", Object::Name(b"SymbolEncoding".to_vec()));
        assert_eq!(
            builtin_base_encoding(&doc, &own),
            Some(BaseEncoding::Symbol)
        );
        let mut other = symbol.clone();
        other.set("Encoding", Object::Name(b"ZapfDingbatsEncoding".to_vec()));
        assert_eq!(builtin_base_encoding(&doc, &other), None);
        let helvetica = lopdf::dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica"
        };
        assert_eq!(builtin_base_encoding(&doc, &helvetica), None);
        // The width fallback follows the same choice: code 0x61 is alpha's
        // advance through the built-in encoding, and has no Symbol glyph
        // (so no width) under a named Latin encoding.
        let alpha = crate::extractor::base14::base14_char_width("Symbol", '\u{03B1}');
        assert!(alpha.is_some());
        let widths = base14_fallback_widths(&doc, &symbol).expect("Symbol widths");
        assert_eq!(widths.widths.get(&0x61).copied(), alpha);
        let widths = base14_fallback_widths(&doc, &indirect).expect("Symbol widths");
        assert_eq!(widths.widths.get(&0x61), None);
        // A control byte gets a width only through the Differences, the
        // one way the decoder reads it.
        let mut remapped = symbol.clone();
        remapped.set(
            "Encoding",
            Object::Dictionary(lopdf::dictionary! {
                "Type" => "Encoding",
                "Differences" => Object::Array(vec![Object::Integer(0x01), Object::Name(b"alpha".to_vec())])
            }),
        );
        let widths = base14_fallback_widths(&doc, &remapped).expect("Symbol widths");
        assert_eq!(widths.widths.get(&0x01).copied(), alpha);
        assert_eq!(widths.widths.get(&0x09), None);
        let widths = base14_fallback_widths(&doc, &symbol).expect("Symbol widths");
        assert_eq!(widths.widths.get(&0x01), None);
    }

    #[test]
    fn predefined_base_encodings_place_accented_letters() {
        assert_eq!(BaseEncoding::WinAnsi.char_for(0xF1), Some('\u{00F1}'));
        assert_eq!(BaseEncoding::WinAnsi.char_for(0x41), Some('A'));
        assert_eq!(BaseEncoding::MacRoman.char_for(0x8E), Some('\u{00E9}'));
        assert_eq!(BaseEncoding::Standard.char_for(0xE1), Some('\u{00C6}'));
        // StandardEncoding has no glyph at 0x80. WinAnsi shows its unused
        // codes above 0x40 (0x81 among them) as bullets, the glyph its
        // code 0x95 names outright.
        assert_eq!(BaseEncoding::Standard.char_for(0x80), None);
        assert_eq!(BaseEncoding::WinAnsi.char_for(0x81), Some('\u{2022}'));
        assert_eq!(BaseEncoding::WinAnsi.char_for(0x95), Some('\u{2022}'));
        assert_eq!(BaseEncoding::Symbol.char_for(0x61), Some('\u{03B1}'));
        assert_eq!(BaseEncoding::ZapfDingbats.char_for(0x33), Some('\u{2713}'));
        assert_eq!(
            BaseEncoding::from_name(b"MacExpertEncoding"),
            Some(BaseEncoding::MacExpert)
        );
        assert_eq!(BaseEncoding::from_name(b"Identity-H"), None);
    }

    #[test]
    fn numbered_glyph_names_are_recognized() {
        use NumberedGlyph::{Cid, Index};
        assert_eq!(numbered_glyph_name("g12"), Some(Index(12)));
        assert_eq!(numbered_glyph_name("gid00053"), Some(Index(53)));
        assert_eq!(numbered_glyph_name("glyph3"), Some(Index(3)));
        assert_eq!(numbered_glyph_name("index7"), Some(Index(7)));
        assert_eq!(numbered_glyph_name("G5"), Some(Index(5)));
        assert_eq!(numbered_glyph_name("cid00012"), Some(Cid(12)));
        assert_eq!(numbered_glyph_name("gamma"), None);
        assert_eq!(numbered_glyph_name("g"), None);
        assert_eq!(numbered_glyph_name("g12a"), None);
    }

    #[test]
    fn item_font_name_prefers_family_over_resource_tag() {
        use super::item_font_name;
        assert_eq!(item_font_name("F2", "ABCDEF+CMMI10"), "ABCDEF+CMMI10");
        assert_eq!(item_font_name("T22", "Times-Roman"), "Times-Roman");
        // Distiller CID-convention resources keep the resource name:
        // is_cid_font keys on the C2_/C0_ prefix for micro-gap joining.
        assert_eq!(item_font_name("C2_0", "ABCDEE+SimSun"), "C2_0");
        assert_eq!(item_font_name("C0_1", "ABCDEE+MSMincho"), "C0_1");
    }

    #[test]
    fn type3_scale_resolves_indirect_matrix_and_bbox_numbers() {
        use lopdf::{dictionary, Document, Object};
        // FontMatrix/FontBBox elements may be indirect references per PDF
        // syntax; the scale must use their resolved values, not zero.
        let mut doc = Document::with_version("1.4");
        let matrix_d = doc.add_object(Object::Real(-1.0));
        let bbox_top = doc.add_object(Object::Integer(3));
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "FontMatrix" => vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Reference(matrix_d),
                Object::Integer(0),
                Object::Integer(0),
            ],
            "FontBBox" => vec![
                Object::Integer(1),
                Object::Integer(-156),
                Object::Integer(37),
                Object::Reference(bbox_top),
            ],
        };
        let mut fonts = std::collections::BTreeMap::new();
        fonts.insert(b"T2".to_vec(), &font_dict);
        let scales = super::build_type3_scales(&doc, &fonts);
        let scale = scales.get("T2").copied().unwrap_or(1.0);
        // bbox height 159 x |matrix_y| 1.0
        assert!(
            (scale - 159.0).abs() < 0.5,
            "scale should use resolved indirect values, got {scale}"
        );
    }

    /// Build a one-font Type3 document and return its computed scale, if any.
    #[cfg(test)]
    fn type3_scale_for(matrix_y: f32, bbox_lo: i64, bbox_hi: i64) -> Option<f32> {
        use lopdf::{dictionary, Document, Object};
        let doc = Document::with_version("1.4");
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "FontMatrix" => vec![
                Object::Real(matrix_y), Object::Integer(0), Object::Integer(0),
                Object::Real(matrix_y), Object::Integer(0), Object::Integer(0),
            ],
            "FontBBox" => vec![
                Object::Integer(0), Object::Integer(bbox_lo),
                Object::Integer(600), Object::Integer(bbox_hi),
            ],
        };
        let mut fonts = std::collections::BTreeMap::new();
        fonts.insert(b"T9".to_vec(), &font_dict);
        super::build_type3_scales(&doc, &fonts).get("T9").copied()
    }

    #[test]
    fn type3_scale_skips_self_consistent_fonts() {
        // Conventional 1/1000 matrix with a descender..ascender bbox of 700
        // units: scale 0.7. The Tf operand is already the rendered size, so
        // renormalizing would report every size at 0.7x.
        assert_eq!(type3_scale_for(0.001, -200, 500), None);
        // Tall-accent bbox slightly over the em (1100 units, scale 1.1).
        assert_eq!(type3_scale_for(0.001, -100, 1000), None);
    }

    #[test]
    fn type3_scale_applies_to_inconsistent_fonts_at_any_matrix_scale() {
        // Non-standard but valid matrix (0.005) with a full-em bbox:
        // scale 5.0, so the declared size is off by 5x and must be fixed.
        let s = type3_scale_for(0.005, 0, 1000).expect("0.005 matrix should rescale");
        assert!((s - 5.0).abs() < 0.01, "got {s}");
        // dvips/PK bitmap pattern: unit matrix, glyphs spanning ~159 units.
        let s = type3_scale_for(1.0, -156, 3).expect("PK pattern should rescale");
        assert!((s - 159.0).abs() < 0.5, "got {s}");
    }

    #[test]
    fn type3_scale_ignores_degenerate_bbox() {
        // [0 0 0 0] is legal and carries no size information.
        assert_eq!(type3_scale_for(0.001, 0, 0), None);
    }

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
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
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
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
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
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
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
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
            (false, false)
        );
    }

    #[test]
    fn missing_descriptor_yields_no_flags() {
        let doc = Document::with_version("1.4");
        let font_dict = dictionary! { "Type" => "Font", "BaseFont" => "Tc1" };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
            (false, false)
        );
    }

    /// Synthetic sfnt containing just the tables needed to parse a face.
    /// No source-document font bytes are needed for these style tests.
    fn sfnt_with_style(mac_style: u16, os2_selection: Option<u16>) -> Vec<u8> {
        sfnt_with_style_and_weight(mac_style, os2_selection.map(|selection| (selection, 0)))
    }

    /// [`sfnt_with_style`] whose OS/2 table also carries a `usWeightClass`.
    fn sfnt_with_style_and_weight(mac_style: u16, os2: Option<(u16, u16)>) -> Vec<u8> {
        sfnt_with_tables(mac_style, os2, None)
    }

    /// [`sfnt_with_style_and_weight`] with a `post` table whose
    /// `isFixedPitch` is `fixed_pitch`, when given.
    fn sfnt_with_tables(
        mac_style: u16,
        os2: Option<(u16, u16)>,
        fixed_pitch: Option<bool>,
    ) -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
        head[44..46].copy_from_slice(&mac_style.to_be_bytes());
        let hhea = vec![0u8; 36];
        let mut maxp = vec![0u8; 6];
        maxp[..4].copy_from_slice(&0x00005000u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&1u16.to_be_bytes()); // numGlyphs
        let mut tables = vec![(*b"head", head), (*b"hhea", hhea), (*b"maxp", maxp)];
        if let Some((selection, weight_class)) = os2 {
            let mut os2 = vec![0u8; 78]; // version 0
            os2[4..6].copy_from_slice(&weight_class.to_be_bytes()); // usWeightClass
            os2[62..64].copy_from_slice(&selection.to_be_bytes()); // fsSelection
            tables.push((*b"OS/2", os2));
        }
        if let Some(fixed_pitch) = fixed_pitch {
            let mut post = vec![0u8; 32]; // version 3.0: header only
            post[..4].copy_from_slice(&0x00030000u32.to_be_bytes());
            post[12..16].copy_from_slice(&u32::from(fixed_pitch).to_be_bytes()); // isFixedPitch
            tables.push((*b"post", post));
        }
        tables.sort_by_key(|(tag, _)| *tag);
        let count = tables.len();
        let mut data = vec![0u8; 12 + count * 16];
        data[..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        data[4..6].copy_from_slice(&(count as u16).to_be_bytes());
        for (i, (tag, table)) in tables.into_iter().enumerate() {
            let offset = data.len() as u32;
            let record = 12 + i * 16;
            data[record..record + 4].copy_from_slice(&tag);
            data[record + 8..record + 12].copy_from_slice(&offset.to_be_bytes());
            data[record + 12..record + 16].copy_from_slice(&(table.len() as u32).to_be_bytes());
            data.extend_from_slice(&table);
            while !data.len().is_multiple_of(4) {
                data.push(0);
            }
        }
        data
    }

    fn descriptor_flags_from_sfnt(mac_style: u16, os2_selection: Option<u16>) -> (bool, bool) {
        let mut doc = Document::with_version("1.4");
        let font_file = doc.add_object(lopdf::Stream::new(
            dictionary! {},
            sfnt_with_style(mac_style, os2_selection),
        ));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+OpaqueFace",
            "Flags" => 4,
            "ItalicAngle" => 0,
            "FontFile2" => font_file,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "ABCDEF+OpaqueFace",
            "FontDescriptor" => descriptor,
        };
        font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags()
    }

    #[test]
    fn embedded_mac_bold_without_os2_survives_opaque_subset_names() {
        assert_eq!(descriptor_flags_from_sfnt(1, None), (false, true));
        assert_eq!(descriptor_flags_from_sfnt(0, None), (false, false));
        // Italic and other macStyle bits must not imply bold.
        assert_eq!(descriptor_flags_from_sfnt(2, None), (false, false));
    }

    #[test]
    fn embedded_os2_bold_remains_authoritative() {
        assert_eq!(descriptor_flags_from_sfnt(0, Some(1 << 5)), (false, true));
        assert_eq!(descriptor_flags_from_sfnt(1, Some(1 << 6)), (false, false));
    }

    /// A TrueType font dictionary embedding `font_file`, with the given
    /// BaseFont and descriptor entries on top of the plain flags.
    fn embedded_font(
        base_font: &str,
        font_file: Vec<u8>,
        descriptor_extra: lopdf::Dictionary,
    ) -> (Document, lopdf::Dictionary) {
        let mut doc = Document::with_version("1.4");
        let font_file = doc.add_object(lopdf::Stream::new(dictionary! {}, font_file));
        let mut descriptor = dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => base_font,
            "Flags" => 4,
            "ItalicAngle" => 0,
            "FontFile2" => font_file,
        };
        for (key, value) in descriptor_extra.into_iter() {
            descriptor.set(key.clone(), value.clone());
        }
        let descriptor = doc.add_object(descriptor);
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => base_font,
            "FontDescriptor" => descriptor,
        };
        (doc, font_dict)
    }

    #[test]
    fn weight_class_comes_from_the_embedded_os2_table_first() {
        // usWeightClass 700 on a face whose fsSelection bold bit is unset,
        // whose descriptor claims /FontWeight 400 and whose name says
        // nothing: the embedded table wins, and it does not make the font
        // bold on its own.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+OpaqueFace",
            sfnt_with_style_and_weight(0, Some((0, 700))),
            dictionary! { "FontWeight" => 400 },
        );
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()),
            FontStyle {
                italic: false,
                bold: false,
                bold_source: None,
                weight: Some(700),
                fixed_pitch: None,
            }
        );
    }

    #[test]
    fn weight_class_falls_back_to_the_descriptor_then_the_name() {
        // No OS/2 table: the descriptor's /FontWeight decides ...
        let (doc, font_dict) = embedded_font(
            "ABCDEF+Face-Bold",
            sfnt_with_style_and_weight(0, None),
            dictionary! { "FontWeight" => 300 },
        );
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
            Some(300)
        );
        // ... and without one the weight word of the BaseFont name does.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+Face-Bold",
            sfnt_with_style_and_weight(0, None),
            dictionary! {},
        );
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
            Some(700)
        );
        // An OS/2 table that leaves usWeightClass at 0 says nothing.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+Face-Md",
            sfnt_with_style_and_weight(0, Some((0, 0))),
            dictionary! {},
        );
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
            Some(500)
        );
    }

    #[test]
    fn descriptor_font_weight_is_clamped_to_the_scale() {
        for (value, expected) in [
            (lopdf::Object::Integer(700), Some(700)),
            (lopdf::Object::Real(500.0), Some(500)),
            (lopdf::Object::Integer(1000), Some(900)),
            (lopdf::Object::Integer(0), None),
            (lopdf::Object::Integer(-1), None),
            (lopdf::Object::Name(b"Bold".to_vec()), None),
        ] {
            let (doc, font_dict) = doc_with_descriptor(dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => "Tc1",
                "Flags" => 32,
                "FontWeight" => value.clone(),
            });
            assert_eq!(
                font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
                expected,
                "{value:?}"
            );
        }
    }

    #[test]
    fn indirect_font_weight_is_resolved() {
        let mut doc = Document::with_version("1.4");
        let weight_id = doc.add_object(lopdf::Object::Integer(600));
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "Flags" => 32,
            "ItalicAngle" => 0,
            "FontWeight" => weight_id,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Tc1",
            "FontDescriptor" => desc_id,
        };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
            Some(600)
        );
    }

    #[test]
    fn type3_fonts_carry_no_weight_class() {
        // A Type3 font's glyph procedures draw whatever they like: neither a
        // weight word in its name nor a /FontWeight in its descriptor says
        // how heavy that ink is, while its style flags stay as they were.
        let mut doc = Document::with_version("1.4");
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Glyphs-Bold",
            "Flags" => 4 | (1 << 18),
            "ItalicAngle" => 0,
            "FontWeight" => 700,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "BaseFont" => "Glyphs-Bold",
            "FontDescriptor" => desc_id,
        };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()),
            FontStyle {
                italic: false,
                bold: true,
                bold_source: Some(BoldSource::FontFlags),
                weight: None,
                fixed_pitch: None,
            }
        );
        // The same holds for a font dictionary without a subtype at all.
        let font_dict = dictionary! { "Type" => "Font", "BaseFont" => "Anything-Bold" };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).weight,
            None
        );
    }

    #[test]
    fn name_weight_needs_no_descriptor() {
        let doc = Document::with_version("1.4");
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica-Light",
        };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()),
            FontStyle {
                italic: false,
                bold: false,
                bold_source: None,
                weight: Some(300),
                fixed_pitch: None,
            }
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
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
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
    fn bare_cff_name_weight_outranks_the_descriptor() {
        // A Type1C program's own PostScript name carries the style
        // abbreviation; the descriptor's /FontWeight and the opaque
        // BaseFont say nothing useful.
        let mut doc = Document::with_version("1.4");
        let ff_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            dictionary! {},
            bare_cff_with_name("ABCDEF+Face-Md"),
        )));
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+Face-Md",
            "ItalicAngle" => 0,
            "Flags" => 32,
            "FontWeight" => 400,
            "FontFile3" => ff_id,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Tc1",
            "FontDescriptor" => desc_id,
        };
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()),
            FontStyle {
                italic: false,
                bold: false,
                bold_source: None,
                weight: Some(500),
                fixed_pitch: None,
            }
        );
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
            font_style(&doc, &font_dict, &mut cache).flags(),
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
            font_style(&doc, &font_dict, &mut cache).flags(),
            (true, true)
        );
        // A cold cache parses the (now garbage) stream, proving the warm
        // call above answered from the memo.
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).flags(),
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

        let (text, _) = result.expect("CID font fallback should still emit a marker");
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

        let (text, _) = extract_text_from_operand(
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
    fn legacy_cleanup_evidence_tracks_changes_without_changing_aliases() {
        for (source, expected, rewritten) in [
            ("AΩμ$•✓", "AΩμ$•✓", false),
            ("\u{f057}\u{f0b7}\u{f0fc}", "W•✓", true),
            ("\u{f010}\u{e123}", "\u{f010}\u{e123}", false),
            ("Price \u{f024}", "Price $", true),
        ] {
            assert_eq!(
                clean_symbol_pua(source.to_string()),
                (expected.to_string(), rewritten)
            );
        }
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

        let (text, _) = extract_text_from_operand(
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
        let (_, has_gid_fonts) =
            build_font_encodings(&doc, &fonts, &cmaps, &mut FontStyleCache::new());
        has_gid_fonts
    }

    #[test]
    fn type3_procedure_names_never_flag_the_page() {
        // A Type3 font names its glyph procedures in /Differences — `g2`,
        // `g10` — and has no glyph table those numbers could index; the
        // same names on a font with a program and no ToUnicode do flag.
        let mut doc = Document::with_version("1.4");
        let differences = || {
            Object::Array(vec![
                2.into(),
                Object::Name(b"g2".to_vec()),
                Object::Name(b"g10".to_vec()),
            ])
        };
        let type3 = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
            "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
            "CharProcs" => dictionary! {},
            "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => differences() },
            "FirstChar" => 2,
            "LastChar" => 3,
            "Widths" => vec![500.into(), 500.into()],
        });
        let truetype = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "SyntheticSubset",
            "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => differences() },
        });
        let cmaps = FontCMaps::from_doc(&doc);
        for (font_id, expected) in [(type3, false), (truetype, true)] {
            let font_dict = doc.get_dictionary(font_id).unwrap().clone();
            let fonts = std::collections::BTreeMap::from([(b"F1".to_vec(), &font_dict)]);
            let (_, has_gid_fonts) =
                build_font_encodings(&doc, &fonts, &cmaps, &mut FontStyleCache::new());
            assert_eq!(has_gid_fonts, expected);
        }
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

    #[test]
    fn parse_cid_w_array_range_and_consecutive() {
        use super::parse_cid_w_array;
        use lopdf::{Document, Object};
        use std::collections::HashMap;

        let doc = Document::new();
        let mut widths = HashMap::new();
        let w = vec![
            Object::Integer(10),
            Object::Integer(12),
            Object::Integer(500),
            Object::Integer(20),
            Object::Array(vec![Object::Integer(100), Object::Integer(200)]),
        ];
        parse_cid_w_array(&doc, &w, &mut widths);
        assert_eq!(widths.get(&10), Some(&500));
        assert_eq!(widths.get(&11), Some(&500));
        assert_eq!(widths.get(&12), Some(&500));
        assert_eq!(widths.get(&20), Some(&100));
        assert_eq!(widths.get(&21), Some(&200));
    }

    #[test]
    fn parse_cid_w_array_repeated_full_ranges_stay_bounded() {
        use super::parse_cid_w_array;
        use crate::tounicode::MAX_CID_W_EXPANSION;
        use lopdf::{Document, Object};
        use std::collections::HashMap;

        let doc = Document::new();
        let mut widths = HashMap::new();
        let mut w = Vec::new();
        for _ in 0..5_000 {
            w.push(Object::Integer(0));
            w.push(Object::Integer(65535));
            w.push(Object::Integer(500));
        }
        parse_cid_w_array(&doc, &w, &mut widths);
        assert!(widths.len() <= MAX_CID_W_EXPANSION);
        assert_eq!(widths.get(&0), Some(&500));
        assert_eq!(widths.get(&65535), Some(&500));
    }
    #[test]
    fn descriptor_fixed_pitch_flag_is_read_only_as_a_yes() {
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => 1 | 32, // FixedPitch, Nonsymbolic
        });
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).fixed_pitch,
            Some(true)
        );
        // An unset bit says nothing: producers write /Flags 4 for any face.
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => 4,
        });
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).fixed_pitch,
            None
        );
    }

    #[test]
    fn indirect_flags_are_resolved_before_their_bits_are_read() {
        let mut doc = Document::with_version("1.4");
        let flags_id = doc.add_object(Object::Integer(1 | (1 << 18))); // FixedPitch, ForceBold
        let desc_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => flags_id,
        });
        let font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Tc1",
            "FontDescriptor" => desc_id,
        };
        let style = font_style(&doc, &font_dict, &mut FontStyleCache::new());
        assert!(style.bold);
        assert_eq!(style.bold_source, Some(BoldSource::FontFlags));
        assert_eq!(style.fixed_pitch, Some(true));
    }

    #[test]
    fn embedded_post_table_declares_fixed_pitch() {
        let (doc, font_dict) = embedded_font(
            "ABCDEF+OpaqueFace",
            sfnt_with_tables(0, Some((1 << 6, 400)), Some(true)),
            dictionary! {},
        );
        let style = font_style(&doc, &font_dict, &mut FontStyleCache::new());
        assert_eq!(style.fixed_pitch, Some(true));
        assert!(!style.bold && style.bold_source.is_none());
        // A program that says it is proportional leaves the question to the
        // width table.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+OpaqueFace",
            sfnt_with_tables(0, Some((1 << 6, 400)), Some(false)),
            dictionary! {},
        );
        assert_eq!(
            font_style(&doc, &font_dict, &mut FontStyleCache::new()).fixed_pitch,
            None
        );
    }

    #[test]
    fn bold_source_names_the_descriptor_flag_or_the_program() {
        // ForceBold in the descriptor.
        let (doc, font_dict) = doc_with_descriptor(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Tc1",
            "ItalicAngle" => 0,
            "Flags" => 1 << 18,
        });
        let style = font_style(&doc, &font_dict, &mut FontStyleCache::new());
        assert!(style.bold);
        assert_eq!(style.bold_source, Some(BoldSource::FontFlags));
        // The embedded program's bold selection.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+OpaqueFace",
            sfnt_with_style_and_weight(0, Some((1 << 5, 700))),
            dictionary! {},
        );
        let style = font_style(&doc, &font_dict, &mut FontStyleCache::new());
        assert!(style.bold);
        assert_eq!(style.bold_source, Some(BoldSource::FontFlags));
        assert_eq!(style.weight, Some(700));
        // A heavy weight class alone is not bold and names no source.
        let (doc, font_dict) = embedded_font(
            "ABCDEF+OpaqueFace",
            sfnt_with_style_and_weight(0, Some((1 << 6, 700))),
            dictionary! {},
        );
        let style = font_style(&doc, &font_dict, &mut FontStyleCache::new());
        assert!(!style.bold);
        assert_eq!(style.bold_source, None);
    }

    #[test]
    fn name_style_outranks_the_flags_as_the_bold_source() {
        let flagged = FontStyle {
            italic: false,
            bold: true,
            bold_source: Some(BoldSource::FontFlags),
            weight: Some(700),
            fixed_pitch: None,
        };
        // A bold word in the name is what a reader sees first.
        assert_eq!(
            flagged.with_name("Foo-Bold").bold_source,
            Some(BoldSource::FontName)
        );
        // A plain name leaves the flags' verdict alone.
        assert_eq!(flagged.with_name("Tc1"), flagged);
        // A name alone makes a plain style bold, or italic; the resource
        // tag of a font without a name says nothing.
        let plain = FontStyle::default();
        let named = plain.with_name("ABCDEF+Face-Demi");
        assert!(named.bold);
        assert_eq!(named.bold_source, Some(BoldSource::FontName));
        assert!(plain.with_name("Foo-Italic").italic);
        assert_eq!(plain.with_name("F1"), plain);
    }

    #[test]
    fn measured_pitch_fills_only_an_unknown() {
        let uniform: Vec<(u16, u16)> = (65u16..77).map(|code| (code, 600)).collect();
        let info = make_font_info(&uniform, 0, false);
        assert_eq!(
            FontStyle::default()
                .with_measured_pitch(Some(&info))
                .fixed_pitch,
            Some(true)
        );
        assert_eq!(
            FontStyle::default().with_measured_pitch(None).fixed_pitch,
            None
        );
        // A declared verdict is not second-guessed by the widths.
        let declared = FontStyle {
            fixed_pitch: Some(true),
            ..FontStyle::default()
        };
        let varied = make_font_info(&[(65, 600), (66, 300)], 0, false);
        assert_eq!(
            declared.with_measured_pitch(Some(&varied)).fixed_pitch,
            Some(true)
        );
    }

    #[test]
    fn fixed_pitch_by_advance_needs_a_dozen_glyphs_for_a_yes_and_two_for_a_no() {
        // Ten tabular digits of a proportional face share an advance and
        // are not enough to call it fixed-pitch.
        let digits: Vec<(u16, u16)> = (48u16..58).map(|code| (code, 556)).collect();
        assert_eq!(
            make_font_info(&digits, 0, false).fixed_pitch_by_advance(),
            None
        );
        let mut dozen = digits.clone();
        dozen.extend([(43, 556), (45, 556)]);
        assert_eq!(
            make_font_info(&dozen, 0, false).fixed_pitch_by_advance(),
            Some(true)
        );
        // A unit of rounding is one advance; a real difference is two.
        let mut rounded = dozen.clone();
        rounded[0].1 = 555;
        assert_eq!(
            make_font_info(&rounded, 0, false).fixed_pitch_by_advance(),
            Some(true)
        );
        assert_eq!(
            make_font_info(&[(65, 600), (66, 500)], 0, false).fixed_pitch_by_advance(),
            Some(false)
        );
        // Zero widths are codes without a glyph and do not count.
        let mut with_gaps = dozen.clone();
        with_gaps.extend([(1, 0), (2, 0)]);
        assert_eq!(
            make_font_info(&with_gaps, 0, false).fixed_pitch_by_advance(),
            Some(true)
        );
        // A CID font's default width covers unlisted glyphs and is not read.
        assert_eq!(
            make_font_info(&[], 1000, true).fixed_pitch_by_advance(),
            None
        );
    }
}

#[cfg(test)]
#[path = "stale_cmap_tests.rs"]
mod stale_cmap_tests;

#[cfg(test)]
#[path = "blank_glyph_tests.rs"]
pub(crate) mod blank_glyph_tests;
