//! Form XObject and image XObject extraction.

use super::fonts::{font_style, FontStyle};
use super::text_paint::{PaintResources, TextPaint};
use crate::text_utils::{effective_font_size, expand_ligatures};
use crate::tounicode::FontCMaps;
use crate::types::{attach_run_coverage, BoldSource, ItemCoverage, ItemType, TextItem};
use lopdf::{Document, Encoding, Object, ObjectId};
use std::collections::HashMap;

use super::content_stream::{estimated_string_advance_ts, PendingSpace};
use super::fonts::{
    build_font_encodings, build_font_kinds, build_font_widths, build_type3_scales,
    build_type3_y_flips, compute_string_width_ts, extract_text_from_operand,
    get_font_file2_obj_num, get_operand_bytes, CMapDecisionCache, FontStyleCache,
};
use super::geometry::{
    advanced_tm, baseline_rotation, estimated_advance_ts, reading_direction, rise_adjusted,
    scaled_run_geometry,
};
use super::word_gaps::{
    is_dependent_sign, offset_takes_spacing_back, tj_gap_thresholds, tj_tracking,
    word_gap_candidate, word_gap_threshold, PenHighWater, PendingWordGaps, WordGapCandidate,
};
use super::{get_number, image_bbox_from_ctm, multiply_matrices};
use crate::PdfError;

/// Best-effort extraction of the raw encoded bytes for an embedded raster
/// XObject, so the markdown pipeline can write the figure to disk and reference
/// it by path instead of emitting a bare `![Image: Im0](image)` placeholder.
///
/// Only lossless-enough, self-describing frames we can hand straight to a viewer
/// are returned: `/DCTDecode` JPEG (the overwhelming common case for embedded
/// photos and charts). The returned bytes are guaranteed to carry the JPEG SOI
/// (`FF D8`) and EOI (`FF D9`) markers so they form a valid standalone file.
/// Other filters (JPX, ASCII85, Flate, …) return `None`; the caller keeps
/// emitting a placeholder until a real decoder is wired in.
///
/// `owner_id` is the owning object's `ObjectId` (a page, or a Form XObject for
/// figures embedded inside a form); `xobject_name` is the resource name as it
/// appears in that object's `/Resources /XObject` dict.
pub(crate) fn extract_image_by_id(
    doc: &Document,
    img_id: ObjectId,
) -> Result<Option<(Vec<u8>, String)>, PdfError> {
    let Ok(stream_obj) = doc.get_object(img_id) else {
        return Ok(None);
    };
    let Object::Stream(stream) = stream_obj else {
        return Ok(None);
    };
    extract_image_from_stream(doc, stream)
}

pub(crate) fn extract_image_from_xobject(
    doc: &Document,
    owner_id: ObjectId,
    xobject_name: &str,
) -> Result<Option<(Vec<u8>, String)>, PdfError> {
    let Some(entry) = find_xobject_entry(doc, owner_id, xobject_name) else {
        return Ok(None);
    };
    // Resolve the XObject entry, whether stored inline or by reference.
    let stream_obj = match entry {
        Object::Reference(r) => doc.get_object(*r)?,
        other => other,
    };
    let Object::Stream(stream) = stream_obj else {
        return Ok(None);
    };
    extract_image_from_stream(doc, stream)
}

