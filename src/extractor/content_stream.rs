//! PDF content-stream operator state machine.
//!
//! Walks the page's content stream, tracking the graphics state and text
//! matrix, and emits `TextItem`s and `PdfRect`s.

use crate::text_utils::{
    decode_text_string, effective_font_size, expand_ligatures, is_bold_font, is_italic_font,
};
use crate::tounicode::FontCMaps;
use crate::types::{FontWidthInfo, ItemType, PageExtraction, PdfLine, PdfRect, TextItem};
use crate::PdfError;
use log::trace;
use lopdf::{Document, Encoding, Object, ObjectId};
use std::collections::HashMap;

use super::fonts::{
    build_font_encodings, build_font_widths, build_type3_scales, build_type3_y_flips,
    compute_string_width_ts, descriptor_style_flags, extract_text_from_operand,
    get_font_file2_obj_num, get_operand_bytes, CMapDecisionCache, FontStyleCache,
};
use super::geometry::{
    estimated_advance_for_glyphs, estimated_advance_ts, normalize_degrees, reading_direction,
    rise_adjusted, run_geometry, PageRotation,
};
use super::underline::UnderlineLine;
use super::xobjects::{extract_form_xobject_text, get_page_xobjects, FormWalkBudget, XObjectType};
use super::{get_number, image_bbox_from_ctm, multiply_matrices};

/// Strip PDF comments (% to end of line) from content stream bytes.
///
/// Some PDF generators (e.g. PD4ML) embed comments in content streams that
/// confuse lopdf's `Content::decode` parser.  Comments inside string literals
/// (parentheses) are NOT stripped — only top-level comments.
fn strip_pdf_comments(data: &[u8]) -> Vec<u8> {
    // Quick check: if no '%' present, return as-is (common case)
    if !data.contains(&b'%') {
        return data.to_vec();
    }

    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;
    let mut in_string = 0i32; // parenthesis nesting depth
    let mut in_hex_string = false;

    while i < data.len() {
        let b = data[i];
        match b {
            // Inside a string literal, a backslash escapes the next byte —
            // `\(`, `\)`, and `\\` must not touch the nesting depth, or a
            // later `%` glyph inside a string gets stripped as a comment,
            // corrupting the stream.
            b'\\' if in_string > 0 => {
                result.push(b);
                if let Some(&next) = data.get(i + 1) {
                    result.push(next);
                    i += 1;
                }
            }
            b'(' if !in_hex_string => {
                in_string += 1;
                result.push(b);
            }
            b')' if !in_hex_string && in_string > 0 => {
                in_string -= 1;
                result.push(b);
            }
            b'<' if in_string == 0 && !in_hex_string => {
                in_hex_string = true;
                result.push(b);
            }
            b'>' if in_hex_string => {
                in_hex_string = false;
                result.push(b);
            }
            b'%' if in_string == 0 && !in_hex_string => {
                // Skip until end of line
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
                // Replace comment with a space to preserve token separation
                result.push(b' ');
                continue; // Don't increment i again
            }
            _ => {
                result.push(b);
            }
        }
        i += 1;
    }

    result
}

fn transform_path_point(x: f32, y: f32, ctm: &[f32; 6]) -> (f32, f32) {
    (
        x * ctm[0] + y * ctm[2] + ctm[4],
        x * ctm[1] + y * ctm[3] + ctm[5],
    )
}

fn transformed_stroke_width(
    line_width: f32,
    ctm: &[f32; 6],
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) -> f32 {
    let user_width = line_width.abs();
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return user_width;
    }

    // PDF stroke width scales perpendicular to the path direction.
    let nx = -dy / len;
    let ny = dx / len;
    let ndx = nx * ctm[0] + ny * ctm[2];
    let ndy = nx * ctm[1] + ny * ctm[3];
    user_width * (ndx * ndx + ndy * ndy).sqrt()
}

/// Number of glyphs a show operand paints: one per code, two bytes per code
/// for CID fonts. Sizes the box of an ActualText span whose font carries no
/// width metrics — the replacement string's length says nothing about what
/// was painted.
pub(crate) fn shown_glyph_count(raw: Option<&[u8]>, font: Option<&FontWidthInfo>) -> usize {
    let code_size = if font.is_some_and(|f| f.is_cid) { 2 } else { 1 };
    raw.map_or(0, |bytes| bytes.len().div_ceil(code_size))
}

/// Advance estimate, in unscaled text-space units, for a string shown with a
/// font that carries no width metrics: half an em per painted code, plus the
/// character spacing per code and the word spacing per single-byte space
/// (code 32), the way the width formula applies them. `em_ts` is the `Tf`
/// size times the Type3 scale where one applies.
pub(crate) fn estimated_string_advance_ts(
    raw: Option<&[u8]>,
    font: Option<&FontWidthInfo>,
    em_ts: f32,
    char_spacing: f32,
    word_spacing: f32,
) -> f32 {
    let Some(bytes) = raw else {
        return 0.0;
    };
    let glyphs = shown_glyph_count(raw, font);
    let spaces = if font.is_some_and(|f| f.is_cid) {
        0
    } else {
        bytes.iter().filter(|&&b| b == b' ').count()
    };
    estimated_advance_for_glyphs(glyphs, em_ts)
        + glyphs as f32 * char_spacing
        + spaces as f32 * word_spacing
}

