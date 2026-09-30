//! Tests of the budgets the executed-content scan runs under: the bytes
//! of form and pattern-cell content one page executes, and how a stream
//! past them is refused before it is held.

use super::super::{
    analyze_page_content, detect_from_document, scan_xobjects_in_resources, DetectionConfig,
    PdfType,
};
use super::fixtures::*;
use super::*;

/// The executed-form byte budget set for the test's thread, restored
/// when dropped.
struct FormBytesBudget(Option<usize>);

impl FormBytesBudget {
    fn set(bytes: usize) -> Self {
        Self(EXECUTED_FORM_BYTES_OVERRIDE.with(|budget| budget.replace(Some(bytes))))
    }
}

impl Drop for FormBytesBudget {
    fn drop(&mut self) {
        EXECUTED_FORM_BYTES_OVERRIDE.with(|budget| budget.set(self.0));
    }
}

/// A page whose forms execute more bytes than the budget allows is not
/// flagged: from the form that would pass the budget on, none is read,
/// the page's evidence is incomplete and it keeps the classification the
/// resource walk gives it; the same page within the budget is a layer
/// nobody sees. A pattern's cell counts against the same budget.
#[test]
fn form_content_past_the_byte_budget_leaves_the_page_unflagged() {
    let layer = glyph_layer(3);
    let form = TestForm {
        name: "FmLayer",
        content: layer.as_str(),
        ..PAGE_FORM
    };
    let (mut doc, page_id, content_id) = synthetic_page(true, false, &[form]);
    // Two invocations fit the budget; a third would pass it.
    let _budget = FormBytesBudget::set(2 * layer.len() + layer.len() / 2);

    let within = format!("{FULL_PAGE_IMAGE}/FmLayer Do /FmLayer Do");
    let state = executed_state(&doc, page_id, &[&within]);
    assert_eq!(state.executed_form_bytes, 2 * layer.len());
    assert!(!state.form_bytes_exceeded && !state.incomplete);
    assert_eq!(
        (state.executed_text_ops, state.executed_hidden_text_ops),
        (240, 240)
    );
    set_page_content(&mut doc, content_id, &within);
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_invisible_text_layer);
    assert_eq!(analysis.executed_form_bytes, 2 * layer.len());
    assert!(!analysis.form_bytes_exceeded);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(
        detected.ocr_reasons_by_page.get(&1),
        Some(&vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER.to_string()])
    );

    let past = format!("{FULL_PAGE_IMAGE}/FmLayer Do /FmLayer Do /FmLayer Do /FmLayer Do");
    let state = executed_state(&doc, page_id, &[&past]);
    assert_eq!(
        state.executed_form_bytes,
        2 * layer.len(),
        "the third invocation would pass the budget: it and the fourth go unread"
    );
    assert!(state.form_bytes_exceeded && state.incomplete);
    assert_eq!(state.executed_text_ops, 240);
    set_page_content(&mut doc, content_id, &past);
    let analysis = analyze_page_content(&doc, page_id);
    assert!(!analysis.has_invisible_text_layer);
    assert!(analysis.form_bytes_exceeded);
    assert_eq!(analysis.executed_form_bytes, 2 * layer.len());
    assert_eq!(
        analysis.text_operator_count, 120,
        "the bound form, counted once"
    );
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(detected.pdf_type, PdfType::TextBased);
    assert!(detected.pages_needing_ocr.is_empty());

    // A pattern's cell: within the budget it is read and paints coverage;
    // past it, it goes unread, and the evidence is incomplete.
    let pattern = TestPattern {
        name: "PImage",
        content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
        shading: false,
    };
    let (doc, page_id, _) = synthetic_page_with_patterns(true, false, &[], &[pattern]);
    let fill = "/Pattern cs /PImage scn 0 0 612 792 re f";
    {
        let _budget = FormBytesBudget::set(pattern.content.len());
        let state = executed_state(&doc, page_id, &[fill]);
        assert!(close(state.covered_image_area(), PAGE_AREA));
        assert_eq!(state.executed_form_bytes, pattern.content.len());
        assert!(!state.incomplete);
    }
    {
        let _budget = FormBytesBudget::set(pattern.content.len() - 1);
        let state = executed_state(&doc, page_id, &[fill]);
        assert!(close(state.covered_image_area(), 0.0));
        assert!(state.form_bytes_exceeded && state.incomplete);
        assert_eq!(state.executed_form_bytes, 0);
    }
}

/// The stream that `name` binds in the page's resources of `category`
/// (`XObject`, `Pattern`).
fn resource_stream_mut<'d>(
    doc: &'d mut Document,
    page_id: ObjectId,
    category: &[u8],
    name: &str,
) -> &'d mut lopdf::Stream {
    let id = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(category)
        .unwrap()
        .as_dict()
        .unwrap()
        .get(name.as_bytes())
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_object_mut(id).unwrap().as_stream_mut().unwrap()
}

