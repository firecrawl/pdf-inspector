//! Tests of the budgets the executed-content scan runs under: the bytes
//! of form and pattern-cell content one page executes, and how a stream
//! past them is refused before it is held.

use super::super::{analyze_page_content, detect_from_document, DetectionConfig, PdfType};
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
