//! Glyph-level reading of a page for the text-layer math reader
//! ([\`crate::math\`]): every painted glyph with its decoded text, raw code,
//! box, baseline and rendered size, in the sheet frame's top-left space.
//! The item extractor merges glyphs into runs; formula structure needs the
//! glyphs one by one, so this walker shows each code on its own.

use super::content_decode::{decode_content_bounded, MAX_PAGE_CONTENT_BYTES, MAX_PAGE_OPERATIONS};
use super::content_stream::strip_pdf_comments;
use super::fonts::{
    build_font_encodings, build_font_kinds, build_font_widths, build_type3_scales,
    compute_string_width_ts, extract_text_from_operand, get_font_file2_obj_num, CMapDecisionCache,
    FontStyleCache,
};
use super::page_box::PageBox;
use super::xobjects::{get_form_fonts, get_form_xobjects, get_page_xobjects, XObjectType};
use super::{get_number, multiply_matrices};
use crate::text_utils::effective_font_size;
use crate::tounicode::{CMapEntry, FontCMaps};
use crate::types::{FontWidthInfo, PageFontEncodings, PageFontKinds, PageFontWidths};
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId, StringFormat};
use std::collections::{BTreeMap, HashMap};

const MAX_FORM_DEPTH: u8 = 5;
const MAX_WALK_OPERATIONS: usize = 2_000_000;

/// One painted glyph. Coordinates are PDF points in the sheet frame's
/// top-left space (\`y\` down); \`base\` is the baseline \`y\`.
#[derive(Debug, Clone)]
pub(crate) struct MathGlyph {
    pub(crate) text: String,
    pub(crate) code: u32,
    pub(crate) font: String,
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    pub(crate) base: f32,
    pub(crate) size: f32,
    pub(crate) invisible: bool,
}

#[derive(Clone)]
struct GraphicsState {
    ctm: [f32; 6],
    font: String,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    leading: f32,
    rise: f32,
    render_mode: i64,
}

struct FontContext<'a> {
    encodings: PageFontEncodings,
    widths: PageFontWidths,
    kinds: PageFontKinds,
    type3_scales: HashMap<String, f32>,
    base_names: HashMap<String, String>,
    tounicode_refs: HashMap<String, u32>,
    inline_cmaps: HashMap<String, CMapEntry>,
    encoding_cache: HashMap<String, Encoding<'a>>,
}

