//! Glyph-level reading of a page for the text-layer math reader
//! ([`crate::math`]): every painted glyph with its decoded text, raw code,
//! advance, baseline and rendered size. The item extractor merges glyphs
//! into runs; formula structure needs the glyphs one by one, so this walker
//! shows each code on its own.

use super::content_decode::{decode_content_bounded, MAX_PAGE_CONTENT_BYTES, MAX_PAGE_OPERATIONS};
use super::content_stream::strip_pdf_comments;
use super::fonts::{
    build_font_encodings, build_font_kinds, build_font_widths, build_type3_scales,
    compute_string_width_ts, extract_text_from_operand, get_font_file2_obj_num, CMapDecisionCache,
    FontStyleCache,
};
use super::page_box::PageBox;
use super::xobjects::{collect_xobjects_from_dict, get_form_fonts, get_form_xobjects, XObjectType};
use super::{get_number, multiply_matrices};
use crate::text_utils::effective_font_size;
use crate::tounicode::{CMapEntry, FontCMaps};
use crate::types::{FontWidthInfo, PageFontEncodings, PageFontKinds, PageFontWidths};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId, StringFormat};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

const MAX_FORM_DEPTH: u8 = 5;
const MAX_WALK_OPERATIONS: usize = 2_000_000;
const MAX_FORM_INVOCATIONS: usize = 10_000;

/// One painted glyph in the page's reading frame: PDF points, `y` down,
/// turned so the page's dominant baseline direction reads left to right
/// (see [`ReadingFrame`]). `base` is the baseline `y`.
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
    /// The font carries a ToUnicode map, so `text` is what it declares.
    pub(crate) to_unicode: bool,
}

/// The frame a page's glyphs are reported in: user space turned by
/// `quarter_turns` quarter turns clockwise, so that text running in the
/// page's dominant direction (up the sheet on a turned landscape page, say)
/// reads left to right, with `y` flipped to grow downwards.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadingFrame {
    quarter_turns: u8,
    page_box: PageBox,
}

impl ReadingFrame {
    fn turn(&self, x: f32, y: f32) -> (f32, f32) {
        let (x, y) = match self.quarter_turns {
            1 => (y, -x),
            2 => (-x, -y),
            3 => (-y, x),
            _ => (x, y),
        };
        (x, -y)
    }

    /// A rect in the sheet frame's top-left space (`[x1, y1, x2, y2]` on the
    /// visible page box, `y` down) as the bounding box of its corners in
    /// this frame.
    pub(crate) fn sheet_rect(&self, rect: [f32; 4]) -> [f32; 4] {
        let b = self.page_box;
        let corners = [
            (rect[0], rect[1]),
            (rect[2], rect[1]),
            (rect[0], rect[3]),
            (rect[2], rect[3]),
        ]
        .map(|(x, y)| self.turn(x + b.x0, b.y1 - y));
        let xs = corners.map(|c| c.0);
        let ys = corners.map(|c| c.1);
        [
            xs.iter().copied().fold(f32::INFINITY, f32::min),
            ys.iter().copied().fold(f32::INFINITY, f32::min),
            xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        ]
    }
}

/// A glyph as painted: origin and advance in user space, and the quarter
/// turn of its baseline.
struct PaintedGlyph {
    text: String,
    code: u32,
    font: String,
    origin: (f32, f32),
    advance: f32,
    quarter_turns: u8,
    size: f32,
    invisible: bool,
    to_unicode: bool,
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
    fonts: BTreeMap<Vec<u8>, &'a Dictionary>,
    encodings: PageFontEncodings,
    widths: PageFontWidths,
    kinds: PageFontKinds,
    type3_scales: HashMap<String, f32>,
    base_names: HashMap<String, String>,
    tounicode_refs: HashMap<String, u32>,
    inline_cmaps: HashMap<String, CMapEntry>,
    encoding_cache: HashMap<String, Encoding<'a>>,
    with_tounicode: HashSet<String>,
}

