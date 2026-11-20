//! The paint state text is shown with: the fill and stroke colours and the
//! text render mode each run reports, and conservative recognition of
//! weight added by painting a glyph twice.

use std::collections::HashMap;
use std::sync::Arc;

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::get_number;

/// Upper bound on the decoded bytes of an `/Indexed` palette's lookup
/// stream. A palette holds at most 256 entries of four components; a stream
/// decoding past this is malformed and its palette is not read.
const MAX_PALETTE_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Icc(ObjectId, usize),
    /// An `/Indexed` palette over one of the spaces above, read into sRGB:
    /// entry `i` is the colour index `i` paints. Only the reported colour
    /// reads it; weight inference treats a palette colour as unknown.
    Indexed(Arc<[[u8; 3]]>),
}

impl ColorSpace {
    fn components(&self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Icc(_, n) => *n,
            Self::Indexed(_) => 1,
        }
    }

    /// The colour `values` give in this space as 8-bit sRGB, each value
    /// clamped to its range first as renderers clamp it: an index to the
    /// palette's entries, a component to 0..=1. `None` when the number of
    /// values is not the space's component count or one is not finite.
    fn srgb(&self, values: &[f32]) -> Option<[u8; 3]> {
        if values.len() != self.components() || !values.iter().all(|v| v.is_finite()) {
            return None;
        }
        match self {
            Self::Indexed(palette) => {
                let last = palette.len().checked_sub(1)?;
                let index = values[0].round().clamp(0.0, last as f32) as usize;
                palette.get(index).copied()
            }
            _ => device_srgb(values),
        }
    }

    /// The colour `cs`/`CS` leaves current on selecting this space, as the
    /// PDF specification sets it: black for DeviceGray, DeviceRGB and
    /// DeviceCMYK (whose initial colour is `0 0 0 1`), every component 0 for
    /// an ICC-based space — black for one or three components, white for
    /// four read as CMYK — and the palette's first entry for an indexed
    /// space.
    fn initial_srgb(&self) -> Option<[u8; 3]> {
        match self {
            Self::Indexed(palette) => palette.first().copied(),
            Self::Icc(_, n) => device_srgb(&[0.0; 4][..*n]),
            _ => Some([0, 0, 0]),
        }
    }
}

/// A colour given by its gray, RGB or CMYK components (by their count) as
/// 8-bit sRGB. RGB is taken as sRGB and gray as equal components; CMYK is
/// converted the way the PDF specification converts DeviceCMYK to
/// DeviceRGB, each of red, green and blue `1 - min(1, c + k)` for its
/// complementary ink. Components are clamped to 0..=1 first.
fn device_srgb(values: &[f32]) -> Option<[u8; 3]> {
    let unit = |v: f32| v.clamp(0.0, 1.0);
    let byte = |v: f32| (unit(v) * 255.0).round() as u8;
    match *values {
        [gray] => Some([byte(gray); 3]),
        [r, g, b] => Some([byte(r), byte(g), byte(b)]),
        [c, m, y, k] => {
            let channel = |ink: f32| byte(1.0 - (unit(ink) + unit(k)).min(1.0));
            Some([channel(c), channel(m), channel(y)])
        }
        _ => None,
    }
}

fn resolve<'a>(doc: &'a Document, mut obj: &'a Object) -> Option<&'a Object> {
    for _ in 0..8 {
        match obj {
            Object::Reference(id) => obj = doc.get_object(*id).ok()?,
            _ => return Some(obj),
        }
    }
    None
}

fn device_space(name: &[u8]) -> Option<ColorSpace> {
    match name {
        b"DeviceGray" => Some(ColorSpace::Gray),
        b"DeviceRGB" => Some(ColorSpace::Rgb),
        b"DeviceCMYK" => Some(ColorSpace::Cmyk),
        _ => None,
    }
}

/// A device space named where the base of an indexed space is read: by its
/// full name, or by the abbreviation inline images give it (`G`, `RGB`,
/// `CMYK`), which a palette written as `[/I ...]` may use for its base.
fn palette_base_device_space(name: &[u8]) -> Option<ColorSpace> {
    device_space(name).or(match name {
        b"G" => Some(ColorSpace::Gray),
        b"RGB" => Some(ColorSpace::Rgb),
        b"CMYK" => Some(ColorSpace::Cmyk),
        _ => None,
    })
}