/// A form whose content would pass the byte budget is refused before it
/// is held: a deflated stream is decoded no further than the budget's
/// remainder, where the decoder refuses it; a raw stream is refused by
/// its length. Nothing of it is executed or kept, the page's evidence is
/// incomplete and the page is not flagged. Within the budget the same
/// form is read as before, and a pattern's cell is admitted the same way.
#[test]
fn a_form_past_the_budget_is_refused_before_it_is_decoded() {
    let layer = glyph_layer(3);
    let form = TestForm {
        name: "FmLayer",
        content: layer.as_str(),
        ..PAGE_FORM
    };
    let content = format!("{FULL_PAGE_IMAGE}/FmLayer Do");
    for deflated in [true, false] {
        let (mut doc, page_id, content_id) = synthetic_page(true, false, &[form]);
        set_page_content(&mut doc, content_id, &content);
        if deflated {
            let stream = resource_stream_mut(&mut doc, page_id, b"XObject", "FmLayer");
            stream.compress().unwrap();
            assert!(
                stream.content.len() < layer.len() / 4,
                "deflated well within the budget"
            );
            // The bounded decoder refuses the stream at the limit rather
            // than decoding it whole.
            assert!(matches!(
                stream.decompressed_content_with_limit(layer.len() - 1),
                Err(lopdf::Error::Decompress(
                    lopdf::DecompressError::MemoryLimitExceeded { .. }
                ))
            ));
        }
        {
            let _budget = FormBytesBudget::set(layer.len() - 1);
            let state = executed_state(&doc, page_id, &[&content]);
            assert!(
                state.form_bytes_exceeded && state.incomplete,
                "deflated {deflated}"
            );
            assert_eq!(state.executed_form_bytes, 0, "deflated {deflated}");
            assert!(
                state.form_content.is_empty(),
                "nothing kept, deflated {deflated}"
            );
            assert_eq!(state.executed_text_ops, 0, "deflated {deflated}");
            let analysis = analyze_page_content(&doc, page_id);
            assert!(!analysis.has_invisible_text_layer, "deflated {deflated}");
            assert!(analysis.form_bytes_exceeded, "deflated {deflated}");
            let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
            assert_eq!(detected.pdf_type, PdfType::TextBased, "deflated {deflated}");
            assert!(detected.pages_needing_ocr.is_empty(), "deflated {deflated}");
        }
        {
            let _budget = FormBytesBudget::set(layer.len());
            let state = executed_state(&doc, page_id, &[&content]);
            assert!(
                !state.form_bytes_exceeded && !state.incomplete,
                "deflated {deflated}"
            );
            assert_eq!(
                state.executed_form_bytes,
                layer.len(),
                "deflated {deflated}"
            );
            assert_eq!(
                (state.executed_text_ops, state.executed_hidden_text_ops),
                (120, 120),
                "deflated {deflated}"
            );
            let analysis = analyze_page_content(&doc, page_id);
            assert!(analysis.has_invisible_text_layer, "deflated {deflated}");
        }
    }

    // A pattern's cell, deflated: refused at the limit, it paints no
    // coverage; admitted, it covers the page.
    let cell = format!(
        "{}q 612 0 0 792 0 0 cm /Im0 Do Q",
        "% a comment line\n".repeat(200)
    );
    let pattern = TestPattern {
        name: "PImage",
        content: cell.as_str(),
        shading: false,
    };
    let (mut doc, page_id, _) = synthetic_page_with_patterns(true, false, &[], &[pattern]);
    let stream = resource_stream_mut(&mut doc, page_id, b"Pattern", "PImage");
    stream.compress().unwrap();
    assert!(stream.content.len() < cell.len() / 4);
    let fill = "/Pattern cs /PImage scn 0 0 612 792 re f";
    {
        let _budget = FormBytesBudget::set(cell.len() - 1);
        let state = executed_state(&doc, page_id, &[fill]);
        assert!(close(state.covered_image_area(), 0.0));
        assert!(state.form_bytes_exceeded && state.incomplete);
        assert_eq!(state.executed_form_bytes, 0);
    }
    {
        let _budget = FormBytesBudget::set(cell.len());
        let state = executed_state(&doc, page_id, &[fill]);
        assert!(close(state.covered_image_area(), PAGE_AREA));
        assert!(!state.incomplete);
        assert_eq!(state.executed_form_bytes, cell.len());
    }
}

/// The page-content byte budget set for the test's thread, restored
/// when dropped.
struct PageBytesBudget(Option<usize>);

impl PageBytesBudget {
    fn set(bytes: usize) -> Self {
        Self(PAGE_CONTENT_BYTES_OVERRIDE.with(|budget| budget.replace(Some(bytes))))
    }
}

impl Drop for PageBytesBudget {
    fn drop(&mut self) {
        PAGE_CONTENT_BYTES_OVERRIDE.with(|budget| budget.set(self.0));
    }
}

/// A Flate stream whose bytes inflate well past a small budget — a
/// handful compressed, a thousand decoded — added to `doc` and returned
/// by its object id.
fn flate_bomb(doc: &mut Document, decoded: usize) -> ObjectId {
    use lopdf::dictionary;
    let id = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {},
        vec![b'0'; decoded],
    )));
    if let Object::Stream(stream) = doc.objects.get_mut(&id).unwrap() {
        stream.compress().unwrap();
    }
    id
}