impl<'a> FontContext<'a> {
    fn new(
        doc: &'a Document,
        fonts: BTreeMap<Vec<u8>, &'a Dictionary>,
        font_cmaps: &FontCMaps,
        style_cache: &mut FontStyleCache,
    ) -> Self {
        let (encodings, _) = build_font_encodings(doc, &fonts, font_cmaps, style_cache);
        let widths = build_font_widths(doc, &fonts, style_cache);
        let mut base_names = HashMap::new();
        let mut tounicode_refs = HashMap::new();
        let mut inline_cmaps = HashMap::new();
        let mut encoding_cache = HashMap::new();
        let mut with_tounicode = HashSet::new();
        for (font_name, font_dict) in &fonts {
            let resource_name = String::from_utf8_lossy(font_name).to_string();
            if let Ok(name) = font_dict.get(b"BaseFont").and_then(Object::as_name) {
                base_names.insert(
                    resource_name.clone(),
                    String::from_utf8_lossy(name).to_string(),
                );
            }
            match font_dict.get(b"ToUnicode") {
                Ok(Object::Reference(obj_ref)) => {
                    if font_cmaps.get_by_obj(obj_ref.0).is_some() {
                        with_tounicode.insert(resource_name.clone());
                    }
                    tounicode_refs.insert(resource_name.clone(), obj_ref.0);
                }
                Ok(Object::Stream(s)) => {
                    let data = s
                        .decompressed_content()
                        .unwrap_or_else(|_| s.content.clone());
                    if let Some(entry) =
                        crate::tounicode::build_cmap_entry_from_stream(&data, font_dict, doc, 0)
                    {
                        with_tounicode.insert(resource_name.clone());
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
            kinds: build_font_kinds(&fonts),
            type3_scales: build_type3_scales(doc, &fonts),
            fonts,
            encodings,
            widths,
            base_names,
            tounicode_refs,
            inline_cmaps,
            encoding_cache,
            with_tounicode,
        }
    }
}

/// A Form XObject as walked: its decoded operators, the fonts it paints
/// with (its own over the ones it was first invoked with), and the forms it
/// can invoke.
struct Form<'a> {
    content: Content,
    fonts: Option<Rc<FontContext<'a>>>,
    xobjects: HashMap<String, XObjectType>,
    matrix: Option<[f32; 6]>,
}

struct Walker<'a, 'b> {
    doc: &'a Document,
    font_cmaps: &'b FontCMaps,
    style_cache: &'b mut FontStyleCache,
    cmap_decisions: CMapDecisionCache,
    operations: usize,
    form_invocations: usize,
    form_bytes: usize,
    /// Forms by object id and the font context they were invoked with: a
    /// form's fonts fall back on its caller's, so one form shown from two
    /// contexts is two forms.
    forms: HashMap<(ObjectId, *const FontContext<'a>), Option<Rc<Form<'a>>>>,
    glyphs: Vec<PaintedGlyph>,
}

/// A page's `/Resources`, or those of its nearest ancestor in the page tree
/// when it has none of its own.
fn page_resources(doc: &Document, page_id: ObjectId) -> Option<&Dictionary> {
    let mut node = doc.get_dictionary(page_id).ok()?;
    for _ in 0..32 {
        if let Ok(resources) = node.get(b"Resources") {
            return match resources {
                Object::Reference(id) => doc.get_dictionary(*id).ok(),
                other => other.as_dict().ok(),
            };
        }
        node = node
            .get(b"Parent")
            .and_then(Object::as_reference)
            .and_then(|id| doc.get_dictionary(id))
            .ok()?;
    }
    None
}

/// The font dictionaries of a resource dictionary, by resource name.
fn resource_fonts<'a>(
    doc: &'a Document,
    resources: &'a Dictionary,
) -> BTreeMap<Vec<u8>, &'a Dictionary> {
    let fonts = match resources.get(b"Font") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok(),
        Ok(other) => other.as_dict().ok(),
        Err(_) => None,
    };
    fonts
        .into_iter()
        .flat_map(|fonts| fonts.iter())
        .filter_map(|(name, font)| {
            let font = match font {
                Object::Reference(id) => doc.get_dictionary(*id).ok()?,
                other => other.as_dict().ok()?,
            };
            Some((name.clone(), font))
        })
        .collect()
}