/// A colour space object that is a device space or an ICC-based space of
/// one, three or four components, by name or `[/ICCBased stream]`. The
/// profile's `/N` may be an indirect object.
fn direct_space(doc: &Document, obj: &Object) -> Option<ColorSpace> {
    match resolve(doc, obj)? {
        Object::Name(name) => device_space(name),
        Object::Array(a) if a.len() == 2 && a[0].as_name().ok() == Some(b"ICCBased") => {
            let id = a[1].as_reference().ok()?;
            let profile = doc.get_object(id).ok()?.as_stream().ok()?;
            let n = get_number(resolve(doc, profile.dict.get(b"N").ok()?)?)?;
            [1, 3, 4]
                .into_iter()
                .find(|&count| n == count as f32)
                .map(|count| ColorSpace::Icc(id, count))
        }
        _ => None,
    }
}

/// Whether a colour space array is an indexed space: `/Indexed`, or `/I`,
/// the abbreviation inline images use for it.
fn is_indexed_family(space: &[Object]) -> bool {
    space
        .first()
        .and_then(|o| o.as_name().ok())
        .is_some_and(|family| matches!(family, b"Indexed" | b"I"))
}

/// `[/Indexed base hival lookup]` over a base [`direct_space`] reads: the
/// lookup table's `hival + 1` entries of the base's components, one byte
/// each mapped to 0..=1, as sRGB. A table shorter than that keeps the
/// entries it completes. A base given by name is a device space's name or
/// abbreviation, else the name of a space in `spaces`, the colour space
/// resources the palette is defined among.
fn indexed_space(doc: &Document, space: &[Object], spaces: &Dictionary) -> Option<ColorSpace> {
    let [_, base, hival, lookup] = space else {
        return None;
    };
    let base = match resolve(doc, base)? {
        Object::Name(name) => palette_base_device_space(name).or_else(|| {
            spaces
                .get(name)
                .ok()
                .and_then(|named| direct_space(doc, named))
        })?,
        base => direct_space(doc, base)?,
    };
    let hival = usize::try_from(resolve(doc, hival)?.as_i64().ok()?).ok()?;
    if hival > 255 {
        return None;
    }
    let table = match resolve(doc, lookup)? {
        Object::String(bytes, _) => bytes.clone(),
        Object::Stream(stream) => stream
            .decompressed_content_with_limit(MAX_PALETTE_BYTES)
            .ok()?,
        _ => return None,
    };
    let n = base.components();
    let palette = table
        .chunks_exact(n)
        .take(hival + 1)
        .map(|entry| {
            let mut values = [0.0f32; 4];
            for (value, &byte) in values.iter_mut().zip(entry) {
                *value = f32::from(byte) / 255.0;
            }
            base.srgb(&values[..n])
        })
        .collect::<Option<Vec<[u8; 3]>>>()?;
    (!palette.is_empty()).then(|| ColorSpace::Indexed(palette.into()))
}

#[derive(Clone, Default)]
pub(crate) struct PaintResources {
    spaces: HashMap<Vec<u8>, Option<ColorSpace>>,
    harmless_states: HashMap<Vec<u8>, bool>,
}