impl<'a> FontContext<'a> {
    fn new(
        doc: &'a Document,
        fonts: &BTreeMap<Vec<u8>, &'a Dictionary>,
        font_cmaps: &FontCMaps,
        style_cache: &mut FontStyleCache,
    ) -> Self {
        let (encodings, _) = build_font_encodings(doc, fonts, font_cmaps, style_cache);
        let widths = build_font_widths(doc, fonts, style_cache);
        let mut base_names = HashMap::new();
        let mut tounicode_refs = HashMap::new();
        let mut inline_cmaps = HashMap::new();
        let mut encoding_cache = HashMap::new();
        for (font_name, font_dict) in fonts {
            let resource_name = String::from_utf8_lossy(font_name).to_string();
            if let Ok(name) = font_dict.get(b"BaseFont").and_then(Object::as_name) {
                base_names.insert(
                    resource_name.clone(),
                    String::from_utf8_lossy(name).to_string(),
                );
            }
            match font_dict.get(b"ToUnicode") {
                Ok(Object::Reference(obj_ref)) => {
                    tounicode_refs.insert(resource_name.clone(), obj_ref.0);
                }
                Ok(Object::Stream(s)) => {
                    let data = s
                        .decompressed_content()
                        .unwrap_or_else(|_| s.content.clone());
                    if let Some(entry) =
                        crate::tounicode::build_cmap_entry_from_stream(&data, font_dict, doc, 0)
                    {
                        inline_cmaps.insert(resource_name.clone(), entry);
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    if let Some(obj_num) = get_font_file2_obj_num(doc, font_dict) {
                        tounicode_refs.insert(resource_name.clone(), obj_num);
                    }
                }
            }
            if let Ok(enc) = font_dict.get_font_encoding(doc) {
                encoding_cache.insert(resource_name, enc);
            }
        }
        FontContext {
            encodings,
            widths,
            kinds: build_font_kinds(fonts),
            type3_scales: build_type3_scales(doc, fonts),
            base_names,
            tounicode_refs,
            inline_cmaps,
            encoding_cache,
        }
    }
}

struct Walker<'a, 'b> {
    doc: &'a Document,
    font_cmaps: &'b FontCMaps,
    style_cache: &'b mut FontStyleCache,
    cmap_decisions: CMapDecisionCache,
    operations: usize,
    glyphs: Vec<MathGlyph>,
}

/// Every glyph painted on the page, in the sheet frame's top-left space of
/// \`page_box\`. Glyphs on a baseline that is not upright left-to-right are
/// left out.
pub(crate) fn page_math_glyphs(
    doc: &Document,
    page_id: ObjectId,
    page_box: PageBox,
    font_cmaps: &FontCMaps,
    style_cache: &mut FontStyleCache,
) -> Vec<MathGlyph> {
    let Ok(data) = doc.get_page_content_with_limit(page_id, MAX_PAGE_CONTENT_BYTES) else {
        return Vec::new();
    };
    let fonts = doc.get_page_fonts(page_id).unwrap_or_default();
    let xobjects = get_page_xobjects(doc, page_id);
    let mut walker = Walker {
        doc,
        font_cmaps,
        style_cache,
        cmap_decisions: CMapDecisionCache::new(),
        operations: 0,
        glyphs: Vec::new(),
    };
    let ctx = FontContext::new(doc, &fonts, font_cmaps, walker.style_cache);
    let state = GraphicsState {
        ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        font: String::new(),
        font_size: 0.0,
        char_spacing: 0.0,
        word_spacing: 0.0,
        horizontal_scale: 1.0,
        leading: 0.0,
        rise: 0.0,
        render_mode: 0,
    };
    walker.walk(&data, &ctx, &xobjects, state, 0);
    let mut glyphs = walker.glyphs;
    for g in &mut glyphs {
        g.x0 -= page_box.x0;
        g.x1 -= page_box.x0;
        g.base = page_box.y1 - g.base;
    }
    glyphs
}

impl<'a> Walker<'a, '_> {
    fn walk(
        &mut self,
        data: &[u8],
        ctx: &FontContext<'a>,
        xobjects: &HashMap<String, XObjectType>,
        initial: GraphicsState,
        depth: u8,
    ) {
        let data = strip_pdf_comments(data);
        let Ok(Some(content)) = decode_content_bounded(&data, MAX_PAGE_OPERATIONS) else {
            return;
        };
        let mut state = initial;
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut tlm = tm;
        for op in &content.operations {
            self.operations += 1;
            if self.operations > MAX_WALK_OPERATIONS {
                return;
            }
            let num = |i: usize| op.operands.get(i).and_then(get_number);
            match op.operator.as_str() {
                "q" => stack.push(state.clone()),
                "Q" => {
                    if let Some(saved) = stack.pop() {
                        state = saved;
                    }
                }
                "cm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (num(0), num(1), num(2), num(3), num(4), num(5))
                    {
                        state.ctm = multiply_matrices(&[a, b, c, d, e, f], &state.ctm);
                    }
                }
                "BT" => {
                    tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                    tlm = tm;
                }
                "Tf" => {
                    if let Some(Ok(name)) = op.operands.first().map(Object::as_name) {
                        state.font = String::from_utf8_lossy(name).to_string();
                    }
                    if let Some(size) = num(1) {
                        state.font_size = size;
                    }
                }
                "Tc" => state.char_spacing = num(0).unwrap_or(state.char_spacing),
                "Tw" => state.word_spacing = num(0).unwrap_or(state.word_spacing),
                "Tz" => {
                    if let Some(scale) = num(0) {
                        state.horizontal_scale = scale / 100.0;
                    }
                }
                "TL" => state.leading = num(0).unwrap_or(state.leading),
                "Ts" => state.rise = num(0).unwrap_or(state.rise),
                "Tr" => {
                    if let Some(Object::Integer(mode)) = op.operands.first() {
                        state.render_mode = *mode;
                    }
                }
                "Td" | "TD" => {
                    if let (Some(tx), Some(ty)) = (num(0), num(1)) {
                        if op.operator == "TD" {
                            state.leading = -ty;
                        }
                        tlm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, tx, ty], &tlm);
                        tm = tlm;
                    }
                }
                "Tm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (num(0), num(1), num(2), num(3), num(4), num(5))
                    {
                        tlm = [a, b, c, d, e, f];
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, 0.0, -state.leading], &tlm);
                    tm = tlm;
                }
                "Tj" => {
                    if let Some(Object::String(bytes, _)) = op.operands.first() {
                        self.show(bytes, ctx, &state, &mut tm);
                    }
                }
                "'" | "\"" => {
                    let text_index = if op.operator == "'" { 0 } else { 2 };
                    if op.operator == "\"" {
                        state.word_spacing = num(0).unwrap_or(state.word_spacing);
                        state.char_spacing = num(1).unwrap_or(state.char_spacing);
                    }
                    tlm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, 0.0, -state.leading], &tlm);
                    tm = tlm;
                    if let Some(Object::String(bytes, _)) = op.operands.get(text_index) {
                        self.show(bytes, ctx, &state, &mut tm);
                    }
                }
                "TJ" => {
                    let Some(Ok(array)) = op.operands.first().map(Object::as_array) else {
                        continue;
                    };
                    for element in array {
                        match element {
                            Object::String(bytes, _) => self.show(bytes, ctx, &state, &mut tm),
                            other => {
                                if let Some(adjust) = get_number(other) {
                                    let tx =
                                        -adjust / 1000.0 * state.font_size * state.horizontal_scale;
                                    tm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &tm);
                                }
                            }
                        }
                    }
                }
                "Do" => {
                    if depth >= MAX_FORM_DEPTH {
                        continue;
                    }
                    let Some(Ok(name)) = op.operands.first().map(Object::as_name) else {
                        continue;
                    };
                    let name = String::from_utf8_lossy(name).to_string();
                    let Some(XObjectType::Form(form_id)) = xobjects.get(&name) else {
                        continue;
                    };
                    self.walk_form(*form_id, ctx, &state, depth);
                }
                _ => {}
            }
        }
    }

    fn walk_form(
        &mut self,
        form_id: ObjectId,
        ctx: &FontContext<'a>,
        state: &GraphicsState,
        depth: u8,
    ) {
        let doc = self.doc;
        let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
            return;
        };
        let data = match stream.decompressed_content_with_limit(MAX_PAGE_CONTENT_BYTES) {
            Ok(data) => data,
            Err(_) if stream.content.len() > MAX_PAGE_CONTENT_BYTES => return,
            Err(_) => stream.content.clone(),
        };
        let mut inner = state.clone();
        if let Ok(matrix) = stream.dict.get(b"Matrix").and_then(Object::as_array) {
            let m: Vec<f32> = matrix.iter().filter_map(get_number).collect();
            if m.len() == 6 {
                inner.ctm = multiply_matrices(&[m[0], m[1], m[2], m[3], m[4], m[5]], &inner.ctm);
            }
        }
        let form_fonts = get_form_fonts(doc, &stream.dict);
        let form_xobjects = get_form_xobjects(doc, &stream.dict);
        if form_fonts.is_empty() {
            self.walk(&data, ctx, &form_xobjects, inner, depth + 1);
        } else {
            let form_ctx = FontContext::new(doc, &form_fonts, self.font_cmaps, self.style_cache);
            self.walk(&data, &form_ctx, &form_xobjects, inner, depth + 1);
        }
    }

    fn show(
        &mut self,
        bytes: &[u8],
        ctx: &FontContext<'a>,
        state: &GraphicsState,
        tm: &mut [f32; 6],
    ) {
        let font = state.font.as_str();
        let width_info: Option<&FontWidthInfo> = ctx.widths.get(font);
        let code_len = if width_info.is_some_and(|info| info.is_cid) {
            2
        } else {
            1
        };
        let base_name = ctx.base_names.get(font).map(String::as_str);
        let label = base_name
            .filter(|n| !n.is_empty())
            .unwrap_or(font)
            .to_string();
        let size_scale = ctx.type3_scales.get(font).copied().unwrap_or(1.0);
        let invisible = matches!(state.render_mode, 3 | 7);
        for code_bytes in bytes.chunks(code_len) {
            let text = extract_text_from_operand(
                &Object::String(code_bytes.to_vec(), StringFormat::Literal),
                font,
                base_name,
                self.font_cmaps,
                &ctx.tounicode_refs,
                &ctx.inline_cmaps,
                &ctx.encodings,
                &ctx.encoding_cache,
                &mut self.cmap_decisions,
                &ctx.widths,
                &ctx.kinds,
            )
            .map(|(text, _)| text)
            .unwrap_or_default();
            let code = code_bytes
                .iter()
                .fold(0u32, |acc, &b| (acc << 8) | b as u32);
            let glyph_width = match width_info {
                Some(info) => compute_string_width_ts(code_bytes, info, state.font_size, 0.0, 0.0),
                None => 0.5 * state.font_size,
            };
            let is_space = code_len == 1 && code == 0x20;
            let advance = (glyph_width
                + state.char_spacing
                + if is_space { state.word_spacing } else { 0.0 })
                * state.horizontal_scale;
            let render = [
                state.font_size * state.horizontal_scale,
                0.0,
                0.0,
                state.font_size,
                0.0,
                state.rise,
            ];
            let text_to_user = multiply_matrices(tm, &state.ctm);
            let glyph_matrix = multiply_matrices(&render, &text_to_user);
            let upright = text_to_user[0] > 0.0
                && text_to_user[1].abs() <= 0.01 * text_to_user[0]
                && text_to_user[2].abs() <= 0.01 * text_to_user[3].abs()
                && text_to_user[3] > 0.0;
            if upright && state.font_size != 0.0 {
                let x0 = glyph_matrix[4];
                let x1 = x0 + glyph_width * state.horizontal_scale * text_to_user[0];
                self.glyphs.push(MathGlyph {
                    text,
                    code,
                    font: label.clone(),
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    base: glyph_matrix[5],
                    size: effective_font_size(state.font_size, &text_to_user) * size_scale,
                    invisible,
                });
            }
            *tm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, advance, 0.0], tm);
        }
    }
}
