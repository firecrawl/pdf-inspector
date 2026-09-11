//! Conservative recognition of weight added by painting a glyph twice.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::get_number;

#[derive(Clone, Copy, PartialEq)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Icc(ObjectId, usize),
}

impl ColorSpace {
    fn components(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Icc(_, n) => n,
        }
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

#[derive(Default)]
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
                    Object::Name(name) => device_space(name),
                    Object::Array(a)
                        if a.len() == 2 && a[0].as_name().ok() == Some(b"ICCBased") =>
                    {
                        let id = a[1].as_reference().ok()?;
                        let profile = doc.get_object(id).ok()?.as_stream().ok()?;
                        let n = profile.dict.get(b"N").ok()?.as_i64().ok()?;
                        matches!(n, 1 | 3 | 4).then_some(ColorSpace::Icc(id, n as usize))
                    }
                    _ => None,
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

    pub(crate) fn form(doc: &Document, form: &Dictionary) -> Self {
        let mut result = Self::default();
        if let Some(resources) = form
            .get(b"Resources")
            .ok()
            .and_then(|o| resolve(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            result.add(doc, resources);
        }
        result
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Color {
    space: ColorSpace,
    values: [f32; 4],
}

#[derive(Clone, Copy)]
pub(crate) struct TextPaint {
    // Paint semantics persist across BT/ET independently of the extractor's
    // existing invisible-layer visibility policy.
    rendering_mode: i32,
    fill_space: Option<ColorSpace>,
    stroke_space: Option<ColorSpace>,
    fill: Option<Color>,
    stroke: Option<Color>,
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
            fill_space: Some(ColorSpace::Gray),
            stroke_space: Some(ColorSpace::Gray),
            fill: black,
            stroke: black,
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
        let set_color = |space: Option<ColorSpace>, operands: &[Object]| -> Option<Color> {
            let space = space?;
            if operands.len() != space.components() {
                return None;
            }
            let mut values = [0.0; 4];
            for (value, operand) in values.iter_mut().zip(operands) {
                *value = get_number(operand)?;
                if !value.is_finite() || !(0.0..=1.0).contains(value) {
                    return None;
                }
            }
            Some(Color { space, values })
        };
        match operator {
            "Tr" => {
                if let Some(mode) = operands.first().and_then(get_number) {
                    self.rendering_mode = mode as i32;
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
                        device_space(n).or_else(|| resources.spaces.get(n).copied().flatten())
                    });
                // Wait for an explicit colour; unknown/pattern spaces fail closed.
                if operator == "cs" {
                    self.fill_space = space;
                    self.fill = None;
                } else {
                    self.stroke_space = space;
                    self.stroke = None;
                }
            }
            "g" | "rg" | "k" => {
                self.fill_space = Some(match operator {
                    "g" => ColorSpace::Gray,
                    "rg" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                });
                self.fill = set_color(self.fill_space, operands);
            }
            "G" | "RG" | "K" => {
                self.stroke_space = Some(match operator {
                    "G" => ColorSpace::Gray,
                    "RG" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                });
                self.stroke = set_color(self.stroke_space, operands);
            }
            "sc" | "scn" => self.fill = set_color(self.fill_space, operands),
            "SC" | "SCN" => self.stroke = set_color(self.stroke_space, operands),
            _ => {}
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
}