/// Returns `(page_extraction, has_gid_fonts, page_rotation, skipped_invisible)`
/// where `has_gid_fonts` indicates the page uses fonts with unresolvable
/// gid-encoded glyphs, `page_rotation` says whether (and which way) the
/// coordinate frame was turned so predominantly rotated text reads along +x
/// — region boxes must follow it (see `PageRotation`) — and
/// `skipped_invisible` reports that invisible (Tr 3) text was present but
/// suppressed — callers can use it to decide whether an `include_invisible`
/// retry could recover anything at all.
pub(crate) fn extract_page_text_items(
    doc: &Document,
    page_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    include_invisible: bool,
    style_cache: &mut FontStyleCache,
    form_budget: &mut FormWalkBudget,
) -> Result<(PageExtraction, bool, PageRotation, bool), PdfError> {
    let mut items = Vec::new();
    let mut rects: Vec<PdfRect> = Vec::new();
    let mut clip_rects: Vec<PdfRect> = Vec::new();
    let mut lines: Vec<PdfLine> = Vec::new();
    let mut underline_lines: Vec<UnderlineLine> = Vec::new();
    // Indexes of items whose raw decoded text is a multi-character RTL run
    // that may be stored in visual order (see fix_visual_order_rtl), plus a
    // count of show ops whose glyph progression walked right-to-left —
    // evidence of logical-order storage.
    let mut rtl_visual_candidates: Vec<usize> = Vec::new();
    let mut rtl_logical_ops: u32 = 0;

    // Path construction state for m/l/h → S/s line extraction
    let mut path_subpath_start: Option<(f32, f32)> = None;
    let mut path_current: Option<(f32, f32)> = None;
    let mut pending_lines: Vec<(f32, f32, f32, f32)> = Vec::new();
    // Completed subpaths (each a vec of line segments) for f/f* rect extraction
    let mut pending_subpaths: Vec<Vec<(f32, f32, f32, f32)>> = Vec::new();
    let mut fill_rects: Vec<PdfRect> = Vec::new();
    // `re` rects awaiting a paint operator. Underline detection must only
    // see painted rects: a `re W n` clip path or `re n` no-op draws nothing
    // on the page, so treating every `re` as ink would underline text that
    // merely sits near an invisible clip boundary.
    let mut pending_re_rects: Vec<PdfRect> = Vec::new();
    let mut painted_rects: Vec<PdfRect> = Vec::new();

    // Get fonts for encoding
    let fonts = doc.get_page_fonts(page_id).unwrap_or_default();

    // Build font encoding maps from Differences arrays
    let (font_encodings, has_gid_fonts) = build_font_encodings(doc, &fonts, font_cmaps);

    // Build font width info for accurate text positioning
    let font_widths = build_font_widths(doc, &fonts);
    let type3_scales = build_type3_scales(doc, &fonts);
    let type3_y_flips = build_type3_y_flips(doc, &fonts);

    // Build maps of font resource names to their base font names and ToUnicode object refs
    let mut font_base_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut font_tounicode_refs: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut inline_cmaps: std::collections::HashMap<String, crate::tounicode::CMapEntry> =
        std::collections::HashMap::new();
    let mut font_style_flags: std::collections::HashMap<String, (bool, bool)> =
        std::collections::HashMap::new();
    for (font_name, font_dict) in &fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        // Descriptor style flags rescue subset fonts whose BaseFont names
        // are opaque tags the name heuristics can't read.
        let style = descriptor_style_flags(doc, font_dict, style_cache);
        if style != (false, false) {
            font_style_flags.insert(resource_name.clone(), style);
        }
        // Track ToUnicode object reference, with FontFile2 fallback for Identity-H/V.
        // Also handle inline ToUnicode streams.
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

    // Cache font encodings from lopdf (once per font, not per text operand).
    // This avoids re-parsing ToUnicode CMap streams for every Tj/TJ operator.
    let mut encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
    for (font_name, font_dict) in &fonts {
        let name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(enc) = font_dict.get_font_encoding(doc) {
            encoding_cache.insert(name, enc);
        }
    }

    let mut cmap_decisions = CMapDecisionCache::new();

    // Get XObjects (images) from page resources
    let xobjects = get_page_xobjects(doc, page_id);

    // Get content, bounding decompression so a page-content bomb (a tiny
    // Flate stream inflating to gigabytes) skips the page instead of
    // exhausting memory — same degradation as the operator cap below. Real
    // page content runs a few MB at most; the bound is deliberately far
    // above that.
    const MAX_PAGE_CONTENT_BYTES: usize = 64 * 1024 * 1024;
    let content_data = match doc.get_page_content_with_limit(page_id, MAX_PAGE_CONTENT_BYTES) {
        Ok(data) => data,
        Err(e) => {
            log::warn!(
                "page {}: skipping extraction — content stream exceeds {} decompressed bytes: {}",
                page_num,
                MAX_PAGE_CONTENT_BYTES,
                e
            );
            return Ok((
                (Vec::new(), Vec::new(), Vec::new()),
                false,
                PageRotation::Upright,
                false,
            ));
        }
    };

    // Strip PDF comments (% to end of line) from the content stream.
    // Some PDF generators (e.g. PD4ML) embed comments that confuse lopdf's
    // Content::decode parser, causing it to skip operators like ET and Q.
    let content_data = strip_pdf_comments(&content_data);

    let content = match super::content_decode::decode_content_bounded(
        &content_data,
        super::content_decode::MAX_PAGE_OPERATIONS,
    )? {
        Some(content) => content,
        None => {
            log::warn!(
                "page {}: skipping extraction — content stream exceeds {} operations",
                page_num,
                super::content_decode::MAX_PAGE_OPERATIONS
            );
            return Ok((
                (Vec::new(), Vec::new(), Vec::new()),
                false,
                PageRotation::Upright,
                false,
            ));
        }
    };

    // Graphics state tracking
    let mut ctm = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0]; // Current Transformation Matrix
    let mut text_rendering_mode: i32 = 0; // 0=fill, 1=stroke, 2=fill+stroke, 3=invisible
                                          // Invisible (Tr 3) text was present but suppressed — reported to callers
                                          // so an include_invisible retry is attempted only when it can recover.
    let mut skipped_invisible = false;
    let mut line_width: f32 = 1.0;
    #[derive(Clone)]
    struct SavedGraphicsState {
        ctm: [f32; 6],
        text_rendering_mode: i32,
        line_width: f32,
        char_spacing: f32,
        word_spacing: f32,
        text_rise: f32,
        text_leading: f32,
        current_font: String,
        current_font_size: f32,
    }
    let mut gstate_stack: Vec<SavedGraphicsState> = Vec::new();

    // Text state tracking
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_leading: f32 = 0.0; // TL parameter (in text-space units)
    let mut char_spacing: f32 = 0.0; // Tc parameter (extra spacing per character, unscaled)
    let mut word_spacing: f32 = 0.0; // Tw parameter (extra spacing per space char, unscaled)
    let mut text_rise: f32 = 0.0; // Ts parameter (baseline shift for super/subscripts, unscaled)
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut line_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text_block = false;

    // Track text direction votes. For each shown run, if |combined[0]| >=
    // |combined[1]| the text runs horizontally (normal); otherwise it is
    // rotated ~90°, one way or the other (see `RotationVotes::cast`).
    let mut rotation_votes = RotationVotes::default();

    // Marked content tracking: (ActualText, MCID) per nesting level
    struct MarkedContentEntry {
        actual_text: Option<String>,
        mcid: Option<i64>,
    }
    let mut marked_content_stack: Vec<MarkedContentEntry> = Vec::new();
    let mut suppress_glyph_extraction = false;
    let mut actual_text_start_tm: Option<[f32; 6]> = None; // text matrix at BDC entry
    let mut actual_text_glyph_tm: Option<[f32; 6]> = None; // text matrix at first glyph inside BDC
                                                           // Text rise in effect at each captured matrix — the item must render at
                                                           // the rise of its GLYPHS, not whatever rise is set by EMC time.
    let mut actual_text_start_rise: f32 = 0.0;
    let mut actual_text_glyph_rise: Option<f32> = None;
    let mut actual_text_glyph_font: Option<String> = None; // font that painted the span's first glyph
    let mut actual_text_glyph_font_size: Option<f32> = None; // `Tf` size in force for that glyph, sign included
    let mut actual_text_glyphs_measured: bool = true; // every painted font had width metrics
    let mut actual_text_estimate_ts: f32 = 0.0; // estimate accumulated per painted run, its own size and spacing
                                                // Glyphs painted inside the current ActualText span: sizes the span's box
                                                // when its font has no width metrics.
    let mut actual_text_glyph_count: usize = 0;
    /// Get the innermost MCID from the marked content stack.
    fn current_mcid(stack: &[MarkedContentEntry]) -> Option<i64> {
        stack.iter().rev().find_map(|e| e.mcid)
    }

    for op in &content.operations {
        trace!("{} {:?}", op.operator, op.operands);
        match op.operator.as_str() {
            "q" => {
                // Save graphics state
                gstate_stack.push(SavedGraphicsState {
                    ctm,
                    text_rendering_mode,
                    line_width,
                    char_spacing,
                    word_spacing,
                    text_rise,
                    text_leading,
                    current_font: current_font.clone(),
                    current_font_size,
                });
            }
            "Q" => {
                // Restore graphics state
                if let Some(saved) = gstate_stack.pop() {
                    ctm = saved.ctm;
                    text_rendering_mode = saved.text_rendering_mode;
                    line_width = saved.line_width;
                    char_spacing = saved.char_spacing;
                    word_spacing = saved.word_spacing;
                    text_rise = saved.text_rise;
                    text_leading = saved.text_leading;
                    current_font = saved.current_font;
                    current_font_size = saved.current_font_size;
                }
            }
            "cm" => {
                // Concatenate matrix to CTM
                if op.operands.len() >= 6 {
                    let new_matrix = [
                        get_number(&op.operands[0]).unwrap_or(1.0),
                        get_number(&op.operands[1]).unwrap_or(0.0),
                        get_number(&op.operands[2]).unwrap_or(0.0),
                        get_number(&op.operands[3]).unwrap_or(1.0),
                        get_number(&op.operands[4]).unwrap_or(0.0),
                        get_number(&op.operands[5]).unwrap_or(0.0),
                    ];
                    ctm = multiply_matrices(&new_matrix, &ctm);
                }
            }
            "w" => {
                if let Some(width) = op.operands.first().and_then(get_number) {
                    line_width = width;
                }
            }
            "BT" => {
                // Begin text block
                in_text_block = true;
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                line_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                text_rendering_mode = 0;
            }
            "ET" => {
                // End text block
                in_text_block = false;
            }
            "Tf" => {
                // Set font and size
                if op.operands.len() >= 2 {
                    if let Ok(name) = op.operands[0].as_name() {
                        current_font = String::from_utf8_lossy(name).to_string();
                    }
                    if let Ok(size) = op.operands[1].as_f32() {
                        current_font_size = size;
                    } else if let Ok(size) = op.operands[1].as_i64() {
                        current_font_size = size as f32;
                    }
                }
            }
            "TL" => {
                // Set text leading (used by T*, ', and " operators)
                if let Some(tl) = op.operands.first().and_then(get_number) {
                    text_leading = tl;
                }
            }
            "Tr" => {
                // Set text rendering mode (3 = invisible / OCR overlay)
                if let Some(mode) = op.operands.first().and_then(get_number) {
                    text_rendering_mode = mode as i32;
                }
            }
            "Tc" => {
                // Set character spacing (extra space added after each character)
                if let Some(tc) = op.operands.first().and_then(get_number) {
                    char_spacing = tc;
                }
            }
            "Tw" => {
                // Set word spacing (extra space added for each space character)
                if let Some(tw) = op.operands.first().and_then(get_number) {
                    word_spacing = tw;
                }
            }
            "Ts" => {
                // Set text rise (baseline shift for superscripts/subscripts)
                if let Some(ts) = op.operands.first().and_then(get_number) {
                    text_rise = ts;
                }
            }
            "Td" | "TD" => {
                // Move text position: TLM = T(tx,ty) × TLM; Tm = TLM
                // tx,ty are in text space — must be scaled by the text line matrix
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
                // Set text matrix
                if op.operands.len() >= 6 {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        text_matrix[i] =
                            get_number(operand).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                    }
                    line_matrix = text_matrix;
                }
            }
            "T*" => {
                // Move to start of next line: equivalent to 0 -TL Td
                let tl = if text_leading != 0.0 {
                    text_leading
                } else {
                    current_font_size * 1.2
                };
                line_matrix[4] += (-tl) * line_matrix[2]; // Usually 0 for non-rotated text
                line_matrix[5] += (-tl) * line_matrix[3];
                text_matrix = line_matrix;
            }
            "Tj" => {
                // Show text string
                if in_text_block && !op.operands.is_empty() {
                    // Advance text matrix regardless of visibility
                    let w_ts_opt = font_widths.get(&current_font).and_then(|fi| {
                        get_operand_bytes(&op.operands[0]).map(|raw| {
                            compute_string_width_ts(
                                raw,
                                fi,
                                current_font_size,
                                char_spacing,
                                word_spacing,
                            )
                        })
                    });
                    let glyph_count = shown_glyph_count(
                        get_operand_bytes(&op.operands[0]),
                        font_widths.get(&current_font),
                    );
                    let em_ts =
                        current_font_size * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                    // Without width metrics the cursor moves by the same
                    // estimate the run's box carries, so following runs do
                    // not pile up on one origin.
                    let estimate_ts = estimated_string_advance_ts(
                        get_operand_bytes(&op.operands[0]),
                        font_widths.get(&current_font),
                        em_ts,
                        char_spacing,
                        word_spacing,
                    );
                    // ActualText: suppress glyph extraction, just advance text matrix.
                    // Capture the FIRST glyph's text matrix as the rendering position
                    // for the ActualText item. Td ops between BDC and the first Tj
                    // may have moved the position to the correct line — the BDC-entry
                    // position (actual_text_start_tm) can be on the previous line.
                    if suppress_glyph_extraction {
                        // The first *painted* glyph decides the span's position
                        // and state; an empty show is not it.
                        if actual_text_glyph_tm.is_none() && glyph_count > 0 {
                            actual_text_glyph_tm = Some(text_matrix);
                            actual_text_glyph_rise = Some(text_rise);
                            actual_text_glyph_font = Some(current_font.clone());
                            actual_text_glyph_font_size = Some(current_font_size);
                        }
                        actual_text_glyph_count += glyph_count;
                        actual_text_glyphs_measured &= w_ts_opt.is_some();
                        actual_text_estimate_ts += estimate_ts;
                        let cursor_ts = w_ts_opt.unwrap_or(estimate_ts);
                        text_matrix[4] += cursor_ts * text_matrix[0];
                        text_matrix[5] += cursor_ts * text_matrix[1];
                        continue;
                    }
                    // Skip invisible (Tr=3) text but still advance text matrix.
                    // For Mixed/template PDFs, include_invisible=true extracts
                    // the OCR text layer that sits behind scanned images.
                    if text_rendering_mode == 3 && !include_invisible {
                        if op
                            .operands
                            .first()
                            .and_then(get_operand_bytes)
                            .is_some_and(|raw| !raw.is_empty())
                        {
                            skipped_invisible = true;
                        }
                        let cursor_ts = w_ts_opt.unwrap_or(estimate_ts);
                        text_matrix[4] += cursor_ts * text_matrix[0];
                        text_matrix[5] += cursor_ts * text_matrix[1];
                        continue;
                    }
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        &current_font,
                        font_base_names.get(&current_font).map(|s| s.as_str()),
                        font_cmaps,
                        &font_tounicode_refs,
                        &inline_cmaps,
                        &font_encodings,
                        &encoding_cache,
                        &mut cmap_decisions,
                        &font_widths,
                    ) {
                        let combined =
                            multiply_matrices(&rise_adjusted(&text_matrix, text_rise), &ctm);
                        let rendered_size = effective_font_size(current_font_size, &combined)
                            * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                        let geometry = run_geometry(
                            &combined,
                            w_ts_opt,
                            if glyph_count > 0 {
                                estimate_ts
                            } else {
                                estimated_advance_ts(&text, em_ts)
                            },
                            rendered_size.copysign(current_font_size),
                            type3_y_flips.contains(&current_font),
                        );
                        let cursor_ts = w_ts_opt.unwrap_or(estimate_ts);
                        text_matrix[4] += cursor_ts * text_matrix[0];
                        text_matrix[5] += cursor_ts * text_matrix[1];
                        // Only create text item for non-whitespace; whitespace
                        // still advances the text matrix above so gap detection works
                        if !text.trim().is_empty() {
                            rotation_votes
                                .cast_direction(reading_direction(&combined, current_font_size));
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            if crate::text_utils::is_visual_rtl_candidate(&text) {
                                // combined[0] is the device-space advance
                                // direction: forward paint order means the
                                // string may be stored in visual order, a
                                // mirrored matrix already paints right-to-left
                                // (logical storage). Rotated matrices carry no
                                // horizontal evidence and stay neutral — same
                                // dominance test as the rotation votes above.
                                if combined[0].abs() > combined[1].abs() {
                                    if combined[0] > 0.0 {
                                        rtl_visual_candidates.push(items.len());
                                    } else {
                                        rtl_logical_ops += 1;
                                    }
                                }
                            }
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
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font) || desc_bold,
                                is_italic: is_italic_font(base_font) || desc_italic,
                                is_underline: false,
                                is_strikeout: false,
                                rotation: geometry.rotation,
                                advance_known: geometry.advance_known,
                                item_type: ItemType::Text,
                                mcid: current_mcid(&marked_content_stack),
                                baseline_shift: 0.0,
                            });
                        }
                    }
                }
            }
            "TJ" => {
                // Show text with positioning — split at column-sized gaps
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        let font_info = font_widths.get(&current_font);
                        // Numeric-only TJ arrays (pure kerning) show no
                        // text — they must not trigger the invisible retry.
                        if text_rendering_mode == 3
                            && !include_invisible
                            && array
                                .iter()
                                .any(|el| get_operand_bytes(el).is_some_and(|raw| !raw.is_empty()))
                        {
                            skipped_invisible = true;
                        }
                        let is_invisible = (text_rendering_mode == 3 && !include_invisible)
                            || suppress_glyph_extraction;
                        // Capture first-glyph position for ActualText
                        let paints_a_glyph = op
                            .operands
                            .first()
                            .and_then(|o| o.as_array().ok())
                            .is_some_and(|elements| {
                                elements.iter().any(|e| {
                                    get_operand_bytes(e).is_some_and(|raw| !raw.is_empty())
                                })
                            });
                        if suppress_glyph_extraction
                            && actual_text_glyph_tm.is_none()
                            && paints_a_glyph
                        {
                            actual_text_glyph_tm = Some(text_matrix);
                            actual_text_glyph_rise = Some(text_rise);
                            actual_text_glyph_font = Some(current_font.clone());
                            actual_text_glyph_font_size = Some(current_font_size);
                        }

                        // Compute space threshold based on font metrics when available
                        let space_threshold = if let Some(font_info) = font_info {
                            let space_em = font_info.space_width as f32 * font_info.units_scale;
                            let threshold = space_em * 1000.0 * 0.4;
                            threshold.max(80.0)
                        } else {
                            120.0
                        };
                        let column_gap_threshold = space_threshold * 4.0;

                        // Track sub-items for column-gap splitting:
                        // (text, start_width_ts, end_width_ts)
                        let mut sub_items: Vec<(String, f32, f32, f32)> = Vec::new();
                        let mut current_text = String::new();
                        let mut current_estimate_ts: f32 = 0.0; // metric-less estimate of `current_text`
                        let mut sub_start_width_ts: f32 = 0.0;
                        let mut total_width_ts: f32 = 0.0;
                        // Positive TJ offsets beyond a space width move the pen
                        // backward past painted glyphs — logical-order RTL
                        // producers position runs right-to-left this way.
                        let mut backward_jump = false;
                        for element in array {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    // A true backtrack puts the pen behind the
                                    // current segment's start — plain positive
                                    // kerning never does.
                                    if n_val > space_threshold
                                        && !current_text.is_empty()
                                        && total_width_ts + displacement < sub_start_width_ts
                                    {
                                        backward_jump = true;
                                    }
                                    if !is_invisible
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        // Column gap: flush current segment
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                            std::mem::take(&mut current_estimate_ts),
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !is_invisible
                                            && n_val < -space_threshold
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
                                    // A true backtrack puts the pen behind the
                                    // current segment's start — plain positive
                                    // kerning never does.
                                    if n_val > space_threshold
                                        && !current_text.is_empty()
                                        && total_width_ts + displacement < sub_start_width_ts
                                    {
                                        backward_jump = true;
                                    }
                                    if !is_invisible
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                            std::mem::take(&mut current_estimate_ts),
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !is_invisible
                                            && n_val < -space_threshold
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
                            let element_glyphs =
                                shown_glyph_count(get_operand_bytes(element), font_info);
                            // The estimate this element would carry without
                            // metrics: its own size, scale, and spacing.
                            let element_estimate_ts = estimated_string_advance_ts(
                                get_operand_bytes(element),
                                font_info,
                                current_font_size
                                    * type3_scales.get(&current_font).copied().unwrap_or(1.0),
                                char_spacing,
                                word_spacing,
                            );
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
                                total_width_ts += element_estimate_ts;
                                current_estimate_ts += element_estimate_ts;
                            }
                            if suppress_glyph_extraction {
                                actual_text_glyph_count += element_glyphs;
                                actual_text_glyphs_measured &= font_info.is_some();
                                actual_text_estimate_ts += element_estimate_ts;
                            }
                            if !is_invisible {
                                if let Some(text) = extract_text_from_operand(
                                    element,
                                    &current_font,
                                    font_base_names.get(&current_font).map(|s| s.as_str()),
                                    font_cmaps,
                                    &font_tounicode_refs,
                                    &inline_cmaps,
                                    &font_encodings,
                                    &encoding_cache,
                                    &mut cmap_decisions,
                                    &font_widths,
                                ) {
                                    current_text.push_str(&text);
                                }
                            }
                        }
                        // Flush remaining text
                        if !is_invisible && !current_text.trim().is_empty() {
                            sub_items.push((
                                current_text,
                                sub_start_width_ts,
                                total_width_ts,
                                current_estimate_ts,
                            ));
                        }
                        // Emit one TextItem per sub-item
                        if !sub_items.is_empty() {
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            rotation_votes
                                .cast_direction(reading_direction(&combined, current_font_size));
                            let rendered_size = effective_font_size(current_font_size, &combined)
                                * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            let scale_x = text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2];
                            // Rotated matrices carry no horizontal evidence:
                            // stay neutral unless the advance is x-dominant.
                            let scale_y = text_matrix[0] * ctm[1] + text_matrix[1] * ctm[3];
                            let horizontal_advance = scale_x.abs() > scale_y.abs();
                            // The op-wide backtrack marker votes once per op —
                            // per-sub-run geometry (mirrored matrices) still
                            // votes per sub-run, symmetric with candidates.
                            let mut op_backtrack_voted = false;
                            for (text, start_w, end_w, estimate_ts) in &sub_items {
                                let offset_tm = [
                                    text_matrix[0],
                                    text_matrix[1],
                                    text_matrix[2],
                                    text_matrix[3],
                                    text_matrix[4] + start_w * text_matrix[0],
                                    text_matrix[5] + start_w * text_matrix[1],
                                ];
                                let combined =
                                    multiply_matrices(&rise_adjusted(&offset_tm, text_rise), &ctm);
                                let geometry = run_geometry(
                                    &combined,
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
                                );
                                if horizontal_advance
                                    && crate::text_utils::is_visual_rtl_candidate(text)
                                {
                                    if scale_x < 0.0 {
                                        rtl_logical_ops += 1;
                                    } else if backward_jump {
                                        if !op_backtrack_voted {
                                            rtl_logical_ops += 1;
                                            op_backtrack_voted = true;
                                        }
                                    } else {
                                        rtl_visual_candidates.push(items.len());
                                    }
                                }
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
                                    font_size: rendered_size,
                                    page: page_num,
                                    is_bold: is_bold_font(base_font) || desc_bold,
                                    is_italic: is_italic_font(base_font) || desc_italic,
                                    is_underline: false,
                                    is_strikeout: false,
                                    rotation: geometry.rotation,
                                    advance_known: geometry.advance_known,
                                    item_type: ItemType::Text,
                                    mcid: current_mcid(&marked_content_stack),
                                    baseline_shift: 0.0,
                                });
                            }
                        }
                        // Always advance the text matrix by the total width —
                        // measured, or estimated for a font without metrics.
                        text_matrix[4] += total_width_ts * text_matrix[0];
                        text_matrix[5] += total_width_ts * text_matrix[1];
                    }
                }
            }
            "'" => {
                // Move to next line and show text (equivalent to T* then Tj)
                let tl = if text_leading != 0.0 {
                    text_leading
                } else {
                    current_font_size * 1.2
                };
                line_matrix[4] += (-tl) * line_matrix[2];
                line_matrix[5] += (-tl) * line_matrix[3];
                text_matrix = line_matrix;
                // Capture first-glyph position for ActualText AFTER the
                // line move — the BDC-entry matrix is on the previous line.
                if suppress_glyph_extraction
                    && actual_text_glyph_tm.is_none()
                    && op
                        .operands
                        .last()
                        .and_then(get_operand_bytes)
                        .is_some_and(|raw| !raw.is_empty())
                {
                    actual_text_glyph_tm = Some(text_matrix);
                    actual_text_glyph_rise = Some(text_rise);
                    actual_text_glyph_font = Some(current_font.clone());
                    actual_text_glyph_font_size = Some(current_font_size);
                }
                if suppress_glyph_extraction {
                    actual_text_glyph_count += shown_glyph_count(
                        op.operands.first().and_then(get_operand_bytes),
                        font_widths.get(&current_font),
                    );
                    actual_text_glyphs_measured &= font_widths.contains_key(&current_font);
                    actual_text_estimate_ts += estimated_string_advance_ts(
                        op.operands.first().and_then(get_operand_bytes),
                        font_widths.get(&current_font),
                        current_font_size * type3_scales.get(&current_font).copied().unwrap_or(1.0),
                        char_spacing,
                        word_spacing,
                    );
                }
                // Advance width, as for Tj — without it the item stays
                // zero-width and geometric underline/strikeout detection
                // rejects it (`is_underline_candidate` needs width > 0).
                let w_ts_opt = font_widths.get(&current_font).and_then(|fi| {
                    op.operands.first().and_then(get_operand_bytes).map(|raw| {
                        compute_string_width_ts(
                            raw,
                            fi,
                            current_font_size,
                            char_spacing,
                            word_spacing,
                        )
                    })
                });
                let glyph_count = shown_glyph_count(
                    op.operands.first().and_then(get_operand_bytes),
                    font_widths.get(&current_font),
                );
                let em_ts =
                    current_font_size * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                let estimate_ts = estimated_string_advance_ts(
                    op.operands.first().and_then(get_operand_bytes),
                    font_widths.get(&current_font),
                    em_ts,
                    char_spacing,
                    word_spacing,
                );
                if text_rendering_mode == 3
                    && !include_invisible
                    && op
                        .operands
                        .first()
                        .and_then(get_operand_bytes)
                        .is_some_and(|raw| !raw.is_empty())
                {
                    skipped_invisible = true;
                }
                if !((text_rendering_mode == 3 && !include_invisible)
                    || suppress_glyph_extraction
                    || op.operands.is_empty())
                {
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        &current_font,
                        font_base_names.get(&current_font).map(|s| s.as_str()),
                        font_cmaps,
                        &font_tounicode_refs,
                        &inline_cmaps,
                        &font_encodings,
                        &encoding_cache,
                        &mut cmap_decisions,
                        &font_widths,
                    ) {
                        if !text.trim().is_empty() {
                            let combined =
                                multiply_matrices(&rise_adjusted(&text_matrix, text_rise), &ctm);
                            rotation_votes
                                .cast_direction(reading_direction(&combined, current_font_size));
                            let rendered_size = effective_font_size(current_font_size, &combined)
                                * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                            let geometry = run_geometry(
                                &combined,
                                w_ts_opt,
                                if glyph_count > 0 {
                                    estimate_ts
                                } else {
                                    estimated_advance_ts(&text, em_ts)
                                },
                                rendered_size.copysign(current_font_size),
                                type3_y_flips.contains(&current_font),
                            );
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            if crate::text_utils::is_visual_rtl_candidate(&text)
                                && combined[0].abs() > combined[1].abs()
                            {
                                if combined[0] > 0.0 {
                                    rtl_visual_candidates.push(items.len());
                                } else {
                                    rtl_logical_ops += 1;
                                }
                            }
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
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font) || desc_bold,
                                is_italic: is_italic_font(base_font) || desc_italic,
                                is_underline: false,
                                is_strikeout: false,
                                rotation: geometry.rotation,
                                advance_known: geometry.advance_known,
                                item_type: ItemType::Text,
                                mcid: current_mcid(&marked_content_stack),
                                baseline_shift: 0.0,
                            });
                        }
                    }
                }
                // Advance regardless of visibility so later show-text
                // operators on the same line stay positioned (as for Tj).
                let cursor_ts = w_ts_opt.unwrap_or(estimate_ts);
                text_matrix[4] += cursor_ts * text_matrix[0];
                text_matrix[5] += cursor_ts * text_matrix[1];
            }
            "Do" => {
                // XObject invocation - could be an image or form
                if !op.operands.is_empty() {
                    if let Ok(name) = op.operands[0].as_name() {
                        let xobj_name = String::from_utf8_lossy(name).to_string();

                        if let Some(xobj_type) = xobjects.get(&xobj_name) {
                            match xobj_type {
                                XObjectType::Image => {
                                    // Emit a positional placeholder for the image
                                    // so downstream consumers (layout-aware
                                    // pipelines, figure-OCR routers) can locate
                                    // raster figures without parsing the PDF
                                    // again. The text field carries the
                                    // XObject resource name in the legacy
                                    // `[Image: Im0]` format that the markdown
                                    // emitter already recognizes.
                                    let (x, y, width, height) = image_bbox_from_ctm(&ctm);
                                    items.push(TextItem {
                                        text: format!("[Image: {}]", xobj_name),
                                        x,
                                        y,
                                        width,
                                        height,
                                        font: String::new(),
                                        font_tag: String::new(),
                                        font_size: 0.0,
                                        page: page_num,
                                        is_bold: false,
                                        is_italic: false,
                                        is_underline: false,
                                        is_strikeout: false,
                                        rotation: 0.0,
                                        advance_known: true,
                                        item_type: ItemType::Image,
                                        mcid: current_mcid(&marked_content_stack),
                                        baseline_shift: 0.0,
                                    });
                                }
                                XObjectType::Form(form_id) => {
                                    // Extract text from Form XObject
                                    let mut form_runs = Vec::new();
                                    extract_form_xobject_text(
                                        doc,
                                        *form_id,
                                        page_num,
                                        font_cmaps,
                                        &ctm,
                                        include_invisible,
                                        text_rendering_mode,
                                        text_rise,
                                        &mut cmap_decisions,
                                        style_cache,
                                        form_budget,
                                    )
                                    .append_into(
                                        &mut items,
                                        &mut rtl_visual_candidates,
                                        &mut rtl_logical_ops,
                                        &mut form_runs,
                                        &mut skipped_invisible,
                                    );
                                    // Form runs vote on page rotation like
                                    // page-stream runs — once per show
                                    // operator, as one TJ can split into
                                    // several items. Print-to-PDF producers
                                    // route the whole page through one form,
                                    // and a rotated page drawn that way must
                                    // be turned like any other.
                                    for rotation in form_runs {
                                        rotation_votes.cast_rotation(rotation);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "BMC" => {
                // Begin Marked Content (no properties)
                marked_content_stack.push(MarkedContentEntry {
                    actual_text: None,
                    mcid: None,
                });
            }
            "BDC" => {
                // Begin Marked Content with properties — extract ActualText and MCID
                let mut actual_text: Option<String> = None;
                let mut mcid: Option<i64> = None;
                if op.operands.len() >= 2 {
                    let dict = match &op.operands[1] {
                        Object::Dictionary(d) => Some(d.clone()),
                        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
                        _ => None,
                    };
                    if let Some(d) = dict {
                        if let Ok(val) = d.get(b"ActualText") {
                            actual_text = match val {
                                Object::String(bytes, _) => Some(decode_text_string(bytes)),
                                _ => None,
                            };
                        }
                        if let Ok(Object::Integer(id)) = d.get(b"MCID") {
                            mcid = Some(*id);
                        }
                    }
                }
                if actual_text.is_some() {
                    suppress_glyph_extraction = true;
                    actual_text_start_tm = Some(text_matrix);
                    actual_text_start_rise = text_rise;
                    actual_text_glyph_tm = None; // reset — will be captured at first Tj/TJ
                    actual_text_glyph_rise = None;
                    actual_text_glyph_font = None;
                    actual_text_glyph_font_size = None;
                    actual_text_glyphs_measured = true;
                    actual_text_estimate_ts = 0.0;
                    actual_text_glyph_count = 0;
                }
                marked_content_stack.push(MarkedContentEntry { actual_text, mcid });
            }
            "EMC" => {
                // End Marked Content — emit ActualText item with correct width
                if let Some(entry) = marked_content_stack.pop() {
                    if let Some(at) = entry.actual_text {
                        // Use the first-glyph position (if available) instead of the
                        // BDC-entry position. Td operators between BDC and the first
                        // Tj may have moved the text position to the correct line —
                        // the BDC-entry position can be on the previous line.
                        let glyph_tm = actual_text_glyph_tm.take();
                        let glyph_rise = actual_text_glyph_rise.take();
                        let entry_tm = actual_text_start_tm.take();
                        if let Some(start_tm) = glyph_tm.or(entry_tm) {
                            let rise = glyph_rise.unwrap_or(actual_text_start_rise);
                            let combined = multiply_matrices(&rise_adjusted(&start_tm, rise), &ctm);
                            // The font and `Tf` size in force when the span's
                            // first glyph was painted decide its size, its turn,
                            // and its vote — not whatever `Tf` selected before EMC.
                            let paint_font = actual_text_glyph_font
                                .take()
                                .unwrap_or_else(|| current_font.clone());
                            let paint_size = actual_text_glyph_font_size
                                .take()
                                .unwrap_or(current_font_size);
                            let rendered_size = effective_font_size(paint_size, &combined)
                                * type3_scales.get(&paint_font).copied().unwrap_or(1.0);
                            // Advance in text-space units: the text matrix
                            // travelled from `start_tm` along its own x axis.
                            // Project the displacement onto that axis — a
                            // rotated run advances through tm[5], and a
                            // scaled matrix ([12 0 0 12] with `/F 1 Tf`)
                            // carries the scale in tm[0], so the raw tm[4]
                            // delta is neither the advance nor device width.
                            // Without width metrics the matrix only moved by
                            // this parser's own estimate, so the displacement
                            // is no measurement. The fonts that painted the
                            // glyphs decide — all of them — not one selected
                            // after them; a span that painted nothing has only
                            // its displacement.
                            let advance_ts =
                                if actual_text_glyph_count > 0 && !actual_text_glyphs_measured {
                                    None
                                } else {
                                    let dx = text_matrix[4] - start_tm[4];
                                    let dy = text_matrix[5] - start_tm[5];
                                    let axis_len_sq =
                                        start_tm[0] * start_tm[0] + start_tm[1] * start_tm[1];
                                    if axis_len_sq > f32::EPSILON {
                                        Some((dx * start_tm[0] + dy * start_tm[1]) / axis_len_sq)
                                    } else {
                                        None
                                    }
                                };
                            let geometry = run_geometry(
                                &combined,
                                advance_ts,
                                // Size the estimate from what was painted, each
                                // run at its own size and spacing; the
                                // replacement text is only what gets emitted.
                                actual_text_estimate_ts,
                                rendered_size.copysign(paint_size),
                                type3_y_flips.contains(&paint_font),
                            );
                            if !at.trim().is_empty() {
                                rotation_votes
                                    .cast_direction(reading_direction(&combined, paint_size));
                                let base_font = font_base_names
                                    .get(&current_font)
                                    .map(|s| s.as_str())
                                    .unwrap_or(&current_font);
                                let (desc_italic, desc_bold) = font_style_flags
                                    .get(&current_font)
                                    .copied()
                                    .unwrap_or((false, false));
                                items.push(TextItem {
                                    text: expand_ligatures(&at),
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
                                    font_size: rendered_size,
                                    page: page_num,
                                    is_bold: is_bold_font(base_font) || desc_bold,
                                    is_italic: is_italic_font(base_font) || desc_italic,
                                    is_underline: false,
                                    is_strikeout: false,
                                    rotation: geometry.rotation,
                                    advance_known: geometry.advance_known,
                                    item_type: ItemType::Text,
                                    mcid: entry
                                        .mcid
                                        .or_else(|| current_mcid(&marked_content_stack)),
                                    baseline_shift: 0.0,
                                });
                            }
                        }
                        suppress_glyph_extraction =
                            marked_content_stack.iter().any(|e| e.actual_text.is_some());
                    }
                }
            }
            "re" => {
                // Rectangle operator: collect for table-grid detection
                if op.operands.len() >= 4 {
                    let rx = get_number(&op.operands[0]).unwrap_or(0.0);
                    let ry = get_number(&op.operands[1]).unwrap_or(0.0);
                    let rw = get_number(&op.operands[2]).unwrap_or(0.0);
                    let rh = get_number(&op.operands[3]).unwrap_or(0.0);
                    // Transform origin to device space
                    let x_dev = rx * ctm[0] + ry * ctm[2] + ctm[4];
                    let y_dev = rx * ctm[1] + ry * ctm[3] + ctm[5];
                    let w_dev = rw * ctm[0];
                    let h_dev = rh * ctm[3];
                    let rect = PdfRect {
                        x: x_dev,
                        y: y_dev,
                        width: w_dev,
                        height: h_dev,
                        page: page_num,
                    };
                    // Underline detection must only see rects that are
                    // actually painted — a `re` used purely as a clip path
                    // (`re W n`) or discarded (`re n`) draws nothing. Hold
                    // the rect as pending until a paint operator confirms it.
                    pending_re_rects.push(rect.clone());
                    rects.push(rect);
                }
            }
            // ── Path construction operators ──────────────────────
            "m" => {
                // moveto: start a new subpath
                if op.operands.len() >= 2 {
                    let px = get_number(&op.operands[0]).unwrap_or(0.0);
                    let py = get_number(&op.operands[1]).unwrap_or(0.0);
                    path_subpath_start = Some((px, py));
                    path_current = Some((px, py));
                }
            }
            "l" => {
                // lineto: add segment from current point
                if op.operands.len() >= 2 {
                    if let Some((cx, cy)) = path_current {
                        let px = get_number(&op.operands[0]).unwrap_or(0.0);
                        let py = get_number(&op.operands[1]).unwrap_or(0.0);
                        pending_lines.push((cx, cy, px, py));
                        path_current = Some((px, py));
                    }
                }
            }
            "h" => {
                // closepath: segment back to subpath start
                if let (Some((cx, cy)), Some((sx, sy))) = (path_current, path_subpath_start) {
                    if (cx - sx).abs() > 0.01 || (cy - sy).abs() > 0.01 {
                        pending_lines.push((cx, cy, sx, sy));
                    }
                    path_current = path_subpath_start;
                }
                // Save completed subpath for f/f* rect extraction and clear pending_lines.
                // The W/W* handler reads from pending_subpaths (last entry) instead.
                if !pending_lines.is_empty() {
                    pending_subpaths.push(std::mem::take(&mut pending_lines));
                }
            }
            // ── Path painting operators ──────────────────────────
            "S" | "s" => {
                // stroke / close-and-stroke: emit pending lines
                if op.operator == "s" {
                    // close first
                    if let (Some((cx, cy)), Some((sx, sy))) = (path_current, path_subpath_start) {
                        if (cx - sx).abs() > 0.01 || (cy - sy).abs() > 0.01 {
                            pending_lines.push((cx, cy, sx, sy));
                        }
                    }
                }
                for (x1, y1, x2, y2) in pending_lines.drain(..) {
                    let (x1d, y1d) = transform_path_point(x1, y1, &ctm);
                    let (x2d, y2d) = transform_path_point(x2, y2, &ctm);
                    lines.push(PdfLine {
                        x1: x1d,
                        y1: y1d,
                        x2: x2d,
                        y2: y2d,
                        page: page_num,
                    });
                    underline_lines.push(UnderlineLine {
                        x1: x1d,
                        y1: y1d,
                        x2: x2d,
                        y2: y2d,
                        stroke_width: transformed_stroke_width(line_width, &ctm, x1, y1, x2, y2),
                        page: page_num,
                    });
                }
                painted_rects.append(&mut pending_re_rects);
                pending_subpaths.clear();
                path_subpath_start = None;
                path_current = None;
            }
            "B" | "B*" | "b" | "b*" => {
                // fill+stroke: emit lines AND clear state
                if op.operator == "b" || op.operator == "b*" {
                    // close first
                    if let (Some((cx, cy)), Some((sx, sy))) = (path_current, path_subpath_start) {
                        if (cx - sx).abs() > 0.01 || (cy - sy).abs() > 0.01 {
                            pending_lines.push((cx, cy, sx, sy));
                        }
                    }
                }
                for (x1, y1, x2, y2) in pending_lines.drain(..) {
                    let (x1d, y1d) = transform_path_point(x1, y1, &ctm);
                    let (x2d, y2d) = transform_path_point(x2, y2, &ctm);
                    lines.push(PdfLine {
                        x1: x1d,
                        y1: y1d,
                        x2: x2d,
                        y2: y2d,
                        page: page_num,
                    });
                    underline_lines.push(UnderlineLine {
                        x1: x1d,
                        y1: y1d,
                        x2: x2d,
                        y2: y2d,
                        stroke_width: transformed_stroke_width(line_width, &ctm, x1, y1, x2, y2),
                        page: page_num,
                    });
                }
                painted_rects.append(&mut pending_re_rects);
                pending_subpaths.clear();
                path_subpath_start = None;
                path_current = None;
            }
            "f" | "F" | "f*" => {
                // fill-only: extract axis-aligned rects from completed subpaths
                // Also check any un-closed segments still in pending_lines
                if !pending_lines.is_empty() {
                    pending_subpaths.push(std::mem::take(&mut pending_lines));
                }
                for subpath in pending_subpaths.drain(..) {
                    // Synthesize closing segment if only 3 segments
                    let mut segs = subpath;
                    if segs.len() == 3 {
                        let (x0, y0, _, _) = segs[0];
                        let (_, _, ex, ey) = segs[2];
                        if (ex - x0).abs() > 0.01 || (ey - y0).abs() > 0.01 {
                            segs.push((ex, ey, x0, y0));
                        }
                    }
                    if segs.len() == 4 {
                        let mut xs = Vec::with_capacity(8);
                        let mut ys = Vec::with_capacity(8);
                        for &(x1, y1, x2, y2) in &segs {
                            xs.push(x1);
                            xs.push(x2);
                            ys.push(y1);
                            ys.push(y2);
                        }
                        let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
                        let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        let min_y = ys.iter().copied().fold(f32::INFINITY, f32::min);
                        let max_y = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        let w = max_x - min_x;
                        let h = max_y - min_y;
                        let eps: f32 = 0.5;
                        let axis_aligned = xs
                            .iter()
                            .all(|&x| (x - min_x).abs() < eps || (x - max_x).abs() < eps)
                            && ys
                                .iter()
                                .all(|&y| (y - min_y).abs() < eps || (y - max_y).abs() < eps);
                        if axis_aligned && w > 1.0 && h > 1.0 {
                            let x_dev = min_x * ctm[0] + min_y * ctm[2] + ctm[4];
                            let y_dev = min_x * ctm[1] + min_y * ctm[3] + ctm[5];
                            let w_dev = w * ctm[0];
                            let h_dev = h * ctm[3];
                            fill_rects.push(PdfRect {
                                x: x_dev,
                                y: y_dev,
                                width: w_dev,
                                height: h_dev,
                                page: page_num,
                            });
                        }
                    }
                }
                painted_rects.append(&mut pending_re_rects);
                pending_lines.clear();
                path_subpath_start = None;
                path_current = None;
            }
            "W" | "W*" => {
                // Clip operator: check if pending path forms an axis-aligned rectangle.
                // Many PDFs define table cells as clipping paths instead of stroked rects.
                // After `h` closes a subpath, pending_lines is cleared and the subpath
                // is saved to pending_subpaths. Read from the last subpath entry.
                let mut segs: Vec<(f32, f32, f32, f32)> = if pending_lines.is_empty() {
                    pending_subpaths.last().cloned().unwrap_or_default()
                } else {
                    pending_lines.clone()
                };
                // If only 3 segments, synthesize closing segment back to subpath start
                if segs.len() == 3 {
                    if let Some((sx, sy)) = path_subpath_start {
                        let (_, _, ex, ey) = segs[2];
                        if (ex - sx).abs() > 0.01 || (ey - sy).abs() > 0.01 {
                            segs.push((ex, ey, sx, sy));
                        }
                    }
                }
                if segs.len() == 4 {
                    // Collect all endpoints and compute bounding box
                    let mut xs = Vec::with_capacity(8);
                    let mut ys = Vec::with_capacity(8);
                    for &(x1, y1, x2, y2) in &segs {
                        xs.push(x1);
                        xs.push(x2);
                        ys.push(y1);
                        ys.push(y2);
                    }
                    let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
                    let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let min_y = ys.iter().copied().fold(f32::INFINITY, f32::min);
                    let max_y = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let w = max_x - min_x;
                    let h = max_y - min_y;
                    // Verify all points lie on bounding box edges (axis-aligned rectangle)
                    let eps: f32 = 0.5;
                    let axis_aligned = xs
                        .iter()
                        .all(|&x| (x - min_x).abs() < eps || (x - max_x).abs() < eps)
                        && ys
                            .iter()
                            .all(|&y| (y - min_y).abs() < eps || (y - max_y).abs() < eps);
                    if axis_aligned && w > 1.0 && h > 1.0 {
                        // Transform to device space using CTM (same as `re` handler)
                        let x_dev = min_x * ctm[0] + min_y * ctm[2] + ctm[4];
                        let y_dev = min_x * ctm[1] + min_y * ctm[3] + ctm[5];
                        let w_dev = w * ctm[0];
                        let h_dev = h * ctm[3];
                        clip_rects.push(PdfRect {
                            x: x_dev,
                            y: y_dev,
                            width: w_dev,
                            height: h_dev,
                            page: page_num,
                        });
                    }
                }
                // Do NOT clear pending_lines — the following `n` does that
            }
            "n" => {
                // end path (no-op): discard — including any `re` rects that
                // were only ever part of a clip path (`re W n`), which draw
                // no ink and must not feed underline detection.
                pending_re_rects.clear();
                pending_lines.clear();
                pending_subpaths.clear();
                path_subpath_start = None;
                path_current = None;
            }
            _ => {}
        }
    }

    if form_budget.was_truncated() {
        log::warn!(
            "page {page_num}: Form XObject expansion truncated (invocation or operation budget reached); nested form text may be incomplete"
        );
    }

    // Underline detection reads only painted ink: `re` rects confirmed by
    // a paint operator plus filled-subpath rects — never clip-only rects,
    // which draw nothing.
    let mut underline_rects = painted_rects;
    underline_rects.extend(fill_rects.iter().cloned());

    // Only use clip/fill rects when no `re` rects exist on this page.
    // Clip rects take priority over fill rects, but first we deduplicate
    // them: some PDFs wrap every text block in a full-page W* clip path,
    // producing thousands of identical rects that yield a degenerate grid.
    // After dedup, if too few unique clip rects remain we fall through to
    // fill rects (explicitly drawn visible rectangles).
    //
    // When fill rects substantially outnumber clip rects, the clips are
    // typically section-level wrappers and the fills are the actual table
    // cell backgrounds (e.g. shaded-header tables drawn with `m`/`l`/`h`/`f*`
    // sequences). In that case, prefer fills.
    if rects.is_empty() {
        dedup_rects(&mut clip_rects);
        let prefer_fills = !fill_rects.is_empty() && fill_rects.len() >= clip_rects.len() * 3;
        if prefer_fills {
            rects = fill_rects;
        } else if clip_rects.len() >= 4 {
            rects = clip_rects;
        } else if !fill_rects.is_empty() {
            rects = fill_rects;
        } else if !clip_rects.is_empty() {
            rects = clip_rects;
        }
    }

    // Reverse visual-order RTL runs while candidate indexes are still valid
    // (merge_text_items below reshapes the item list).
    crate::text_utils::fix_visual_order_rtl(&mut items, &rtl_visual_candidates, rtl_logical_ops);

    // Detect dominant text rotation and transform coordinates if needed.
    // Some PDFs embed landscape content in portrait pages using a rotated text
    // matrix (e.g. [0, b, -b, 0, tx, ty] for 90° CCW).  The layout engine
    // assumes x=horizontal, y=vertical — so we swap coordinates to match.
    let (mut items, rects, lines, page_rotation) =
        correct_rotated_page(items, rects, lines, &rotation_votes);
    if page_rotation != PageRotation::Upright {
        rotate_underline_graphics(&mut underline_rects, &mut underline_lines, page_rotation);
    }
    super::underline::mark_underlined_items(
        &mut items,
        &underline_rects,
        &underline_lines,
        page_num,
    );

    let items = super::merge_text_items(items);
    let items = super::merge_subscript_items(items);
    Ok((
        (items, rects, lines),
        has_gid_fonts,
        page_rotation,
        skipped_invisible,
    ))
}

/// Counts of text-producing show operators by baseline direction: the
/// page-rotation vote. Operators, not items — one TJ array can split into
/// several items — and never whitespace-only runs or image placeholders.
#[derive(Default)]
struct RotationVotes {
    horizontal: u32,
    /// Runs reading bottom-to-top (baseline turned 90° counter-clockwise).
    ccw: u32,
    /// Runs reading top-to-bottom (baseline turned 90° clockwise).
    cw: u32,
}

impl RotationVotes {
    /// Vote with the device-space direction `(a, b)` of a run's baseline.
    /// Only near-cardinal runs vote: within ~20° of the x axis they are
    /// horizontal, within ~20° of the y axis they split by which way they
    /// run. Diagonal runs (curved titles, watermarks, callouts) abstain, so
    /// a page of them is never turned by a quarter it does not read in.
    fn cast(&mut self, a: f32, b: f32) {
        const TAN_20_DEG: f32 = 0.364;
        let (ax, bx) = (a.abs(), b.abs());
        if bx <= ax * TAN_20_DEG {
            self.horizontal += 1;
        } else if ax <= bx * TAN_20_DEG {
            if b > 0.0 {
                self.ccw += 1;
            } else {
                self.cw += 1;
            }
        }
    }

    /// Vote with a run's reading direction (see `geometry::reading_direction`).
    fn cast_direction(&mut self, (a, b): (f32, f32)) {
        self.cast(a, b);
    }

    /// Vote with a finished item's baseline angle (Form XObject runs arrive
    /// as items, their matrices already consumed).
    fn cast_rotation(&mut self, rotation: f32) {
        let (b, a) = rotation.to_radians().sin_cos();
        self.cast(a, b);
    }
}

/// Detect if most text runs on a page are rotated 90° or 270°, and if so,
/// turn the coordinate frame so they read along +x — the layout engine
/// assumes x is the reading direction and y stacks the lines.
fn correct_rotated_page(
    mut items: Vec<TextItem>,
    mut rects: Vec<PdfRect>,
    mut lines: Vec<PdfLine>,
    votes: &RotationVotes,
) -> (Vec<TextItem>, Vec<PdfRect>, Vec<PdfLine>, PageRotation) {
    // Use the direction votes collected during extraction: for normal text
    // combined[0] (the x-component of the text x-axis) dominates, for 90°
    // rotated text combined[1] does. Votes count text-producing show
    // operators, never items or placeholders: a single rotated run is a
    // stamp or a caption, not a landscape layout, even when a TJ array
    // splits it into several items or an image sits next to it.
    let rotated = votes.ccw + votes.cw;
    let total_votes = votes.horizontal + rotated;
    if total_votes < 2 || rotated * 3 < total_votes * 2 {
        // Less than ~67% of text operators are rotated → not a rotated page
        return (items, rects, lines, PageRotation::Upright);
    }

    // Turn the frame against the dominant direction so those runs read along
    // +x and report `rotation == 0`: counter-clockwise pages (Tm = [0 b -b 0])
    // map (x, y) → (y, -x), clockwise pages ([0 -b b 0]) map (x, y) → (-y, x).
    // The layout engine sorts by y descending, and either mapping sends the
    // visual top of the page (low device x on a CCW page, high device x on a
    // CW page) to high y. A tie between the two directions turns nothing:
    // either choice would mirror half the page.
    let rotation = match votes.cw.cmp(&votes.ccw) {
        std::cmp::Ordering::Greater => PageRotation::Cw,
        std::cmp::Ordering::Less => PageRotation::Ccw,
        std::cmp::Ordering::Equal => return (items, rects, lines, PageRotation::Upright),
    };
    log::debug!(
        "detected rotated page text: {}/{} text ops are rotated ({:?}) — turning coordinates",
        rotated,
        total_votes,
        rotation
    );

    for item in &mut items {
        // Turn the axis-aligned box exactly like the rects below. Items carry
        // their true rotated-run box (see `run_geometry`), so a dominant run
        // lands with x at its start, y on its baseline, and the real advance
        // as `width` — no character-count estimate needed. The same turn puts
        // an upright stray (page number, stamp) where it renders in the
        // corrected frame: as a vertical run.
        rotation.rotate_box(&mut item.x, &mut item.y, &mut item.width, &mut item.height);
        // Only text runs have a baseline; image placeholders keep the `0`
        // they were extracted with.
        if matches!(item.item_type, ItemType::Text) {
            item.rotation = normalize_degrees(item.rotation + rotation.baseline_rebase_degrees());
        }
    }

    for rect in &mut rects {
        rotation.rotate_box(&mut rect.x, &mut rect.y, &mut rect.width, &mut rect.height);
    }

    for line in &mut lines {
        let (x1, y1) = rotation.rotate_point(line.x1, line.y1);
        let (x2, y2) = rotation.rotate_point(line.x2, line.y2);
        line.x1 = x1;
        line.y1 = y1;
        line.x2 = x2;
        line.y2 = y2;
    }

    (items, rects, lines, rotation)
}

fn rotate_underline_graphics(
    rects: &mut [PdfRect],
    lines: &mut [UnderlineLine],
    rotation: PageRotation,
) {
    for rect in rects {
        rotation.rotate_box(&mut rect.x, &mut rect.y, &mut rect.width, &mut rect.height);
    }

    for line in lines {
        let (x1, y1) = rotation.rotate_point(line.x1, line.y1);
        let (x2, y2) = rotation.rotate_point(line.x2, line.y2);
        line.x1 = x1;
        line.y1 = y1;
        line.x2 = x2;
        line.y2 = y2;
    }
}

/// Remove near-duplicate rects (same coordinates within 0.5 pt tolerance).
/// Some PDFs emit a full-page clip path for every text block, producing
/// thousands of identical rects. After dedup these collapse to one rect,
/// which is too few for table detection and gets naturally skipped.
fn dedup_rects(rects: &mut Vec<PdfRect>) {
    if rects.len() <= 1 {
        return;
    }
    // Round to 0.5-pt grid for tolerance, then sort and dedup.
    rects.sort_by(|a, b| {
        let ak = (
            a.page,
            (a.x * 2.0) as i32,
            (a.y * 2.0) as i32,
            (a.width * 2.0) as i32,
            (a.height * 2.0) as i32,
        );
        let bk = (
            b.page,
            (b.x * 2.0) as i32,
            (b.y * 2.0) as i32,
            (b.width * 2.0) as i32,
            (b.height * 2.0) as i32,
        );
        ak.cmp(&bk)
    });
    rects.dedup_by(|a, b| {
        a.page == b.page
            && ((a.x - b.x).abs() < 0.5)
            && ((a.y - b.y).abs() < 0.5)
            && ((a.width - b.width).abs() < 0.5)
            && ((a.height - b.height).abs() < 0.5)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32, page: u32) -> PdfRect {
        PdfRect {
            x,
            y,
            width: w,
            height: h,
            page,
        }
    }

    fn simple_doc_with_content(content: &[u8]) -> (lopdf::Document, lopdf::ObjectId) {
        use lopdf::{dictionary, Object, Stream};

        let mut doc = lopdf::Document::new();
        let widths: Vec<Object> = (0..=255).map(|_| 600.into()).collect();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        });
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => Object::Reference(font_id),
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

    fn extract_simple_items(content: &[u8]) -> Vec<TextItem> {
        use crate::tounicode::FontCMaps;

        let (doc, page_id) = simple_doc_with_content(content);
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
    fn test_dedup_rects_identical() {
        let mut rects = vec![rect(0.0, 0.0, 612.0, 792.0, 1); 3759];
        dedup_rects(&mut rects);
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn test_dedup_rects_within_tolerance() {
        let mut rects = vec![
            rect(10.0, 20.0, 100.0, 50.0, 1),
            rect(10.2, 20.1, 100.3, 50.4, 1),
        ];
        dedup_rects(&mut rects);
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn test_dedup_rects_distinct_kept() {
        let mut rects = vec![
            rect(10.0, 20.0, 100.0, 50.0, 1),
            rect(120.0, 20.0, 100.0, 50.0, 1),
            rect(10.0, 80.0, 100.0, 50.0, 1),
        ];
        dedup_rects(&mut rects);
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn test_dedup_rects_different_pages_kept() {
        let mut rects = vec![
            rect(0.0, 0.0, 612.0, 792.0, 1),
            rect(0.0, 0.0, 612.0, 792.0, 2),
        ];
        dedup_rects(&mut rects);
        assert_eq!(rects.len(), 2);
    }

    #[test]
    fn test_dedup_rects_empty_and_single() {
        let mut empty: Vec<PdfRect> = vec![];
        dedup_rects(&mut empty);
        assert!(empty.is_empty());

        let mut single = vec![rect(1.0, 2.0, 3.0, 4.0, 1)];
        dedup_rects(&mut single);
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn thick_stroked_rule_does_not_mark_underline() {
        let content = b"BT /F1 12 Tf 1 0 0 1 100 500 Tm (THICK) Tj ET
4 w
100 498 m 170 498 l S
BT /F1 12 Tf 1 0 0 1 100 480 Tm (THIN) Tj ET
1 w
100 478 m 160 478 l S";

        let items = extract_simple_items(content);
        let thick = items.iter().find(|item| item.text == "THICK").unwrap();
        let thin = items.iter().find(|item| item.text == "THIN").unwrap();

        assert!(!thick.is_underline);
        assert!(thin.is_underline);
    }

    #[test]
    fn rotated_page_underline_is_detected_after_coordinate_correction() {
        let content = b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (WORLD) Tj ET
1 w
202 100 m 202 170 l S";

        let items = extract_simple_items(content);
        let hello = items.iter().find(|item| item.text == "HELLO").unwrap();
        let world = items.iter().find(|item| item.text == "WORLD").unwrap();

        assert!(hello.is_underline);
        assert!(!world.is_underline);
    }

    #[test]
    fn quote_operator_text_carries_advance_width() {
        // `'` (move-to-next-line-and-show-text) must retain the string's
        // advance width like Tj — zero-width items are invisible to
        // geometric underline/strikeout detection.
        let content = b"BT /F1 12 Tf 12 TL 1 0 0 1 100 512 Tm (first) Tj (struck) ' ET
1 w
99 503 m 145 503 l S";

        let items = extract_simple_items(content);
        let struck = items.iter().find(|item| item.text == "struck").unwrap();

        // 6 glyphs x 600/1000 x 12pt = 43.2pt, drawn one leading below Tm.
        assert!((struck.width - 43.2).abs() < 0.1);
        assert!((struck.y - 500.0).abs() < 0.1);
        assert!(struck.is_strikeout);
        assert!(!struck.is_underline);
    }

    #[test]
    fn quote_operator_advances_text_matrix() {
        // Text shown after `'` on the same line must start past the shown
        // string: "CD" lands at x=114.4 (2 glyphs x 600/1000 x 12pt after
        // x=100), flush against "AB", so the merge pass joins them. Without
        // the advance "CD" overlaps "AB" at x=100 and the items stay apart.
        let content = b"BT /F1 12 Tf 12 TL 1 0 0 1 100 512 Tm (AB) ' (CD) Tj ET";

        let items = extract_simple_items(content);
        let merged = items.iter().find(|item| item.text == "ABCD").unwrap();

        assert!((merged.x - 100.0).abs() < 0.1);
        assert!((merged.width - 28.8).abs() < 0.1);
        assert!((merged.y - 500.0).abs() < 0.1);
    }

    #[test]
    fn text_rise_shifts_item_baseline() {
        // Ts displaces the glyph origin vertically without touching the
        // advance; the next run at rise 0 must return to the original
        // baseline and follow the raised run horizontally.
        let content =
            b"BT /F1 12 Tf 1 0 0 1 100 500 Tm (base) Tj 5 Ts (super) Tj 0 Ts (after) Tj ET";

        let items = extract_simple_items(content);
        let base = items.iter().find(|item| item.text == "base").unwrap();
        let raised = items.iter().find(|item| item.text == "super").unwrap();
        let after = items.iter().find(|item| item.text == "after").unwrap();

        assert!((base.y - 500.0).abs() < 0.1);
        assert!((raised.y - 505.0).abs() < 0.1);
        assert!((after.y - 500.0).abs() < 0.1);
        assert!(after.x > raised.x);
    }

    #[test]
    fn actual_text_item_uses_glyph_rise() {
        // The ActualText replacement item must render at the rise in
        // effect when its glyphs were drawn — not the unshifted BDC
        // baseline, and not whatever rise is set by EMC time.
        let content = b"BT /F1 12 Tf 1 0 0 1 100 500 Tm \
/Span <</ActualText (super) >> BDC 5 Ts (sup) Tj 0 Ts EMC (after) Tj ET";

        let items = extract_simple_items(content);
        let sup = items.iter().find(|item| item.text == "super").unwrap();
        let after = items.iter().find(|item| item.text == "after").unwrap();

        assert!((sup.y - 505.0).abs() < 0.1);
        assert!((after.y - 500.0).abs() < 0.1);
    }

    #[test]
    fn actual_text_shown_with_quote_op_uses_moved_risen_baseline() {
        // When the tagged span's show op is `'`, the glyph position is
        // only known AFTER its line move — falling back to the BDC-entry
        // matrix would place the item on the previous line, unrisen.
        let content = b"BT /F1 12 Tf 14 TL 1 0 0 1 100 500 Tm \
/Span <</ActualText (replaced) >> BDC 3 Ts (raw) ' 0 Ts EMC ET";

        let items = extract_simple_items(content);
        let item = items.iter().find(|item| item.text == "replaced").unwrap();

        // Line move: 500 - 14 = 486; rise: +3 -> 489.
        assert!((item.y - 489.0).abs() < 0.1);
        assert!(item.width > 0.0);
    }

    #[test]
    fn strikeout_detected_on_risen_text() {
        // The rule crosses the glyphs at their risen position; without the
        // rise in item.y the strike window sits 4pt too low and misses.
        let content = b"BT /F1 12 Tf 1 0 0 1 100 500 Tm 4 Ts (struck) Tj ET
1 w
99 507 m 145 507 l S";

        let items = extract_simple_items(content);
        let struck = items.iter().find(|item| item.text == "struck").unwrap();

        assert!((struck.y - 504.0).abs() < 0.1);
        assert!(struck.is_strikeout);
        assert!(!struck.is_underline);
    }

    #[test]
    fn test_skip_excessive_operations() {
        use crate::tounicode::FontCMaps;
        use lopdf::{dictionary, Object, Stream};

        let mut doc = lopdf::Document::new();

        // "0 0 m\n" = 6 bytes per op, 1_100_000 ops → ~6.6 MB content stream
        let ops_bytes = "0 0 m\n".repeat(1_100_000).into_bytes();
        let stream = Stream::new(dictionary! {}, ops_bytes);
        let content_id = doc.add_object(Object::Stream(stream));

        let page_dict = dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {},
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        };
        let page_id = doc.add_object(page_dict);

        // Register the page so get_page_content can find it
        let pages_dict = dictionary! {
            "Type" => "Pages",
            "Count" => Object::Integer(1),
            "Kids" => vec![Object::Reference(page_id)],
        };
        let pages_id = doc.add_object(pages_dict);
        let catalog = dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        };
        doc.add_object(catalog);

        let font_cmaps = FontCMaps::from_doc(&doc);
        let result = extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
            &mut FormWalkBudget::new(),
        )
        .unwrap();
        let ((items, rects, lines), _has_gid, _coords_rotated, _skipped_invisible) = result;
        assert!(items.is_empty());
        assert!(rects.is_empty());
        assert!(lines.is_empty());
    }

    #[test]
    fn test_q_restores_current_font_for_text_decoding() {
        use crate::tounicode::FontCMaps;
        use lopdf::{dictionary, Object, Stream};

        fn cmap_stream(dst_hex: &str) -> Stream {
            let cmap = format!(
                r#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Test-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <{dst_hex}>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#
            );
            Stream::new(dictionary! {}, cmap.into_bytes())
        }

        let mut doc = lopdf::Document::new();
        let f1_cmap = doc.add_object(Object::Stream(cmap_stream("0058"))); // X
        let f2_cmap = doc.add_object(Object::Stream(cmap_stream("0059"))); // Y
        let f1 = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "ToUnicode" => Object::Reference(f1_cmap),
        });
        let f2 = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "ToUnicode" => Object::Reference(f2_cmap),
        });

        let content = b"BT /F1 12 Tf 10 700 Tm <41> Tj ET
q
BT /F2 12 Tf 20 700 Tm <41> Tj ET
Q
BT 30 700 Tm <41> Tj ET";
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => Object::Reference(f1),
                    "F2" => Object::Reference(f2),
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
        let text = items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<String>();

        assert_eq!(text, "XYX");
    }

    /// Build a one-page document whose F1 font maps bytes 41-44 to Hebrew
    /// שלום letters (41→ש 42→ל 43→ו 44→ם) via ToUnicode, run extraction, and
    /// return the items.
    fn extract_hebrew_items(content: &[u8]) -> Vec<TextItem> {
        extract_items_with_cmap(content, HEBREW_CMAP)
    }

    const HEBREW_CMAP: &[u8] = br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Test-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
4 beginbfchar
<41> <05E9>
<42> <05DC>
<43> <05D5>
<44> <05DD>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#;

    /// A page with `F1`, a TrueType font WITHOUT width metrics whose
    /// ToUnicode is `cmap`, and `F2`, a measured 600-unit Helvetica.
    fn extract_items_with_cmap(content: &[u8], cmap: &[u8]) -> Vec<TextItem> {
        use crate::tounicode::FontCMaps;
        use lopdf::{dictionary, Object, Stream};

        let mut doc = lopdf::Document::new();
        let cmap_id = doc.add_object(Object::Stream(Stream::new(dictionary! {}, cmap.to_vec())));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "TestHebrew",
            "ToUnicode" => Object::Reference(cmap_id),
        });
        let widths: Vec<Object> = (0..=255).map(|_| 600.into()).collect();
        let measured_font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        });
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! {
                    "F1" => Object::Reference(font_id),
                    "F2" => Object::Reference(measured_font_id),
                },
            },
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages",
            "Count" => Object::Integer(1),
            "Kids" => vec![Object::Reference(page_id)],
        });
        let catalog = dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        };
        doc.add_object(catalog);

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

    const SHALOM_LOGICAL: &str = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}"; // שלום

    #[test]
    fn items_carry_family_name_and_resource_tag() {
        // `font` is the resolved /BaseFont family name; `font_tag` keeps the
        // raw page resource tag so consumers can partition by font program
        // even when two resources share a family.
        let content = b"BT /F1 12 Tf 100 700 Tm <44434241> Tj ET";
        let items = extract_hebrew_items(content);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].font, "TestHebrew");
        assert_eq!(items[0].font_tag, "F1");
    }

    #[test]
    fn visual_order_hebrew_ops_are_reversed() {
        // Two show ops on one baseline painted left-to-right, each holding
        // the visual (reversed) string — the shaped-visible-text convention.
        let content = b"BT /F1 12 Tf 100 700 Tm <44434241> Tj 60 0 Td <44434241> Tj ET";
        let items = extract_hebrew_items(content);
        assert_eq!(items.len(), 2);
        for item in &items {
            assert_eq!(item.text, SHALOM_LOGICAL, "visual run must be reversed");
        }
    }

    #[test]
    fn logical_order_hebrew_ops_stay_logical() {
        // Two show ops positioned right-to-left, each already in reading
        // order — the OCR-text-layer convention. Must NOT be reversed.
        let content = b"BT /F1 12 Tf 160 700 Tm <41424344> Tj -60 0 Td <41424344> Tj ET";
        let items = extract_hebrew_items(content);
        assert_eq!(items.len(), 2);
        for item in &items {
            assert_eq!(
                item.text, SHALOM_LOGICAL,
                "logical run must not be reversed"
            );
        }
    }

    #[test]
    fn font_without_widths_marks_the_advance_unknown() {
        // The Hebrew test font carries no /Widths: the run's box is the em
        // alone and `advance_known` says so, instead of a zero width that
        // could also mean a genuine zero advance.
        let items = extract_hebrew_items(b"BT /F1 12 Tf 100 700 Td <41424344> Tj ET");
        assert_eq!(items.len(), 1);
        assert!(!items[0].advance_known);
        // Four glyphs at 12pt: a 24pt estimate laid along the baseline.
        assert_eq!((items[0].width, items[0].height), (24.0, 12.0));
    }

    #[test]
    fn width_less_estimate_counts_painted_glyphs_not_decoded_characters() {
        // One code whose ToUnicode entry is the two-letter "fi": the box is
        // one glyph's half em, not two characters' worth.
        const LIGATURE_CMAP: &[u8] = br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Test-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<45> <00660069>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#;
        let items = extract_items_with_cmap(b"BT /F1 12 Tf 100 700 Td <45> Tj ET", LIGATURE_CMAP);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "fi");
        assert!(!items[0].advance_known);
        assert_eq!(items[0].width, 6.0);
    }

    #[test]
    fn width_less_runs_lay_out_along_their_estimates() {
        // Without width metrics the cursor moves by the estimate the box
        // carries, so the next show operator starts where this one's estimate
        // ends instead of on top of it — for `Tj` and `TJ` alike.
        let items = extract_hebrew_items(b"BT /F1 12 Tf 100 700 Td <4142> Tj <4344> Tj ET");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!((items[0].x, items[0].width), (100.0, 12.0));
        assert_eq!((items[1].x, items[1].width), (112.0, 12.0));
        let items = extract_hebrew_items(b"BT /F1 12 Tf 100 700 Td [<4142> <4344>] TJ <41> Tj ET");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!((items[0].x, items[0].width), (100.0, 24.0));
        assert_eq!((items[1].x, items[1].width), (124.0, 6.0));
    }

    #[test]
    fn actual_text_span_is_sized_by_the_font_that_painted_it() {
        // The span's glyphs are painted with the measured F2, then the stream
        // selects the width-less F1 before EMC: the displacement those glyphs
        // produced is a measurement and must not give way to an estimate.
        let items = extract_hebrew_items(
            b"BT /F2 12 Tf 100 700 Td /Span <</ActualText (AB) >> BDC (AB) Tj /F1 12 Tf EMC ET",
        );
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "AB");
        assert!(items[0].advance_known);
        assert!(
            (items[0].width - 14.4).abs() < 1e-3,
            "width = {}",
            items[0].width
        );
    }

    #[test]
    fn actual_text_span_painted_with_mixed_fonts_is_estimated() {
        // One glyph from the measured F2, one from the width-less F1: the
        // displacement covers only the first, so the whole span is estimated
        // from its two painted glyphs and says so.
        let items = extract_hebrew_items(
            b"BT /F2 12 Tf 100 700 Td /Span <</ActualText (Ab) >> BDC (A) Tj /F1 12 Tf <41> Tj EMC ET",
        );
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "Ab");
        assert!(!items[0].advance_known);
        assert_eq!(items[0].width, 12.0);
    }

    #[test]
    fn actual_text_span_without_glyphs_keeps_its_displacement() {
        // Nothing painted inside the span: there is no glyph count to
        // estimate from, and the replacement string is not a stand-in for
        // one — the item carries the (zero) displacement as a measurement.
        let items =
            extract_hebrew_items(b"BT /F1 12 Tf 100 700 Td /Span <</ActualText (x) >> BDC EMC ET");
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "x");
        assert!(items[0].advance_known);
        assert_eq!(items[0].width, 0.0);
    }

    #[test]
    fn width_less_estimates_include_character_and_word_spacing() {
        // `Tc` adds to every code and `Tw` to every single-byte space, as the
        // width formula does: 3 codes × (6 + 2) + one space × 5 = 29pt for
        // the first run, 6 + 2 = 8pt for the second, and the cursor moves by
        // the same amounts. Page extraction merges adjacent items, so the two
        // runs — contiguous at 129pt — come back as one item whose box is
        // exactly the sum of both estimates and whose text gains no space at
        // the seam; had the cursor stayed put, the second run would overlap
        // the first and the merge would have kept them apart.
        const LATIN_CMAP: &[u8] = br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Test-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
4 beginbfchar
<20> <0020>
<41> <0041>
<42> <0042>
<43> <0043>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#;
        let items = extract_items_with_cmap(
            b"BT /F1 12 Tf 2 Tc 5 Tw 100 700 Td <412042> Tj <43> Tj ET",
            LATIN_CMAP,
        );
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "A BC");
        assert!(!items[0].advance_known);
        assert_eq!((items[0].x, items[0].width), (100.0, 37.0));
    }

    #[test]
    fn actual_text_span_estimate_follows_each_runs_size_and_spacing() {
        // Two width-less runs inside one span at different sizes with `Tc`
        // and `Tw` set: one code at 12pt (6 + 2), then a space and a code at
        // 24pt (2 × (12 + 2) + one space × 5) — 8 + 33 = 41pt, not one size
        // for all three codes.
        let items = extract_hebrew_items(
            b"BT /F1 12 Tf 2 Tc 5 Tw 100 700 Td /Span <</ActualText (AB C) >> BDC <41> Tj /F1 24 Tf <2042> Tj EMC ET",
        );
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "AB C");
        assert!(!items[0].advance_known);
        assert!(
            (items[0].width - 41.0).abs() < 1e-3,
            "width = {}",
            items[0].width
        );
    }

    #[test]
    fn metric_less_tj_with_negative_font_size_keeps_its_signed_estimate() {
        // Two codes at `-12 Tf` without metrics: the estimate is -12pt, the
        // run reads backwards from its origin with its glyphs below the
        // baseline, and the box says so.
        let items = extract_hebrew_items(b"BT /F1 -12 Tf 100 700 Td [<4142>] TJ ET");
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(!items[0].advance_known);
        assert_eq!((items[0].x, items[0].width), (88.0, 12.0));
        assert_eq!((items[0].y, items[0].height), (688.0, 12.0));
        assert_eq!(items[0].rotation, 180.0);
    }

    #[test]
    fn actual_text_on_a_width_less_font_is_estimated_from_painted_glyphs() {
        // The text matrix never moves for a font without widths, so the
        // ActualText span's zero displacement must not pass for a genuine
        // zero advance — and the estimate follows the four painted glyphs,
        // not the fourteen-character replacement.
        let items = extract_hebrew_items(
            b"BT /F1 12 Tf 100 700 Td /Span <</ActualText (Shalom Alaikum) >> BDC <41424344> Tj EMC ET",
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Shalom Alaikum");
        assert!(!items[0].advance_known);
        assert_eq!(items[0].width, 24.0);
    }

    #[test]
    fn rotated_hebrew_ops_stay_neutral() {
        // 90°-rotated text matrix: the advance has no horizontal component,
        // so the run carries no storage-order evidence and must pass through
        // unreversed. The font has no widths, so the run's extent along the
        // (vertical) baseline is the estimate: four glyphs × 6pt.
        let content = b"BT /F1 12 Tf 0 1 -1 0 100 700 Tm <41424344> Tj ET";
        let items = extract_hebrew_items(content);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, SHALOM_LOGICAL);
        assert!(!items[0].advance_known);
        assert_eq!(
            (items[0].x, items[0].y, items[0].width, items[0].height),
            (88.0, 700.0, 12.0, 24.0)
        );
    }

    #[test]
    fn tj_backward_jump_marks_logical_storage() {
        // A single TJ whose positive offset moves the pen backward past the
        // painted glyphs: logical-order storage positioning runs
        // right-to-left inside one op. No reversal.
        let content = b"BT /F1 12 Tf 160 700 Tm [<41424344> 6000 <41424344>] TJ ET";
        let items = extract_hebrew_items(content);
        assert!(!items.is_empty());
        for item in &items {
            assert!(
                item.text.contains(SHALOM_LOGICAL),
                "backward-jump TJ must not be reversed: {:?}",
                item.text
            );
        }
    }

    #[test]
    fn test_strip_pdf_comments() {
        // Basic comment stripping
        let input = b"BT\n% comment\nTj\nET\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, b"BT\n \nTj\nET\n");

        // No comments = unchanged
        let input = b"BT\nTj\nET\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, input.to_vec());

        // Don't strip inside string literals
        let input = b"(text with % not a comment)\n% real comment\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, b"(text with % not a comment)\n \n");

        // Don't strip inside hex strings
        let input = b"<0033% not a comment>\n% real comment\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, b"<0033% not a comment>\n \n");

        // PD4ML style: comment between Tj and ET
        let input = b"<0033> Tj\n\t% Mission Statement\n\tET\n";
        let output = strip_pdf_comments(input);
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("ET"),
            "ET should be preserved after comment stripping"
        );
    }

    #[test]
    fn test_strip_pdf_comments_escaped_parens() {
        // An escaped `\)` must not close the string: the `%` after it is
        // still string content, not a comment (subset fonts routinely map
        // glyphs to `%` and to escaped parens in the same TJ array).
        let input = b"[ (a\\)b) 1 (%) 1 (c) ] TJ\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, input.to_vec());

        // Same for an escaped `\(` — must not open a phantom string that
        // shields a real comment.
        let input = b"(x\\(y) Tj % real comment\nET\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, b"(x\\(y) Tj  \nET\n");

        // Escaped backslash before a real close-paren: `\\` ends the escape,
        // the `)` does close the string, and the comment is stripped.
        let input = b"(x\\\\) Tj % comment\nET\n";
        let output = strip_pdf_comments(input);
        assert_eq!(output, b"(x\\\\) Tj  \nET\n");
    }

    #[test]
    fn oversized_content_stream_skips_extraction() {
        let mut content =
            Vec::with_capacity((super::super::content_decode::MAX_PAGE_OPERATIONS + 1) * 2);
        for _ in 0..=super::super::content_decode::MAX_PAGE_OPERATIONS {
            content.extend_from_slice(b"q\n");
        }
        content.extend_from_slice(b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET\n");
        let items = extract_simple_items(&content);
        assert!(
            items.is_empty(),
            "pages over the operator cap must not be decoded"
        );
    }

    // ── Rotated-run geometry ─────────────────────────────────────────────

    const UPRIGHT_BODY: &str = "BT /F1 12 Tf 72 700 Td (Body line one) Tj ET
BT /F1 12 Tf 72 686 Td (Body line two) Tj ET
BT /F1 12 Tf 72 672 Td (Body line three) Tj ET
";

    /// Three upright lines keep the page from being classified as rotated,
    /// so `extra_ops` is measured in plain page coordinates.
    fn upright_page_with(extra_ops: &str) -> Vec<TextItem> {
        extract_simple_items(format!("{UPRIGHT_BODY}{extra_ops}").as_bytes())
    }

    fn find_item<'a>(items: &'a [TextItem], text: &str) -> &'a TextItem {
        items
            .iter()
            .find(|item| item.text == text)
            .unwrap_or_else(|| {
                let found: Vec<&String> = items.iter().map(|i| &i.text).collect();
                panic!("no item {text:?} in {found:?}")
            })
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{what} = {actual}, expected {expected}"
        );
    }

    #[test]
    fn rotated_ccw_run_gets_tall_thin_box() {
        // A 20pt arXiv-style stamp reading bottom-to-top along the left
        // margin: 16 glyphs × 0.6em = 192pt of advance running up the page,
        // glyph tops facing left. Projecting the advance onto x used to
        // leave it `width == 0`.
        let items = upright_page_with("BT /F1 20 Tf 0 1 -1 0 32 200 Tm (arXiv:2301.00001) Tj ET");
        let stamp = find_item(&items, "arXiv:2301.00001");
        assert_close(stamp.rotation, 90.0, "rotation");
        assert!(!stamp.is_horizontal());
        assert_close(stamp.x, 12.0, "x");
        assert_close(stamp.y, 200.0, "y");
        assert_close(stamp.width, 20.0, "width");
        assert_close(stamp.height, 192.0, "height");
        assert_close(stamp.font_size, 20.0, "font_size");
        assert!(stamp.advance_known);

        // Upright text keeps its historical box: baseline y, em height,
        // advance width, no rotation.
        let body = find_item(&items, "Body line one");
        assert_eq!(body.rotation, 0.0);
        assert!(body.is_horizontal());
        assert_close(body.x, 72.0, "x");
        assert_close(body.y, 700.0, "y");
        assert_close(body.width, 13.0 * 7.2, "width");
        assert_close(body.height, 12.0, "height");
    }

    #[test]
    fn rotated_cw_run_box_hangs_below_its_start() {
        // Top-to-bottom text (270°): the advance runs down the page and the
        // glyph tops face right.
        let items = upright_page_with("BT /F1 10 Tf 0 -1 1 0 580 700 Tm (HEADER) Tj ET");
        let header = find_item(&items, "HEADER");
        assert_close(header.rotation, 270.0, "rotation");
        assert!(!header.is_horizontal());
        assert_close(header.x, 580.0, "x");
        assert_close(header.width, 10.0, "width");
        assert_close(header.y, 700.0 - 36.0, "y");
        assert_close(header.height, 36.0, "height");
    }

    #[test]
    fn upside_down_run_box_covers_its_glyphs() {
        let items = upright_page_with("BT /F1 10 Tf -1 0 0 -1 300 400 Tm (FLIP) Tj ET");
        let flip = find_item(&items, "FLIP");
        assert_close(flip.rotation, 180.0, "rotation");
        assert!(flip.is_horizontal());
        // 4 × 6pt of advance running left; the glyphs hang below the baseline.
        assert_close(flip.x, 276.0, "x");
        assert_close(flip.width, 24.0, "width");
        assert_close(flip.y, 390.0, "y");
        assert_close(flip.height, 10.0, "height");
    }

    /// A page whose only font is a dvips/PK-style Type3 font: `FontMatrix
    /// [1 0 0 -1 0 0]`, glyphs measured in device pixels (a 100-unit em,
    /// 50-unit advances), shown at 0.12pt per pixel through the text matrix.
    fn type3_doc_with_content(content: &[u8]) -> (lopdf::Document, lopdf::ObjectId) {
        use lopdf::{dictionary, Object, Stream};

        let mut doc = lopdf::Document::new();
        let widths: Vec<Object> = (0..=255).map(|_| 50.into()).collect();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "FontMatrix" => vec![1.into(), 0.into(), 0.into(), Object::Real(-1.0), 0.into(), 0.into()],
            "FontBBox" => vec![0.into(), Object::Integer(-25), 60.into(), 75.into()],
            "CharProcs" => dictionary! {},
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![
                    72.into(), Object::Name(b"H".to_vec()),
                    69.into(), Object::Name(b"E".to_vec()),
                    76.into(), Object::Name(b"L".to_vec()),
                    79.into(), Object::Name(b"O".to_vec()),
                ],
            },
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
            "Resources" => dictionary! {},
        });
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! { "T1" => Object::Reference(font_id) },
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

    #[test]
    fn dvips_type3_text_keeps_its_box_above_the_baseline() {
        // dvips: `/T1 1 Tf` with the pixel size in a y-flipped text matrix,
        // undone by the font's mirrored FontMatrix. The 100-pixel em renders
        // at 12pt; 5 glyphs × 50 pixels × 0.12pt = 30pt of advance.
        use crate::tounicode::FontCMaps;

        let (doc, page_id) =
            type3_doc_with_content(b"BT /T1 1 Tf 0.12 0 0 -0.12 100 500 Tm (HELLO) Tj ET");
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
        let hello = find_item(&items, "HELLO");
        assert_eq!(hello.rotation, 0.0);
        assert_close(hello.x, 100.0, "x");
        assert_close(hello.y, 500.0, "y");
        assert_close(hello.width, 30.0, "width");
        assert_close(hello.height, 12.0, "height");
    }

    #[test]
    fn mirrored_text_matrix_keeps_glyphs_above_the_baseline() {
        // `[-1 0 0 1]`: the advance runs towards -x but the glyphs still
        // stand on the baseline. 3 glyphs × 7.2pt = 21.6pt, running left. A
        // reflection has no rotation, so the run reports how its glyphs
        // stand — upright — and merging, decoration, and line grouping treat
        // it as the upright run it looks like.
        let items = upright_page_with("BT /F1 12 Tf -1 0 0 1 300 500 Tm (ABC) Tj ET");
        let abc = find_item(&items, "ABC");
        assert_close(abc.rotation, 0.0, "rotation");
        assert!(abc.is_upright());
        assert_close(abc.x, 300.0 - 21.6, "x");
        assert_close(abc.y, 500.0, "y");
        assert_close(abc.width, 21.6, "width");
        assert_close(abc.height, 12.0, "height");
    }

    #[test]
    fn negative_font_size_turns_the_run_around() {
        // `-12 Tf` negates the glyph matrix: the run reads towards -x with
        // its glyphs hanging below the baseline, exactly like a 180° turn,
        // and its size is still 12pt.
        let items = upright_page_with("BT /F1 -12 Tf 1 0 0 1 100 700 Tm (HELLO) Tj ET");
        let hello = find_item(&items, "HELLO");
        assert_close(hello.rotation, 180.0, "rotation");
        assert_close(hello.font_size, 12.0, "font_size");
        assert_close(hello.x, 64.0, "x");
        assert_close(hello.width, 36.0, "width");
        assert_close(hello.y, 688.0, "y");
        assert_close(hello.height, 12.0, "height");
        assert!(hello.advance_known);
    }

    #[test]
    fn negative_font_size_vertical_runs_vote_with_their_turned_direction() {
        // `-12 Tf` on a bottom-to-top matrix reads top-to-bottom: the page
        // vote must follow the items (270° before the turn), so the frame
        // turns clockwise and the runs come out upright — not the other way.
        let content = b"BT /F1 -12 Tf 0 1 -1 0 100 100 Tm (UP) Tj ET
BT /F1 -12 Tf 0 1 -1 0 130 100 Tm [(UP)] TJ ET
BT /F1 -12 Tf 0 1 -1 0 160 100 Tm (UP) ' ET";
        let (items, page_rotation) = extract_simple_page(content);
        assert_eq!(page_rotation, PageRotation::Cw);
        assert_eq!(items.len(), 3, "{items:?}");
        for item in &items {
            assert_close(item.rotation, 0.0, "rotation");
            assert_close(item.font_size, 12.0, "font_size");
        }
    }

    #[test]
    fn actual_text_span_votes_and_sizes_with_the_size_that_painted_it() {
        // Each span paints its glyphs at `-12 Tf` (reading top-to-bottom on
        // this bottom-to-top matrix) and then selects `12 Tf` before EMC. The
        // vote, the turn, and the size follow the painting size: the page
        // turns clockwise and the replacements come out upright at 12pt.
        let content = b"BT /F1 -12 Tf 0 1 -1 0 100 100 Tm /Span <</ActualText (Up) >> BDC (UP) Tj /F1 12 Tf EMC ET
BT /F1 -12 Tf 0 1 -1 0 130 100 Tm /Span <</ActualText (Up) >> BDC (UP) Tj /F1 12 Tf EMC ET";
        let (items, page_rotation) = extract_simple_page(content);
        assert_eq!(page_rotation, PageRotation::Cw);
        assert_eq!(items.len(), 2, "{items:?}");
        for item in &items {
            assert_eq!(item.text, "Up");
            assert_close(item.rotation, 0.0, "rotation");
            assert_close(item.font_size, 12.0, "font_size");
            assert!(item.advance_known);
        }
    }

    #[test]
    fn actual_text_span_captures_its_state_at_the_first_painted_glyph() {
        // An empty `Tj` and a numeric-only `TJ` at 12pt precede the glyphs,
        // which are painted at 24pt after the cursor moved 6pt: the
        // replacement takes the painting size and position, not the state of
        // the shows that painted nothing.
        let (items, _) = extract_simple_page(
            b"BT /F1 12 Tf 100 700 Td /Span <</ActualText (AB) >> BDC () Tj [-500] TJ /F1 24 Tf (AB) Tj EMC ET",
        );
        let ab = find_item(&items, "AB");
        assert_close(ab.font_size, 24.0, "font_size");
        assert_close(ab.x, 106.0, "x");
        assert_close(ab.width, 28.8, "width");
        assert!(ab.advance_known);
    }

    #[test]
    fn rotated_tj_run_boxes_cover_the_whole_advance() {
        // A vertical TJ with a 6em positioning gap: whether or not the gap
        // splits the array into sub-runs, every box sits on the em column
        // left of the baseline and together they span the full advance
        // (2 glyphs + 60pt gap + 2 glyphs = 84pt).
        let items = upright_page_with("BT /F1 10 Tf 0 1 -1 0 40 100 Tm [(AB) -6000 (CD)] TJ ET");
        let runs: Vec<&TextItem> = items.iter().filter(|i| i.font_size == 10.0).collect();
        assert!(!runs.is_empty());
        for run in &runs {
            assert_close(run.rotation, 90.0, "rotation");
            assert_close(run.x, 30.0, "x");
            assert_close(run.width, 10.0, "width");
        }
        let bottom = runs.iter().map(|r| r.y).fold(f32::INFINITY, f32::min);
        let top = runs
            .iter()
            .map(|r| r.y + r.height)
            .fold(f32::NEG_INFINITY, f32::max);
        assert_close(bottom, 100.0, "bottom");
        assert_close(top, 184.0, "top");
    }

    #[test]
    fn actual_text_on_rotated_run_gets_tall_box() {
        let items = upright_page_with(
            "BT /F1 10 Tf 0 1 -1 0 40 100 Tm /Span <</ActualText (Stamp) >> BDC (STAMP) Tj EMC ET",
        );
        let stamp = find_item(&items, "Stamp");
        assert_close(stamp.rotation, 90.0, "rotation");
        assert_close(stamp.x, 30.0, "x");
        assert_close(stamp.width, 10.0, "width");
        assert_close(stamp.y, 100.0, "y");
        assert_close(stamp.height, 30.0, "height");
    }

    #[test]
    fn actual_text_width_follows_scaled_text_matrix() {
        // `/F1 1 Tf` with the size carried by Tm ([12 0 0 12]): the advance
        // is in tm[0]-scaled units. The old tm[4]-delta × device-scale
        // formula applied the scale twice and reported a 432pt-wide "Hello".
        let items = upright_page_with(
            "BT /F1 1 Tf 12 0 0 12 72 500 Tm /Span <</ActualText (Hello) >> BDC (Hello) Tj EMC ET",
        );
        let hello = find_item(&items, "Hello");
        assert_eq!(hello.rotation, 0.0);
        assert_close(hello.x, 72.0, "x");
        assert_close(hello.y, 500.0, "y");
        assert_close(hello.width, 36.0, "width");
        assert_close(hello.height, 12.0, "height");
    }

    #[test]
    fn rotated_page_correction_keeps_real_advance_and_rebases_rotation() {
        let content = b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (WORLD) Tj ET";
        let items = extract_simple_items(content);
        let hello = find_item(&items, "HELLO");
        // Corrected frame: x = run start (old y), y = -(baseline x), width =
        // the real 5 × 7.2pt advance instead of a character-count estimate,
        // em height, and the dominant runs now read as horizontal.
        assert_eq!(hello.rotation, 0.0);
        assert!(hello.is_horizontal());
        assert_close(hello.x, 100.0, "x");
        assert_close(hello.y, -200.0, "y");
        assert_close(hello.width, 36.0, "width");
        assert_close(hello.height, 12.0, "height");
        let world = find_item(&items, "WORLD");
        assert_close(world.y, -240.0, "y");
    }

    fn extract_simple_page(content: &[u8]) -> (Vec<TextItem>, PageRotation) {
        use crate::tounicode::FontCMaps;

        let (doc, page_id) = simple_doc_with_content(content);
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
        (items, page_rotation)
    }

    #[test]
    fn clockwise_page_is_turned_so_its_runs_read_left_to_right() {
        // Top-to-bottom runs (Tm = [0 -1 1 0]): "HELLO" then "WORLD" run down
        // the page at x = 200, the next line sits to the LEFT at x = 180.
        // Turning the frame clockwise must keep reading order and stack the
        // lines top-down — the old fixed counter-clockwise turn mirrored
        // both.
        let content = b"BT /F1 12 Tf 0 -1 1 0 200 700 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 -1 1 0 200 650 Tm (WORLD) Tj ET
BT /F1 12 Tf 0 -1 1 0 180 700 Tm (SECOND) Tj ET";
        let (items, page_rotation) = extract_simple_page(content);
        assert_eq!(page_rotation, PageRotation::Cw);
        let hello = find_item(&items, "HELLO");
        let world = find_item(&items, "WORLD");
        let second = find_item(&items, "SECOND");
        for item in [hello, world, second] {
            assert_eq!(item.rotation, 0.0, "{}", item.text);
            assert!(item.is_horizontal());
            assert_close(item.height, 12.0, "height");
        }
        // Corrected frame: x = -(run end), y = baseline x, width = advance.
        assert_close(hello.x, -700.0, "x");
        assert_close(hello.y, 200.0, "y");
        assert_close(hello.width, 36.0, "width");
        assert_close(world.x, -650.0, "x");
        assert_close(world.y, 200.0, "y");
        assert!(hello.x < world.x, "reading order must be preserved");
        assert_close(second.x, -700.0, "x");
        assert_close(second.y, 180.0, "y");
        assert!(second.y < hello.y, "the next line must stack below");
    }

    #[test]
    fn diagonal_pages_are_not_turned() {
        // Curved titles, watermarks, and callouts: runs 30° or 60° off the
        // axes carry no cardinal direction to turn the page into, so they
        // abstain from the vote and keep their own angle.
        for (matrix, angle) in [
            ("0.866 0.5 -0.5 0.866", 30.0),
            ("0.5 0.866 -0.866 0.5", 60.0),
        ] {
            let content = format!(
                "BT /F1 12 Tf {matrix} 100 500 Tm (ONE) Tj ET
BT /F1 12 Tf {matrix} 100 560 Tm (TWO) Tj ET
BT /F1 12 Tf {matrix} 100 620 Tm (THREE) Tj ET"
            );
            let (items, page_rotation) = extract_simple_page(content.as_bytes());
            assert_eq!(page_rotation, PageRotation::Upright, "{angle}°");
            assert_eq!(items.len(), 3);
            for item in &items {
                assert_close(item.rotation, angle, "rotation");
            }
        }
    }

    #[test]
    fn evenly_split_vertical_runs_leave_the_page_in_its_own_frame() {
        // One run reads up, one reads down: neither direction dominates, and
        // turning either way would mirror half the page, so nothing turns.
        let content = b"BT /F1 12 Tf 0 1 -1 0 100 100 Tm (UP) Tj ET
BT /F1 12 Tf 0 -1 1 0 300 700 Tm (DOWN) Tj ET";
        let (items, page_rotation) = extract_simple_page(content);
        assert_eq!(page_rotation, PageRotation::Upright);
        assert_close(find_item(&items, "UP").rotation, 90.0, "rotation");
        assert_close(find_item(&items, "DOWN").rotation, 270.0, "rotation");
    }

    #[test]
    fn upright_stray_on_clockwise_page_reports_90() {
        let content = b"BT /F1 12 Tf 0 -1 1 0 200 700 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 -1 1 0 200 650 Tm (WORLD) Tj ET
BT /F1 12 Tf 0 -1 1 0 180 700 Tm (SECOND) Tj ET
BT /F1 10 Tf 300 30 Td (7) Tj ET";
        let (items, page_rotation) = extract_simple_page(content);
        assert_eq!(page_rotation, PageRotation::Cw);
        let seven = find_item(&items, "7");
        assert_close(seven.rotation, 90.0, "rotation");
        assert!(!seven.is_horizontal());
        // Old box (300, 30, 6 × 10): x = -(old top), y = old x, swapped.
        assert_close(seven.x, -40.0, "x");
        assert_close(seven.y, 300.0, "y");
        assert_close(seven.width, 10.0, "width");
        assert_close(seven.height, 6.0, "height");
    }

    #[test]
    fn image_placeholders_keep_zero_rotation_on_rotated_pages() {
        let text = |x: f32, y: f32| TextItem {
            baseline_shift: 0.0,
            text: "run".to_string(),
            x,
            y,
            width: 12.0,
            height: 36.0,
            rotation: 90.0,
            advance_known: true,
            font: "Helvetica".to_string(),
            font_tag: "F1".to_string(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        };
        let mut image = text(50.0, 50.0);
        image.text = "[Image: Im0]".to_string();
        image.item_type = ItemType::Image;
        image.rotation = 0.0;
        image.width = 100.0;
        image.height = 40.0;
        let votes = RotationVotes {
            horizontal: 0,
            ccw: 3,
            cw: 0,
        };
        let (items, _, _, rotation) = correct_rotated_page(
            vec![
                text(188.0, 100.0),
                text(228.0, 100.0),
                text(268.0, 100.0),
                image,
            ],
            Vec::new(),
            Vec::new(),
            &votes,
        );
        assert_eq!(rotation, PageRotation::Ccw);
        let image = items.iter().find(|i| i.text.starts_with("[Image")).unwrap();
        assert_eq!(image.rotation, 0.0);
        // The box still turns with the page: x = old y, y = -(old right edge).
        assert_eq!(
            (image.x, image.y, image.width, image.height),
            (50.0, -150.0, 40.0, 100.0)
        );
        assert!(items
            .iter()
            .filter(|i| i.text == "run")
            .all(|i| i.rotation == 0.0));
    }

    #[test]
    fn lone_rotated_tj_split_at_a_gap_does_not_turn_the_page() {
        // One rotated TJ with a 6em positioning gap yields two items but is
        // a single show operator: a lone stamp, not a rotated page.
        let (items, page_rotation) =
            extract_simple_page(b"BT /F1 10 Tf 0 1 -1 0 40 100 Tm [(AB) -6000 (CD)] TJ ET");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(page_rotation, PageRotation::Upright);
        assert!(items.iter().all(|i| (i.rotation - 90.0).abs() < 1e-3));
    }

    #[test]
    fn whitespace_only_runs_do_not_vote_on_page_rotation() {
        // A rotated word plus a rotated whitespace-only run: the latter
        // produces no item, so it must not be the second vote that turns
        // the page.
        let (items, page_rotation) = extract_simple_page(
            b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (   ) Tj ET",
        );
        assert_eq!(page_rotation, PageRotation::Upright);
        let hello = find_item(&items, "HELLO");
        assert_close(hello.rotation, 90.0, "rotation");
    }

    #[test]
    fn single_rotated_run_next_to_an_image_does_not_turn_the_page() {
        let run = TextItem {
            baseline_shift: 0.0,
            text: "stamp".to_string(),
            x: 188.0,
            y: 100.0,
            width: 12.0,
            height: 36.0,
            rotation: 90.0,
            advance_known: true,
            font: "Helvetica".to_string(),
            font_tag: "F1".to_string(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        };
        let mut image = run.clone();
        image.text = "[Image: Im0]".to_string();
        image.item_type = ItemType::Image;
        image.rotation = 0.0;
        let votes = RotationVotes {
            horizontal: 0,
            ccw: 1,
            cw: 0,
        };
        let (items, _, _, rotation) =
            correct_rotated_page(vec![run.clone(), image], Vec::new(), Vec::new(), &votes);
        assert_eq!(rotation, PageRotation::Upright);
        let kept = items.iter().find(|i| i.text == "stamp").unwrap();
        assert_eq!((kept.x, kept.y, kept.rotation), (run.x, run.y, 90.0));
    }

    #[test]
    fn upright_stray_on_rotated_page_becomes_vertical_in_corrected_frame() {
        // A page number set upright on a page whose text is rotated 90°:
        // after correction it reads top-to-bottom and its box turns with it
        // (old box (300, 30, 6 × 10) → x = old y, y = -(old right edge)).
        let content = b"BT /F1 12 Tf 0 1 -1 0 200 100 Tm (HELLO) Tj ET
BT /F1 12 Tf 0 1 -1 0 240 100 Tm (WORLD) Tj ET
BT /F1 12 Tf 0 1 -1 0 280 100 Tm (AGAIN) Tj ET
BT /F1 10 Tf 300 30 Td (7) Tj ET";
        let items = extract_simple_items(content);
        let seven = find_item(&items, "7");
        assert_close(seven.rotation, 270.0, "rotation");
        assert!(!seven.is_horizontal());
        assert_close(seven.x, 30.0, "x");
        assert_close(seven.y, -306.0, "y");
        assert_close(seven.width, 10.0, "width");
        assert_close(seven.height, 6.0, "height");
    }
}
