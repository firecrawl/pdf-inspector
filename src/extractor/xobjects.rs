//! Form XObject and image XObject extraction.

use super::fonts::descriptor_style_flags;
use crate::text_utils::{effective_font_size, expand_ligatures, is_bold_font, is_italic_font};
use crate::tounicode::FontCMaps;
use crate::types::{ItemType, TextItem};
use lopdf::{Document, Encoding, Object, ObjectId};
use std::collections::HashMap;

use super::fonts::{
    build_font_encodings, build_font_widths, build_type3_scales, compute_string_width_ts,
    extract_text_from_operand, get_font_file2_obj_num, get_operand_bytes, CMapDecisionCache,
    FontStyleCache,
};
use super::{get_number, image_bbox_from_ctm, multiply_matrices};

const MAX_FORM_XOBJECT_DEPTH: u8 = 5;

pub(crate) enum XObjectType {
    Image,
    Form(ObjectId),
}

/// Get XObjects from page resources, categorized by type
pub(crate) fn get_page_xobjects(
    doc: &Document,
    page_id: ObjectId,
) -> std::collections::HashMap<String, XObjectType> {
    let mut xobject_types = std::collections::HashMap::new();

    // Try to get the page dictionary
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        // Get Resources dictionary
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
                                    xobject_types.insert(name_str, XObjectType::Image);
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

/// Extract text items from a Form XObject
pub(crate) fn extract_form_xobject_text(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
) -> Vec<TextItem> {
    extract_form_xobject_text_inner(
        doc,
        form_id,
        page_num,
        font_cmaps,
        parent_ctm,
        cmap_decisions,
        style_cache,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_form_xobject_text_inner(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
    depth: u8,
) -> Vec<TextItem> {
    use lopdf::content::Content;

    let mut items = Vec::new();

    // Get the Form XObject stream
    let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
        return items;
    };

    // Decompress the content stream (fall back to raw bytes for uncompressed streams)
    let content_data = match stream.decompressed_content() {
        Ok(data) => data,
        Err(_) => stream.content.clone(),
    };

    // Decode the content stream
    let Ok(content) = Content::decode(&content_data) else {
        return items;
    };

    // Get fonts from the Form's Resources
    let form_fonts = get_form_fonts(doc, &stream.dict);
    let (font_encodings, _has_gid_fonts) = build_font_encodings(doc, &form_fonts, font_cmaps);

    // Build font width info for the form
    let font_widths = build_font_widths(doc, &form_fonts);
    let type3_scales = build_type3_scales(doc, &form_fonts);

    // Build font base names and ToUnicode refs for the form
    let mut font_base_names: HashMap<String, String> = HashMap::new();
    let mut font_tounicode_refs: HashMap<String, u32> = HashMap::new();
    let mut inline_cmaps: HashMap<String, crate::tounicode::CMapEntry> = HashMap::new();

    let mut font_style_flags: HashMap<String, (bool, bool)> = HashMap::new();
    for (font_name, font_dict) in &form_fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        let style = descriptor_style_flags(doc, font_dict, style_cache);
        if style != (false, false) {
            font_style_flags.insert(resource_name.clone(), style);
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
    let mut in_text_block = false;
    let mut fill_is_white = false;
    let mut ctm = base_ctm;

    // Text state (Tc/Tw/TL/Tf) is part of the graphics state and must be
    // saved/restored by q/Q alongside the CTM.
    #[derive(Clone)]
    struct GraphicsState {
        ctm: [f32; 6],
        char_spacing: f32,
        word_spacing: f32,
        text_leading: f32,
        current_font: String,
        current_font_size: f32,
    }
    let mut ctm_stack: Vec<GraphicsState> = Vec::new();

    for op in &content.operations {
        match op.operator.as_str() {
            "q" => {
                ctm_stack.push(GraphicsState {
                    ctm,
                    char_spacing,
                    word_spacing,
                    text_leading,
                    current_font: current_font.clone(),
                    current_font_size,
                });
            }
            "Q" => {
                if let Some(saved) = ctm_stack.pop() {
                    ctm = saved.ctm;
                    char_spacing = saved.char_spacing;
                    word_spacing = saved.word_spacing;
                    text_leading = saved.text_leading;
                    current_font = saved.current_font;
                    current_font_size = saved.current_font_size;
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
                if !op.operands.is_empty() {
                    if let Ok(name) = op.operands[0].as_name() {
                        let xobj_name = String::from_utf8_lossy(name).to_string();
                        match form_xobjects.get(&xobj_name) {
                            Some(XObjectType::Form(nested_id)) => {
                                if depth < MAX_FORM_XOBJECT_DEPTH {
                                    let nested_items = extract_form_xobject_text_inner(
                                        doc,
                                        *nested_id,
                                        page_num,
                                        font_cmaps,
                                        &ctm,
                                        cmap_decisions,
                                        style_cache,
                                        depth + 1,
                                    );
                                    items.extend(nested_items);
                                }
                            }
                            Some(XObjectType::Image) => {
                                // Mirror the top-level Image-XObject emission
                                // in content_stream.rs so figures embedded
                                // inside Form XObjects (common in print-to-PDF
                                // workflows) aren't silently dropped.
                                let (x, y, width, height) = image_bbox_from_ctm(&ctm);
                                items.push(TextItem {
                                    text: format!("[Image: {}]", xobj_name),
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
                                    item_type: ItemType::Image,
                                    mcid: None,
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
                    if fill_is_white {
                        if let Some(font_info) = font_widths.get(&current_font) {
                            if let Some(raw_bytes) = get_operand_bytes(show_operand) {
                                let w_ts = compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    char_spacing,
                                    word_spacing,
                                );
                                text_matrix[4] += w_ts * text_matrix[0];
                                text_matrix[5] += w_ts * text_matrix[1];
                            }
                        }
                        continue;
                    }
                    if let Some(text) = extract_text_from_operand(
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
                    ) {
                        let combined = multiply_matrices(&text_matrix, &ctm);
                        let rendered_size = effective_font_size(current_font_size, &combined)
                            * type3_scales.get(&current_font).copied().unwrap_or(1.0);
                        let (x, y) = (combined[4], combined[5]);
                        let width = if let Some(font_info) = font_widths.get(&current_font) {
                            if let Some(raw_bytes) = get_operand_bytes(show_operand) {
                                let w_ts = compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    char_spacing,
                                    word_spacing,
                                );
                                text_matrix[4] += w_ts * text_matrix[0];
                                text_matrix[5] += w_ts * text_matrix[1];
                                (w_ts * (text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2])).abs()
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        };
                        // Only create text item for non-whitespace; whitespace
                        // still advances the text matrix above so gap detection works
                        if !text.trim().is_empty() {
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            items.push(TextItem {
                                text: expand_ligatures(&text),
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font) || desc_bold,
                                is_italic: is_italic_font(base_font) || desc_italic,
                                is_underline: false,
                                is_strikeout: false,
                                item_type: ItemType::Text,
                                mcid: None,
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

                        let space_threshold = if let Some(fi) = font_info {
                            let space_em = fi.space_width as f32 * fi.units_scale;
                            let threshold = space_em * 1000.0 * 0.4;
                            threshold.max(80.0)
                        } else {
                            120.0
                        };
                        let column_gap_threshold = space_threshold * 4.0;

                        let mut sub_items: Vec<(String, f32, f32)> = Vec::new();
                        let mut current_text = String::new();
                        let mut sub_start_width_ts: f32 = 0.0;
                        let mut total_width_ts: f32 = 0.0;
                        for element in array {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    if !fill_is_white
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !fill_is_white
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
                                    if !fill_is_white
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !fill_is_white
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
                            }
                            if !fill_is_white {
                                if let Some(text) = extract_text_from_operand(
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
                                ) {
                                    current_text.push_str(&text);
                                }
                            }
                        }
                        if !fill_is_white && !current_text.trim().is_empty() {
                            sub_items.push((current_text, sub_start_width_ts, total_width_ts));
                        }
                        if !sub_items.is_empty() {
                            let combined = multiply_matrices(&text_matrix, &ctm);
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
                            for (text, start_w, end_w) in &sub_items {
                                let offset_tm = [
                                    text_matrix[0],
                                    text_matrix[1],
                                    text_matrix[2],
                                    text_matrix[3],
                                    text_matrix[4] + start_w * text_matrix[0],
                                    text_matrix[5] + start_w * text_matrix[1],
                                ];
                                let combined_mat = multiply_matrices(&offset_tm, &ctm);
                                let (x, y) = (combined_mat[4], combined_mat[5]);
                                let width = if font_info.is_some() {
                                    ((end_w - start_w) * scale_x).abs()
                                } else {
                                    0.0
                                };
                                items.push(TextItem {
                                    text: expand_ligatures(text),
                                    x,
                                    y,
                                    width,
                                    height: rendered_size,
                                    font: current_font.clone(),
                                    font_size: rendered_size,
                                    page: page_num,
                                    is_bold: is_bold_font(base_font) || desc_bold,
                                    is_italic: is_italic_font(base_font) || desc_italic,
                                    is_underline: false,
                                    is_strikeout: false,
                                    item_type: ItemType::Text,
                                    mcid: None,
                                });
                            }
                        }
                        // Always advance text matrix
                        if font_info.is_some() {
                            text_matrix[4] += total_width_ts * text_matrix[0];
                            text_matrix[5] += total_width_ts * text_matrix[1];
                        }
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Get fonts from a Form XObject's Resources
pub(crate) fn get_form_fonts<'a>(
    doc: &'a Document,
    form_dict: &lopdf::Dictionary,
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
        if let Ok(obj_ref) = value.as_reference() {
            if let Ok(dict) = doc.get_dictionary(obj_ref) {
                fonts.insert(name.clone(), dict);
            }
        }
    }

    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream};

    /// Build a document whose page draws *all* of its content through a single
    /// Form XObject — the shape emitted by print-to-PDF producers like PDFlib,
    /// where the page stream itself is only `q /X1 Do Q`.
    fn doc_with_form_content(form_content: &[u8]) -> (Document, ObjectId) {
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
        (doc, page_id)
    }

    fn form_items(form_content: &[u8]) -> Vec<TextItem> {
        let (doc, page_id) = doc_with_form_content(form_content);
        let font_cmaps = FontCMaps::from_doc(&doc);
        let ((items, _, _), _, _, _) = super::super::content_stream::extract_page_text_items(
            &doc,
            page_id,
            1,
            &font_cmaps,
            false,
            &mut FontStyleCache::new(),
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
}