/// Every glyph painted on the page in its reading frame. Glyphs whose
/// baseline is not a quarter turn of the page's, or that are mirrored, are
/// left out; the frame turns with the direction most glyphs run in.
pub(crate) fn page_math_glyphs(
    doc: &Document,
    page_id: ObjectId,
    page_box: PageBox,
    font_cmaps: &FontCMaps,
    style_cache: &mut FontStyleCache,
) -> (Vec<MathGlyph>, ReadingFrame) {
    let mut frame = ReadingFrame {
        quarter_turns: 0,
        page_box,
    };
    let Ok(data) = doc.get_page_content_with_limit(page_id, MAX_PAGE_CONTENT_BYTES) else {
        return (Vec::new(), frame);
    };
    let data = strip_pdf_comments(&data);
    let Ok(Some(content)) = decode_content_bounded(&data, MAX_PAGE_OPERATIONS) else {
        return (Vec::new(), frame);
    };
    let resources = page_resources(doc, page_id);
    let fonts = resources
        .map(|r| resource_fonts(doc, r))
        .unwrap_or_default();
    let mut xobjects = HashMap::new();
    if let Some(resources) = resources {
        collect_xobjects_from_dict(doc, resources, &mut xobjects);
    }
    let mut walker = Walker {
        doc,
        font_cmaps,
        style_cache,
        cmap_decisions: CMapDecisionCache::new(),
        operations: 0,
        form_invocations: 0,
        form_bytes: 0,
        forms: HashMap::new(),
        glyphs: Vec::new(),
    };
    let ctx = Rc::new(FontContext::new(doc, fonts, font_cmaps, walker.style_cache));
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
    walker.walk(&content, &ctx, &xobjects, state, 0);
    let mut votes = [0usize; 4];
    for g in &walker.glyphs {
        votes[g.quarter_turns as usize] += 1;
    }
    frame.quarter_turns = (0..4u8)
        .max_by_key(|&k| (votes[k as usize], k == 0))
        .unwrap_or(0);
    let glyphs = walker
        .glyphs
        .into_iter()
        .filter(|g| g.quarter_turns == frame.quarter_turns)
        .map(|g| {
            let (x, base) = frame.turn(g.origin.0, g.origin.1);
            MathGlyph {
                text: g.text,
                code: g.code,
                font: g.font,
                x0: x,
                x1: x + g.advance,
                base,
                size: g.size,
                invisible: g.invisible,
                to_unicode: g.to_unicode,
            }
        })
        .collect();
    (glyphs, frame)
}

/// The quarter turn of a text-space-to-user matrix's baseline, counted
/// anticlockwise, when it is one and not mirrored.
fn quarter_turns(m: &[f32; 6]) -> Option<u8> {
    let (dx, dy, ux, uy) = (m[0], m[1], m[2], m[3]);
    let aligned = |a: f32, b: f32| a.abs() <= 0.01 * b.abs();
    let turns = if aligned(dy, dx) && aligned(ux, uy) {
        if dx > 0.0 && uy > 0.0 {
            0
        } else if dx < 0.0 && uy < 0.0 {
            2
        } else {
            return None;
        }
    } else if aligned(dx, dy) && aligned(uy, ux) {
        if dy > 0.0 && ux < 0.0 {
            1
        } else if dy < 0.0 && ux > 0.0 {
            3
        } else {
            return None;
        }
    } else {
        return None;
    };
    Some(turns)
}

