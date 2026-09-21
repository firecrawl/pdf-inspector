//! Fixtures shared by the tests of the executed-content scan: synthetic
//! pages with images, forms and patterns bound, text layers to lay over
//! them, and the scan run over a page's content.

use super::*;

/// A scan of `content` on its own — no page, nothing followed through
/// `Do` — as its counts and executed tallies: (counts, text ops,
/// hidden text ops).
pub(super) fn scan_alone(content: &[u8]) -> (ContentCounts, u32, u32) {
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    let counts = scan_content_stream(
        content,
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut state,
        &[],
    );
    (
        counts,
        state.executed_text_ops,
        state.executed_hidden_text_ops,
    )
}

/// A Form XObject of [`synthetic_page`]: the page's font as `F1`, the
/// `/BBox` and `/Matrix` given, and in its own resources the XObjects of
/// `xobjects` — each a name to bind, and the name of an image or of an
/// earlier form of the page for it to stand for.
#[derive(Clone, Copy)]
pub(super) struct TestForm<'a> {
    pub(super) name: &'a str,
    pub(super) content: &'a str,
    pub(super) matrix: Option<[i64; 6]>,
    pub(super) bbox: &'a [i64],
    pub(super) xobjects: &'a [(&'a str, &'a str)],
}

/// A page-sized form with nothing in it, to fill in.
pub(super) const PAGE_FORM: TestForm<'static> = TestForm {
    name: "",
    content: "",
    matrix: None,
    bbox: &[0, 0, 612, 792],
    xobjects: &[],
};

/// A pattern of [`synthetic_page_with_patterns`]: a tiling pattern whose
/// cell runs `content`, with the page's image as `Im0` and font as `F1`
/// in its resources — or a shading pattern, when `shading`.
#[derive(Clone, Copy)]
pub(super) struct TestPattern<'a> {
    pub(super) name: &'a str,
    pub(super) content: &'a str,
    pub(super) shading: bool,
}

/// A one-page 612×792 document whose content stream is set with
/// [`set_page_content`]: a 2×2 gray image `Im0` when `image`; a
/// 1500×2383 flat gray image `ImBig`, bound whether or not the content
/// draws it, when `large_image`; and the given forms, bound whether or
/// not the content invokes them.
pub(super) fn synthetic_page(
    image: bool,
    large_image: bool,
    forms: &[TestForm<'_>],
) -> (Document, ObjectId, ObjectId) {
    synthetic_page_with_patterns(image, large_image, forms, &[])
}

/// [`synthetic_page`] with the given patterns bound as well.
pub(super) fn synthetic_page_with_patterns(
    image: bool,
    large_image: bool,
    forms: &[TestForm<'_>],
    patterns: &[TestPattern<'_>],
) -> (Document, ObjectId, ObjectId) {
    use lopdf::dictionary;
    let mut doc = Document::with_version("1.4");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => Object::Name(b"Type1".to_vec()),
        "BaseFont" => Object::Name(b"Helvetica".to_vec()),
    });
    let mut xobjects = dictionary! {};
    let mut add_image = |doc: &mut Document, name: &str, width: i64, height: i64, data: Vec<u8>| {
        let mut stream = lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Image".to_vec()),
                "Width" => Object::Integer(width),
                "Height" => Object::Integer(height),
                "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                "BitsPerComponent" => Object::Integer(8),
            },
            data,
        );
        // A raster of scan size is deflated, as a producer stores it.
        stream.compress().expect("a raster deflates");
        let image_id = doc.add_object(Object::Stream(stream));
        xobjects.set(name, Object::Reference(image_id));
        image_id
    };
    // The images and forms made so far, by name, for a form's own
    // resources to bind.
    let mut named: Vec<(&str, ObjectId)> = Vec::new();
    let mut page_image = None;
    if image {
        let image_id = add_image(&mut doc, "Im0", 2, 2, vec![200, 60, 60, 200]);
        page_image = Some(image_id);
        named.push(("Im0", image_id));
    }
    if large_image {
        let image_id = add_image(&mut doc, "ImBig", 1500, 2383, vec![128; 1500 * 2383]);
        named.push(("ImBig", image_id));
    }
    for form in forms {
        let mut resources = dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
        };
        if !form.xobjects.is_empty() {
            let mut bound = dictionary! {};
            for &(name, stands_for) in form.xobjects {
                let &(_, id) = named
                    .iter()
                    .find(|(known, _)| *known == stands_for)
                    .expect("an image or an earlier form of the page");
                bound.set(name, Object::Reference(id));
            }
            resources.set("XObject", bound);
        }
        let mut dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => Object::Name(b"Form".to_vec()),
            "BBox" => form
                .bbox
                .iter()
                .map(|&value| Object::Integer(value))
                .collect::<Vec<_>>(),
            "Resources" => resources,
        };
        if let Some(matrix) = form.matrix {
            dict.set(
                "Matrix",
                matrix
                    .iter()
                    .map(|&value| Object::Integer(value))
                    .collect::<Vec<_>>(),
            );
        }
        let form_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dict,
            form.content.as_bytes().to_vec(),
        )));
        xobjects.set(form.name, Object::Reference(form_id));
        named.push((form.name, form_id));
    }
    let mut pattern_dict = dictionary! {};
    for pattern in patterns {
        let object = if pattern.shading {
            Object::Dictionary(dictionary! {
                "Type" => "Pattern",
                "PatternType" => Object::Integer(2),
                "Shading" => dictionary! {
                    "ShadingType" => Object::Integer(2),
                    "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                    "Coords" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                },
            })
        } else {
            let mut resources = dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            };
            if let Some(image_id) = page_image {
                resources.set(
                    "XObject",
                    dictionary! { "Im0" => Object::Reference(image_id) },
                );
            }
            Object::Stream(lopdf::Stream::new(
                dictionary! {
                    "Type" => "Pattern",
                    "PatternType" => Object::Integer(1),
                    "PaintType" => Object::Integer(1),
                    "TilingType" => Object::Integer(1),
                    "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                    "XStep" => Object::Integer(612),
                    "YStep" => Object::Integer(792),
                    "Resources" => resources,
                },
                pattern.content.as_bytes().to_vec(),
            ))
        };
        let pattern_id = doc.add_object(object);
        pattern_dict.set(pattern.name, Object::Reference(pattern_id));
    }
    let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {},
        Vec::new(),
    )));

    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                "XObject" => xobjects,
                "Pattern" => pattern_dict,
            },
            "Contents" => Object::Reference(content_id),
        }),
    );
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => Object::Integer(1),
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    (doc, page_id, content_id)
}