impl PaintResources {
    pub(crate) fn add(&mut self, doc: &Document, resources: &Dictionary) {
        if let Some(spaces) = resources
            .get(b"ColorSpace")
            .ok()
            .and_then(|o| resolve(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            for (name, obj) in spaces {
                let space = resolve(doc, obj).and_then(|obj| match obj {
                    Object::Array(a) if is_indexed_family(a) => indexed_space(doc, a, spaces),
                    other => direct_space(doc, other),
                });
                self.spaces.entry(name.clone()).or_insert(space);
            }
        }
        if let Some(states) = resources
            .get(b"ExtGState")
            .ok()
            .and_then(|o| resolve(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            for (name, obj) in states {
                let harmless = resolve(doc, obj)
                    .and_then(|o| o.as_dict().ok())
                    .is_some_and(|state| {
                        state.iter().all(|(key, value)| {
                            let Some(value) = resolve(doc, value) else {
                                return false;
                            };
                            match key.as_slice() {
                                b"Type" => value.as_name().ok() == Some(b"ExtGState"),
                                b"SM" => get_number(value)
                                    .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
                                b"OPM" => value.as_i64().is_ok_and(|n| matches!(n, 0 | 1)),
                                _ => false,
                            }
                        })
                    });
                self.harmless_states.entry(name.clone()).or_insert(harmless);
            }
        }
    }

    pub(crate) fn page(doc: &Document, page_id: ObjectId) -> Self {
        let mut result = Self::default();
        if let Ok((own, inherited)) = doc.get_page_resources(page_id) {
            if let Some(resources) = own {
                result.add(doc, resources);
            }
            for id in inherited {
                if let Ok(resources) = doc.get_dictionary(id) {
                    result.add(doc, resources);
                }
            }
        }
        result
    }

    pub(crate) fn form(doc: &Document, form: &Dictionary, parent: &Self) -> Self {
        let Ok(resource) = form.get(b"Resources") else {
            return parent.clone();
        };
        let mut result = Self::default();
        if let Some(resources) = resolve(doc, resource).and_then(|o| o.as_dict().ok()) {
            result.add(doc, resources);
        }
        result
    }
}

#[derive(Clone, PartialEq)]
struct Color {
    space: ColorSpace,
    values: [f32; 4],
}

/// What a shown run reports of the paint it was shown with (see
/// `TextItem::fill_color`, `stroke_color` and `render_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunPaint {
    /// The fill (non-stroking) colour as 8-bit sRGB, `None` when unknown.
    pub(crate) fill_color: Option<[u8; 3]>,
    /// The stroke colour as 8-bit sRGB, `None` when unknown.
    pub(crate) stroke_color: Option<[u8; 3]>,
    /// The text render mode, 0..=7.
    pub(crate) render_mode: u8,
}

#[derive(Clone)]
pub(crate) struct TextPaint {
    // Paint semantics persist across BT/ET independently of the extractor's
    // existing invisible-layer visibility policy.
    rendering_mode: i32,
    /// The render mode a run reports: the last `Tr` whose operand is an
    /// integer in 0..=7. Any other operand is ignored, as renderers ignore
    /// it; `rendering_mode` keeps the reading weight inference has always
    /// made of it.
    render_mode: u8,
    fill_space: Option<ColorSpace>,
    stroke_space: Option<ColorSpace>,
    fill: Option<Color>,
    stroke: Option<Color>,
    /// The fill and stroke colours a run reports, as 8-bit sRGB: what a
    /// renderer paints with, from components clamped to their range, the
    /// initial colour a `cs`/`CS` selects, or a palette entry. `None` when
    /// the space or its operands are not understood. Weight inference reads
    /// the stricter `fill` and `stroke` above.
    fill_srgb: Option<[u8; 3]>,
    stroke_srgb: Option<[u8; 3]>,
    width: f32,
    solid: bool,
    known_compositing: bool,
}

impl Default for TextPaint {
    fn default() -> Self {
        let black = Some(Color {
            space: ColorSpace::Gray,
            values: [0.0; 4],
        });
        Self {
            rendering_mode: 0,
            render_mode: 0,
            fill_space: Some(ColorSpace::Gray),
            stroke_space: Some(ColorSpace::Gray),
            fill: black.clone(),
            stroke: black,
            fill_srgb: Some([0, 0, 0]),
            stroke_srgb: Some([0, 0, 0]),
            width: 1.0,
            solid: true,
            known_compositing: true,
        }
    }
}

impl TextPaint {
    pub(crate) fn observe(
        &mut self,
        operator: &str,
        operands: &[Object],
        resources: &PaintResources,
    ) {
        let set_color = |space: Option<&ColorSpace>, operands: &[Object]| -> Option<Color> {
            let space = space?;
            // Palettes are read for the reported colour only.
            if matches!(space, ColorSpace::Indexed(_)) || operands.len() != space.components() {
                return None;
            }
            let mut values = [0.0; 4];
            for (value, operand) in values.iter_mut().zip(operands) {
                *value = get_number(operand)?;
                if !value.is_finite() || !(0.0..=1.0).contains(value) {
                    return None;
                }
            }
            Some(Color {
                space: space.clone(),
                values,
            })
        };
        let srgb = |space: Option<&ColorSpace>, operands: &[Object]| -> Option<[u8; 3]> {
            if operands.len() > 4 {
                return None;
            }
            let mut values = [0.0f32; 4];
            for (value, operand) in values.iter_mut().zip(operands) {
                *value = get_number(operand)?;
            }
            space?.srgb(&values[..operands.len()])
        };
        match operator {
            "Tr" => {
                if let Some(mode) = operands.first().and_then(get_number) {
                    self.rendering_mode = mode as i32;
                    if mode.fract() == 0.0 && (0.0..=7.0).contains(&mode) {
                        self.render_mode = mode as u8;
                    }
                }
            }
            "w" => self.width = operands.first().and_then(get_number).unwrap_or(f32::NAN),
            "d" => {
                self.solid = operands
                    .first()
                    .and_then(|o| o.as_array().ok())
                    .is_some_and(|a| a.is_empty())
            }
            // Smoothness does not change paint. OPM alone cannot enable
            // overprinting while the default disabled state is still known.
            // Other entries may affect opacity, blending, masks or stroke
            // geometry. Never clear an earlier unknown-state latch.
            "gs" => {
                self.known_compositing &= operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|name| resources.harmless_states.get(name))
                    .copied()
                    .unwrap_or(false);
            }
            "cs" | "CS" => {
                let space = operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| {
                        device_space(n).or_else(|| resources.spaces.get(n).cloned().flatten())
                    });
                let initial = space.as_ref().and_then(ColorSpace::initial_srgb);
                // Wait for an explicit colour; unknown/pattern spaces fail closed.
                if operator == "cs" {
                    self.fill_space = space;
                    self.fill = None;
                    self.fill_srgb = initial;
                } else {
                    self.stroke_space = space;
                    self.stroke = None;
                    self.stroke_srgb = initial;
                }
            }
            "g" | "rg" | "k" => {
                self.fill_space = Some(match operator {
                    "g" => ColorSpace::Gray,
                    "rg" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                });
                self.fill = set_color(self.fill_space.as_ref(), operands);
                self.fill_srgb = srgb(self.fill_space.as_ref(), operands);
            }
            "G" | "RG" | "K" => {
                self.stroke_space = Some(match operator {
                    "G" => ColorSpace::Gray,
                    "RG" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                });
                self.stroke = set_color(self.stroke_space.as_ref(), operands);
                self.stroke_srgb = srgb(self.stroke_space.as_ref(), operands);
            }
            "sc" | "scn" => {
                self.fill = set_color(self.fill_space.as_ref(), operands);
                self.fill_srgb = srgb(self.fill_space.as_ref(), operands);
            }
            "SC" | "SCN" => {
                self.stroke = set_color(self.stroke_space.as_ref(), operands);
                self.stroke_srgb = srgb(self.stroke_space.as_ref(), operands);
            }
            _ => {}
        }
    }

    /// What a run shown now reports of its paint.
    pub(crate) fn run_paint(&self) -> RunPaint {
        RunPaint {
            fill_color: self.fill_srgb,
            stroke_color: self.stroke_srgb,
            render_mode: self.render_mode,
        }
    }

    pub(crate) fn adds_bold(
        &self,
        text: &str,
        rendered_size: f32,
        font_name: &str,
        ctm: &[f32; 6],
    ) -> bool {
        if !matches!(self.rendering_mode, 2 | 6) {
            return false;
        }
        let family = font_name.to_ascii_lowercase();
        let symbol_font = ["wingdings", "webdings", "zapfdingbats"]
            .iter()
            .any(|name| family.contains(name));
        // Modes 2/6 fill and stroke; 1/5 merely outline and 7 only clips.
        // Symbol-only runs are glyph drawings, not evidence of text emphasis.
        if symbol_font
            || !self.known_compositing
            || !self.solid
            || self.fill.is_none()
            || self.fill != self.stroke
            || !text.chars().any(char::is_alphanumeric)
            || !self.width.is_finite()
            || self.width <= 0.0
            || !rendered_size.is_finite()
            || rendered_size <= 0.0
        {
            return false;
        }
        // Stroke width is in user space, independent of the text matrix.
        // A device hairline or a stroke below 1% of the em does not provide
        // dependable evidence of extra weight. Require it on both CTM axes.
        let determinant = ctm[0] * ctm[3] - ctm[1] * ctm[2];
        if !ctm.iter().all(|v| v.is_finite()) || !determinant.is_finite() || determinant == 0.0 {
            return false;
        }
        let scale = ctm[0].hypot(ctm[1]).min(ctm[2].hypot(ctm[3]));
        let stroke = self.width * scale;
        stroke.is_finite() && stroke >= rendered_size * 0.01
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{content::Content, dictionary};

    fn painted(ops: &[u8], resources: &PaintResources) -> TextPaint {
        let mut paint = TextPaint {
            rendering_mode: 2,
            ..TextPaint::default()
        };
        for op in Content::decode(ops).unwrap().operations {
            paint.observe(&op.operator, &op.operands, resources);
        }
        paint
    }

    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    #[test]
    fn paint_requires_matching_known_colors_and_visible_weight() {
        let resources = PaintResources::default();
        for ops in [b"0.3 w".as_slice(), b"0.3 w 0 0.2 0.4 rg 0 0.2 0.4 RG"] {
            assert!(painted(ops, &resources).adds_bold("Label", 12.0, "Regular", &IDENTITY));
        }
        for ops in [
            b"0 w".as_slice(),
            b"-1 w",
            b"0.001 w",
            b"0.3 w 1 G",
            b"0.3 w /Unknown cs",
            b"0.3 w /Unknown gs",
            b"0.3 w [1] 0 d",
        ] {
            assert!(!painted(ops, &resources).adds_bold("Label", 12.0, "Regular", &IDENTITY));
        }
    }

    #[test]
    fn symbolic_fonts_and_glyphs_do_not_gain_semantic_emphasis() {
        let paint = painted(b"0.3 w", &PaintResources::default());
        for name in ["Wingdings-Regular", "ABCDEF+ZapfDingbats", "Webdings"] {
            assert!(!paint.adds_bold("A", 12.0, name, &IDENTITY));
        }
        for glyph in ["✓", "\u{f0fc}", "•", ""] {
            assert!(!paint.adds_bold(glyph, 12.0, "Regular", &IDENTITY));
        }
    }

    #[test]
    fn stroke_weight_uses_user_space_and_rejects_degenerate_geometry() {
        let paint = painted(b"0.3 w", &PaintResources::default());
        assert!(paint.adds_bold("Label", 6.0, "Regular", &[0.5, 0.0, 0.0, 0.5, 0.0, 0.0]));
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(!paint.adds_bold("Label", size, "Regular", &IDENTITY));
        }
        for ctm in [
            [1.0, 1.0, 1.0, 1.0, 0.0, 0.0],
            [f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0],
            [0.01, 0.0, 0.0, 0.01, 0.0, 0.0],
        ] {
            assert!(!paint.adds_bold("Label", 12.0, "Regular", &ctm));
        }
    }

    #[test]
    fn unsupported_local_color_space_shadows_parent_alias() {
        let doc = Document::new();
        let local = dictionary! { "ColorSpace" => dictionary! { "Tone" => "Pattern" } };
        let parent = dictionary! { "ColorSpace" => dictionary! { "Tone" => "DeviceRGB" } };
        let mut resources = PaintResources::default();
        resources.add(&doc, &local);
        resources.add(&doc, &parent);
        let paint = painted(b"0.3 w /Tone cs 0 0 0 sc /Tone CS 0 0 0 SC", &resources);
        assert!(!paint.adds_bold("Label", 12.0, "Regular", &IDENTITY));
    }

    #[test]
    fn harmless_graphics_states_do_not_clear_unknown_compositing() {
        let mut doc = Document::new();
        let smooth = doc.add_object(dictionary! { "Type" => "ExtGState", "SM" => 0.02 });
        let resources_dict = dictionary! { "ExtGState" => dictionary! {
            "Smooth" => Object::Reference(smooth),
            "OverprintMode" => dictionary! { "OPM" => 1 },
            "Alpha" => dictionary! { "ca" => 0.5 },
            "Blend" => dictionary! { "BM" => "Multiply" },
            "Mask" => dictionary! { "SMask" => "None" },
            "Overprint" => dictionary! { "OP" => true },
            "Stroke" => dictionary! { "LW" => 1 },
            "InvalidSmooth" => dictionary! { "SM" => 2 },
            "NotFiniteSmooth" => dictionary! { "SM" => Object::Real(f32::NAN) },
            "InvalidMode" => dictionary! { "OPM" => 2 },
            "InvalidType" => dictionary! { "Type" => "Other" },
        }};
        let mut resources = PaintResources::default();
        resources.add(&doc, &resources_dict);
        assert!(painted(b"0.3 w /Smooth gs /OverprintMode gs", &resources)
            .adds_bold("Label", 12.0, "Regular", &IDENTITY));
        for name in [
            "Alpha",
            "Blend",
            "Mask",
            "Overprint",
            "Stroke",
            "InvalidSmooth",
            "NotFiniteSmooth",
            "InvalidMode",
            "InvalidType",
            "Missing",
        ] {
            let ops = format!("0.3 w /{name} gs /Smooth gs /OverprintMode gs");
            assert!(
                !painted(ops.as_bytes(), &resources).adds_bold("Label", 12.0, "Regular", &IDENTITY),
                "{name}"
            );
        }

        let mut shadowed = PaintResources::default();
        shadowed.add(
            &doc,
            &dictionary! { "ExtGState" => dictionary! { "Smooth" => Object::Reference((999, 0)) } },
        );
        shadowed.add(&doc, &resources_dict);
        assert!(
            !painted(b"0.3 w /Smooth gs", &shadowed).adds_bold("Label", 12.0, "Regular", &IDENTITY)
        );
    }

    #[test]
    fn same_icc_profile_and_components_establish_same_paint() {
        let mut doc = Document::new();
        let profile = doc.add_object(lopdf::Stream::new(dictionary! { "N" => 3 }, vec![]));
        let mut resources = PaintResources::default();
        resources.add(
            &doc,
            &dictionary! { "ColorSpace" => dictionary! {
                "Tone" => vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(profile)]
            }},
        );
        let paint = painted(
            b"0.3 w /Tone cs 0 0.2 0.4 sc /Tone CS 0 0.2 0.4 SC",
            &resources,
        );
        assert!(paint.adds_bold("Label", 12.0, "Regular", &IDENTITY));
    }

    /// The paint a run shown after `ops` reports.
    fn run_paint(ops: &[u8], resources: &PaintResources) -> RunPaint {
        let mut paint = TextPaint::default();
        for op in Content::decode(ops).unwrap().operations {
            paint.observe(&op.operator, &op.operands, resources);
        }
        paint.run_paint()
    }

    fn fill(ops: &[u8], resources: &PaintResources) -> Option<[u8; 3]> {
        run_paint(ops, resources).fill_color
    }

    fn stroke(ops: &[u8], resources: &PaintResources) -> Option<[u8; 3]> {
        run_paint(ops, resources).stroke_color
    }

    #[test]
    fn default_paint_is_black_fill_and_stroke_in_mode_zero() {
        let paint = run_paint(b"", &PaintResources::default());
        assert_eq!(
            paint,
            RunPaint {
                fill_color: Some([0, 0, 0]),
                stroke_color: Some([0, 0, 0]),
                render_mode: 0,
            }
        );
    }

    #[test]
    fn device_colours_convert_to_srgb() {
        let none = PaintResources::default();
        assert_eq!(fill(b"0.5 g", &none), Some([128, 128, 128]));
        assert_eq!(fill(b"1 0.5 0 rg", &none), Some([255, 128, 0]));
        assert_eq!(stroke(b"0 0 1 RG", &none), Some([0, 0, 255]));
        assert_eq!(stroke(b"0.25 G", &none), Some([64, 64, 64]));
        // DeviceCMYK as the specification converts it to DeviceRGB: each of
        // red, green and blue is 1 - min(1, ink + black).
        assert_eq!(fill(b"0 0 0 1 k", &none), Some([0, 0, 0]));
        assert_eq!(fill(b"0 0 0 0 k", &none), Some([255, 255, 255]));
        assert_eq!(fill(b"1 0 0 0 k", &none), Some([0, 255, 255]));
        assert_eq!(fill(b"0.5 0 0 0.5 k", &none), Some([0, 128, 128]));
        assert_eq!(stroke(b"0 1 1 0 K", &none), Some([255, 0, 0]));
        // The same colours through `cs`/`sc` and `CS`/`SCN` with the device
        // spaces' own names.
        assert_eq!(
            fill(b"/DeviceRGB cs 0.2 0.4 0.6 sc", &none),
            Some([51, 102, 153])
        );
        assert_eq!(fill(b"/DeviceGray cs 1 scn", &none), Some([255, 255, 255]));
        assert_eq!(
            stroke(b"/DeviceCMYK CS 0 0 1 0 SCN", &none),
            Some([255, 255, 0])
        );
        // Stroke and fill are separate: setting one leaves the other.
        let paint = run_paint(b"1 0 0 rg 0 1 0 RG", &none);
        assert_eq!(paint.fill_color, Some([255, 0, 0]));
        assert_eq!(paint.stroke_color, Some([0, 255, 0]));
    }

    #[test]
    fn out_of_range_components_are_clamped_and_malformed_colours_are_unknown() {
        let none = PaintResources::default();
        assert_eq!(fill(b"1.2 g", &none), Some([255, 255, 255]));
        assert_eq!(fill(b"-0.5 0.5 2 rg", &none), Some([0, 128, 255]));
        // A colour operator with the wrong number of operands, or one that
        // is not a number, names no colour.
        assert_eq!(fill(b"0.5 0.5 g", &none), None);
        assert_eq!(fill(b"1 0 rg", &none), None);
        assert_eq!(fill(b"/DeviceRGB cs 1 sc", &none), None);
        assert_eq!(fill(b"/DeviceRGB cs (a) 0 0 sc", &none), None);
        // A later valid colour is read again.
        assert_eq!(fill(b"1 0 rg 0 0 1 rg", &none), Some([0, 0, 255]));
    }

    #[test]
    fn selecting_a_colour_space_sets_its_initial_colour() {
        let none = PaintResources::default();
        assert_eq!(fill(b"1 0 0 rg /DeviceRGB cs", &none), Some([0, 0, 0]));
        assert_eq!(fill(b"0.5 g /DeviceCMYK cs", &none), Some([0, 0, 0]));
        assert_eq!(stroke(b"1 G /DeviceGray CS", &none), Some([0, 0, 0]));
        // An unknown space has no colour until a known one is selected.
        assert_eq!(fill(b"/Unknown cs", &none), None);
        assert_eq!(fill(b"/Unknown cs 1 0 0 sc", &none), None);
        assert_eq!(fill(b"/Unknown cs 1 0 0 rg", &none), Some([255, 0, 0]));
    }

    #[test]
    fn icc_based_spaces_are_read_by_their_component_count() {
        let mut doc = Document::new();
        let mut spaces = Dictionary::new();
        let indirect_four = doc.add_object(Object::Integer(4));
        for (name, n) in [
            ("Gray", Object::Integer(1)),
            ("Rgb", Object::Integer(3)),
            ("Cmyk", Object::Integer(4)),
            ("Two", Object::Integer(2)),
            // `/N` written as an indirect object, or as a real.
            ("IndirectCmyk", Object::Reference(indirect_four)),
            ("RealRgb", Object::Real(3.0)),
        ] {
            let profile = doc.add_object(lopdf::Stream::new(dictionary! { "N" => n }, vec![]));
            spaces.set(
                name,
                vec![
                    Object::Name(b"ICCBased".to_vec()),
                    Object::Reference(profile),
                ],
            );
        }
        let mut resources = PaintResources::default();
        resources.add(&doc, &dictionary! { "ColorSpace" => spaces });
        assert_eq!(fill(b"/Gray cs 0.5 sc", &resources), Some([128, 128, 128]));
        assert_eq!(fill(b"/Rgb cs 1 0 0 scn", &resources), Some([255, 0, 0]));
        assert_eq!(stroke(b"/Cmyk CS 0 0 0 1 SC", &resources), Some([0, 0, 0]));
        assert_eq!(
            stroke(b"/Cmyk CS 0 1 0 0 SCN", &resources),
            Some([255, 0, 255])
        );
        assert_eq!(
            fill(b"/IndirectCmyk cs 0 1 0 0 sc", &resources),
            Some([255, 0, 255])
        );
        assert_eq!(fill(b"/RealRgb cs 0 0 1 sc", &resources), Some([0, 0, 255]));
        // Selecting an ICC-based space starts it at every component 0, as
        // the specification sets it: black for one or three components,
        // white for four (DeviceCMYK itself starts at black, `0 0 0 1`).
        assert_eq!(fill(b"/Gray cs", &resources), Some([0, 0, 0]));
        assert_eq!(fill(b"/Rgb cs", &resources), Some([0, 0, 0]));
        assert_eq!(fill(b"/Cmyk cs", &resources), Some([255, 255, 255]));
        assert_eq!(
            stroke(b"/IndirectCmyk CS", &resources),
            Some([255, 255, 255])
        );
        // A profile of any other component count names no colour.
        assert_eq!(fill(b"/Two cs 0 0 sc", &resources), None);
    }

    #[test]
    fn indexed_palettes_are_read_through_their_base_space() {
        let mut doc = Document::new();
        let cmyk_profile = doc.add_object(lopdf::Stream::new(dictionary! { "N" => 4 }, vec![]));
        // An uncompressed lookup stream of CMYK entries: black, then cyan.
        let cmyk_lookup = doc.add_object(lopdf::Stream::new(
            dictionary! {},
            vec![0, 0, 0, 255, 255, 0, 0, 0],
        ));
        let indexed = |base: Object, hival: i64, lookup: Object| {
            Object::Array(vec![
                Object::Name(b"Indexed".to_vec()),
                base,
                hival.into(),
                lookup,
            ])
        };
        let spaces = dictionary! {
            // Red, green, blue.
            "Rgb" => indexed(
                Object::Name(b"DeviceRGB".to_vec()),
                2,
                Object::String(
                    vec![255, 0, 0, 0, 255, 0, 0, 0, 255],
                    lopdf::StringFormat::Hexadecimal,
                ),
            ),
            "Cmyk" => indexed(
                vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(cmyk_profile)].into(),
                1,
                Object::Reference(cmyk_lookup),
            ),
            // The table completes one entry of the two `hival` asks for.
            "Short" => indexed(
                Object::Name(b"DeviceGray".to_vec()),
                1,
                Object::string_literal(vec![128u8]),
            ),
            "OverSeparation" => indexed(
                vec![
                    Object::Name(b"Separation".to_vec()),
                    Object::Name(b"Spot".to_vec()),
                    Object::Name(b"DeviceCMYK".to_vec()),
                    Object::Null,
                ]
                .into(),
                0,
                Object::string_literal(vec![255u8]),
            ),
        };
        let mut resources = PaintResources::default();
        resources.add(&doc, &dictionary! { "ColorSpace" => spaces });
        assert_eq!(fill(b"/Rgb cs 1 sc", &resources), Some([0, 255, 0]));
        assert_eq!(stroke(b"/Rgb CS 2 SCN", &resources), Some([0, 0, 255]));
        // Selecting the space starts it at entry 0; an index past the table
        // is clamped to its last entry, a fractional one rounded.
        assert_eq!(fill(b"/Rgb cs", &resources), Some([255, 0, 0]));
        assert_eq!(fill(b"/Rgb cs 9 sc", &resources), Some([0, 0, 255]));
        assert_eq!(fill(b"/Rgb cs 0.6 sc", &resources), Some([0, 255, 0]));
        assert_eq!(fill(b"/Rgb cs 0 0 sc", &resources), None);
        assert_eq!(fill(b"/Cmyk cs 0 sc", &resources), Some([0, 0, 0]));
        assert_eq!(fill(b"/Cmyk cs 1 sc", &resources), Some([0, 255, 255]));
        assert_eq!(fill(b"/Short cs 1 sc", &resources), Some([128, 128, 128]));
        assert_eq!(fill(b"/OverSeparation cs 0 sc", &resources), None);

        // The base named by a resource of the same ColorSpace dictionary, and
        // the `/I` abbreviation with an abbreviated device base.
        let named = dictionary! {
            "Profile" => vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(cmyk_profile)],
            "Gray" => Object::Name(b"DeviceGray".to_vec()),
            "OverProfile" => indexed(
                Object::Name(b"Profile".to_vec()),
                1,
                Object::Reference(cmyk_lookup),
            ),
            "OverGray" => indexed(
                Object::Name(b"Gray".to_vec()),
                0,
                Object::string_literal(vec![64u8]),
            ),
            "OverMissing" => indexed(
                Object::Name(b"Missing".to_vec()),
                0,
                Object::string_literal(vec![64u8]),
            ),
            "Abbreviated" => Object::Array(vec![
                Object::Name(b"I".to_vec()),
                Object::Name(b"RGB".to_vec()),
                1.into(),
                Object::string_literal(vec![0u8, 0, 0, 255, 128, 0]),
            ]),
        };
        let mut named_resources = PaintResources::default();
        named_resources.add(&doc, &dictionary! { "ColorSpace" => named });
        assert_eq!(
            fill(b"/OverProfile cs 1 sc", &named_resources),
            Some([0, 255, 255])
        );
        assert_eq!(
            fill(b"/OverGray cs 0 sc", &named_resources),
            Some([64, 64, 64])
        );
        assert_eq!(fill(b"/OverMissing cs 0 sc", &named_resources), None);
        assert_eq!(
            fill(b"/Abbreviated cs 1 sc", &named_resources),
            Some([255, 128, 0])
        );
        assert_eq!(
            stroke(b"/Abbreviated CS", &named_resources),
            Some([0, 0, 0])
        );

        // Weight inference still treats a palette colour as unknown.
        let paint = painted(b"0.3 w /Rgb cs /Rgb CS 1 sc 1 SC", &resources);
        assert_eq!(paint.run_paint().fill_color, paint.run_paint().stroke_color);
        assert!(!paint.adds_bold("Label", 12.0, "Regular", &IDENTITY));
    }

    #[test]
    fn separation_devicen_pattern_and_other_spaces_report_no_colour() {
        let doc = Document::new();
        let spaces = dictionary! {
            "Spot" => vec![
                Object::Name(b"Separation".to_vec()),
                Object::Name(b"PANTONE".to_vec()),
                Object::Name(b"DeviceCMYK".to_vec()),
                Object::Null,
            ],
            "Inks" => vec![
                Object::Name(b"DeviceN".to_vec()),
                vec![Object::Name(b"Cyan".to_vec())].into(),
                Object::Name(b"DeviceCMYK".to_vec()),
                Object::Null,
            ],
            "Tiles" => vec![Object::Name(b"Pattern".to_vec()), Object::Name(b"DeviceRGB".to_vec())],
            "Calibrated" => vec![Object::Name(b"CalRGB".to_vec()), dictionary! {}.into()],
        };
        let mut resources = PaintResources::default();
        resources.add(&doc, &dictionary! { "ColorSpace" => spaces });
        for ops in [
            b"/Spot cs 1 sc".as_slice(),
            b"/Inks cs 0.5 scn",
            b"/Tiles cs 1 0 0 /P0 scn",
            b"/Pattern cs /P0 scn",
            b"/Calibrated cs 1 0 0 sc",
        ] {
            assert_eq!(
                fill(ops, &resources),
                None,
                "{}",
                String::from_utf8_lossy(ops)
            );
        }
        assert_eq!(stroke(b"/Spot CS 1 SCN", &resources), None);
    }

    #[test]
    fn render_mode_reads_integer_modes_zero_to_seven_only() {
        let none = PaintResources::default();
        for mode in 0..=7u8 {
            let ops = format!("{mode} Tr");
            assert_eq!(run_paint(ops.as_bytes(), &none).render_mode, mode);
        }
        // An operand that is not an integer in 0..=7 leaves the mode in force.
        for ops in [
            "3 Tr 8 Tr",
            "3 Tr -1 Tr",
            "3 Tr 2.5 Tr",
            "3 Tr /Name Tr",
            "3 Tr Tr",
        ] {
            assert_eq!(run_paint(ops.as_bytes(), &none).render_mode, 3, "{ops}");
        }
        assert_eq!(run_paint(b"2.0 Tr", &none).render_mode, 2);
    }
}