impl<'a> Walker<'a, '_> {
    fn walk(
        &mut self,
        content: &Content,
        ctx: &Rc<FontContext<'a>>,
        xobjects: &HashMap<String, XObjectType>,
        initial: GraphicsState,
        depth: u8,
    ) {
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
                    if depth >= MAX_FORM_DEPTH || self.form_invocations >= MAX_FORM_INVOCATIONS {
                        continue;
                    }
                    let Some(Ok(name)) = op.operands.first().map(Object::as_name) else {
                        continue;
                    };
                    let name = String::from_utf8_lossy(name).to_string();
                    let Some(XObjectType::Form(form_id)) = xobjects.get(&name) else {
                        continue;
                    };
                    self.form_invocations += 1;
                    let Some(form) = self.form(*form_id, ctx) else {
                        continue;
                    };
                    let mut inner = state.clone();
                    if let Some(matrix) = form.matrix {
                        inner.ctm = multiply_matrices(&matrix, &inner.ctm);
                    }
                    let form_ctx = form.fonts.clone().unwrap_or_else(|| ctx.clone());
                    self.walk(&form.content, &form_ctx, &form.xobjects, inner, depth + 1);
                }
                _ => {}
            }
        }
    }

    /// A Form XObject, decoded once per page. The decompressed bytes of all
    /// forms share one page-content budget; a form past it is skipped.
    fn form(&mut self, form_id: ObjectId, parent: &Rc<FontContext<'a>>) -> Option<Rc<Form<'a>>> {
        let key = (form_id, Rc::as_ptr(parent));
        if let Some(form) = self.forms.get(&key) {
            return form.clone();
        }
        let doc = self.doc;
        let form = (|| {
            let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
                return None;
            };
            let budget = MAX_PAGE_CONTENT_BYTES.saturating_sub(self.form_bytes);
            let data = match stream.decompressed_content_with_limit(budget) {
                Ok(data) => data,
                Err(_) if stream.content.len() > budget => return None,
                Err(_) => stream.content.clone(),
            };
            self.form_bytes += data.len();
            let data = strip_pdf_comments(&data);
            let content = decode_content_bounded(&data, MAX_PAGE_OPERATIONS).ok()??;
            let matrix = stream
                .dict
                .get(b"Matrix")
                .and_then(Object::as_array)
                .ok()
                .filter(|m| m.len() >= 6)
                .and_then(|m| {
                    m.iter()
                        .take(6)
                        .map(get_number)
                        .collect::<Option<Vec<f32>>>()
                })
                .map(|m| [m[0], m[1], m[2], m[3], m[4], m[5]]);
            let own_fonts = get_form_fonts(doc, &stream.dict);
            let fonts = (!own_fonts.is_empty()).then(|| {
                let mut fonts = parent.fonts.clone();
                fonts.extend(own_fonts);
                Rc::new(FontContext::new(
                    doc,
                    fonts,
                    self.font_cmaps,
                    self.style_cache,
                ))
            });
            Some(Rc::new(Form {
                content,
                fonts,
                xobjects: get_form_xobjects(doc, &stream.dict),
                matrix,
            }))
        })();
        self.forms.insert(key, form.clone());
        form
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
        let to_unicode = ctx.with_tounicode.contains(font);
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
            let text_to_user = multiply_matrices(tm, &state.ctm);
            let turns = quarter_turns(&text_to_user);
            if let (Some(quarter_turns), true) = (turns, state.font_size != 0.0) {
                let origin =
                    multiply_matrices(&[1.0, 0.0, 0.0, 1.0, 0.0, state.rise], &text_to_user);
                let scale = (text_to_user[0].powi(2) + text_to_user[1].powi(2)).sqrt();
                self.glyphs.push(PaintedGlyph {
                    text,
                    code,
                    font: label.clone(),
                    origin: (origin[4], origin[5]),
                    advance: (glyph_width * state.horizontal_scale * scale).abs(),
                    quarter_turns,
                    size: effective_font_size(state.font_size, &text_to_user) * size_scale,
                    invisible,
                    to_unicode,
                });
            }
            *tm = multiply_matrices(&[1.0, 0.0, 0.0, 1.0, advance, 0.0], tm);
        }
    }
}