pub(super) fn set_page_content(doc: &mut Document, content_id: ObjectId, content: &str) {
    doc.objects.insert(
        content_id,
        Object::Stream(lopdf::Stream::new(
            lopdf::dictionary! {},
            content.as_bytes().to_vec(),
        )),
    );
}

/// The executed tallies of `streams` run, in order, as the page's
/// content: (text ops, hidden text ops, covered image area).
pub(super) fn executed(doc: &Document, page_id: ObjectId, streams: &[&str]) -> (u32, u32, f64) {
    let state = executed_state(doc, page_id, streams);
    (
        state.executed_text_ops,
        state.executed_hidden_text_ops,
        state.covered_image_area(),
    )
}

/// The scan's state after `streams` ran, in order, as the page's content.
pub(super) fn executed_state<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    streams: &[&str],
) -> ContentScanState<'a> {
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    let (own, ancestors) = doc.get_page_resources(page_id).unwrap();
    let resources: Vec<&lopdf::Dictionary> = own
        .into_iter()
        .chain(
            ancestors
                .iter()
                .filter_map(|id| doc.get_dictionary(*id).ok()),
        )
        .collect();
    let mut state = ContentScanState::new(doc, page_box, true);
    for stream in streams {
        scan_content_stream(
            stream.as_bytes(),
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut state,
            &resources,
        );
    }
    state
}

pub(super) const PAGE_AREA: f64 = 612.0 * 792.0;
pub(super) const FULL_PAGE_IMAGE: &str = "q 612 0 0 792 0 0 cm /Im0 Do Q\n";

/// 120 one-glyph `Tj` blocks under `mode`, as a producer writes a text
/// layer.
pub(super) fn glyph_layer(mode: u8) -> String {
    let mut layer = format!("{mode} Tr\n");
    let glyphs = "thepagecarriesalayernobodysees".chars().cycle().take(120);
    for (n, glyph) in glyphs.enumerate() {
        let x = 72 + (n % 40) * 12;
        let y = 720 - (n / 40) * 14;
        layer.push_str(&format!(
            "BT 1 0 0 1 {x} {y} Tm /F1 10 Tf ({glyph}) Tj ET\n"
        ));
    }
    layer
}

pub(super) fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// Within a tenth of the expected area — the grid's resolution costs a
/// cell along each edge — and nothing but zero for none.
pub(super) fn about(covered: f64, expected: f64) -> bool {
    if expected == 0.0 {
        covered == 0.0
    } else {
        (covered - expected).abs() <= expected * 0.1
    }
}

/// A one-page document: a 2×2 gray image drawn over the whole page when
/// `covering_image`; a layer of 120 one-glyph `Tj` blocks under
/// `layer_mode` when given, in the page's content or, when
/// `layer_in_form`, in a Form XObject the page invokes; and a visible
/// caption line when given.
pub(super) fn layered_scan_page(
    covering_image: bool,
    layer_mode: Option<u8>,
    layer_in_form: bool,
    caption: Option<&str>,
) -> (Document, ObjectId) {
    let layer = layer_mode.map(glyph_layer).unwrap_or_default();
    let forms: Vec<TestForm> = if layer_in_form {
        vec![TestForm {
            name: "Fm0",
            content: layer.as_str(),
            ..PAGE_FORM
        }]
    } else {
        Vec::new()
    };
    let (mut doc, page_id, content_id) = synthetic_page(covering_image, false, &forms);
    let mut content = String::new();
    if covering_image {
        content.push_str(FULL_PAGE_IMAGE);
    }
    if layer_in_form {
        content.push_str("/Fm0 Do\n");
    } else {
        content.push_str(&layer);
    }
    if let Some(caption) = caption {
        content.push_str(&format!("BT 0 Tr /F1 12 Tf 72 40 Td ({caption}) Tj ET\n"));
    }
    set_page_content(&mut doc, content_id, &content);
    (doc, page_id)
}