pub(crate) fn extract_image_from_stream(
    doc: &Document,
    stream: &lopdf::Stream,
) -> Result<Option<(Vec<u8>, String)>, PdfError> {
    let is_dct = match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => name == b"DCTDecode",
        Ok(Object::Array(arr)) => arr
            .iter()
            .any(|item| item.as_name().map(|n| n == b"DCTDecode").unwrap_or(false)),
        _ => false,
    };
    if is_dct {
        let mut bytes = stream.content.clone();
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
            if bytes[bytes.len() - 2] != 0xFF || bytes[bytes.len() - 1] != 0xD9 {
                bytes.push(0xFF);
                bytes.push(0xD9);
            }
            return Ok(Some((bytes, "jpg".to_string())));
        } else if bytes.len() > 4 {
            bytes.insert(0, 0xD8);
            bytes.insert(0, 0xFF);
            bytes.push(0xFF);
            bytes.push(0xD9);
            return Ok(Some((bytes, "jpg".to_string())));
        }
        return Ok(None);
    }

    let is_jpx = match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => name == b"JPXDecode",
        Ok(Object::Array(arr)) => arr
            .iter()
            .any(|item| item.as_name().map(|n| n == b"JPXDecode").unwrap_or(false)),
        _ => false,
    };
    if is_jpx {
        return Ok(Some((stream.content.clone(), "jp2".to_string())));
    }

    // JBIG2 bi-level image stream
    let is_jbig2 = match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => name == b"JBIG2Decode",
        Ok(Object::Array(arr)) => arr
            .iter()
            .any(|item| item.as_name().map(|n| n == b"JBIG2Decode").unwrap_or(false)),
        _ => false,
    };
    if is_jbig2 {
        let jbig2_bytes = match stream.decompressed_content() {
            Ok(data) => data,
            Err(_) => stream.content.clone(),
        };
        if !jbig2_bytes.is_empty() {
            let globals_bytes = resolve_jbig2_globals(doc, stream);
            if let Ok(img) =
                hayro_jbig2::Image::new_embedded(&jbig2_bytes, globals_bytes.as_deref())
            {
                let w = img.width();
                let h = img.height();
                if w > 0 && h > 0 {
                    let mut buf = vec![255u8; (w * h) as usize];
                    let mut dec = Jbig2LumaDecoder {
                        buf: &mut buf,
                        pos: 0,
                    };
                    if img.decode(&mut dec).is_ok() {
                        // Invert if /Decode array specifies inverted polarity [1, 0]
                        if let Ok(Object::Array(arr)) = stream.dict.get(b"Decode") {
                            if arr.len() >= 2 {
                                let d0 = arr[0]
                                    .as_float()
                                    .or_else(|_| arr[0].as_i64().map(|v| v as f32))
                                    .unwrap_or(0.0);
                                let d1 = arr[1]
                                    .as_float()
                                    .or_else(|_| arr[1].as_i64().map(|v| v as f32))
                                    .unwrap_or(1.0);
                                if d0 > d1 {
                                    for b in &mut buf {
                                        *b = 255 - *b;
                                    }
                                }
                            }
                        }

                        if let Some(png) = encode_png(w, h, 0, &buf) {
                            return Ok(Some((png, "png".to_string())));
                        }
                    }
                }
            }
        }
        return Ok(None);
    }

    // FlateDecode or uncompressed raster image
    let is_flate = match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => name == b"FlateDecode",
        Ok(Object::Array(arr)) => arr
            .iter()
            .any(|item| item.as_name().map(|n| n == b"FlateDecode").unwrap_or(false)),
        Err(_) => true, // No filter = raw stream
        _ => false,
    };
    if is_flate {
        let width = stream
            .dict
            .get(b"Width")
            .ok()
            .and_then(get_number)
            .map(|n| n as u32);
        let height = stream
            .dict
            .get(b"Height")
            .ok()
            .and_then(get_number)
            .map(|n| n as u32);
        let bpc = stream
            .dict
            .get(b"BitsPerComponent")
            .ok()
            .and_then(get_number)
            .map(|n| n as u32)
            .unwrap_or(8);
        if let (Some(w), Some(h)) = (width, height) {
            if w > 0 && h > 0 {
                let decompressed = match stream.decompressed_content() {
                    Ok(data) => data,
                    Err(_) => stream.content.clone(),
                };

                let cs_obj = match stream.dict.get(b"ColorSpace") {
                    Ok(Object::Reference(r)) => doc.get_object(*r).ok(),
                    Ok(obj) => Some(obj),
                    Err(_) => None,
                };

                // Check for Indexed ColorSpace
                if let Some(Object::Array(arr)) = cs_obj {
                    if arr.first().and_then(|item| item.as_name().ok()) == Some(b"Indexed") {
                        if let Some(res) = extract_indexed_image(doc, arr, w, h, bpc, &decompressed)
                        {
                            return Ok(Some(res));
                        }
                    }
                }

                // Check for 1-bit monochrome (DeviceGray or implicit mask)
                if bpc == 1 {
                    let (cs_name, _) = match cs_obj {
                        Some(Object::Name(name)) => (Some(name.as_slice()), None),
                        Some(Object::Array(arr)) => parse_cs_array(doc, arr),
                        _ => (None, None),
                    };
                    if cs_name.is_none() || cs_name == Some(b"DeviceGray") {
                        let row_bytes = (w as usize).div_ceil(8);
                        if decompressed.len() >= row_bytes * (h as usize) {
                            let mut gray = Vec::with_capacity((w * h) as usize);
                            for row in 0..h as usize {
                                let row_slice =
                                    &decompressed[row * row_bytes..(row + 1) * row_bytes];
                                for x in 0..w as usize {
                                    let byte = row_slice[x / 8];
                                    let bit = (byte >> (7 - (x % 8))) & 1;
                                    gray.push(if bit == 1 { 255 } else { 0 });
                                }
                            }
                            if let Some(png) = encode_png(w, h, 0, &gray) {
                                return Ok(Some((png, "png".to_string())));
                            }
                        }
                    }
                }

                if bpc == 8 {
                    let (cs_name, _) = match cs_obj {
                        Some(Object::Name(name)) => (Some(name.as_slice()), None),
                        Some(Object::Reference(r)) => {
                            if let Ok(obj) = doc.get_object(*r) {
                                match obj {
                                    Object::Name(name) => (Some(name.as_slice()), None),
                                    Object::Array(arr) => parse_cs_array(doc, arr),
                                    _ => (None, None),
                                }
                            } else {
                                (None, None)
                            }
                        }
                        Some(Object::Array(arr)) => parse_cs_array(doc, arr),
                        _ => (None, None),
                    };
                    if let Some(cs) = cs_name {
                        if cs == b"DeviceRGB" && decompressed.len() >= (w * h * 3) as usize {
                            if let Some(png) =
                                encode_png(w, h, 2, &decompressed[..(w * h * 3) as usize])
                            {
                                return Ok(Some((png, "png".to_string())));
                            }
                        } else if cs == b"DeviceGray" && decompressed.len() >= (w * h) as usize {
                            if let Some(png) =
                                encode_png(w, h, 0, &decompressed[..(w * h) as usize])
                            {
                                return Ok(Some((png, "png".to_string())));
                            }
                        } else if cs == b"DeviceCMYK" && decompressed.len() >= (w * h * 4) as usize
                        {
                            let total_pixels = (w * h) as usize;
                            let mut rgb = Vec::with_capacity(total_pixels * 3);
                            for chunk in decompressed[..(total_pixels * 4)].chunks_exact(4) {
                                let c = chunk[0] as f32 / 255.0;
                                let m = chunk[1] as f32 / 255.0;
                                let y = chunk[2] as f32 / 255.0;
                                let k = chunk[3] as f32 / 255.0;
                                let r = ((1.0 - c) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                                let g = ((1.0 - m) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                                let b = ((1.0 - y) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                                rgb.push(r);
                                rgb.push(g);
                                rgb.push(b);
                            }
                            if let Some(png) = encode_png(w, h, 2, &rgb) {
                                return Ok(Some((png, "png".to_string())));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

fn extract_indexed_image(
    doc: &Document,
    arr: &[Object],
    w: u32,
    h: u32,
    bpc: u32,
    decompressed: &[u8],
) -> Option<(Vec<u8>, String)> {
    if arr.len() < 4 {
        return None;
    }
    let base_obj = match &arr[1] {
        Object::Reference(r) => doc.get_object(*r).ok(),
        other => Some(other),
    }?;
    let (_base_cs, num_components) = match base_obj {
        Object::Name(name) => {
            if name == b"DeviceRGB" {
                (b"DeviceRGB".as_slice(), 3usize)
            } else if name == b"DeviceGray" {
                (b"DeviceGray".as_slice(), 1usize)
            } else if name == b"DeviceCMYK" {
                (b"DeviceCMYK".as_slice(), 4usize)
            } else {
                return None;
            }
        }
        Object::Array(sub_arr) => {
            let (name, n) = parse_cs_array(doc, sub_arr);
            (name?, n? as usize)
        }
        _ => return None,
    };

    let hival = match &arr[2] {
        Object::Integer(i) => *i as usize,
        Object::Real(r) => *r as usize,
        _ => return None,
    };
    if hival > 255 {
        return None;
    }

    let lookup_data: Vec<u8> = match &arr[3] {
        Object::String(bytes, _) => bytes.clone(),
        Object::Reference(r) => {
            if let Ok(obj) = doc.get_object(*r) {
                match obj {
                    Object::String(bytes, _) => bytes.clone(),
                    Object::Stream(s) => s
                        .decompressed_content()
                        .unwrap_or_else(|_| s.content.clone()),
                    _ => return None,
                }
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let required_lookup_len = (hival + 1) * num_components;
    if lookup_data.len() < required_lookup_len {
        return None;
    }

    let total_pixels = (w as usize).checked_mul(h as usize)?;
    let mut rgb = Vec::with_capacity(total_pixels * 3);

    if bpc == 8 {
        if decompressed.len() < total_pixels {
            return None;
        }
        for &sample in &decompressed[..total_pixels] {
            let idx = (sample as usize).min(hival);
            match num_components {
                3 => {
                    let offset = idx * 3;
                    rgb.extend_from_slice(&lookup_data[offset..offset + 3]);
                }
                1 => {
                    let g = lookup_data[idx];
                    rgb.push(g);
                    rgb.push(g);
                    rgb.push(g);
                }
                4 => {
                    let offset = idx * 4;
                    let c = lookup_data[offset] as f32 / 255.0;
                    let m = lookup_data[offset + 1] as f32 / 255.0;
                    let y = lookup_data[offset + 2] as f32 / 255.0;
                    let k = lookup_data[offset + 3] as f32 / 255.0;
                    let r = ((1.0 - c) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                    let g = ((1.0 - m) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                    let b = ((1.0 - y) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
                    rgb.push(r);
                    rgb.push(g);
                    rgb.push(b);
                }
                _ => return None,
            }
        }
    } else if bpc == 4 {
        let row_bytes = (w as usize).div_ceil(2);
        if decompressed.len() < row_bytes * (h as usize) {
            return None;
        }
        for row in 0..h as usize {
            let row_slice = &decompressed[row * row_bytes..(row + 1) * row_bytes];
            for x in 0..w as usize {
                let byte = row_slice[x / 2];
                let sample = if x % 2 == 0 {
                    (byte >> 4) & 0x0F
                } else {
                    byte & 0x0F
                };
                let idx = (sample as usize).min(hival);
                match num_components {
                    3 => {
                        let offset = idx * 3;
                        rgb.extend_from_slice(&lookup_data[offset..offset + 3]);
                    }
                    1 => {
                        let g = lookup_data[idx];
                        rgb.push(g);
                        rgb.push(g);
                        rgb.push(g);
                    }
                    _ => return None,
                }
            }
        }
    } else if bpc == 1 {
        let row_bytes = (w as usize).div_ceil(8);
        if decompressed.len() < row_bytes * (h as usize) {
            return None;
        }
        for row in 0..h as usize {
            let row_slice = &decompressed[row * row_bytes..(row + 1) * row_bytes];
            for x in 0..w as usize {
                let byte = row_slice[x / 8];
                let bit = (byte >> (7 - (x % 8))) & 1;
                let idx = (bit as usize).min(hival);
                match num_components {
                    3 => {
                        let offset = idx * 3;
                        rgb.extend_from_slice(&lookup_data[offset..offset + 3]);
                    }
                    1 => {
                        let g = lookup_data[idx];
                        rgb.push(g);
                        rgb.push(g);
                        rgb.push(g);
                    }
                    _ => return None,
                }
            }
        }
    } else {
        return None;
    }

    let png = encode_png(w, h, 2, &rgb)?;
    Some((png, "png".to_string()))
}

fn parse_cs_array<'a>(doc: &'a Document, arr: &'a [Object]) -> (Option<&'a [u8]>, Option<u32>) {
    if arr.is_empty() {
        return (None, None);
    }
    if let Ok(name) = arr[0].as_name() {
        if name == b"DeviceRGB" {
            return (Some(b"DeviceRGB"), Some(3));
        } else if name == b"DeviceGray" {
            return (Some(b"DeviceGray"), Some(1));
        } else if name == b"DeviceCMYK" {
            return (Some(b"DeviceCMYK"), Some(4));
        } else if name == b"ICCBased" && arr.len() > 1 {
            let icc_stream = match &arr[1] {
                Object::Reference(r) => doc.get_object(*r).ok(),
                other => Some(other),
            };
            if let Some(Object::Stream(s)) = icc_stream {
                if let Some(n) = s.dict.get(b"N").ok().and_then(get_number) {
                    let n = n as u32;
                    return match n {
                        1 => (Some(b"DeviceGray"), Some(1)),
                        3 => (Some(b"DeviceRGB"), Some(3)),
                        4 => (Some(b"DeviceCMYK"), Some(4)),
                        _ => (None, Some(n)),
                    };
                }
            }
        }
    }
    (None, None)
}

fn encode_png(width: u32, height: u32, color_type: u8, raw_pixels: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let bpp = match color_type {
        0 => 1, // Grayscale
        2 => 3, // RGB
        6 => 4, // RGBA
        _ => return None,
    };
    let row_bytes = (width as usize).checked_mul(bpp)?;
    let total_bytes = row_bytes.checked_mul(height as usize)?;
    if raw_pixels.len() < total_bytes {
        return None;
    }

    let mut filtered_data = Vec::with_capacity((row_bytes + 1) * height as usize);
    for row in 0..height as usize {
        filtered_data.push(0); // Filter type 0 (None)
        let start = row * row_bytes;
        let end = start + row_bytes;
        filtered_data.extend_from_slice(&raw_pixels[start..end]);
    }

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    if encoder.write_all(&filtered_data).is_err() {
        return None;
    }
    let compressed_idat = encoder.finish().ok()?;

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr_data = Vec::with_capacity(13);
    ihdr_data.extend_from_slice(&width.to_be_bytes());
    ihdr_data.extend_from_slice(&height.to_be_bytes());
    ihdr_data.push(8); // bit depth 8
    ihdr_data.push(color_type);
    ihdr_data.push(0); // compression
    ihdr_data.push(0); // filter
    ihdr_data.push(0); // interlace
    write_png_chunk(&mut png, b"IHDR", &ihdr_data);

    write_png_chunk(&mut png, b"IDAT", &compressed_idat);
    write_png_chunk(&mut png, b"IEND", &[]);

    Some(png)
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    let crc = hasher.finalize();
    out.extend_from_slice(&crc.to_be_bytes());
}

struct Jbig2LumaDecoder<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl hayro_jbig2::Decoder for Jbig2LumaDecoder<'_> {
    fn push_pixel(&mut self, black: bool) {
        if self.pos < self.buf.len() {
            self.buf[self.pos] = if black { 0 } else { 255 };
            self.pos += 1;
        }
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let luma = if black { 0 } else { 255 };
        let count = chunk_count as usize * 8;
        let end = (self.pos + count).min(self.buf.len());
        if self.pos < end {
            self.buf[self.pos..end].fill(luma);
            self.pos = end;
        }
    }

    fn next_line(&mut self) {}
}

fn resolve_jbig2_globals(doc: &Document, stream: &lopdf::Stream) -> Option<Vec<u8>> {
    let dp = stream.dict.get(b"DecodeParms").ok()?;
    let globals_obj = match dp {
        Object::Dictionary(dict) => dict.get(b"JBIG2Globals").ok()?,
        Object::Array(arr) => arr.iter().find_map(|item| match item {
            Object::Dictionary(dict) => dict.get(b"JBIG2Globals").ok(),
            _ => None,
        })?,
        _ => return None,
    };
    let resolved = match globals_obj {
        Object::Reference(r) => doc.get_object(*r).ok()?,
        other => other,
    };
    match resolved {
        Object::Stream(s) => match s.decompressed_content() {
            Ok(data) => Some(data),
            Err(_) => Some(s.content.clone()),
        },
        _ => None,
    }
}

/// Resolve a dictionary field value (inline dict or reference) to a borrowed
/// dictionary, propagating lookup errors.
fn resolve_dict<'a>(
    doc: &'a Document,
    value: &'a Object,
) -> Result<&'a lopdf::Dictionary, PdfError> {
    if let Ok(obj_ref) = value.as_reference() {
        doc.get_dictionary(obj_ref).map_err(PdfError::from)
    } else {
        value.as_dict().map_err(PdfError::from)
    }
}

const MAX_FORM_XOBJECT_DEPTH: u8 = 5;

/// Upper bound on Form XObject invocations during a single page extraction.
/// Depth alone is not enough: an acyclic DAG where each form invokes the next
/// N times expands to N^depth work before the depth cap is reached.
const MAX_FORM_XOBJECT_INVOCATIONS: usize = 10_000;

/// Upper bound on content-stream operations walked across all Form XObject
/// expansions for a page. Nested forms are decoded independently of the
/// page-level operation cap, so this keeps total form work in the same
/// ballpark as that page cap.
const MAX_FORM_XOBJECT_OPERATIONS: usize = 1_000_000;

/// Shared budget for Form XObject expansion on a page. Bounds both nested DAG
/// expansion and repeated sibling `/Do` invocations of the same form.
pub(crate) struct FormWalkBudget {
    invocations: usize,
    operations: usize,
    max_invocations: usize,
    max_operations: usize,
    truncated: bool,
}

impl FormWalkBudget {
    pub(crate) fn new() -> Self {
        Self::with_limits(MAX_FORM_XOBJECT_INVOCATIONS, MAX_FORM_XOBJECT_OPERATIONS)
    }

    fn with_limits(max_invocations: usize, max_operations: usize) -> Self {
        Self {
            invocations: 0,
            operations: 0,
            max_invocations,
            max_operations,
            truncated: false,
        }
    }

    fn exhausted(&mut self) -> bool {
        if self.invocations >= self.max_invocations || self.operations >= self.max_operations {
            self.truncated = true;
            true
        } else {
            false
        }
    }

    fn charge_invocation(&mut self) -> bool {
        if self.exhausted() {
            return false;
        }
        self.invocations += 1;
        true
    }

    /// Charge one walked content-stream operator. Independent of the
    /// invocation cap so a form that was already admitted can finish its
    /// stream (up to the operation cap).
    fn charge_operation(&mut self) -> bool {
        if self.operations >= self.max_operations {
            self.truncated = true;
            return false;
        }
        self.operations += 1;
        true
    }

    pub(crate) fn was_truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug)]
pub(crate) enum XObjectType {
    Image(ObjectId),
    Form(ObjectId),
}

fn find_xobject_entry<'a>(
    doc: &'a Document,
    owner_id: ObjectId,
    xobject_name: &str,
) -> Option<&'a Object> {
    let name_bytes = xobject_name.as_bytes();
    // 1. Try page resources (includes inherited resources from parent /Pages)
    if let Ok((own_dict, inherited_ids)) = doc.get_page_resources(owner_id) {
        if let Some(res) = own_dict {
            if let Ok(xobjects_obj) = res.get(b"XObject") {
                if let Ok(xobjects) = resolve_dict(doc, xobjects_obj) {
                    if let Ok(entry) = xobjects.get(name_bytes) {
                        return Some(entry);
                    }
                }
            }
        }
        for id in inherited_ids {
            if let Ok(res) = doc.get_dictionary(id) {
                if let Ok(xobjects_obj) = res.get(b"XObject") {
                    if let Ok(xobjects) = resolve_dict(doc, xobjects_obj) {
                        if let Ok(entry) = xobjects.get(name_bytes) {
                            return Some(entry);
                        }
                    }
                }
            }
        }
    }

    // 2. Direct lookup on owner_dict (e.g. Form XObject)
    if let Ok(owner_dict) = doc.get_dictionary(owner_id) {
        if let Ok(res_obj) = owner_dict.get(b"Resources") {
            if let Ok(resources) = resolve_dict(doc, res_obj) {
                if let Ok(xobjects_obj) = resources.get(b"XObject") {
                    if let Ok(xobjects) = resolve_dict(doc, xobjects_obj) {
                        if let Ok(entry) = xobjects.get(name_bytes) {
                            return Some(entry);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Get XObjects from page resources, categorized by type
pub(crate) fn get_page_xobjects(
    doc: &Document,
    page_id: ObjectId,
) -> std::collections::HashMap<String, XObjectType> {
    let mut xobject_types = std::collections::HashMap::new();

    if let Ok((own_dict, inherited_ids)) = doc.get_page_resources(page_id) {
        for id in inherited_ids {
            if let Ok(resources) = doc.get_dictionary(id) {
                collect_xobjects_from_dict(doc, resources, &mut xobject_types);
            }
        }
        if let Some(resources) = own_dict {
            collect_xobjects_from_dict(doc, resources, &mut xobject_types);
        }
    } else if let Ok(page_dict) = doc.get_dictionary(page_id) {
        let resources = if let Ok(res_ref) = page_dict.get(b"Resources") {
            if let Ok(obj_ref) = res_ref.as_reference() {
                doc.get_dictionary(obj_ref).ok()
            } else {
                res_ref.as_dict().ok()
            }
        } else {
            None
        };

        if let Some(resources) = resources {
            collect_xobjects_from_dict(doc, resources, &mut xobject_types);
        }
    }

    xobject_types
}

/// Get XObjects from a Form XObject's Resources
fn get_form_xobjects(
    doc: &Document,
    form_dict: &lopdf::Dictionary,
) -> HashMap<String, XObjectType> {
    let mut xobject_types = HashMap::new();

    let resources = if let Ok(res_ref) = form_dict.get(b"Resources") {
        if let Ok(obj_ref) = res_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            res_ref.as_dict().ok()
        }
    } else {
        return xobject_types;
    };

    if let Some(resources) = resources {
        collect_xobjects_from_dict(doc, resources, &mut xobject_types);
    }

    xobject_types
}

/// Collect XObject entries from a Resources dictionary
fn collect_xobjects_from_dict(
    doc: &Document,
    resources: &lopdf::Dictionary,
    xobject_types: &mut HashMap<String, XObjectType>,
) {
    if let Ok(xobjects_ref) = resources.get(b"XObject") {
        let xobjects = if let Ok(obj_ref) = xobjects_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            xobjects_ref.as_dict().ok()
        };

        if let Some(xobjects) = xobjects {
            for (name, value) in xobjects.iter() {
                let name_str = String::from_utf8_lossy(name).to_string();

                if let Ok(obj_ref) = value.as_reference() {
                    if let Ok(Object::Stream(stream)) = doc.get_object(obj_ref) {
                        if let Ok(subtype) = stream.dict.get(b"Subtype") {
                            if let Ok(subtype_name) = subtype.as_name() {
                                if subtype_name == b"Image" {
                                    xobject_types.insert(name_str, XObjectType::Image(obj_ref));
                                } else if subtype_name == b"Form" {
                                    xobject_types.insert(name_str, XObjectType::Form(obj_ref));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Text items extracted from a content stream together with the visual-order
/// RTL evidence gathered while parsing them, for the page-level
/// `fix_visual_order_rtl` pass: indexes of candidate items (see
/// `is_visual_rtl_candidate`) and of the items shown by logical-order ops.
pub(crate) struct ExtractedText {
    pub(crate) items: Vec<TextItem>,
    /// The CMap coverage of each item, parallel to `items`.
    pub(crate) item_coverage: ItemCoverage,
    pub(crate) rtl_visual_candidates: Vec<usize>,
    pub(crate) rtl_logical_runs: Vec<usize>,
    /// Indexes of the items shown by visible ops of several RTL letters
    /// painted forwards (see `is_visual_rtl_run`): visual-storage votes.
    pub(crate) rtl_visual_runs: Vec<usize>,
    /// Baseline angle of every text-producing show operator, in stream
    /// order: this form's share of the page-rotation vote. Per operator,
    /// not per item — one TJ array can split into several items.
    pub(crate) run_rotations: Vec<f32>,
    /// Invisible (Tr 3) text was present but suppressed — the same signal
    /// the page parser reports, so a hidden OCR layer drawn through a form
    /// still earns the `include_invisible` retry.
    pub(crate) skipped_invisible: bool,
}

impl ExtractedText {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            item_coverage: Vec::new(),
            rtl_visual_candidates: Vec::new(),
            rtl_logical_runs: Vec::new(),
            rtl_visual_runs: Vec::new(),
            run_rotations: Vec::new(),
            skipped_invisible: false,
        }
    }

    /// Append this extraction into a caller's accumulators, rebasing the
    /// candidate indexes onto the caller's item vector. Keeping the rebase
    /// here is what stops item and RTL-evidence bookkeeping from drifting
    /// apart across the page/form extraction paths.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_into(
        self,
        items: &mut Vec<TextItem>,
        item_coverage: &mut ItemCoverage,
        rtl_visual_candidates: &mut Vec<usize>,
        rtl_logical_runs: &mut Vec<usize>,
        rtl_visual_runs: &mut Vec<usize>,
        run_rotations: &mut Vec<f32>,
        skipped_invisible: &mut bool,
    ) {
        let base = items.len();
        rtl_visual_candidates.extend(self.rtl_visual_candidates.into_iter().map(|c| c + base));
        rtl_logical_runs.extend(self.rtl_logical_runs.into_iter().map(|c| c + base));
        rtl_visual_runs.extend(self.rtl_visual_runs.into_iter().map(|c| c + base));
        run_rotations.extend(self.run_rotations);
        *skipped_invisible |= self.skipped_invisible;
        debug_assert_eq!(self.items.len(), self.item_coverage.len());
        items.extend(self.items);
        item_coverage.extend(self.item_coverage);
    }
}

/// Extract text items from a Form XObject.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_form_xobject_text(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    include_invisible: bool,
    inherited_render_mode: i32,
    inherited_text_rise: f32,
    inherited_horizontal_scale: f32,
    inherited_text_paint: TextPaint,
    inherited_fill_is_white: bool,
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
    budget: &mut FormWalkBudget,
) -> ExtractedText {
    extract_form_xobject_text_inner(
        doc,
        form_id,
        page_num,
        font_cmaps,
        parent_ctm,
        include_invisible,
        inherited_render_mode,
        inherited_text_rise,
        inherited_horizontal_scale,
        inherited_text_paint,
        inherited_fill_is_white,
        cmap_decisions,
        style_cache,
        0,
        budget,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_form_xobject_text_inner(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    include_invisible: bool,
    inherited_render_mode: i32,
    inherited_text_rise: f32,
    inherited_horizontal_scale: f32,
    inherited_text_paint: TextPaint,
    inherited_fill_is_white: bool,
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
    depth: u8,
    budget: &mut FormWalkBudget,
) -> ExtractedText {
    let mut extracted = ExtractedText::new();

    if !budget.charge_invocation() {
        return extracted;
    }

    // Get the Form XObject stream
    let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
        return extracted;
    };

    // Decompress the content stream within the page-content bound: a form
    // inflating past it is skipped, as a page over it is, before anything
    // is allocated for it. A stream that fails to decode for another
    // reason is read raw, if that fits the bound too.
    let content_data = match stream
        .decompressed_content_with_limit(super::content_decode::MAX_PAGE_CONTENT_BYTES)
    {
        Ok(data) => data,
        Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. })) => {
            log::warn!(
                "form xobject {:?}: skipping — content stream exceeds {} decompressed bytes",
                form_id,
                super::content_decode::MAX_PAGE_CONTENT_BYTES
            );
            return extracted;
        }
        Err(_) if stream.content.len() > super::content_decode::MAX_PAGE_CONTENT_BYTES => {
            log::warn!(
                "form xobject {:?}: skipping — raw content stream exceeds {} bytes",
                form_id,
                super::content_decode::MAX_PAGE_CONTENT_BYTES
            );
            return extracted;
        }
        Err(_) => stream.content.clone(),
    };

    // Decode the content stream. Cap before lopdf materializes the operator
    // vector — the walk budget cannot help if decode itself allocates first.
    let Ok(Some(content)) = super::content_decode::decode_content_bounded(
        &content_data,
        super::content_decode::MAX_PAGE_OPERATIONS,
    ) else {
        return extracted;
    };
    let items = &mut extracted.items;
    let item_coverage = &mut extracted.item_coverage;
    let rtl_visual_candidates = &mut extracted.rtl_visual_candidates;
    let rtl_logical_runs = &mut extracted.rtl_logical_runs;
    let rtl_visual_runs = &mut extracted.rtl_visual_runs;
    let run_rotations = &mut extracted.run_rotations;
    let skipped_invisible = &mut extracted.skipped_invisible;

    // Get fonts from the Form's Resources
    let form_fonts = get_form_fonts(doc, &stream.dict);
    let paint_resources = PaintResources::form(doc, &stream.dict);
    // Unknown font resources may be Type3; infer stroke weight only for
    // positively resolved ordinary text fonts.
    let paintable_fonts: std::collections::HashSet<String> = form_fonts
        .iter()
        .filter(|(_, font)| {
            font.get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
                .is_some_and(|subtype| {
                    matches!(subtype, b"Type0" | b"Type1" | b"MMType1" | b"TrueType")
                })
        })
        .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
        .collect();
    let (font_encodings, _has_gid_fonts) =
        build_font_encodings(doc, &form_fonts, font_cmaps, style_cache);

    // Build font width info for the form
    let font_widths = build_font_widths(doc, &form_fonts, style_cache);
    let font_kinds = build_font_kinds(&form_fonts);
    let type3_scales = build_type3_scales(doc, &form_fonts);
    let type3_y_flips = build_type3_y_flips(doc, &form_fonts);

    // Build font base names and ToUnicode refs for the form
    let mut font_base_names: HashMap<String, String> = HashMap::new();
    let mut font_tounicode_refs: HashMap<String, u32> = HashMap::new();
    let mut inline_cmaps: HashMap<String, crate::tounicode::CMapEntry> = HashMap::new();

    let mut font_styles: HashMap<String, FontStyle> = HashMap::new();
    for (font_name, font_dict) in &form_fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        // The name (the resource tag for a font without one) and the width
        // table are read once here rather than for every run.
        let style = font_style(doc, font_dict, style_cache)
            .with_name(
                font_base_names
                    .get(&resource_name)
                    .map_or(resource_name.as_str(), String::as_str),
            )
            .with_measured_pitch(font_widths.get(&resource_name));
        if style != FontStyle::default() {
            font_styles.insert(resource_name.clone(), style);
        }
        match font_dict.get(b"ToUnicode") {
            Ok(tounicode) => {
                if let Ok(obj_ref) = tounicode.as_reference() {
                    font_tounicode_refs.insert(resource_name, obj_ref.0);
                } else if let Object::Stream(s) = tounicode {
                    let data = s
                        .decompressed_content()
                        .unwrap_or_else(|_| s.content.clone());
                    if let Some(entry) =
                        crate::tounicode::build_cmap_entry_from_stream(&data, font_dict, doc, 0)
                    {
                        inline_cmaps.insert(resource_name, entry);
                    }
                }
            }
            Err(_) => {
                if let Some(ff2_obj_num) = get_font_file2_obj_num(doc, font_dict) {
                    font_tounicode_refs.insert(resource_name, ff2_obj_num);
                }
            }
        }
    }

    // Cache font encodings for form fonts
    let mut encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
    for (font_name, font_dict) in &form_fonts {
        let name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(enc) = font_dict.get_font_encoding(doc) {
            encoding_cache.insert(name, enc);
        }
    }

    // Build XObject map from the Form's own Resources for nested Do
    let form_xobjects = get_form_xobjects(doc, &stream.dict);

    // Apply the Form XObject's own Matrix (if any) to the parent CTM
    let form_matrix = if let Ok(matrix_obj) = stream.dict.get(b"Matrix") {
        if let Ok(arr) = matrix_obj.as_array() {
            if arr.len() >= 6 {
                let mut m = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
                for (i, v) in arr.iter().take(6).enumerate() {
                    m[i] = get_number(v).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                }
                m
            } else {
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
            }
        } else {
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        }
    } else {
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
    };
    let base_ctm = multiply_matrices(&form_matrix, parent_ctm);

    // Process the content stream
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    // Text line matrix (TLM) — Td/TD/T* move relative to the start of the
    // current line, not to the position left by the last show operator.
    let mut line_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut text_leading: f32 = 0.0; // TL parameter (text-space units)
    let mut char_spacing: f32 = 0.0; // Tc parameter
    let mut word_spacing: f32 = 0.0; // Tw parameter
                                     // Ts parameter (baseline shift, unscaled). Text state is graphics state,
                                     // so a form starts with the rise in force where it was invoked.
    let mut horizontal_scale: f32 = inherited_horizontal_scale;
    let mut pending_space: Option<PendingSpace> = None;
    let mut pending_word_gaps: Option<PendingWordGaps> = None;
    let mut text_rise: f32 = inherited_text_rise;
    // Tr is graphics state, so a form starts in the mode the invoking stream
    // left it in: `3 Tr` set on the page or in an outer form hides the text
    // drawn here too.
    let mut text_rendering_mode: i32 = inherited_render_mode;
    let mut text_paint = inherited_text_paint;
    let mut in_text_block = false;
    // The fill colour is graphics state too: a form invoked under a white
    // fill paints white until it sets its own colour.
    let mut fill_is_white = inherited_fill_is_white;
    let mut ctm = base_ctm;

    // Text state (Tc/Tw/TL/Tf) and the fill colour are part of the graphics
    // state and must be saved/restored by q/Q alongside the CTM.
    #[derive(Clone)]
    struct GraphicsState {
        ctm: [f32; 6],
        char_spacing: f32,
        word_spacing: f32,
        horizontal_scale: f32,
        text_rise: f32,
        text_rendering_mode: i32,
        text_paint: TextPaint,
        text_leading: f32,
        current_font: String,
        current_font_size: f32,
        fill_is_white: bool,
    }
    let mut ctm_stack: Vec<GraphicsState> = Vec::new();

    for op in &content.operations {
        // The coverage of the preceding operator's decodes goes to the first
        // item it appended (see `attach_run_coverage`).
        attach_run_coverage(item_coverage, items.len(), || {
            cmap_decisions.take_run_coverage()
        });
        if !budget.charge_operation() {
            break;
        }
        text_paint.observe(&op.operator, &op.operands, &paint_resources);
        match op.operator.as_str() {
            "q" => {
                ctm_stack.push(GraphicsState {
                    ctm,
                    char_spacing,
                    word_spacing,
                    horizontal_scale,
                    text_rise,
                    text_rendering_mode,
                    text_paint: text_paint.clone(),
                    text_leading,
                    current_font: current_font.clone(),
                    current_font_size,
                    fill_is_white,
                });
            }
            "Q" => {
                if let Some(saved) = ctm_stack.pop() {
                    ctm = saved.ctm;
                    char_spacing = saved.char_spacing;
                    word_spacing = saved.word_spacing;
                    horizontal_scale = saved.horizontal_scale;
                    text_rise = saved.text_rise;
                    text_rendering_mode = saved.text_rendering_mode;
                    text_paint = saved.text_paint;
                    text_leading = saved.text_leading;
                    current_font = saved.current_font;
                    current_font_size = saved.current_font_size;
                    fill_is_white = saved.fill_is_white;
                }
            }
            "cm" => {
                if op.operands.len() >= 6 {
                    let mut m = [0.0f32; 6];
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        m[i] = get_number(operand).unwrap_or(0.0);
                    }
                    ctm = multiply_matrices(&m, &ctm);
                }
            }
            "Do" => {
                // A nested form's runs belong to its own stream and never
                // continue a wide-spaced boundary string of this one.
                pending_word_gaps = None;
                if !op.operands.is_empty() {
                    if let Ok(name) = op.operands[0].as_name() {
                        let xobj_name = String::from_utf8_lossy(name).to_string();
                        match form_xobjects.get(&xobj_name) {
                            Some(XObjectType::Form(nested_id)) => {
                                if depth < MAX_FORM_XOBJECT_DEPTH && !budget.exhausted() {
                                    extract_form_xobject_text_inner(
                                        doc,
                                        *nested_id,
                                        page_num,
                                        font_cmaps,
                                        &ctm,
                                        include_invisible,
                                        text_rendering_mode,
                                        text_rise,
                                        horizontal_scale,
                                        text_paint.clone(),
                                        fill_is_white,
                                        cmap_decisions,
                                        style_cache,
                                        depth + 1,
                                        budget,
                                    )
                                    .append_into(
                                        items,
                                        item_coverage,
                                        rtl_visual_candidates,
                                        rtl_logical_runs,
                                        rtl_visual_runs,
                                        run_rotations,
                                        skipped_invisible,
                                    );
                                }
                            }
                            Some(XObjectType::Image(img_id)) => {
                                // Mirror the top-level Image-XObject emission
                                // in content_stream.rs so figures embedded
                                // inside Form XObjects (common in print-to-PDF
                                // workflows) aren't silently dropped.
                                let (x, y, width, height) = image_bbox_from_ctm(&ctm);
                                // Pull the frame from the image object id directly or fallback to name lookup
                                let (image_data, image_format) =
                                    match extract_image_by_id(doc, *img_id) {
                                        Ok(Some((data, fmt))) => (Some(data), Some(fmt)),
                                        _ => match extract_image_from_xobject(
                                            doc, form_id, &xobj_name,
                                        ) {
                                            Ok(Some((data, fmt))) => (Some(data), Some(fmt)),
                                            _ => (None, None),
                                        },
                                    };
                                items.push(TextItem {
                                    text: format!("[Image: {}]", xobj_name),
                                    x,
                                    y,
                                    width,
                                    height,
                                    font: String::new(),
                                    font_tag: String::new(),
                                    legacy_symbol_rewrite: false,
                                    font_size: 0.0,
                                    page: page_num,
                                    is_bold: false,
                                    is_italic: false,
                                    font_weight: None,
                                    bold_source: None,
                                    fixed_pitch: None,
                                    fill_color: None,
                                    stroke_color: None,
                                    render_mode: None,
                                    is_underline: false,
                                    is_strikeout: false,
                                    rotation: 0.0,
                                    advance_known: true,
                                    item_type: ItemType::Image,
                                    mcid: None,
                                    baseline_shift: 0.0,
                                    image_data,
                                    image_format,
                                });
                            }
                            None => {}
                        }
                    }
                }
            }
            "BT" => {
                in_text_block = true;
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                line_matrix = text_matrix;
            }
            "ET" => {
                in_text_block = false;
                // A wide-spaced boundary string is continued within its text
                // object; a later object's geometry is unrelated to it.
                pending_word_gaps = None;
            }
            "Tf" => {
                if op.operands.len() >= 2 {
                    if let Ok(name) = op.operands[0].as_name() {
                        current_font = String::from_utf8_lossy(name).to_string();
                    }
                    current_font_size = get_number(&op.operands[1]).unwrap_or(12.0);
                }
            }
            "TL" => {
                // Set text leading (used by T*, ', and ")
                if let Some(tl) = op.operands.first().and_then(get_number) {
                    text_leading = tl;
                }
            }
            "Tc" => {
                if let Some(tc) = op.operands.first().and_then(get_number) {
                    char_spacing = tc;
                }
            }
            "Tw" => {
                if let Some(tw) = op.operands.first().and_then(get_number) {
                    word_spacing = tw;
                }
            }
            "Tr" => {
                // Text rendering mode (3 = invisible / OCR overlay). Hidden
                // text is skipped exactly like the page parser skips it —
                // it neither emits items nor votes on page rotation — unless
                // the caller asked for the hidden layer.
                if let Some(mode) = op.operands.first().and_then(get_number) {
                    text_rendering_mode = mode as i32;
                }
            }
            "Tz" => {
                if let Some(scale) = op.operands.first().and_then(get_number) {
                    if scale.is_finite() {
                        horizontal_scale = scale / 100.0;
                    }
                }
            }
            "Ts" => {
                // Text rise: baseline shift for superscripts/subscripts. It
                // moves the glyph origin, never the advance (see
                // `rise_adjusted`).
                if let Some(ts) = op.operands.first().and_then(get_number) {
                    text_rise = ts;
                }
            }
            "Td" | "TD" => {
                // Move text position: TLM = T(tx,ty) x TLM; Tm = TLM
                if op.operands.len() >= 2 {
                    let tx = get_number(&op.operands[0]).unwrap_or(0.0);
                    let ty = get_number(&op.operands[1]).unwrap_or(0.0);
                    line_matrix[4] += tx * line_matrix[0] + ty * line_matrix[2];
                    line_matrix[5] += tx * line_matrix[1] + ty * line_matrix[3];
                    text_matrix = line_matrix;
                    if op.operator == "TD" {
                        text_leading = -ty;
                    }
                }
            }
            "Tm" => {
                if op.operands.len() >= 6 {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        text_matrix[i] =
                            get_number(operand).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                    }
                    line_matrix = text_matrix;
                }
            }
            "T*" => {
                // Move to start of next line: equivalent to `0 -TL Td`
                let tl = if text_leading != 0.0 {
                    text_leading
                } else {
                    current_font_size * 1.2
                };
                line_matrix[4] += (-tl) * line_matrix[2];
                line_matrix[5] += (-tl) * line_matrix[3];
                text_matrix = line_matrix;
            }
            "g" => {
                if let Some(gray) = op.operands.first().and_then(get_number) {
                    fill_is_white = gray > 0.95;
                }
            }
            "rg" => {
                if op.operands.len() >= 3 {
                    let r = get_number(&op.operands[0]).unwrap_or(0.0);
                    let g = get_number(&op.operands[1]).unwrap_or(0.0);
                    let b = get_number(&op.operands[2]).unwrap_or(0.0);
                    fill_is_white = r > 0.95 && g > 0.95 && b > 0.95;
                }
            }
            "k" => {
                if op.operands.len() >= 4 {
                    let c = get_number(&op.operands[0]).unwrap_or(1.0);
                    let m = get_number(&op.operands[1]).unwrap_or(1.0);
                    let y = get_number(&op.operands[2]).unwrap_or(1.0);
                    let k = get_number(&op.operands[3]).unwrap_or(1.0);
                    fill_is_white = c < 0.05 && m < 0.05 && y < 0.05 && k < 0.05;
                }
            }
            "sc" | "scn" => {
                let nums: Vec<f32> = op.operands.iter().filter_map(get_number).collect();
                match nums.len() {
                    3 => {
                        fill_is_white = nums[0] > 0.95 && nums[1] > 0.95 && nums[2] > 0.95;
                    }
                    4 => {
                        fill_is_white =
                            nums[0] < 0.05 && nums[1] < 0.05 && nums[2] < 0.05 && nums[3] < 0.05;
                    }
                    _ => fill_is_white = false,
                }
            }
            "Tj" | "'" | "\"" => {
                // `'` = move to next line then show; `"` = set word/char spacing,
                // move to next line, then show (string is the last operand).
                if op.operator != "Tj" {
                    if op.operator == "\"" && op.operands.len() >= 3 {
                        word_spacing = get_number(&op.operands[0]).unwrap_or(word_spacing);
                        char_spacing = get_number(&op.operands[1]).unwrap_or(char_spacing);
                    }
                    let tl = if text_leading != 0.0 {
                        text_leading
                    } else {
                        current_font_size * 1.2
                    };
                    line_matrix[4] += (-tl) * line_matrix[2];
                    line_matrix[5] += (-tl) * line_matrix[3];
                    text_matrix = line_matrix;
                }
                if let (true, Some(show_operand)) = (in_text_block, op.operands.last()) {
                    // Where this run starts says whether the run before it, a
                    // wide-spaced boundary string, had its spacing taken back.
                    // An empty show paints nothing and decides nothing.
                    if get_operand_bytes(show_operand).is_some_and(|raw| !raw.is_empty()) {
                        if let Some(pending) = pending_word_gaps.take() {
                            pending.resolve(items, &text_matrix, &ctm);
                        }
                    }
                    let invisible = text_rendering_mode == 3 && !include_invisible;
                    if invisible
                        && get_operand_bytes(show_operand).is_some_and(|raw| !raw.is_empty())
                    {
                        *skipped_invisible = true;
                    }
                    if crate::text_utils::white_fill_hides(text_rendering_mode, fill_is_white)
                        || invisible
                    {
                        if let Some(font_info) = font_widths.get(&current_font) {
                            if let Some(raw_bytes) = get_operand_bytes(show_operand) {
                                let w_ts = compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    char_spacing,
                                    word_spacing,
                                );
                                text_matrix[4] += w_ts * horizontal_scale * text_matrix[0];
                                text_matrix[5] += w_ts * horizontal_scale * text_matrix[1];
                            }
                        } else {
                            // No width metrics: move by the estimate the run
                            // would have carried, as the page parser does.
                            let estimate_ts = estimated_string_advance_ts(
                                get_operand_bytes(show_operand),
                                None,
                                current_font_size
                                    * type3_scales.get(&current_font).copied().unwrap_or(1.0),
                                char_spacing,
                                word_spacing,
                            );
                            text_matrix[4] += estimate_ts * horizontal_scale * text_matrix[0];
                            text_matrix[5] += estimate_ts * horizontal_scale * text_matrix[1];
                        }
                        continue;
                    }
                    if let Some((text, legacy_symbol_rewrite)) = extract_text_from_operand(
                        show_operand,
                        &current_font,
                        font_base_names.get(&current_font).map(|s| s.as_str()),
                        font_cmaps,
                        &font_tounicode_refs,
                        &inline_cmaps,
                        &font_encodings,
                        &encoding_cache,
                        cmap_decisions,
                        &font_widths,
                        &font_kinds,
                    ) {
                        let combined =
                            multiply_matrices(&rise_adjusted(&text_matrix, text_rise), &ctm);
                        let rendered_size = effective_font_size(current_font_size, &combined)
                            * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                        let advance_ts = font_widths.get(&current_font).and_then(|font_info| {
                            get_operand_bytes(show_operand).map(|raw_bytes| {
                                compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    char_spacing,
                                    word_spacing,
                                )
                            })
                        });
                        let em_ts = current_font_size
                            * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                        let raw = get_operand_bytes(show_operand);
                        let fallback_ts = if raw.is_some_and(|bytes| !bytes.is_empty()) {
                            estimated_string_advance_ts(
                                raw,
                                font_widths.get(&current_font),
                                em_ts,
                                char_spacing,
                                word_spacing,
                            )
                        } else {
                            estimated_advance_ts(&text, em_ts)
                        };
                        let geometry = scaled_run_geometry(
                            &combined,
                            advance_ts,
                            fallback_ts,
                            rendered_size.copysign(current_font_size),
                            type3_y_flips.contains(&current_font),
                            horizontal_scale,
                        );
                        // Without width metrics the cursor moves by the same
                        // estimate the run's box carries.
                        let cursor_ts = advance_ts.unwrap_or(fallback_ts);
                        text_matrix[4] += cursor_ts * horizontal_scale * text_matrix[0];
                        text_matrix[5] += cursor_ts * horizontal_scale * text_matrix[1];
                        // Only create text item for non-whitespace; whitespace
                        // still advances the text matrix above so gap detection
                        // works, and a space run hands its word space to the
                        // item it follows.
                        if text.trim().is_empty() {
                            pending_space = PendingSpace::note(
                                pending_space.take(),
                                items,
                                &geometry,
                                page_num,
                            );
                        } else {
                            if let Some(pending) = pending_space.take() {
                                pending.resolve(items, &geometry, &text, rendered_size);
                            }
                            let (dir_x, dir_y) =
                                reading_direction(&combined, current_font_size * horizontal_scale);
                            run_rotations.push(baseline_rotation(dir_x, dir_y));
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let style = font_styles.get(&current_font).copied().unwrap_or_default();
                            // Forward paint order (positive device-space
                            // advance) may be visual storage; a mirrored
                            // matrix already paints right-to-left. Rotated
                            // matrices carry no horizontal evidence and stay
                            // neutral.
                            if crate::text_utils::is_visual_rtl_candidate(&text)
                                && combined[0].abs() > combined[1].abs()
                            {
                                if combined[0] * horizontal_scale > 0.0 {
                                    rtl_visual_candidates.push(items.len());
                                    if !crate::text_utils::white_fill_hides(
                                        text_rendering_mode,
                                        fill_is_white,
                                    ) && crate::text_utils::render_mode_paints(
                                        text_rendering_mode,
                                    ) && crate::text_utils::is_visual_rtl_run(&text)
                                    {
                                        rtl_visual_runs.push(items.len());
                                    }
                                } else {
                                    rtl_logical_runs.push(items.len());
                                }
                            }
                            let painted_bold = paintable_fonts.contains(&current_font)
                                && text_paint.adds_bold(&text, rendered_size, base_font, &ctm);
                            let paint = text_paint.run_paint();
                            items.push(TextItem {
                                text: expand_ligatures(&text),
                                x: geometry.x,
                                y: geometry.y,
                                width: geometry.width,
                                height: geometry.height,
                                font: crate::extractor::fonts::item_font_name(
                                    &current_font,
                                    base_font,
                                )
                                .to_string(),
                                font_tag: current_font.clone(),
                                legacy_symbol_rewrite,
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: style.bold || painted_bold,
                                is_italic: style.italic,
                                font_weight: style.weight,
                                bold_source: style
                                    .bold_source
                                    .or(painted_bold.then_some(BoldSource::Painted)),
                                fixed_pitch: style.fixed_pitch,
                                fill_color: paint.fill_color,
                                stroke_color: paint.stroke_color,
                                render_mode: Some(paint.render_mode),
                                is_underline: false,
                                is_strikeout: false,
                                rotation: geometry.rotation,
                                advance_known: geometry.advance_known,
                                item_type: ItemType::Text,
                                mcid: None,
                                baseline_shift: 0.0,

                                image_data: None,
                                image_format: None,
                            });
                            // A short string with word-gap character spacing
                            // shows its spaces once the next run proves the
                            // spacing after it was taken back (see `word_gaps`).
                            pending_word_gaps = get_operand_bytes(show_operand).and_then(|raw| {
                                PendingWordGaps::for_shown_string(
                                    items.len() - 1,
                                    raw,
                                    &text,
                                    font_widths.get(&current_font),
                                    current_font_size,
                                    char_spacing,
                                    word_spacing,
                                    &text_matrix,
                                    &ctm,
                                    horizontal_scale,
                                    |code| {
                                        cmap_decisions.without_coverage(|decisions| {
                                            extract_text_from_operand(
                                                code,
                                                &current_font,
                                                font_base_names
                                                    .get(&current_font)
                                                    .map(|s| s.as_str()),
                                                font_cmaps,
                                                &font_tounicode_refs,
                                                &inline_cmaps,
                                                &font_encodings,
                                                &encoding_cache,
                                                decisions,
                                                &font_widths,
                                                &font_kinds,
                                            )
                                        })
                                    },
                                )
                            });
                        }
                    }
                }
            }
            "TJ" => {
                // Show text with positioning — split at column-sized gaps
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        // Invisible (Tr 3) text is hidden like white-on-white
                        // text: it advances the pen but shows nothing and
                        // must not vote on page rotation. Numeric-only arrays
                        // (pure kerning) show no text and must not trigger
                        // the invisible retry.
                        let invisible = text_rendering_mode == 3 && !include_invisible;
                        if invisible
                            && array
                                .iter()
                                .any(|el| get_operand_bytes(el).is_some_and(|raw| !raw.is_empty()))
                        {
                            *skipped_invisible = true;
                        }
                        let hidden =
                            crate::text_utils::white_fill_hides(text_rendering_mode, fill_is_white)
                                || invisible;
                        let font_info = font_widths.get(&current_font);

                        // Word-space threshold for `TJ` offsets and character
                        // spacing alike, from the font metrics when available.
                        let space_threshold = word_gap_threshold(font_info);
                        // A tracked display run is judged over its own
                        // tracking, as on the page (see `tj_tracking` and
                        // `tj_gap_thresholds`); a hidden run is not read
                        // for it, and the reader decodes the glyphs of a run
                        // in the tracking band on a copy of the CMap
                        // decisions.
                        let tracking = if hidden {
                            None
                        } else {
                            let mut probe_decisions: Option<CMapDecisionCache> = None;
                            tj_tracking(array, font_info, space_threshold, |element| {
                                extract_text_from_operand(
                                    element,
                                    &current_font,
                                    font_base_names.get(&current_font).map(|s| s.as_str()),
                                    font_cmaps,
                                    &font_tounicode_refs,
                                    &inline_cmaps,
                                    &font_encodings,
                                    &encoding_cache,
                                    probe_decisions.get_or_insert_with(|| cmap_decisions.clone()),
                                    &font_widths,
                                    &font_kinds,
                                )
                            })
                        };
                        let baseline_horizontal = {
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            combined[0].abs() >= combined[1].abs()
                        };
                        let (word_gap, split_gap) =
                            tj_gap_thresholds(space_threshold, tracking, baseline_horizontal);

                        let mut sub_items: Vec<(String, f32, f32, f32, bool)> = Vec::new();
                        let mut current_text = String::new();
                        let mut current_symbol_rewrite = false;
                        let mut current_estimate_ts: f32 = 0.0; // metric-less estimate of `current_text`
                        let mut sub_start_width_ts: f32 = 0.0;
                        let mut total_width_ts: f32 = 0.0;
                        // A sub-run's box starts at its first painted glyph.
                        // Positioning ahead of that glyph — `[-2973 (oduction)]
                        // TJ` rejoining a word whose head was painted first
                        // from another `Tm` — carries the pen from the `Tm`
                        // origin, not the box.
                        let mut sub_run_painted = false;
                        // Positive TJ offsets beyond a space width move the pen
                        // backward past painted glyphs — logical-order RTL
                        // producers position runs right-to-left this way.
                        let mut backward_jump = false;
                        // The farthest the pen has been, for a return from a
                        // zero-advance sign placed behind it: no gap opens on
                        // the page until the pen is past the mark again (see
                        // `PenHighWater`).
                        let mut pen_high_water = PenHighWater::new();
                        // The array's last string, when it is a wide-spaced
                        // boundary string, and the sub-run text before it.
                        let mut deferred_word_gaps: Option<(String, WordGapCandidate)> = None;
                        // Only positioning may follow the array's last string;
                        // the next run decides for a candidate there.
                        let last_string_index = array.iter().rposition(|el| {
                            get_operand_bytes(el).is_some_and(|raw| !raw.is_empty())
                        });
                        for (index, element) in array.iter().enumerate() {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    // The offset as the thresholds judge it:
                                    // itself, or on a return from a sign placed
                                    // behind the high-water mark only the
                                    // travel beyond the mark.
                                    let judged = pen_high_water.judge_offset(
                                        n_val,
                                        total_width_ts,
                                        total_width_ts + displacement,
                                        current_font_size,
                                    );
                                    // A true backtrack puts the pen behind the
                                    // current segment's start — plain positive
                                    // kerning never does.
                                    if n_val > space_threshold
                                        && !current_text.is_empty()
                                        && total_width_ts + displacement < sub_start_width_ts
                                    {
                                        backward_jump = true;
                                    }
                                    if !hidden && judged < -split_gap && !current_text.is_empty() {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                            std::mem::take(&mut current_estimate_ts),
                                            std::mem::take(&mut current_symbol_rewrite),
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                        sub_run_painted = false;
                                    } else {
                                        total_width_ts += displacement;
                                        if !hidden
                                            && judged < -word_gap
                                            && !current_text.is_empty()
                                            && !current_text.ends_with(' ')
                                        {
                                            current_text.push(' ');
                                        }
                                    }
                                    continue;
                                }
                                Object::Real(n) => {
                                    let n_val = *n;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    // The offset as the thresholds judge it:
                                    // itself, or on a return from a sign placed
                                    // behind the high-water mark only the
                                    // travel beyond the mark.
                                    let judged = pen_high_water.judge_offset(
                                        n_val,
                                        total_width_ts,
                                        total_width_ts + displacement,
                                        current_font_size,
                                    );
                                    // A true backtrack puts the pen behind the
                                    // current segment's start — plain positive
                                    // kerning never does.
                                    if n_val > space_threshold
                                        && !current_text.is_empty()
                                        && total_width_ts + displacement < sub_start_width_ts
                                    {
                                        backward_jump = true;
                                    }
                                    if !hidden && judged < -split_gap && !current_text.is_empty() {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                            std::mem::take(&mut current_estimate_ts),
                                            std::mem::take(&mut current_symbol_rewrite),
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                        sub_run_painted = false;
                                    } else {
                                        total_width_ts += displacement;
                                        if !hidden
                                            && judged < -word_gap
                                            && !current_text.is_empty()
                                            && !current_text.ends_with(' ')
                                        {
                                            current_text.push(' ');
                                        }
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                            if !sub_run_painted
                                && get_operand_bytes(element).is_some_and(|raw| !raw.is_empty())
                            {
                                sub_start_width_ts = total_width_ts;
                                sub_run_painted = true;
                                if let Some(pending) = pending_word_gaps.take() {
                                    pending.resolve(
                                        items,
                                        &advanced_tm(
                                            &text_matrix,
                                            total_width_ts,
                                            horizontal_scale,
                                        ),
                                        &ctm,
                                    );
                                }
                            }
                            if let Some(fi) = font_info {
                                if let Some(raw_bytes) = get_operand_bytes(element) {
                                    total_width_ts += compute_string_width_ts(
                                        raw_bytes,
                                        fi,
                                        current_font_size,
                                        char_spacing,
                                        word_spacing,
                                    );
                                }
                            } else {
                                // No width metrics: the cursor moves by the
                                // estimate the sub-run's box will carry.
                                let element_estimate_ts = estimated_string_advance_ts(
                                    get_operand_bytes(element),
                                    None,
                                    current_font_size
                                        * type3_scales.get(&current_font).copied().unwrap_or(1.0),
                                    char_spacing,
                                    word_spacing,
                                );
                                total_width_ts += element_estimate_ts;
                                current_estimate_ts += element_estimate_ts;
                            }
                            if let Some(raw) =
                                get_operand_bytes(element).filter(|raw| !raw.is_empty())
                            {
                                pen_high_water
                                    .painted(total_width_ts, is_dependent_sign(raw, font_info));
                            }
                            if !hidden {
                                if let Some((text, legacy_symbol_rewrite)) =
                                    extract_text_from_operand(
                                        element,
                                        &current_font,
                                        font_base_names.get(&current_font).map(|s| s.as_str()),
                                        font_cmaps,
                                        &font_tounicode_refs,
                                        &inline_cmaps,
                                        &font_encodings,
                                        &encoding_cache,
                                        cmap_decisions,
                                        &font_widths,
                                        &font_kinds,
                                    )
                                {
                                    // A short string with word-gap character
                                    // spacing shows its spaces once the spacing
                                    // after it is taken back: by the positive
                                    // offset that follows it here, or — for the
                                    // array's last string — by where the next
                                    // run starts (see `word_gaps`).
                                    let candidate = get_operand_bytes(element).and_then(|raw| {
                                        word_gap_candidate(
                                            raw,
                                            &text,
                                            font_info,
                                            current_font_size,
                                            char_spacing,
                                            word_spacing,
                                            space_threshold,
                                            |code| {
                                                cmap_decisions.without_coverage(|decisions| {
                                                    extract_text_from_operand(
                                                        code,
                                                        &current_font,
                                                        font_base_names
                                                            .get(&current_font)
                                                            .map(|s| s.as_str()),
                                                        font_cmaps,
                                                        &font_tounicode_refs,
                                                        &inline_cmaps,
                                                        &font_encodings,
                                                        &encoding_cache,
                                                        decisions,
                                                        &font_widths,
                                                        &font_kinds,
                                                    )
                                                })
                                            },
                                        )
                                    });
                                    let strings_follow =
                                        last_string_index.is_some_and(|last| index < last);
                                    let text = match (candidate, array.get(index + 1)) {
                                        (Some(candidate), Some(next))
                                            if offset_takes_spacing_back(
                                                next,
                                                candidate.trailing_spacing_ts,
                                                current_font_size,
                                            ) =>
                                        {
                                            candidate.spaced_text
                                        }
                                        (Some(candidate), _) if !strings_follow => {
                                            deferred_word_gaps =
                                                Some((current_text.clone(), candidate));
                                            text
                                        }
                                        _ => text,
                                    };
                                    current_text.push_str(&text);
                                    current_symbol_rewrite |= legacy_symbol_rewrite;
                                }
                            }
                        }
                        if !hidden && !current_text.trim().is_empty() {
                            sub_items.push((
                                current_text,
                                sub_start_width_ts,
                                total_width_ts,
                                current_estimate_ts,
                                current_symbol_rewrite,
                            ));
                        } else if !hidden && sub_items.is_empty() && !current_text.is_empty() {
                            // A whitespace-only array is a space run like a
                            // whitespace-only `Tj`: it may be the word space
                            // of the item before it.
                            let offset_tm =
                                advanced_tm(&text_matrix, sub_start_width_ts, horizontal_scale);
                            let combined =
                                multiply_matrices(&rise_adjusted(&offset_tm, text_rise), &ctm);
                            let rendered_size = effective_font_size(current_font_size, &combined)
                                * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                            let geometry = scaled_run_geometry(
                                &combined,
                                font_info.map(|_| total_width_ts - sub_start_width_ts),
                                current_estimate_ts,
                                rendered_size.copysign(current_font_size),
                                type3_y_flips.contains(&current_font),
                                horizontal_scale,
                            );
                            pending_space = PendingSpace::note(
                                pending_space.take(),
                                items,
                                &geometry,
                                page_num,
                            );
                        }
                        if !sub_items.is_empty() {
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let (dir_x, dir_y) =
                                reading_direction(&combined, current_font_size * horizontal_scale);
                            run_rotations.push(baseline_rotation(dir_x, dir_y));
                            let rendered_size = effective_font_size(current_font_size, &combined)
                                * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let style = font_styles.get(&current_font).copied().unwrap_or_default();
                            let scale_x = (text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2])
                                * horizontal_scale;
                            // Rotated matrices carry no horizontal evidence:
                            // stay neutral unless the advance is x-dominant.
                            let scale_y = (text_matrix[0] * ctm[1] + text_matrix[1] * ctm[3])
                                * horizontal_scale;
                            let horizontal_advance = scale_x.abs() > scale_y.abs();
                            // The op-wide backtrack marker votes once per op —
                            // per-sub-run geometry (mirrored matrices) still
                            // votes per sub-run, symmetric with candidates.
                            let mut op_backtrack_voted = false;
                            for (text, start_w, end_w, estimate_ts, legacy_symbol_rewrite) in
                                &sub_items
                            {
                                let offset_tm =
                                    advanced_tm(&text_matrix, *start_w, horizontal_scale);
                                let combined_mat =
                                    multiply_matrices(&rise_adjusted(&offset_tm, text_rise), &ctm);
                                let geometry = scaled_run_geometry(
                                    &combined_mat,
                                    font_info.map(|_| end_w - start_w),
                                    // A measured sub-run's advance is the `Some`
                                    // above and this fallback goes unused. Without
                                    // metrics the accumulated width IS the sub-run's
                                    // estimate, kerning included — signed, since a
                                    // negative `Tf` size reads backwards; if kerning
                                    // walked it past zero the painted codes' own
                                    // estimate stands.
                                    if font_info.is_some()
                                        || (end_w - start_w != 0.0
                                            && ((end_w - start_w > 0.0) == (*estimate_ts > 0.0)))
                                    {
                                        end_w - start_w
                                    } else if *estimate_ts != 0.0 {
                                        *estimate_ts
                                    } else {
                                        estimated_advance_ts(
                                            text,
                                            current_font_size
                                                * type3_scales
                                                    .get(&current_font)
                                                    .copied()
                                                    .unwrap_or(1.0),
                                        )
                                    },
                                    rendered_size.copysign(current_font_size),
                                    type3_y_flips.contains(&current_font),
                                    horizontal_scale,
                                );
                                if horizontal_advance
                                    && crate::text_utils::is_visual_rtl_candidate(text)
                                {
                                    if scale_x < 0.0 {
                                        rtl_logical_runs.push(items.len());
                                    } else if backward_jump {
                                        if !op_backtrack_voted {
                                            rtl_logical_runs.push(items.len());
                                            op_backtrack_voted = true;
                                        }
                                    } else {
                                        rtl_visual_candidates.push(items.len());
                                        if !hidden
                                            && crate::text_utils::render_mode_paints(
                                                text_rendering_mode,
                                            )
                                            && crate::text_utils::is_visual_rtl_run(text)
                                        {
                                            rtl_visual_runs.push(items.len());
                                        }
                                    }
                                }
                                if let Some(pending) = pending_space.take() {
                                    pending.resolve(items, &geometry, text, rendered_size);
                                }
                                let painted_bold = paintable_fonts.contains(&current_font)
                                    && text_paint.adds_bold(text, rendered_size, base_font, &ctm);
                                let paint = text_paint.run_paint();
                                items.push(TextItem {
                                    text: expand_ligatures(text),
                                    x: geometry.x,
                                    y: geometry.y,
                                    width: geometry.width,
                                    height: geometry.height,
                                    font: crate::extractor::fonts::item_font_name(
                                        &current_font,
                                        base_font,
                                    )
                                    .to_string(),
                                    font_tag: current_font.clone(),
                                    legacy_symbol_rewrite: *legacy_symbol_rewrite,
                                    font_size: rendered_size,
                                    page: page_num,
                                    is_bold: style.bold || painted_bold,
                                    is_italic: style.italic,
                                    font_weight: style.weight,
                                    bold_source: style
                                        .bold_source
                                        .or(painted_bold.then_some(BoldSource::Painted)),
                                    fixed_pitch: style.fixed_pitch,
                                    fill_color: paint.fill_color,
                                    stroke_color: paint.stroke_color,
                                    render_mode: Some(paint.render_mode),
                                    is_underline: false,
                                    is_strikeout: false,
                                    rotation: geometry.rotation,
                                    advance_known: geometry.advance_known,
                                    item_type: ItemType::Text,
                                    mcid: None,
                                    baseline_shift: 0.0,

                                    image_data: None,
                                    image_format: None,
                                });
                            }
                        }
                        // Always advance the text matrix by the total width —
                        // measured, or estimated for a font without metrics.
                        text_matrix[4] += total_width_ts * horizontal_scale * text_matrix[0];
                        text_matrix[5] += total_width_ts * horizontal_scale * text_matrix[1];
                        // The array's last string was a candidate: the next
                        // run decides, against the pen it left.
                        if let Some((prefix, candidate)) = deferred_word_gaps {
                            if !sub_items.is_empty() {
                                pending_word_gaps = Some(PendingWordGaps::new(
                                    items.len() - 1,
                                    expand_ligatures(&format!("{prefix}{}", candidate.spaced_text)),
                                    &text_matrix,
                                    &ctm,
                                    horizontal_scale,
                                    candidate.trailing_spacing_ts,
                                    current_font_size,
                                ));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Coverage no item of the form followed stays waiting for the page's
    // next item, or for the page's end (see `attach_run_coverage`).
    attach_run_coverage(item_coverage, items.len(), || {
        cmap_decisions.take_run_coverage()
    });

    extracted
}

/// Get fonts from a Form XObject's Resources
pub(crate) fn get_form_fonts<'a>(
    doc: &'a Document,
    form_dict: &'a lopdf::Dictionary,
) -> std::collections::BTreeMap<Vec<u8>, &'a lopdf::Dictionary> {
    let mut fonts = std::collections::BTreeMap::new();

    // Get Resources from Form dictionary
    let resources = if let Ok(res_ref) = form_dict.get(b"Resources") {
        if let Ok(obj_ref) = res_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            res_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(resources) = resources else {
        return fonts;
    };

    // Get Font dictionary
    let font_dict = if let Ok(font_ref) = resources.get(b"Font") {
        if let Ok(obj_ref) = font_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            font_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(font_dict) = font_dict else {
        return fonts;
    };

    // Collect fonts
    for (name, value) in font_dict.iter() {
        let dict = match value {
            Object::Reference(id) => doc.get_dictionary(*id).ok(),
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        };
        if let Some(dict) = dict {
            fonts.insert(name.clone(), dict);
        }
    }

    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::content_stream::extract_page_text_items;
    use lopdf::{dictionary, Dictionary, Stream};

    /// Build an acyclic Form XObject DAG: `levels` form objects, each non-leaf
    /// invoking the next form `branches` times. The leaf draws a single `(X)`.
    /// Returns `(doc, root_form_id)`.
    fn form_dag(branches: usize, levels: usize) -> (Document, ObjectId) {
        assert!(levels >= 2);
        let mut doc = Document::new();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let ids: Vec<ObjectId> = (0..levels).map(|_| doc.new_object_id()).collect();
        for level in 0..levels {
            let stream = if level + 1 == levels {
                Stream::new(
                    dictionary! {
                        "Type" => "XObject",
                        "Subtype" => "Form",
                        "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                        "Resources" => dictionary! {
                            "Font" => dictionary! {
                                "F1" => Object::Reference(font_id),
                            },
                        },
                    },
                    b"BT /F1 10 Tf 10 10 Td (X) Tj ET\n".to_vec(),
                )
            } else {
                let next_name = format!("Fm{}", level + 1);
                let content = format!("/{next_name} Do\n").repeat(branches);
                let mut xobjects = Dictionary::new();
                xobjects.set(next_name, Object::Reference(ids[level + 1]));
                let mut resources = Dictionary::new();
                resources.set("XObject", Object::Dictionary(xobjects));
                let mut dict = dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                };
                dict.set("Resources", Object::Dictionary(resources));
                Stream::new(dict, content.into_bytes())
            };
            doc.set_object(ids[level], Object::Stream(stream));
        }
        (doc, ids[0])
    }

    fn page_invoking_form(mut doc: Document, form_id: ObjectId) -> (Document, ObjectId) {
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            b"/Fm0 Do\n".to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "XObject" => dictionary! {
                    "Fm0" => Object::Reference(form_id),
                },
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

    fn extract_form(
        doc: &Document,
        form_id: ObjectId,
        budget: &mut FormWalkBudget,
    ) -> Vec<TextItem> {
        extract_form_xobject_text(
            doc,
            form_id,
            1,
            &FontCMaps::from_doc(doc),
            &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            false,
            0,
            0.0,
            1.0,
            TextPaint::default(),
            false,
            &mut CMapDecisionCache::new(),
            &mut FontStyleCache::new(),
            budget,
        )
        .items
    }

    #[test]
    fn nested_form_still_extracts_leaf_text() {
        let (doc, root) = form_dag(1, 3);
        let items = extract_form(&doc, root, &mut FormWalkBudget::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "X");
    }

    #[test]
    fn form_items_carry_family_name_and_resource_tag() {
        // Parity with content_stream.rs: `font` is the /BaseFont family
        // name, `font_tag` the raw resource tag, in both parsers.
        let (doc, root) = form_dag(1, 2);
        let items = extract_form(&doc, root, &mut FormWalkBudget::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].font, "Helvetica");
        assert_eq!(items[0].font_tag, "F1");
    }

    #[test]
    fn acyclic_form_dag_within_budget_keeps_all_leaves() {
        // 4 sibling invocations across 4 nested levels → 4^4 leaf drawings.
        // Default budgets are far above 256, so legitimate nesting is intact.
        let (doc, root) = form_dag(4, 5);
        let items = extract_form(&doc, root, &mut FormWalkBudget::new());
        assert_eq!(items.len(), 4usize.pow(4));
        assert!(items.iter().all(|item| item.text == "X"));
    }

    #[test]
    fn acyclic_form_dag_stops_at_invocation_budget() {
        // Same DAG as above would draw 256 leaves; a tiny invocation cap must
        // stop expansion rather than walking the full tree.
        let (doc, root) = form_dag(4, 5);
        let mut budget = FormWalkBudget::with_limits(20, MAX_FORM_XOBJECT_OPERATIONS);
        let items = extract_form(&doc, root, &mut budget);
        assert!(
            items.len() < 4usize.pow(4),
            "invocation budget must truncate DAG expansion; got {} items",
            items.len()
        );
        assert!(budget.was_truncated());
    }

    #[test]
    fn form_operations_stop_at_budget() {
        let mut doc = Document::new();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let mut content = b"q Q\n".repeat(50);
        content.extend_from_slice(b"BT /F1 10 Tf 10 10 Td (X) Tj ET\n");
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => Object::Reference(font_id),
                    },
                },
            },
            content,
        )));

        let mut budget = FormWalkBudget::with_limits(MAX_FORM_XOBJECT_INVOCATIONS, 10);
        let items = extract_form(&doc, form_id, &mut budget);
        assert!(
            items.is_empty(),
            "operation budget must stop before the trailing text show"
        );
        assert!(budget.was_truncated());
    }

    #[test]
    fn page_level_form_dag_stays_within_production_budget() {
        // A page-level `/Do` of an 8-wide, 6-level Form DAG would expand to
        // 8^5 = 32_768 leaf drawings without a budget. The production
        // invocation cap must keep extraction bounded.
        let (doc, root) = form_dag(8, 6);
        let (doc, page_id) = page_invoking_form(doc, root);

        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        assert!(
            items.len() <= MAX_FORM_XOBJECT_INVOCATIONS,
            "page-level Form expansion must stay within the invocation cap; got {}",
            items.len()
        );
        assert!(
            !items.is_empty(),
            "budget must still allow some nested form text through"
        );
    }

    #[test]
    fn shared_form_budget_spans_two_extraction_passes() {
        // The invisible-layer retry calls extract_page_text_items twice for
        // the same page; both passes must share one budget.
        let (doc, root) = form_dag(1, 2);
        let (doc, page_id) = page_invoking_form(doc, root);
        let font_cmaps = FontCMaps::from_doc(&doc);
        // Root + leaf = 2 invocations on the first pass.
        let mut budget = FormWalkBudget::with_limits(2, MAX_FORM_XOBJECT_OPERATIONS);
        let ((first, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut budget,
        )
        .unwrap();
        assert_eq!(first.iter().filter(|item| item.text == "X").count(), 1);
        assert!(!budget.was_truncated());

        let ((second, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            true,
            &mut FontStyleCache::new(),
            &mut budget,
        )
        .unwrap();
        assert!(
            second.iter().all(|item| item.text != "X"),
            "second pass must not get a fresh invocation budget"
        );
        assert!(budget.was_truncated());
    }

    /// Build a document whose page draws *all* of its content through a single
    /// Form XObject — the shape emitted by print-to-PDF producers like PDFlib,
    /// where the page stream itself is only `q /X1 Do Q`.
    fn doc_with_form_content(form_content: &[u8]) -> (Document, ObjectId) {
        doc_with_page_and_forms(b"q /X1 Do Q", &[form_content])
    }

    /// A page drawing `page_content` with forms `X1`, `X2`, … available to
    /// the page and to each other (so a form can invoke a nested form).
    fn doc_with_page_and_forms(page_content: &[u8], forms: &[&[u8]]) -> (Document, ObjectId) {
        let mut doc = Document::new();
        let widths: Vec<Object> = (0..=255).map(|_| 600.into()).collect();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        });
        let form_ids: Vec<ObjectId> = forms.iter().map(|_| doc.new_object_id()).collect();
        let xobjects = || {
            let mut dict = lopdf::Dictionary::new();
            for (index, id) in form_ids.iter().enumerate() {
                dict.set(format!("X{}", index + 1), Object::Reference(*id));
            }
            dict
        };
        for (id, content) in form_ids.iter().zip(forms) {
            doc.set_object(
                *id,
                Object::Stream(Stream::new(
                    dictionary! {
                        "Type" => "XObject",
                        "Subtype" => "Form",
                        "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                        "Resources" => dictionary! {
                            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                            "XObject" => xobjects(),
                        },
                    },
                    content.to_vec(),
                )),
            );
        }
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            page_content.to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                "XObject" => xobjects(),
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

    fn form_items(form_content: &[u8]) -> Vec<TextItem> {
        let (doc, page_id) = doc_with_form_content(form_content);
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        items
    }

    fn find<'a>(items: &'a [TextItem], text: &str) -> &'a TextItem {
        items
            .iter()
            .find(|item| item.text == text)
            .unwrap_or_else(|| {
                let found: Vec<&String> = items.iter().map(|i| &i.text).collect();
                panic!("no item {text:?} in {found:?}")
            })
    }

    /// A form whose content is over the page-content bound is skipped, as
    /// a page over it is; the forms invoked beside it are still read.
    #[test]
    fn form_content_over_the_byte_bound_is_skipped() {
        let mut oversized = b"BT /F1 12 Tf 72 700 Td (lost) Tj ET".to_vec();
        oversized.resize(
            crate::extractor::content_decode::MAX_PAGE_CONTENT_BYTES + 1,
            b' ',
        );
        let (doc, page_id) = doc_with_page_and_forms(
            b"q /X1 Do Q q /X2 Do Q",
            &[&oversized, b"BT /F1 12 Tf 72 600 Td (kept) Tj ET"],
        );
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["kept"]);
    }

    #[test]
    fn t_star_inside_form_moves_to_next_line() {
        // T* was previously unhandled inside Form XObjects, so every line after
        // the first piled onto the preceding baseline and drifted right.
        let items =
            form_items(b"BT /F1 12 Tf 12 TL 1 0 0 1 100 700 Tm (first) Tj T* (second) Tj ET");

        let first = find(&items, "first");
        let second = find(&items, "second");
        assert!((first.y - 700.0).abs() < 0.1, "first y = {}", first.y);
        assert!((second.y - 688.0).abs() < 0.1, "second y = {}", second.y);
        assert!((second.x - 100.0).abs() < 0.1, "second x = {}", second.x);
    }

    #[test]
    fn td_inside_form_is_relative_to_line_start_not_shown_text() {
        // Td moves relative to the text *line* matrix. Applying it to the
        // matrix already advanced by Tj marched each line off the right edge.
        let items = form_items(b"BT /F1 12 Tf 1 0 0 1 100 700 Tm (AAAAA) Tj 0 -12 Td (B) Tj ET");

        let b = find(&items, "B");
        assert!((b.x - 100.0).abs() < 0.1, "B x = {} (expected 100)", b.x);
        assert!((b.y - 688.0).abs() < 0.1, "B y = {}", b.y);
    }

    #[test]
    fn td_inside_form_sets_leading_for_later_t_star() {
        // `TD` sets the leading to -ty as a side effect; a following T* must
        // reuse it.
        let items = form_items(
            b"BT /F1 12 Tf 1 0 0 1 100 700 Tm (one) Tj 0 -15 TD (two) Tj T* (three) Tj ET",
        );

        assert!((find(&items, "two").y - 685.0).abs() < 0.1);
        let three = find(&items, "three");
        assert!((three.y - 670.0).abs() < 0.1, "three y = {}", three.y);
        assert!((three.x - 100.0).abs() < 0.1, "three x = {}", three.x);
    }

    #[test]
    fn quote_operator_inside_form_moves_to_next_line() {
        let items = form_items(b"BT /F1 12 Tf 12 TL 1 0 0 1 100 700 Tm (first) Tj (second) ' ET");

        let second = find(&items, "second");
        assert!((second.y - 688.0).abs() < 0.1, "second y = {}", second.y);
        assert!((second.x - 100.0).abs() < 0.1, "second x = {}", second.x);
    }

    #[test]
    fn double_quote_operator_inside_form_sets_spacing_and_moves() {
        // `aw ac (string) "` — set word spacing and char spacing, then T* and show.
        let items =
            form_items(b"BT /F1 12 Tf 12 TL 1 0 0 1 100 700 Tm (first) Tj 0 0 (second) \" ET");

        let second = find(&items, "second");
        assert!((second.y - 688.0).abs() < 0.1, "second y = {}", second.y);
        assert!((second.x - 100.0).abs() < 0.1, "second x = {}", second.x);
    }

    #[test]
    fn char_spacing_inside_form_widens_advance() {
        // Tc was hardcoded to 0 in the form parser, so advance widths drifted.
        // 2 glyphs x 600/1000 x 12pt = 14.4, plus 2 x Tc(2.0) = 18.4.
        let items = form_items(b"BT /F1 12 Tf 1 0 0 1 100 700 Tm 2 Tc (AB) Tj ET");

        let ab = find(&items, "AB");
        assert!((ab.width - 18.4).abs() < 0.1, "AB width = {}", ab.width);
    }

    #[test]
    fn q_restores_fill_colour_inside_form() {
        // A white fill set inside q/Q must not leak past the Q — otherwise the
        // following black text is treated as invisible and dropped entirely.
        let items = form_items(
            b"BT /F1 12 Tf 12 TL 1 0 0 1 100 700 Tm q 1 g (hidden) Tj Q T* (visible) Tj ET",
        );

        assert!(
            items.iter().any(|item| item.text == "visible"),
            "text after Q was dropped: {:?}",
            items.iter().map(|i| &i.text).collect::<Vec<_>>()
        );
        assert!(
            !items.iter().any(|item| item.text == "hidden"),
            "white-filled text should still be suppressed"
        );
    }

    #[test]
    fn q_restores_text_state_inside_form() {
        // Tc/TL live in the graphics state; `Q` must roll them back.
        let items =
            form_items(b"BT /F1 12 Tf 12 TL 1 0 0 1 100 700 Tm q 30 TL (a) Tj Q T* (b) Tj ET");

        let b = find(&items, "b");
        assert!(
            (b.y - 688.0).abs() < 0.1,
            "b y = {} (leading should restore to 12)",
            b.y
        );
    }

    #[test]
    fn form_only_rotated_page_is_turned_like_page_stream_text() {
        // The whole page is one Form XObject whose runs are all 90°: the
        // form runs must vote, so the page is re-based exactly as if the
        // runs had been shown by the page stream itself.
        let (doc, page_id) = doc_with_form_content(
            b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (WORLD) Tj ET",
        );
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, page_rotation, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        assert_eq!(page_rotation, crate::extractor::geometry::PageRotation::Ccw);
        let hello = find(&items, "HELLO");
        assert_eq!(hello.rotation, 0.0);
        assert!((hello.x - 100.0).abs() < 0.01, "x = {}", hello.x);
        assert!((hello.y + 200.0).abs() < 0.01, "y = {}", hello.y);
        assert!((hello.width - 36.0).abs() < 0.01, "width = {}", hello.width);
    }

    #[test]
    fn form_only_page_with_a_lone_split_tj_stays_upright() {
        // One rotated TJ that splits at a 6em gap yields two items but is a
        // single show operator: a lone stamp, not a rotated page.
        let (doc, page_id) =
            doc_with_form_content(b"BT /F1 10 Tf 0 1 -1 0 40 100 Tm [(AB) -6000 (CD)] TJ ET");
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, page_rotation, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(
            page_rotation,
            crate::extractor::geometry::PageRotation::Upright
        );
        assert!(items.iter().all(|i| (i.rotation - 90.0).abs() < 1e-3));
    }

    #[test]
    fn invisible_form_text_is_skipped_and_does_not_vote() {
        // An OCR layer drawn inside a form with `3 Tr`: two rotated hidden
        // runs next to one visible upright caption. The hidden runs must
        // neither appear nor turn the page, and the page must report that
        // it skipped them so the `include_invisible` retry recovers them,
        // exactly as for page-stream text.
        let content = b"BT /F1 12 Tf 72 700 Td (Caption) Tj ET
BT 3 Tr /F1 12 Tf 0 1 -1 0 200 100 Tm (HIDDEN) Tj ET
BT 3 Tr /F1 12 Tf 0 1 -1 0 240 100 Tm [(ALSO) -3000 (HIDDEN)] TJ ET";
        let (doc, page_id) = doc_with_form_content(content);
        let font_cmaps = FontCMaps::from_doc(&doc);
        let extract = |include_invisible: bool| {
            extract_page_text_items(
                &doc,
                page_id,
                1,
                &font_cmaps,
                include_invisible,
                &mut FontStyleCache::new(),
                &mut FormWalkBudget::new(),
            )
            .unwrap()
        };

        let ((items, _, _), _, page_rotation, skipped_invisible) = extract(false);
        assert_eq!(
            page_rotation,
            crate::extractor::geometry::PageRotation::Upright
        );
        assert!(skipped_invisible);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Caption"]);

        let ((items, _, _), _, page_rotation, _) = extract(true);
        assert!(items.iter().any(|i| i.text == "HIDDEN"), "{items:?}");
        assert!(
            items.iter().any(|i| i.text.starts_with("ALSO")),
            "{items:?}"
        );
        // Two rotated operators against one upright: the recovered layer
        // now turns the page like any other rotated text.
        assert_eq!(page_rotation, crate::extractor::geometry::PageRotation::Ccw);
    }

    fn extract_page(
        doc: &Document,
        page_id: ObjectId,
        include_invisible: bool,
    ) -> (Vec<TextItem>, bool) {
        let font_cmaps = FontCMaps::from_doc(doc);
        let ((items, _, _), _, _, skipped_invisible) = extract_page_text_items(
            doc,
            page_id,
            1,
            &font_cmaps,
            include_invisible,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        (items, skipped_invisible)
    }

    #[test]
    fn form_inherits_the_pages_text_rendering_mode() {
        // `3 Tr` set by the page stream before `Do`: the form's text is an
        // OCR-style hidden layer and must stay hidden on the visible pass.
        let (doc, page_id) = doc_with_page_and_forms(
            b"BT 3 Tr ET q /X1 Do Q",
            &[b"BT /F1 12 Tf 72 700 Td (Hidden) Tj ET"],
        );
        let (items, skipped_invisible) = extract_page(&doc, page_id, false);
        assert!(items.is_empty(), "{items:?}");
        assert!(skipped_invisible);
        let (items, _) = extract_page(&doc, page_id, true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Hidden");
    }

    #[test]
    fn form_inherits_fill_stroke_weight_and_restores_it() {
        let (doc, page_id) = doc_with_page_and_forms(
            b"0.3 w 2 Tr BT ET /X1 Do 0 Tr /X2 Do",
            &[
                b"BT /F1 12 Tf 72 700 Td (Lead) Tj ET q 0 Tr BT /F1 12 Tf 72 680 Td (Plain) Tj ET Q BT /F1 12 Tf 72 660 Td (Restored) Tj ET",
                b"BT /F1 12 Tf 72 640 Td (Body) Tj ET",
            ],
        );
        let (items, _) = extract_page(&doc, page_id, false);
        let styles: Vec<_> = items.iter().map(|i| (i.text.as_str(), i.is_bold)).collect();
        assert_eq!(
            styles,
            [
                ("Lead", true),
                ("Plain", false),
                ("Restored", true),
                ("Body", false)
            ]
        );
    }

    /// `(fill_color, stroke_color, render_mode)` of the item reading `text`.
    fn paint_of(items: &[TextItem], text: &str) -> (Option<[u8; 3]>, Option<[u8; 3]>, Option<u8>) {
        let item = find(items, text);
        (item.fill_color, item.stroke_color, item.render_mode)
    }

    #[test]
    fn form_text_reports_the_paint_it_inherits_and_restores_it() {
        // The form starts with the paint in force where the page invokes it;
        // what it sets inside its own `q`/`Q` ends there, a nested form
        // starts with the outer form's paint, and nothing leaks back onto
        // the page when the form returns.
        let (doc, page_id) = doc_with_page_and_forms(
            b"1 0 0 rg 0 0 1 RG 1 Tr q /X1 Do Q BT /F1 12 Tf 72 500 Td (Page) Tj ET",
            &[
                b"BT /F1 12 Tf 72 700 Td (Inherited) Tj ET
                  q 0 1 0 rg 0 Tr BT /F1 12 Tf 72 680 Td (Own) Tj ET /X2 Do Q
                  BT /F1 12 Tf 72 660 Td (Restored) Tj ET",
                b"BT /F1 12 Tf 72 640 Td [(Nes) (ted)] TJ ET",
            ],
        );
        let (items, _) = extract_page(&doc, page_id, false);
        let red = Some([255, 0, 0]);
        let green = Some([0, 255, 0]);
        let blue = Some([0, 0, 255]);
        assert_eq!(paint_of(&items, "Inherited"), (red, blue, Some(1)));
        assert_eq!(paint_of(&items, "Own"), (green, blue, Some(0)));
        assert_eq!(paint_of(&items, "Nested"), (green, blue, Some(0)));
        assert_eq!(paint_of(&items, "Restored"), (red, blue, Some(1)));
        assert_eq!(paint_of(&items, "Page"), (red, blue, Some(1)));
    }

    #[test]
    fn form_reads_a_palette_colour_in_the_space_it_inherits() {
        // The page selects a palette from its own resources and a colour in
        // it; the form, whose resources know no such space, starts with that
        // colour and can pick another entry of the inherited palette.
        let (mut doc, page_id) = doc_with_page_and_forms(
            b"/Pal cs 1 sc /X1 Do",
            &[b"BT /F1 12 Tf 72 700 Td (First) Tj ET 2 sc BT /F1 12 Tf 72 680 Td (Second) Tj ET"],
        );
        let palette = Object::Array(vec![
            Object::Name(b"Indexed".to_vec()),
            Object::Name(b"DeviceRGB".to_vec()),
            2.into(),
            Object::String(
                vec![0, 0, 0, 255, 0, 0, 0, 0, 255],
                lopdf::StringFormat::Hexadecimal,
            ),
        ]);
        doc.get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .get_mut(b"Resources")
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("ColorSpace", dictionary! { "Pal" => palette });
        let (items, _) = extract_page(&doc, page_id, false);
        assert_eq!(paint_of(&items, "First").0, Some([255, 0, 0]));
        assert_eq!(paint_of(&items, "Second").0, Some([0, 0, 255]));
    }

    #[test]
    fn form_items_of_every_show_operator_report_their_paint() {
        let items = form_items(
            b"1 0 0 rg 2 Tr BT /F1 12 Tf 12 TL 72 700 Td (Shown) Tj (Quoted) ' 1 0 (Spaced) \" T* [(Ar) (ray)] TJ ET",
        );
        for text in ["Shown", "Quoted", "Spaced", "Array"] {
            assert_eq!(
                paint_of(&items, text),
                (Some([255, 0, 0]), Some([0, 0, 0]), Some(2)),
                "{text}"
            );
        }
    }

    #[test]
    fn form_graphics_states_are_scoped_and_restored() {
        let (mut doc, page_id) = doc_with_page_and_forms(
            b"/State gs 0.3 w 2 Tr /X1 Do /X2 Do",
            &[
                b"/State gs BT /F1 12 Tf 72 700 Td (Plain) Tj ET",
                b"/State gs BT /F1 12 Tf 72 680 Td (Lead) Tj ET
                  q /Missing gs BT /F1 12 Tf 72 660 Td (Unknown) Tj ET Q
                  BT /F1 12 Tf 72 640 Td (Restored) Tj ET",
            ],
        );
        doc.get_dictionary_mut(page_id)
            .unwrap()
            .get_mut(b"Resources")
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "ExtGState",
                dictionary! { "State" => dictionary! { "SM" => 0.02 } },
            );
        for (id, state) in [
            ((2, 0), dictionary! { "ca" => 0.5 }),
            ((3, 0), dictionary! { "OPM" => 1 }),
        ] {
            doc.get_object_mut(id)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .dict
                .get_mut(b"Resources")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("ExtGState", dictionary! { "State" => state });
        }
        let (items, _) = extract_page(&doc, page_id, false);
        let styles: Vec<_> = items.iter().map(|i| (i.text.as_str(), i.is_bold)).collect();
        assert_eq!(
            styles,
            [
                ("Plain", false),
                ("Lead", true),
                ("Unknown", false),
                ("Restored", true)
            ]
        );
    }

    #[test]
    fn nested_form_resolves_indirect_paint_resources_without_leaking_state() {
        let (mut doc, page_id) = doc_with_page_and_forms(
            b"0.3 w 2 Tr /X1 Do",
            &[
                b"/Tone cs 0.2 0.3 0.4 sc /Tone CS 0.2 0.3 0.4 SC /State gs
                  BT /F1 12 Tf 72 700 Td (Outer) Tj ET /X2 Do
                  0.2 0.3 0.4 rg BT /F1 12 Tf 72 660 Td (Restored) Tj ET",
                b"/Tone cs 0 sc /Tone CS 0 SC /State gs
                  BT /F1 12 Tf 72 680 Td (Inner) Tj ET",
            ],
        );
        for (id, color_space, state) in [
            ((2, 0), "DeviceRGB", dictionary! { "SM" => 0.02 }),
            ((3, 0), "DeviceGray", dictionary! { "OPM" => 1 }),
        ] {
            let mut resources = doc
                .get_object(id)
                .unwrap()
                .as_stream()
                .unwrap()
                .dict
                .get(b"Resources")
                .unwrap()
                .as_dict()
                .unwrap()
                .clone();
            let space_id = doc.add_object(Object::Name(color_space.as_bytes().to_vec()));
            let state_id = doc.add_object(state);
            let spaces_id = doc.add_object(dictionary! { "Tone" => Object::Reference(space_id) });
            let states_id = doc.add_object(dictionary! { "State" => Object::Reference(state_id) });
            resources.set("ColorSpace", Object::Reference(spaces_id));
            resources.set("ExtGState", Object::Reference(states_id));
            let resources_id = doc.add_object(resources);
            doc.get_object_mut(id)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .dict
                .set("Resources", Object::Reference(resources_id));
        }
        let (items, _) = extract_page(&doc, page_id, false);
        assert_eq!(
            items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Outer", "Inner", "Restored"]
        );
        assert!(items.iter().all(|i| i.is_bold));
    }

    #[test]
    fn form_fill_stroke_covers_all_show_operators() {
        for show in [
            "(Styled) Tj",
            "[(Sty) (led)] TJ",
            "(Styled) '",
            "0 0 (Styled) \"",
        ] {
            let content = format!("BT /F1 12 Tf 72 700 Td {show} ET");
            let (doc, page_id) =
                doc_with_page_and_forms(b"0.3 w 2 Tr /X1 Do", &[content.as_bytes()]);
            let (items, _) = extract_page(&doc, page_id, false);
            assert_eq!(items.len(), 1, "{show}: {items:?}");
            assert_eq!(items[0].text, "Styled");
            assert!(items[0].is_bold, "{show}: {items:?}");
        }
    }

    #[test]
    fn unresolved_form_font_does_not_gain_painted_bold() {
        let items = form_items(b"0.3 w 2 Tr BT /Missing 12 Tf 72 700 Td (Alpha) Tj ET");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Alpha");
        assert!(!items[0].is_bold);
    }

    #[test]
    fn inline_form_fonts_match_referenced_style_and_geometry() {
        for (subtype, name, mode, text, bold, italic) in [
            ("Type1", "Helvetica", 2, "Alpha", true, false),
            ("Type1", "Helvetica-BoldOblique", 0, "Alpha", true, true),
            ("Type1", "Wingdings", 2, "A", false, false),
            ("Type3", "Shape", 2, "A", false, false),
        ] {
            let content = format!("0.3 w {mode} Tr BT /F1 12 Tf 72 700 Td ({text}) Tj ET");
            let (mut referenced_doc, page_id) =
                doc_with_page_and_forms(b"/X1 Do", &[content.as_bytes()]);
            let glyph = referenced_doc.add_object(Stream::new(
                dictionary! {},
                b"600 0 0 0 600 700 d1 0 0 600 700 re f".to_vec(),
            ));
            let font = referenced_doc.get_dictionary_mut((1, 0)).unwrap();
            font.set("Subtype", Object::Name(subtype.as_bytes().to_vec()));
            font.set("BaseFont", Object::Name(name.as_bytes().to_vec()));
            if subtype == "Type3" {
                font.set(
                    "FontMatrix",
                    vec![
                        0.001.into(),
                        0.into(),
                        0.into(),
                        0.001.into(),
                        0.into(),
                        0.into(),
                    ],
                );
                font.set("FontBBox", vec![0.into(), 0.into(), 600.into(), 700.into()]);
                font.set("CharProcs", dictionary! { "A" => Object::Reference(glyph) });
                font.set(
                    "Encoding",
                    dictionary! { "Differences" => vec![65.into(), Object::Name(b"A".to_vec())] },
                );
            }
            let direct_font = font.clone();
            let mut inline_doc = referenced_doc.clone();
            inline_doc
                .get_object_mut((2, 0))
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .dict
                .get_mut(b"Resources")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .get_mut(b"Font")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("F1", direct_font);
            let (referenced_items, _) = extract_page(&referenced_doc, page_id, false);
            let (inline_items, _) = extract_page(&inline_doc, page_id, false);
            assert_eq!(referenced_items.len(), 1, "{name}");
            assert_eq!(inline_items.len(), 1, "{name}");
            let expected = &referenced_items[0];
            let actual = &inline_items[0];
            assert_eq!(actual.text, text, "{name}");
            assert_eq!((actual.is_bold, actual.is_italic), (bold, italic), "{name}");
            assert_eq!(
                (
                    &actual.font,
                    actual.font_size,
                    actual.width,
                    actual.height,
                    actual.x,
                    actual.y,
                    actual.is_bold,
                    actual.is_italic,
                    actual.advance_known
                ),
                (
                    &expected.font,
                    expected.font_size,
                    expected.width,
                    expected.height,
                    expected.x,
                    expected.y,
                    expected.is_bold,
                    expected.is_italic,
                    expected.advance_known
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn nested_form_inherits_the_outer_forms_text_rendering_mode() {
        // The outer form sets `3 Tr` and invokes the inner form, whose text
        // must stay hidden; an outer run at the default mode stays visible.
        let (doc, page_id) = doc_with_page_and_forms(
            b"q /X1 Do Q",
            &[
                b"BT /F1 12 Tf 72 700 Td (Visible) Tj ET BT 3 Tr ET q /X2 Do Q",
                b"BT /F1 12 Tf 72 650 Td (Hidden) Tj ET",
            ],
        );
        let (items, skipped_invisible) = extract_page(&doc, page_id, false);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Visible"]);
        assert!(skipped_invisible);
        let (items, _) = extract_page(&doc, page_id, true);
        assert!(items.iter().any(|i| i.text == "Hidden"), "{items:?}");
    }

    #[test]
    fn form_inherits_the_pages_text_rise() {
        // `5 Ts` set by the page stream before `Do`: text state is graphics
        // state, so the form's first run is raised until the form itself
        // resets the rise.
        let (doc, page_id) = doc_with_page_and_forms(
            b"BT 5 Ts ET q /X1 Do Q",
            &[b"BT /F1 12 Tf 1 0 0 1 100 500 Tm (raised) Tj 0 Ts (base) Tj ET"],
        );
        let (items, _) = extract_page(&doc, page_id, false);
        let raised = find(&items, "raised");
        let base = find(&items, "base");
        assert!((raised.y - 505.0).abs() < 0.1, "raised y = {}", raised.y);
        assert!((base.y - 500.0).abs() < 0.1, "base y = {}", base.y);
    }

    #[test]
    fn nested_form_inherits_the_outer_forms_text_rise() {
        // The outer form raises the baseline and invokes the inner form; the
        // inner run is raised, the outer's own run at rise 0 is not.
        let (doc, page_id) = doc_with_page_and_forms(
            b"q /X1 Do Q",
            &[
                b"BT /F1 12 Tf 1 0 0 1 100 500 Tm (outer) Tj ET BT 5 Ts ET q /X2 Do Q",
                b"BT /F1 12 Tf 1 0 0 1 100 400 Tm (inner) Tj ET",
            ],
        );
        let (items, _) = extract_page(&doc, page_id, false);
        let outer = find(&items, "outer");
        let inner = find(&items, "inner");
        assert!((outer.y - 500.0).abs() < 0.1, "outer y = {}", outer.y);
        assert!((inner.y - 405.0).abs() < 0.1, "inner y = {}", inner.y);
    }

    #[test]
    fn form_tj_at_a_negative_size_votes_with_its_own_items() {
        // A form's vertical TJ runs at `-12 Tf` read top-to-bottom: their
        // page-rotation votes must say so, like the items they produce, or
        // the page would be turned against them.
        let (doc, page_id) = doc_with_page_and_forms(
            b"q /X1 Do Q",
            &[b"BT /F1 -12 Tf 0 1 -1 0 100 100 Tm [(UP)] TJ ET BT /F1 -12 Tf 0 1 -1 0 130 100 Tm [(UP)] TJ ET"],
        );
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, page_rotation, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        assert_eq!(page_rotation, crate::extractor::geometry::PageRotation::Cw);
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(items.iter().all(|i| i.rotation == 0.0), "{items:?}");
    }

    #[test]
    fn text_rise_inside_form_shifts_the_baseline() {
        // Ts displaces the glyph origin without touching the advance, in a
        // form exactly as in the page stream; the next run at rise 0 returns
        // to the original baseline and follows the raised run horizontally.
        let items = form_items(
            b"BT /F1 12 Tf 1 0 0 1 100 500 Tm (base) Tj 5 Ts (super) Tj 0 Ts (after) Tj ET",
        );
        let base = find(&items, "base");
        let raised = find(&items, "super");
        let after = find(&items, "after");
        assert!((base.y - 500.0).abs() < 0.1, "base y = {}", base.y);
        assert!((raised.y - 505.0).abs() < 0.1, "raised y = {}", raised.y);
        assert!((after.y - 500.0).abs() < 0.1, "after y = {}", after.y);
        assert!(after.x > raised.x);
    }

    #[test]
    fn rotated_run_inside_form_gets_tall_thin_box() {
        // Same contract as the page-level parser: a 20pt stamp reading
        // bottom-to-top gets its em as width and its advance as height, for
        // both Tj and TJ.
        let items = form_items(
            b"BT /F1 12 Tf 72 700 Td (Body line one) Tj ET
BT /F1 12 Tf 72 686 Td (Body line two) Tj ET
BT /F1 12 Tf 72 672 Td (Body line three) Tj ET
BT /F1 20 Tf 0 1 -1 0 32 200 Tm (arXiv:2301.00001) Tj ET
BT /F1 10 Tf 0 1 -1 0 60 200 Tm [(ABCD)] TJ ET",
        );
        let stamp = find(&items, "arXiv:2301.00001");
        assert!(
            (stamp.rotation - 90.0).abs() < 1e-3,
            "rotation = {}",
            stamp.rotation
        );
        assert!((stamp.x - 12.0).abs() < 0.01, "x = {}", stamp.x);
        assert!((stamp.y - 200.0).abs() < 0.01, "y = {}", stamp.y);
        assert!((stamp.width - 20.0).abs() < 0.01, "width = {}", stamp.width);
        assert!(
            (stamp.height - 192.0).abs() < 0.01,
            "height = {}",
            stamp.height
        );

        let tj = find(&items, "ABCD");
        assert!(
            (tj.rotation - 90.0).abs() < 1e-3,
            "rotation = {}",
            tj.rotation
        );
        assert!((tj.x - 50.0).abs() < 0.01, "x = {}", tj.x);
        assert!((tj.width - 10.0).abs() < 0.01, "width = {}", tj.width);
        assert!((tj.y - 200.0).abs() < 0.01, "y = {}", tj.y);
        assert!((tj.height - 24.0).abs() < 0.01, "height = {}", tj.height);

        let body = find(&items, "Body line one");
        assert_eq!(body.rotation, 0.0);
        assert!(
            (body.width - 13.0 * 7.2).abs() < 0.01,
            "width = {}",
            body.width
        );
        assert_eq!(body.height, 12.0);
    }

    /// The form parser positions `TJ` sub-runs like the page parser: pen
    /// travel ahead of the first glyph moves the box, not just the pen.
    #[test]
    fn tj_positioning_ahead_of_the_first_glyph_inside_form_moves_the_box() {
        // -5400 at 10pt carries the pen 54pt from x=100 to 154, flush against
        // "Intr" (130..154), so the merge pass rejoins the word.
        let items = form_items(
            b"BT /F1 10 Tf 1 0 0 1 130 700 Tm (Intr) Tj 1 0 0 1 100 700 Tm [-5400 (oduction)] TJ ET",
        );
        let word = find(&items, "Introduction");
        assert!((word.x - 130.0).abs() < 0.05, "{items:?}");
        assert!((word.width - 72.0).abs() < 0.05, "{items:?}");

        // A squeezed space run positioned the same way is still the word
        // space of the item before it.
        let items = form_items(
            b"BT /F1 12 Tf 72 700 Td (for) Tj -6 Tc 1 0 0 1 60 700 Tm [-2800 ( )] TJ 0 Tc 1 0 0 1 94.8 700 Tm (the) Tj ET",
        );
        find(&items, "for the");
    }

    /// Word gaps carried by character spacing inside a form read as word
    /// spaces once the spacing is taken back, for every show operator —
    /// `"` sets the spacing itself; spacing that stays is tracking.
    #[test]
    fn character_spacing_word_gaps_inside_form_read_as_word_spaces() {
        for (content, expected) in [
            (
                "BT /F1 10 Tf 72 700 Td (sen) Tj 3 Tc 18 0 Td [(dt) 300 (oM)] TJ 0 Tc 30 0 Td (ars) Tj ET",
                "send to Mars",
            ),
            (
                "BT /F1 10 Tf 72 700 Td (sen) Tj 3 Tc 18 0 Td (dt) Tj 0 Tc 15 0 Td (o) Tj ET",
                "send to",
            ),
            ("BT /F1 10 Tf 12 TL 72 712 Td 3 Tc (dt) ' 0 Tc 15 0 Td (o) Tj ET", "d to"),
            ("BT /F1 10 Tf 12 TL 72 712 Td 0 3 (dt) \" 0 Tc 15 0 Td (o) Tj ET", "d to"),
            ("BT /F1 10 Tf 72 700 Td 3 Tc [(dt) 20] TJ 0 Tc 14.8 0 Td (o) Tj ET", "d to"),
            ("BT /F1 10 Tf 72 700 Td 3 Tc (dt) Tj 18 0 Td (o) Tj ET", "dto"),
            ("BT /F1 10 Tf 72 700 Td 3 Tc (HEADING) Tj ET", "HEADING"),
        ] {
            let items = form_items(content.as_bytes());
            let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
            assert_eq!(texts, [expected], "{content}: {items:?}");
        }
    }

    /// An XObject painted between a candidate and the run that would have
    /// taken its spacing back belongs to another stream: the candidate is
    /// dropped, on the page and inside a form alike.
    #[test]
    fn an_xobject_in_between_drops_the_word_gap_candidate() {
        let (doc, page_id) = doc_with_page_and_forms(
            b"BT /F1 10 Tf 72 700 Td 3 Tc (dt) Tj q /X1 Do Q 0 Tc 15 0 Td (o) Tj ET q /X2 Do Q",
            &[
                b"",
                b"BT /F1 10 Tf 72 600 Td 3 Tc (dt) Tj q /X1 Do Q 0 Tc 15 0 Td (o) Tj ET",
            ],
        );
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["dto", "dto"], "{items:?}");
    }

    /// Tracked display text set as a glyph-per-string `TJ` array inside a
    /// form is judged over its own tracking, as on the page; positioning
    /// between whole words reads as before. The form font's space is 0.6
    /// em, so its word-gap threshold is 240 thousandths.
    #[test]
    fn tracked_tj_title_inside_form_stays_one_word() {
        for (content, expected) in [
            (
                "BT /F1 24 Tf 72 700 Td [(V) -216 (A) -333 (L) -166 (L) -250 (E) -290 (Y)] TJ ET",
                "VALLEY",
            ),
            (
                "BT /F1 24 Tf 72 700 Td [(V) -250 (A) -250 (L) -250 (L) -250 (E) -250 (Y) -560 (R) -250 (O) -250 (A) -250 (D)] TJ ET",
                "VALLEY ROAD",
            ),
            (
                "BT /F1 12 Tf 72 700 Td [(The) -258 (quick) -300 (brown)] TJ ET",
                "The quick brown",
            ),
        ] {
            let items = form_items(content.as_bytes());
            let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
            assert_eq!(texts.join(" "), expected, "{content}: {items:?}");
        }
    }

    /// Items of a page whose only content is `q /X1 Do Q`, the form showing
    /// `form_content` through `F1`, the zero-advance-sign font of
    /// `content_stream::add_zero_advance_sign_font`.
    fn form_items_with_zero_advance_signs(form_content: &[u8]) -> Vec<TextItem> {
        let mut doc = Document::new();
        let font_id = crate::extractor::content_stream::add_zero_advance_sign_font(&mut doc);
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                },
            },
            form_content.to_vec(),
        )));
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            b"q /X1 Do Q".to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "XObject" => dictionary! { "X1" => Object::Reference(form_id) },
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
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        items
    }

    #[test]
    fn form_tj_returns_from_signs_placed_behind_the_pen_open_no_word_gap() {
        // As on the page: a zero-advance sign placed 0.223 em back over
        // the glyph before it, the pen returned 0.221 em, is no word gap;
        // a forward offset from the pen's farthest point still is.
        use crate::extractor::content_stream::SIGNED_WORD;

        let items = form_items_with_zero_advance_signs(
            b"BT /F1 14 Tf 20 700 Td [<0001> 223 <0002> -221 <0003> <0004> 246 <0005> -221 <0006>] TJ ET",
        );
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, [SIGNED_WORD]);
        assert!(
            (items[0].x - 20.0).abs() < 0.01 && (items[0].width - 29.4).abs() < 0.01,
            "{:?}",
            items[0]
        );

        let items = form_items_with_zero_advance_signs(
            b"BT /F1 14 Tf 20 700 Td [<0001> 223 <0002> -621 <0003>] TJ ET",
        );
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["\u{1789}\u{17D2}\u{1789} \u{179C}"]);

        // Under `0.5 Tc` the sign is a sign by its glyph, though the
        // spacing moves the pen for it.
        let items = form_items_with_zero_advance_signs(
            b"BT /F1 14 Tf 0.5 Tc 20 700 Td [<0001> 223 <0002> -221 <0003> <0004> 246 <0005> -221 <0006>] TJ ET",
        );
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, [SIGNED_WORD]);

        // Twenty zero-advance glyphs behind the pen are hidden text, not a
        // sign: the return after them is the word gap it reads as.
        let hidden = "0002".repeat(20);
        let items = form_items_with_zero_advance_signs(
            format!("BT /F1 14 Tf 20 700 Td [<0003> 300 <{hidden}> -300 <0004>] TJ ET").as_bytes(),
        );
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        let expected = format!("\u{179C}{} \u{178F}\u{17D2}", "\u{1789}".repeat(20));
        assert_eq!(texts, [expected.as_str()]);
    }

    #[test]
    fn test_upstage_image_extraction() {
        let doc = Document::load("tests/fixtures/upstage_key_functions.pdf").unwrap();
        let page_id = doc.get_pages().get(&1).copied().unwrap();
        let xobjects = get_page_xobjects(&doc, page_id);
        assert!(!xobjects.is_empty());
        for (_name, xobj) in &xobjects {
            if let XObjectType::Image(id) = xobj {
                let img = extract_image_by_id(&doc, *id).unwrap();
                assert!(img.is_some());
            }
        }
    }

    #[test]
    fn test_indexed_color_image_extraction() {
        let doc = Document::with_version("1.7");
        let palette = vec![
            255, 0, 0, // index 0: Red
            0, 255, 0, // index 1: Green
            0, 0, 255, // index 2: Blue
            255, 255, 255, // index 3: White
        ];
        let cs_array = vec![
            Object::Name(b"Indexed".to_vec()),
            Object::Name(b"DeviceRGB".to_vec()),
            Object::Integer(3),
            Object::String(palette, lopdf::StringFormat::Literal),
        ];
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", Object::Name(b"XObject".to_vec()));
        dict.set(b"Subtype", Object::Name(b"Image".to_vec()));
        dict.set(b"Width", Object::Integer(2));
        dict.set(b"Height", Object::Integer(2));
        dict.set(b"BitsPerComponent", Object::Integer(8));
        dict.set(b"ColorSpace", Object::Array(cs_array));

        // 2x2 image with indices 0, 1, 2, 3
        let content = vec![0, 1, 2, 3];
        let stream = lopdf::Stream::new(dict, content);
        let extracted = extract_image_from_stream(&doc, &stream).unwrap();
        assert!(extracted.is_some());
        let (data, fmt) = extracted.unwrap();
        assert_eq!(fmt, "png");
        assert!(data.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn test_monochrome_1bit_image_extraction() {
        let doc = Document::with_version("1.7");
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", Object::Name(b"XObject".to_vec()));
        dict.set(b"Subtype", Object::Name(b"Image".to_vec()));
        dict.set(b"Width", Object::Integer(8));
        dict.set(b"Height", Object::Integer(1));
        dict.set(b"BitsPerComponent", Object::Integer(1));
        dict.set(b"ColorSpace", Object::Name(b"DeviceGray".to_vec()));

        let content = vec![0b10101010];
        let stream = lopdf::Stream::new(dict, content);
        let extracted = extract_image_from_stream(&doc, &stream).unwrap();
        assert!(extracted.is_some());
        let (data, fmt) = extracted.unwrap();
        assert_eq!(fmt, "png");
        assert!(data.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn test_jbig2_image_extraction() {
        let doc = Document::with_version("1.7");
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", Object::Name(b"XObject".to_vec()));
        dict.set(b"Subtype", Object::Name(b"Image".to_vec()));
        dict.set(b"Width", Object::Integer(8));
        dict.set(b"Height", Object::Integer(1));
        dict.set(b"ColorSpace", Object::Name(b"DeviceGray".to_vec()));
        dict.set(b"Filter", Object::Name(b"JBIG2Decode".to_vec()));

        // Minimal valid embedded JBIG2 stream:
        // Segment 0: Page Information (19 bytes data, width=8, height=1)
        // Segment 1: End of Page (0 bytes data)
        let mut content = Vec::new();
        // Seg 0 header: seg_num=0 (4B), flags=0x30 (Page Info, 1B), no refs (1B), page_assoc=1 (1B), len=19 (4B)
        content.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x30, 0x00, 0x01, 0x00, 0x00, 0x00, 0x13,
        ]);
        // Seg 0 data: width=8 (4B), height=1 (4B), xres=0 (4B), yres=0 (4B), flags=0 (1B), striping=0 (2B)
        content.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        // Seg 1 header: seg_num=1 (4B), flags=0x31 (End of Page, 1B), no refs (1B), page_assoc=1 (1B), len=0 (4B)
        content.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x01, 0x31, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ]);

        let stream = lopdf::Stream::new(dict, content);
        let extracted = extract_image_from_stream(&doc, &stream).unwrap();
        assert!(
            extracted.is_some(),
            "JBIG2 stream should decode successfully"
        );
        let (data, fmt) = extracted.unwrap();
        assert_eq!(fmt, "png");
        assert!(data.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