/// [`flate_bomb`] as a bound Form XObject: a page's resource walk reads
/// the content of a `Do`-able form, invoked or not, so a form is the
/// placement that reaches the walk's decode.
fn flate_form_bomb(doc: &mut Document, decoded: usize) -> ObjectId {
    use lopdf::dictionary;
    let id = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => Object::Name(b"Form".to_vec()),
            "BBox" => vec![Object::Integer(0), Object::Integer(0),
                           Object::Integer(612), Object::Integer(792)],
        },
        vec![b'0'; decoded],
    )));
    if let Object::Stream(stream) = doc.objects.get_mut(&id).unwrap() {
        stream.compress().unwrap();
    }
    id
}

/// A page bound to `resources`, its `Contents` the object ids given in
/// order, each already a stream object.
fn page_of_streams(
    doc: &mut Document,
    resources: lopdf::Dictionary,
    contents: Vec<ObjectId>,
) -> ObjectId {
    use lopdf::dictionary;
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0),
                               Object::Integer(612), Object::Integer(792)],
            "Resources" => Object::Dictionary(resources),
            "Contents" => contents
                .into_iter()
                .map(Object::Reference)
                .collect::<Vec<_>>(),
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
    page_id
}

/// A page's own Flate stream that inflates past the byte budget is
/// refused before it is held, and no content stream after it is read:
/// the budget bounds all of the page's streams together, whatever their
/// number, and from the stream that would pass it on none is read — the
/// streams before the bomb are scanned whole.
#[test]
fn page_content_inflating_past_the_byte_budget_is_skipped() {
    use lopdf::dictionary;
    let mut doc = Document::with_version("1.4");
    let text = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 72 720 Td (Hi) Tj ET".to_vec(),
    )));
    let bomb = flate_bomb(&mut doc, 1000);
    let after = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 72 700 Td (Yo) Tj ET".to_vec(),
    )));
    let page_id = page_of_streams(&mut doc, dictionary! {}, vec![text, bomb, after]);

    let _budget = PageBytesBudget::set(200);
    let (state, counts) = scan_page(&doc, page_id, &mut HashSet::new(), &mut HashSet::new());
    assert_eq!(
        counts.text_ops, 1,
        "the stream before the bomb is scanned; the bomb and the stream after it are not"
    );
    assert!(
        state.incomplete,
        "a refused content stream leaves the page's evidence incomplete, so no \
         shows-only-a-hidden-text-layer verdict can rest on it"
    );
}
/// A bound form's Flate content that inflates past the walk's remaining
/// byte budget is refused before it is held, and no bound form after it
/// is read — as the executed-form budget refuses — while the forms the
/// budget still admits before it are scanned whole.
#[test]
fn bound_form_inflating_past_the_walk_budget_is_skipped() {
    use lopdf::dictionary;
    let mut doc = Document::with_version("1.4");
    let text_form = {
        let id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "BBox" => vec![Object::Integer(0), Object::Integer(0),
                               Object::Integer(612), Object::Integer(792)],
            },
            b"BT /F1 12 Tf 72 720 Td (Hi) Tj ET".to_vec(),
        )));
        id
    };
    let bomb = flate_form_bomb(&mut doc, 1000);
    let after_form = {
        let id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "BBox" => vec![Object::Integer(0), Object::Integer(0),
                               Object::Integer(612), Object::Integer(792)],
            },
            b"BT /F1 12 Tf 72 700 Td (Yo) Tj ET".to_vec(),
        )));
        id
    };
    let resources = dictionary! {
        "Font" => dictionary! {
            "F1" => Object::Reference(
                doc.add_object(dictionary! {
                    "Type" => "Font",
                    "Subtype" => Object::Name(b"Type1".to_vec()),
                    "BaseFont" => Object::Name(b"Helvetica".to_vec()),
                })
            ),
        },
        "XObject" => dictionary! {
            "FmA" => Object::Reference(text_form),
            "FmZ" => Object::Reference(bomb),
            "FmZz" => Object::Reference(after_form),
        },
    };
    let page_id = page_of_streams(&mut doc, resources, vec![]);
    let page_resources = doc
        .get_object(page_id)
        .and_then(Object::as_dict)
        .unwrap()
        .get(b"Resources")
        .and_then(Object::as_dict)
        .unwrap()
        .clone();

    let mut visited = HashSet::new();
    let mut unique_chars = HashSet::new();
    let mut used_font_ids = HashSet::new();
    let mut font_map = HashMap::new();
    let mut bytes_left = 200usize;
    let mut truncated = false;
    let counts = scan_xobjects_in_resources(
        &doc,
        &page_resources,
        &mut visited,
        &mut unique_chars,
        &mut used_font_ids,
        &mut font_map,
        &mut bytes_left,
        &mut truncated,
    );
    assert_eq!(
        counts.text_ops, 1,
        "the form before the bomb is read; the bomb and the form after it are not"
    );
    assert!(
        truncated,
        "the refused form leaves the walk's tallies incomplete, so no claim rests on them"
    );
}
