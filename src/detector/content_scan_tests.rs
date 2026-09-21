//! Tests of the executed-content scan, and of the invisible-text-layer
//! signal `analyze_page_content` builds on it.

use super::super::content_mask::{
    decode_name_escapes, inline_image_data_length, mask_strings_comments_and_inline_images,
};
use super::super::{
    analyze_page_content, detect_from_document, page_ocr_reasons, page_ocr_signals,
    DetectionConfig, PdfType,
};
use super::fixtures::*;
use super::*;

#[test]
fn render_mode_splits_hidden_text_ops() {
    let content = b"BT /F1 12 Tf 3 Tr (a) Tj 0 Tr (b) Tj 7 Tr [(c)] TJ 1 Tr (d) Tj ET";
    let (counts, executed, hidden) = scan_alone(content);
    assert_eq!(counts.text_ops, 4);
    assert_eq!(executed, 4);
    assert_eq!(hidden, 2, "modes 3 and 7 paint nothing");
}

#[test]
fn render_mode_follows_q_and_capital_q() {
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    // `Q` restores the mode `q` saved; a `Q` with nothing saved changes
    // nothing; a nested save and restore keeps the outer mode.
    let content = b"Q q 3 Tr (a) Tj Q (b) Tj q q 3 Tr Q (c) Tj Q (d) Tj";
    scan_content_stream(
        content,
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut state,
        &[],
    );
    assert_eq!(state.executed_text_ops, 4);
    assert_eq!(state.executed_hidden_text_ops, 1);
    assert_eq!(state.render_mode, 0);
}

#[test]
fn render_mode_carries_across_a_page_s_content_streams() {
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    let mut scan = |content: &[u8]| {
        scan_content_stream(
            content,
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut state,
            &[],
        )
    };
    assert_eq!(scan(b"q 3 Tr").text_ops, 0);
    assert_eq!(scan(b"BT (a) Tj ET Q BT (b) Tj ET").text_ops, 2);
    assert_eq!(state.executed_hidden_text_ops, 1);
}

#[test]
fn strings_comments_and_inline_image_data_hold_no_operators() {
    // Text saying `3 Tr`, a comment saying it, operators spelled inside
    // strings and inline image data: none of them is an operator, and
    // the text is still read.
    let content = b"BT /F1 12 Tf (3 Tr) Tj % 3 Tr\n(a m b c d f) Tj (/F2 9 Tf) Tj \
                    [(x] TJ)] TJ <3320547220> Tj ET % q\n\
                    BI /W 1 /H 1 /BPC 8 /CS /G ID q 3 Tr Q EI BT (e) Tj ET";
    let mut unique_chars = HashSet::new();
    let mut fonts = HashSet::new();
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    let counts = scan_content_stream(content, &mut unique_chars, &mut fonts, &mut state, &[]);
    assert_eq!(counts.text_ops, 6);
    assert_eq!(state.executed_text_ops, 6);
    assert_eq!(state.executed_hidden_text_ops, 0);
    assert_eq!(counts.path_ops, 0);
    assert_eq!(counts.font_changes, 1);
    assert_eq!(fonts.len(), 1);
    for &ch in b"3Tramdxe" {
        assert!(
            unique_chars.contains(&ch),
            "the text is still read: {}",
            ch as char
        );
    }
}

#[test]
fn quote_show_text_operators_are_counted_and_follow_the_render_mode() {
    // `'` and `"` show text as `Tj` does; an apostrophe inside a
    // string is not an operator.
    let hidden = b"BT /F1 10 Tf 3 Tr 12 TL 72 720 Td (a) ' (b) ' 1 0 (c) \" (don't) Tj ET";
    let (counts, executed_ops, hidden_ops) = scan_alone(hidden);
    assert_eq!((counts.text_ops, executed_ops, hidden_ops), (4, 4, 4));

    let visible = b"BT /F1 10 Tf 12 TL 72 720 Td (a) ' (b) ' 1 0 (c) \" ET";
    let (counts, executed_ops, hidden_ops) = scan_alone(visible);
    assert_eq!((counts.text_ops, executed_ops, hidden_ops), (3, 3, 0));

    // Their strings are read like `Tj`'s.
    let mut unique_chars = HashSet::new();
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    scan_content_stream(
        visible,
        &mut unique_chars,
        &mut HashSet::new(),
        &mut state,
        &[],
    );
    for &ch in b"abc" {
        assert!(unique_chars.contains(&ch), "{}", ch as char);
    }
}

#[test]
fn id_begins_image_data_only_inside_an_inline_image() {
    // `/ID` as a name, and a bare `ID` with no inline image open, are
    // left alone; the `ID` of a `BI` still hides its data through `EI`.
    let content = b"/Span <</ID 7 /MCID 0>> BDC BT /F1 12 Tf (a) Tj ET EMC \
                    ID BT (b) Tj ET \
                    BI /W 1 /H 1 /BPC 8 /CS /G ID q 3 Tr Q EI BT (c) Tj ET";
    let (counts, executed_ops, hidden) = scan_alone(content);
    assert_eq!((counts.text_ops, executed_ops, hidden), (3, 3, 0));
}

#[test]
fn render_mode_out_of_range_is_not_set() {
    let (counts, _, hidden) = scan_alone(b"9 Tr (a) Tj 3.5 Tr (b) Tj");
    assert_eq!(counts.text_ops, 2);
    assert_eq!(hidden, 0);
}

#[test]
fn saved_states_past_the_depth_cap_restore_nothing() {
    let mut content = Vec::new();
    for _ in 0..=SCAN_STATE_MAX_DEPTH {
        content.extend_from_slice(b"q ");
    }
    content.extend_from_slice(b"3 Tr ");
    for _ in 0..=SCAN_STATE_MAX_DEPTH {
        content.extend_from_slice(b"Q ");
    }
    content.extend_from_slice(b"(a) Tj");
    let (counts, _, hidden) = scan_alone(&content);
    assert_eq!(counts.text_ops, 1);
    assert_eq!(hidden, 0, "the outermost `Q` restores mode 0");
}

#[test]
fn covered_image_area_follows_the_matrix_and_the_page() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    assert!(close(covered("q 612 0 0 792 0 0 cm /Im0 Do Q"), PAGE_AREA));
    // Turned, its box runs past the right edge: only what lies on the
    // page counts.
    assert!(about(
        covered("q 0 612 -792 0 792 0 cm /Im0 Do Q"),
        612.0 * 612.0
    ));
    // Scaled under nested q/Q.
    assert!(about(
        covered("q 2 0 0 2 0 0 cm q 100 0 0 50 0 0 cm /Im0 Do Q Q"),
        20_000.0
    ));
    // Shifted mostly off the page, or a little.
    assert!(about(
        covered("q 612 0 0 792 500 0 cm /Im0 Do Q"),
        112.0 * 792.0
    ));
    assert!(about(
        covered("q 612 0 0 792 -100 0 cm /Im0 Do Q"),
        512.0 * 792.0
    ));
    // Drawn twice, or as two strips: the page, once.
    assert!(close(
        covered("q 612 0 0 792 0 0 cm /Im0 Do Q q 612 0 0 792 0 0 cm /Im0 Do Q"),
        PAGE_AREA
    ));
    assert!(close(
        covered("q 612 0 0 396 0 0 cm /Im0 Do Q q 612 0 0 396 0 396 cm /Im0 Do Q"),
        PAGE_AREA
    ));
    // A scan tiled into two thousand strips, each thinner than a cell.
    let strips: String = (0..2000)
        .map(|k| format!("q 612 0 0 0.396 0 {} cm /Im0 Do Q\n", k as f64 * 0.396))
        .collect();
    assert!(about(covered(&strips), PAGE_AREA));
    // A name bound to nothing draws nothing.
    assert!(close(covered("q 612 0 0 792 0 0 cm /Im9 Do Q"), 0.0));
}

#[test]
fn forms_are_run_in_place_under_the_state_in_force() {
    let (doc, page_id, _) = synthetic_page(
        true,
        false,
        &[
            TestForm {
                name: "FmEmpty",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmHidden",
                content: "3 Tr BT /F1 10 Tf 72 700 Td (b) Tj ET",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmImage",
                content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmText",
                content: "BT /F1 10 Tf 0 Tr 72 700 Td (c) Tj ET",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmPlain",
                content: "BT /F1 10 Tf 72 700 Td (c) Tj ET",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmScaled",
                content: "306 0 0 396 0 0 cm /Im0 Do",
                matrix: Some([2, 0, 0, 2, 0, 0]),
                ..PAGE_FORM
            },
            TestForm {
                name: "FmUnbalanced",
                content: "Q Q 3 Tr BT /F1 10 Tf (a) Tj ET q q",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmLoop",
                content: "/FmLoop Do BT /F1 10 Tf 0 Tr (z) Tj ET",
                ..PAGE_FORM
            },
        ],
    );
    let run = |content: &str| executed(&doc, page_id, &[content]);

    // A form invoked inside a clip-only text's level shows it through
    // only when it paints: an empty form or an invisible-only form
    // does not; an image or visible text inside one does.
    let clipped = |form: &str| run(&format!("q BT /F1 10 Tf 7 Tr (a) Tj ET /{form} Do Q"));
    let (executed_ops, hidden, _) = clipped("FmEmpty");
    assert_eq!((executed_ops, hidden), (1, 1));
    let (executed_ops, hidden, _) = clipped("FmHidden");
    assert_eq!((executed_ops, hidden), (2, 2));
    let (executed_ops, hidden, covered) = clipped("FmImage");
    assert_eq!((executed_ops, hidden), (1, 0));
    assert!(
        close(covered, PAGE_AREA),
        "the image resolves in the invoker's resources"
    );
    let (executed_ops, hidden, _) = clipped("FmText");
    assert_eq!((executed_ops, hidden), (2, 0));

    // A form inherits the render mode in force, unless it sets its own.
    let (executed_ops, hidden, _) = run("3 Tr /FmPlain Do");
    assert_eq!((executed_ops, hidden), (1, 1));
    let (executed_ops, hidden, _) = run("3 Tr /FmText Do");
    assert_eq!((executed_ops, hidden), (1, 0));

    // A form's `/Matrix` scales what it draws.
    assert!(close(run("/FmScaled Do").2, PAGE_AREA));

    // A form's `Q`s cannot close its invoker's levels, and the levels
    // it leaves open close with it.
    let (executed_ops, hidden, _) = run("q 3 Tr /FmUnbalanced Do Q BT /F1 10 Tf (d) Tj ET");
    assert_eq!((executed_ops, hidden), (2, 1));

    // A form is run at each invocation, under the matrix in force
    // there: drawn off the page first, then over it.
    let (executed_ops, hidden, _) = run("/FmText Do /FmText Do");
    assert_eq!((executed_ops, hidden), (2, 0));
    assert!(
        close(
            run("q 1 0 0 1 700 0 cm /FmImage Do Q /FmImage Do").2,
            PAGE_AREA
        ),
        "the second invocation draws the image over the page"
    );

    // A form invoking itself is not run again.
    let (executed_ops, hidden, _) = run("/FmLoop Do");
    assert_eq!((executed_ops, hidden), (1, 0));
}

#[test]
fn a_form_s_bbox_clips_what_it_draws() {
    let (doc, page_id, _) = synthetic_page(
        true,
        false,
        &[TestForm {
            name: "FmCorner",
            content: "q 612 0 0 792 0 0 cm /Im0 Do Q BT /F1 10 Tf 0 Tr 10 10 Td (t) Tj ET",
            bbox: &[0, 0, 50, 50],
            ..PAGE_FORM
        }],
    );
    let run = |content: &str| executed(&doc, page_id, &[content]);

    // A page-sized image drawn inside a form whose box is a corner of
    // the page covers that corner only. The text inside still counts:
    // text positions are not followed.
    let (executed_ops, _, covered) = run("/FmCorner Do");
    assert_eq!(executed_ops, 1);
    assert!(covered > 0.0 && covered < PAGE_AREA / 20.0, "{covered}");

    // The same form moved off the page shows nothing and is not read.
    let (executed_ops, _, covered) = run("q 1 0 0 1 700 0 cm /FmCorner Do Q");
    assert_eq!((executed_ops, covered), (0, 0.0));

    // Scaled up, the corner grows with it.
    let (_, _, covered) = run("q 12.24 0 0 15.84 0 0 cm /FmCorner Do Q");
    assert!(close(covered, PAGE_AREA));
}

#[test]
fn an_exhausted_form_budget_leaves_the_page_unflagged() {
    let (mut doc, page_id, content_id) = synthetic_page(
        true,
        false,
        &[
            TestForm {
                name: "FmEmpty",
                ..PAGE_FORM
            },
            TestForm {
                name: "FmText",
                content: "BT /F1 10 Tf 0 Tr 72 700 Td (c) Tj ET",
                ..PAGE_FORM
            },
        ],
    );
    let layer = glyph_layer(3);
    let page_with = |empties: usize| {
        format!(
            "{FULL_PAGE_IMAGE}{layer}{}/FmText Do",
            "/FmEmpty Do\n".repeat(empties)
        )
    };

    // Within the budget, the visible text in the last form is read,
    // and the page is no hidden layer.
    set_page_content(&mut doc, content_id, &page_with(FORM_INVOCATIONS_MAX - 1));
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 121);
    assert!(!analysis.has_invisible_text_layer);

    // One invocation more and the last form goes unread: the page's
    // evidence is incomplete, so it is not flagged either.
    set_page_content(&mut doc, content_id, &page_with(FORM_INVOCATIONS_MAX));
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(
        analysis.executed_text_operator_count, 120,
        "the last form was not read"
    );
    assert!(analysis.has_covering_image);
    assert!(!analysis.has_invisible_text_layer);
    assert!(!page_ocr_signals(&doc, page_id).has_invisible_text_layer);
}

#[test]
fn subtype_held_by_reference_is_resolved() {
    let (mut doc, page_id, content_id) = synthetic_page(
        true,
        false,
        &[TestForm {
            name: "FmHidden",
            content: "3 Tr BT /F1 10 Tf 72 700 Td (a) Tj (b) Tj ET",
            ..PAGE_FORM
        }],
    );
    let form_name = doc.add_object(Object::Name(b"Form".to_vec()));
    let image_name = doc.add_object(Object::Name(b"Image".to_vec()));
    let xobjects: Vec<(Vec<u8>, ObjectId)> = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap()
        .iter()
        .map(|(name, value)| (name.clone(), value.as_reference().unwrap()))
        .collect();
    let point_subtypes_at = |doc: &mut Document, image: ObjectId, form: ObjectId| {
        for (name, id) in &xobjects {
            if let Ok(Object::Stream(stream)) = doc.get_object_mut(*id) {
                let target = if name == b"Im0" { image } else { form };
                stream.dict.set("Subtype", Object::Reference(target));
            }
        }
    };
    set_page_content(
        &mut doc,
        content_id,
        "q 612 0 0 792 0 0 cm /Im0 Do Q /FmHidden Do",
    );

    // Both `/Subtype`s held by reference to a name object.
    point_subtypes_at(&mut doc, image_name, form_name);
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(
        (
            analysis.executed_text_operator_count,
            analysis.invisible_text_operator_count
        ),
        (2, 2)
    );
    assert!(analysis.has_covering_image);
    assert!(analysis.has_invisible_text_layer);

    // A reference that resolves to nothing leaves the stream neither
    // image nor form, as the resource walks leave it.
    let dangling = doc.new_object_id();
    point_subtypes_at(&mut doc, dangling, dangling);
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 0);
    assert!(!analysis.has_covering_image);
    assert!(!analysis.has_invisible_text_layer);
}

#[test]
fn inline_image_data_without_a_delimited_ei_is_bounded() {
    // Data written flush against its `EI`, and data with no `EI` at
    // all, blank no more than the image: the text after is counted.
    let (counts, executed_ops, _) =
        scan_alone(b"BI /W 1 /H 1 /BPC 8 /CS /G ID xEI BT /F1 10 Tf (a) Tj ET");
    assert_eq!((counts.text_ops, executed_ops), (1, 1));
    let (counts, _, _) = scan_alone(b"BI /W 2 /H 1 /BPC 8 /CS /G ID abEIBT /F1 10 Tf (a) Tj ET");
    assert_eq!(counts.text_ops, 1);

    let unknown = |_: &[u8]| None;
    let length = |header: &[u8]| inline_image_data_length(header, &unknown);
    assert_eq!(
        length(b"/W 3 /H 2 /BPC 1 /CS /G"),
        Some(2),
        "rows padded to bytes"
    );
    assert_eq!(length(b"/W 2 /H 2 /BPC 8 /CS /RGB"), Some(12));
    assert_eq!(length(b"/W 2/H 2/BPC 8/CS/CMYK"), Some(16), "no whitespace");
    assert_eq!(
        length(b"/Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray"),
        Some(4)
    );
    assert_eq!(
        length(b"/W 8 /H 1 /IM true"),
        Some(1),
        "an image mask: one bit a sample"
    );
    assert_eq!(length(b"/W 8 /H 1 /IM true /BPC 8"), Some(1));
    assert_eq!(
        length(b"/W 2 /H 2 /BPC 8 /CS [/I /RGB 1 <000000ffffff>]"),
        Some(4)
    );
    assert_eq!(
        length(b"/W 2 /H 2 /BPC 8 /CS /G /D [1 0] /DP << /Predictor 1 >>"),
        Some(4)
    );
    assert_eq!(
        length(b"/W 2 /H 2 /BPC 8 /CS /G /F /Fl /L 7"),
        Some(7),
        "a stated length"
    );
    assert_eq!(
        length(b"/W 2 /H 2 /F /AHx"),
        None,
        "filtered: no length known"
    );
    assert_eq!(length(b"/W 2 /H 2 /BPC 8 /CS /G /F [/AHx /Fl]"), None);
    assert_eq!(
        length(b"/W 2 /H 2 /BPC 8 /CS /G /F []"),
        Some(4),
        "no filter after all"
    );
    assert_eq!(length(b"/W 2 /H 2 /CS /G"), None, "no bits per component");
    assert_eq!(length(b"/W 2 /H 2 /BPC 8"), None, "no colour space");
    assert_eq!(
        length(b"/W 2 /H 2 /BPC 8 /CS /CS0"),
        None,
        "a name the resources must say"
    );
    let three = |name: &[u8]| (name == b"CS0").then_some(3);
    assert_eq!(
        inline_image_data_length(b"/W 2 /H 2 /BPC 8 /CS /CS0", &three),
        Some(12)
    );
}

#[test]
fn resources_merely_bound_are_not_content() {
    let hidden_layer = "3 Tr BT /F1 10 Tf 72 700 Td (a) Tj (b) Tj ET";
    let (mut doc, page_id, content_id) = synthetic_page(
        false,
        true,
        &[TestForm {
            name: "FmHidden",
            content: hidden_layer,
            ..PAGE_FORM
        }],
    );

    // Visible text on a page that binds, without using them, a large
    // image and a form holding a hidden layer.
    set_page_content(
        &mut doc,
        content_id,
        "BT /F1 12 Tf 72 720 Td (Plain visible text) Tj 0 -14 Td (on an ordinary page) Tj ET",
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(
        analysis.text_operator_count, 4,
        "the tally over every form bound is as it was"
    );
    assert!(
        analysis.has_template_image,
        "sanity: the bound image has the pixels of a template image"
    );
    assert_eq!(analysis.executed_text_operator_count, 2);
    assert_eq!(analysis.invisible_text_operator_count, 0);
    assert!(!analysis.has_covering_image);
    assert!(!analysis.has_invisible_text_layer);
    assert!(!page_ocr_signals(&doc, page_id).has_invisible_text_layer);

    // The same image drawn over the page and the same form invoked,
    // and nothing else: a layer nobody sees.
    set_page_content(
        &mut doc,
        content_id,
        "q 612 0 0 792 0 0 cm /ImBig Do Q /FmHidden Do",
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 2);
    assert_eq!(analysis.invisible_text_operator_count, 2);
    assert!(analysis.has_covering_image);
    assert!(analysis.has_invisible_text_layer);
    assert!(page_ocr_signals(&doc, page_id).has_invisible_text_layer);
}

#[test]
fn an_image_drawn_off_the_page_does_not_cover_it() {
    let (mut doc, page_id, content_id) = synthetic_page(true, false, &[]);
    let layer = glyph_layer(3);
    set_page_content(
        &mut doc,
        content_id,
        &format!("q 612 0 0 792 500 0 cm /Im0 Do Q\n{layer}"),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(
        !analysis.has_covering_image,
        "less than a fifth of the image lies on the page"
    );
    assert!(!analysis.has_invisible_text_layer);

    set_page_content(
        &mut doc,
        content_id,
        &format!("q 612 0 0 792 -100 0 cm /Im0 Do Q\n{layer}"),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(
        analysis.has_covering_image,
        "five sixths of the image lies on the page"
    );
    assert!(analysis.has_invisible_text_layer);
}

#[test]
fn invisible_layer_under_a_covering_image_is_flagged() {
    let (doc, page_id) = layered_scan_page(true, Some(3), false, None);
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.text_operator_count, 120);
    assert_eq!(analysis.executed_text_operator_count, 120);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(
        analysis.has_covering_image,
        "a 2×2 image scaled to the page covers it"
    );
    assert!(
        !analysis.has_template_image,
        "sanity: four pixels are no template image"
    );
    assert!(analysis.has_invisible_text_layer);
    assert_eq!(
        page_ocr_reasons(&analysis),
        vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER]
    );
    let signals = page_ocr_signals(&doc, page_id);
    assert!(signals.has_invisible_text_layer);
    assert!(!signals.template_image_needs_ocr);
    assert!(!signals.has_vector_text);

    // Mode 7 (clip only) with nothing painted through it paints
    // nothing either.
    let (doc, page_id) = layered_scan_page(true, Some(7), false, None);
    assert!(analyze_page_content(&doc, page_id).has_invisible_text_layer);
}

#[test]
fn invisible_layer_inside_a_form_xobject_is_followed() {
    let (doc, page_id) = layered_scan_page(true, Some(3), true, None);
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.text_operator_count, 120);
    assert_eq!(analysis.executed_text_operator_count, 120);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(analysis.has_invisible_text_layer);

    // The form's mode does not reach a caption the page paints itself.
    let (doc, page_id) = layered_scan_page(true, Some(3), true, Some("Figure 1"));
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 121);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(!analysis.has_invisible_text_layer);
}

#[test]
fn painted_layer_caption_no_image_or_image_alone_is_not_an_invisible_layer() {
    // The same layer painted (mode 0) is a text page over an image.
    let (doc, page_id) = layered_scan_page(true, Some(0), false, None);
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_covering_image);
    assert_eq!(analysis.invisible_text_operator_count, 0);
    assert!(!analysis.has_invisible_text_layer);
    assert!(!page_ocr_signals(&doc, page_id).has_invisible_text_layer);

    // One visible caption over the image: the page shows text of its own.
    let (doc, page_id) = layered_scan_page(true, Some(3), false, Some("Figure 1"));
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 121);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(!analysis.has_invisible_text_layer);

    // Invisible text with no image under it is not a scan.
    let (doc, page_id) = layered_scan_page(false, Some(3), false, None);
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.invisible_text_operator_count, 120);
    assert!(!analysis.has_covering_image);
    assert!(!analysis.has_invisible_text_layer);

    // An image alone is a scan with no text layer at all.
    let (doc, page_id) = layered_scan_page(true, None, false, None);
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_covering_image);
    assert_eq!(analysis.executed_text_operator_count, 0);
    assert!(!analysis.has_invisible_text_layer);
    assert_eq!(page_ocr_reasons(&analysis), vec![crate::OCR_REASON_SCANNED]);
}

#[test]
fn an_inline_image_paints_the_unit_square_as_an_image_xobject_does() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let run = |content: &str| executed(&doc, page_id, &[content]);
    let inline = "BI /W 1 /H 1 /BPC 8 /CS /G ID x EI";
    // Covering the page, or shifted mostly off it.
    assert!(close(
        run(&format!("q 612 0 0 792 0 0 cm {inline} Q")).2,
        PAGE_AREA
    ));
    assert!(about(
        run(&format!("q 612 0 0 792 500 0 cm {inline} Q")).2,
        112.0 * 792.0
    ));
    // It paints through a clip-only text's clip like any image.
    let (executed_ops, hidden, _) = run(&format!("q BT /F1 10 Tf 7 Tr (a) Tj ET {inline} Q"));
    assert_eq!((executed_ops, hidden), (1, 0));
}

#[test]
fn a_clipping_path_narrows_what_an_image_covers() {
    let (doc, page_id, _) = synthetic_page(
        true,
        false,
        &[TestForm {
            name: "FmImage",
            content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
            ..PAGE_FORM
        }],
    );
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    let full = "q 612 0 0 792 0 0 cm /Im0 Do Q";
    // A rectangle clips exactly; `Q` restores the clip that was in force.
    assert!(about(
        covered(&format!("0 0 100 100 re W n {full}")),
        100.0 * 100.0
    ));
    assert!(close(
        covered(&format!("q 0 0 100 100 re W n {full} Q {full}")),
        PAGE_AREA
    ));
    assert!(about(
        covered(&format!("0 0 100 100 re W* n {full}")),
        100.0 * 100.0
    ));
    // The clip is set under the matrix in force, and narrows forms too.
    assert!(about(
        covered(&format!("q 2 0 0 2 0 0 cm 0 0 50 50 re W n {full} Q")),
        100.0 * 100.0
    ));
    assert!(about(
        covered("0 0 100 100 re W n /FmImage Do"),
        100.0 * 100.0
    ));
    // Any other shape is approximated by its bounding box; a path with no
    // extent clips everything away.
    assert!(about(
        covered(&format!("0 0 m 100 0 l 0 100 l h W n {full}")),
        100.0 * 100.0
    ));
    assert!(close(covered(&format!("W n {full}")), 0.0));
    // A path that is painted rather than clipped changes nothing.
    assert!(close(
        covered(&format!("0 0 100 100 re f {full}")),
        PAGE_AREA
    ));
}

#[test]
fn a_form_box_with_trailing_numbers_is_read_by_its_first_four() {
    let (doc, page_id, _) = synthetic_page(
        true,
        false,
        &[
            TestForm {
                name: "FmSix",
                content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
                bbox: &[0, 0, 50, 50, 612, 792],
                ..PAGE_FORM
            },
            TestForm {
                name: "FmThree",
                content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
                bbox: &[0, 0, 50],
                ..PAGE_FORM
            },
        ],
    );
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    assert!(about(covered("/FmSix Do"), 50.0 * 50.0));
    // Three numbers are no box: there is nothing to clip by, and the form
    // runs unclipped.
    assert!(close(covered("/FmThree Do"), PAGE_AREA));
}

#[test]
fn empty_show_operands_show_no_text() {
    // Nothing to show is nothing shown: neither counted nor hidden.
    let (counts, executed_ops, hidden) =
        scan_alone(b"BT /F1 10 Tf 3 Tr () Tj <> Tj [] TJ [5 -8] TJ ( ) Tj ET");
    assert_eq!((counts.text_ops, executed_ops, hidden), (1, 1, 1));

    // A covering image with only empty shows over it hides no text layer.
    let (mut doc, page_id, content_id) = synthetic_page(true, false, &[]);
    set_page_content(
        &mut doc,
        content_id,
        "q 612 0 0 792 0 0 cm /Im0 Do Q\n3 Tr BT /F1 10 Tf () Tj <> Tj [] TJ [5 -8] TJ ET",
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert_eq!(analysis.executed_text_operator_count, 0);
    assert!(analysis.has_covering_image);
    assert!(!analysis.has_invisible_text_layer);
}

#[test]
fn operators_need_no_whitespace_after_them() {
    let (counts, executed_ops, hidden) = scan_alone(b"3 Tr (a)Tj(b)Tj");
    assert_eq!((counts.text_ops, executed_ops, hidden), (2, 2, 2));
    let (counts, executed_ops, hidden) = scan_alone(b"(a)Tj[<41>]TJ");
    assert_eq!((counts.text_ops, executed_ops, hidden), (2, 2, 0));
    // Path operators and the font operator too.
    let (counts, _, _) = scan_alone(b"/F1 12 Tf(a)Tj 0 0 m 1 1 l S/Im0 Do 0 0 1 1 re f(b)Tj");
    assert_eq!(counts.font_changes, 1);
    assert_eq!(counts.path_ops, 5);
    assert_eq!(counts.text_ops, 2);
}

#[test]
fn a_tiling_pattern_that_draws_an_image_paints_coverage() {
    let patterns = [
        TestPattern {
            name: "PImage",
            content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
            shading: false,
        },
        TestPattern {
            name: "PInline",
            content: "q 10 0 0 10 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q",
            shading: false,
        },
        TestPattern {
            name: "PPaths",
            content: "0 0 10 10 re f",
            shading: false,
        },
        TestPattern {
            name: "PShade",
            content: "",
            shading: true,
        },
    ];
    let (doc, page_id, _) = synthetic_page_with_patterns(true, false, &[], &patterns);
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    let fill = "0 0 612 792 re f";

    // A fill with a pattern whose cell draws an image — an XObject or an
    // inline image — covers the path's box.
    assert!(close(
        covered(&format!("/Pattern cs /PImage scn {fill}")),
        PAGE_AREA
    ));
    assert!(close(
        covered(&format!("/Pattern cs /PInline scn {fill}")),
        PAGE_AREA
    ));
    // A cell drawing only paths, a shading pattern, or no pattern at all
    // paints no coverage.
    assert!(close(
        covered(&format!("/Pattern cs /PPaths scn {fill}")),
        0.0
    ));
    assert!(close(
        covered(&format!("/Pattern cs /PShade scn {fill}")),
        0.0
    ));
    assert!(close(covered(fill), 0.0));
    // A stroke likewise, over the stroked path's box; the stroke colour
    // does not fill.
    assert!(close(
        covered("/Pattern CS /PImage SCN 0 0 m 612 792 l S"),
        PAGE_AREA
    ));
    assert!(close(
        covered("/Pattern CS /PImage SCN 0 0 612 792 re f"),
        0.0
    ));
    // The selection follows the graphics state: `Q` restores it, and a
    // plain colour, another colour space or numbers end it.
    assert!(close(
        covered(&format!("q /Pattern cs /PImage scn Q {fill}")),
        0.0
    ));
    assert!(close(
        covered(&format!("/Pattern cs /PImage scn 0.5 g {fill}")),
        0.0
    ));
    assert!(close(
        covered(&format!("/Pattern cs /PImage scn /DeviceGray cs {fill}")),
        0.0
    ));
    assert!(close(
        covered(&format!("/Pattern cs /PImage scn 0.2 0.4 scn {fill}")),
        0.0
    ));
    // The clip in force applies.
    assert!(about(
        covered(&format!(
            "0 0 100 100 re W n /Pattern cs /PImage scn {fill}"
        )),
        100.0 * 100.0
    ));

    // A hidden layer over such a fill is a layer nobody sees.
    let (mut doc, page_id, content_id) =
        synthetic_page_with_patterns(true, false, &[], &patterns[..1]);
    set_page_content(
        &mut doc,
        content_id,
        &format!("/Pattern cs /PImage scn {fill}\n{}", glyph_layer(3)),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_covering_image);
    assert!(analysis.has_invisible_text_layer);
}

#[test]
fn an_inline_image_alone_makes_an_image_page() {
    // No image XObject anywhere: the page's only raster is inline.
    let (mut doc, page_id, content_id) = synthetic_page(false, false, &[]);
    let raster = "q 612 0 0 792 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q\n";

    // Under a hidden layer, an image page with a text layer nobody sees,
    // for the document and for the page alike.
    set_page_content(&mut doc, content_id, &format!("{raster}{}", glyph_layer(3)));
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_images);
    assert_eq!(analysis.image_count, 1);
    assert!(analysis.has_invisible_text_layer);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_ne!(detected.pdf_type, PdfType::TextBased);
    assert_eq!(detected.pages_needing_ocr, vec![1]);
    assert_eq!(
        detected.ocr_reasons_by_page.get(&1),
        Some(&vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER.to_string()])
    );
    assert!(page_ocr_signals(&doc, page_id).has_invisible_text_layer);

    // With no text at all, a scan.
    set_page_content(&mut doc, content_id, raster);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(detected.pdf_type, PdfType::Scanned);
    assert_eq!(
        detected.ocr_reasons_by_page.get(&1),
        Some(&vec![crate::OCR_REASON_SCANNED.to_string()])
    );
}

#[test]
fn an_inline_image_in_a_form_counts_only_when_the_form_runs() {
    // No image XObject anywhere; the page's only raster sits in a form.
    let (mut doc, page_id, content_id) = synthetic_page(
        false,
        false,
        &[TestForm {
            name: "FmInline",
            content: "q 612 0 0 792 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q",
            ..PAGE_FORM
        }],
    );
    let body: String = (0..12)
        .map(|n| {
            format!(
                "BT /F1 12 Tf 72 {} Td (Paragraph line {n} of body text) Tj ET\n",
                720 - 16 * n
            )
        })
        .collect();

    // Bound but never invoked, the form's inline image is no image of the
    // page: a text page.
    set_page_content(&mut doc, content_id, &body);
    let analysis = analyze_page_content(&doc, page_id);
    assert!(!analysis.has_images);
    assert_eq!(analysis.image_count, 0);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(detected.pdf_type, PdfType::TextBased);
    assert!(detected.pages_needing_ocr.is_empty());

    // Invoked, it is: a text page with a picture, and under a hidden layer
    // an image page with a text layer nobody sees.
    set_page_content(&mut doc, content_id, &format!("/FmInline Do\n{body}"));
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_images);
    assert!(!analysis.has_invisible_text_layer);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(detected.pdf_type, PdfType::TextBased);
    assert!(detected.pages_needing_ocr.is_empty());

    set_page_content(
        &mut doc,
        content_id,
        &format!("/FmInline Do\n{}", glyph_layer(3)),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_images);
    assert!(analysis.has_invisible_text_layer);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_ne!(detected.pdf_type, PdfType::TextBased);
    assert_eq!(
        detected.ocr_reasons_by_page.get(&1),
        Some(&vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER.to_string()])
    );
}

/// A pattern whose cell draws a name its own resources do not bind draws
/// what the resources in force where it is used bind: the image in one
/// form, a form drawing paths in another, nothing in the page's own —
/// whichever was used first.
#[test]
fn a_pattern_is_judged_in_the_resources_in_force_where_it_is_used() {
    let forms = [
        TestForm {
            name: "FmPaths",
            content: "0 0 10 10 re f",
            ..PAGE_FORM
        },
        TestForm {
            name: "FmLeft",
            content: "/Pattern cs /PX scn 0 0 306 792 re f",
            xobjects: &[("X", "Im0")],
            ..PAGE_FORM
        },
        TestForm {
            name: "FmRight",
            content: "/Pattern cs /PX scn 306 0 306 792 re f",
            xobjects: &[("X", "FmPaths")],
            ..PAGE_FORM
        },
    ];
    let pattern = TestPattern {
        name: "PX",
        content: "q 612 0 0 792 0 0 cm /X Do Q",
        shading: false,
    };
    let (doc, page_id, _) = synthetic_page_with_patterns(true, false, &forms, &[pattern]);
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    // Used where `X` is the image, and where it is the paths form, in
    // either order: the half the image fills is covered, the other not.
    assert!(close(covered("/FmLeft Do /FmRight Do"), PAGE_AREA / 2.0));
    assert!(close(covered("/FmRight Do /FmLeft Do"), PAGE_AREA / 2.0));
    // Where nothing binds `X` the pattern draws no image; used there
    // first, it still draws one where the image is.
    let page_fill = "/Pattern cs /PX scn 0 0 612 792 re f";
    assert!(close(covered(page_fill), 0.0));
    assert!(close(
        covered(&format!("{page_fill} /FmLeft Do")),
        PAGE_AREA / 2.0
    ));
}

/// A form invoked again is decompressed and masked once: both copies are
/// kept while the budget lasts, and a form too large for both is kept
/// not at all.
#[test]
fn a_form_s_masked_content_is_kept_with_it() {
    let form = TestForm {
        name: "FmTwice",
        content: "BT /F1 12 Tf (a % not a comment) Tj ET",
        ..PAGE_FORM
    };
    let (doc, page_id, _) = synthetic_page(false, false, &[form]);
    let state = executed_state(&doc, page_id, &["/FmTwice Do /FmTwice Do"]);
    assert_eq!(state.executed_text_ops, 2);
    let kept = state
        .form_content
        .values()
        .next()
        .expect("the form is kept");
    assert_eq!(kept.content, form.content.as_bytes());
    assert_eq!(
        kept.masked,
        mask_strings_comments_and_inline_images(form.content.as_bytes(), &|_| None)
    );
    assert_eq!(state.form_content_bytes, 2 * form.content.len());

    let too_large = "(a) Tj ".repeat(FORM_CONTENT_CACHE_MAX_BYTES / 14 + 1);
    let form = TestForm {
        name: "FmLarge",
        content: too_large.as_str(),
        ..PAGE_FORM
    };
    let (doc, page_id, _) = synthetic_page(false, false, &[form]);
    let state = executed_state(&doc, page_id, &["/FmLarge Do /FmLarge Do"]);
    assert_eq!(
        state.executed_text_ops as usize,
        2 * (FORM_CONTENT_CACHE_MAX_BYTES / 14 + 1)
    );
    assert!(state.form_content.is_empty());
    assert_eq!(state.form_content_bytes, 0);
}

/// A name written with `#xx` escapes names the same resource as one
/// written plainly: `/Im#30` is `Im0`, for an image, a form, a pattern
/// and a font alike; a `#` not followed by two hex digits is kept.
#[test]
fn names_written_with_escapes_find_their_resources() {
    assert_eq!(decode_name_escapes(b"Im#30"), b"Im0");
    assert_eq!(decode_name_escapes(b"#46#6d#30"), b"Fm0");
    assert_eq!(decode_name_escapes(b"A#2"), b"A#2");
    assert_eq!(decode_name_escapes(b"A#zz"), b"A#zz");
    assert_eq!(decode_name_escapes(b"plain"), b"plain");

    let form = TestForm {
        name: "Fm0",
        content: "q 612 0 0 792 0 0 cm /Im#30 Do Q",
        ..PAGE_FORM
    };
    let pattern = TestPattern {
        name: "PImage",
        content: "q 612 0 0 792 0 0 cm /Im0 Do Q",
        shading: false,
    };
    let (mut doc, page_id, content_id) =
        synthetic_page_with_patterns(true, false, &[form], &[pattern]);
    let covered = |content: &str| executed(&doc, page_id, &[content]).2;
    assert!(close(
        covered("q 612 0 0 792 0 0 cm /Im#30 Do Q"),
        PAGE_AREA
    ));
    assert!(close(covered("/Fm#30 Do"), PAGE_AREA));
    assert!(close(
        covered("/Pattern cs /P#49mage scn 0 0 612 792 re f"),
        PAGE_AREA
    ));
    assert!(close(covered("q 612 0 0 792 0 0 cm /Im#31 Do Q"), 0.0));

    let mut fonts = HashSet::new();
    let empty = Document::new();
    let mut state = ContentScanState::new(&empty, PageBox::LETTER, false);
    scan_content_stream(
        b"BT /F#31 12 Tf (a) Tj ET",
        &mut HashSet::new(),
        &mut fonts,
        &mut state,
        &[],
    );
    assert!(fonts.contains(b"F1".as_slice()));

    // A scan drawn by its escaped name under a hidden layer is a layer
    // nobody sees.
    set_page_content(
        &mut doc,
        content_id,
        &format!("q 612 0 0 792 0 0 cm /Im#30 Do Q\n{}", glyph_layer(3)),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_covering_image && analysis.has_invisible_text_layer);
    let detected = detect_from_document(&doc, 1, &DetectionConfig::default()).unwrap();
    assert_eq!(
        detected.ocr_reasons_by_page.get(&1),
        Some(&vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER.to_string()])
    );
}

/// NUL separates operands and operators as the other whitespace bytes of
/// the file format do — in the scan's token boundaries and in every
/// operand lookback: numbers, names, show operands, font names.
#[test]
fn nul_is_whitespace_between_operands() {
    let content = b"BT\0/F1\x0012\0Tf\x003\0Tr\0(a)\0Tj\0[(b)]\0TJ\0ET";
    let mut fonts = HashSet::new();
    let doc = Document::new();
    let mut state = ContentScanState::new(&doc, PageBox::LETTER, false);
    let counts = scan_content_stream(content, &mut HashSet::new(), &mut fonts, &mut state, &[]);
    assert_eq!(counts.text_ops, 2);
    assert_eq!(counts.font_changes, 1);
    assert!(fonts.contains(b"F1".as_slice()));
    assert_eq!(state.executed_hidden_text_ops, 2, "`3 Tr` read across NULs");
    assert_eq!(state.font_size, Some(12.0));

    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let (executed_ops, hidden, covered) = executed(
        &doc,
        page_id,
        &["q\x00612\x000\x000\x00792\x000\x000\x00cm\x00/Im0\x00Do\x00Q\x00BT\x007\x00Tr\x00/F1\x0010\x00Tf\x0072\x00700\x00Td\x00(a)\x00Tj\x00ET"],
    );
    assert!(close(covered, PAGE_AREA), "`cm` and `Do` read across NULs");
    assert_eq!(
        (executed_ops, hidden),
        (1, 1),
        "placed and clip-only across NULs"
    );
}

/// `q` and `Q` save and restore the text state with the rest of the
/// graphics state — the render mode, the font size, the leading — so text
/// shown after the `Q` that closes a `3 Tr` paints, as a renderer paints
/// it.
#[test]
fn text_state_is_saved_and_restored_with_the_graphics_state() {
    let (doc, page_id, _) = synthetic_page(false, false, &[]);
    let page =
        "q 3 Tr BT /F1 12 Tf 72 700 Td (hidden) Tj ET Q BT /F1 12 Tf 72 680 Td (shown) Tj ET";
    let (executed_ops, hidden, _) = executed(&doc, page_id, &[page]);
    assert_eq!(executed_ops, 2);
    assert_eq!(
        hidden, 1,
        "the second string paints once the `Q` restores mode 0"
    );
    assert!(hidden < executed_ops);

    let state = executed_state(&doc, page_id, &["/F1 12 Tf 14 TL q /F1 36 Tf 40 TL 7 Tr Q"]);
    assert_eq!(state.render_mode, 0);
    assert_eq!(state.font_size, Some(12.0));
    assert_eq!(state.leading, 14.0);
}

/// Unfiltered inline image data is skipped by the length its header
/// gives, whatever bytes it holds — an `EI` set off by whitespace among
/// them, operators after it — and the `EI` after the data ends the image;
/// the operators that follow are read as written. Filtered data, whose
/// length is not known, ends at the first `EI` set off by whitespace,
/// however long the header would make it; a stated `/L` is the length,
/// filtered or not.
#[test]
fn unfiltered_inline_image_data_is_skipped_by_its_length() {
    // Eight bytes of gray samples spelling ` EI 3 Tr`, then the `EI`, then
    // a shown string and, under the page's own `3 Tr`, a hidden one.
    let (counts, executed_ops, hidden) = scan_alone(
        b"BI /W 8 /H 1 /BPC 8 /CS /G ID  EI 3 Tr EI BT /F1 12 Tf (a) Tj ET 3 Tr BT (b) Tj ET",
    );
    assert_eq!((counts.text_ops, executed_ops, hidden), (2, 2, 1));

    // The same samples under an image mask of 64 one-bit samples a row.
    let (counts, executed_ops, hidden) =
        scan_alone(b"BI /W 64 /H 1 /IM true ID  EI 3 Tr EI BT /F1 12 Tf (a) Tj ET");
    assert_eq!((counts.text_ops, executed_ops, hidden), (1, 1, 0));

    // Filtered: the header's ten thousand bytes are not taken; the data
    // ends at its `EI`, and the text after it is read.
    let (counts, executed_ops, hidden) = scan_alone(
        b"BI /W 100 /H 100 /BPC 8 /CS /G /F /Fl ID xxxxxxxxxx EI BT /F1 12 Tf (a) Tj ET",
    );
    assert_eq!((counts.text_ops, executed_ops, hidden), (1, 1, 0));
    // Filtered data holding an `EI` set off by whitespace ends there: its
    // length is not known, and this is the reading that was always made.
    let (counts, _, hidden) =
        scan_alone(b"BI /W 8 /H 1 /BPC 8 /CS /G /F /AHx ID  EI 3 Tr EI BT /F1 12 Tf (a) Tj ET");
    assert_eq!((counts.text_ops, hidden), (1, 1));
    // A stated length is taken, filtered or not.
    let (counts, _, hidden) = scan_alone(
        b"BI /W 8 /H 1 /BPC 8 /CS /G /F /AHx /L 8 ID  EI 3 Tr EI BT /F1 12 Tf (a) Tj ET",
    );
    assert_eq!((counts.text_ops, hidden), (1, 0));

    // A header whose length runs past the data — a lying width — still
    // ends at the first `EI` beyond it rather than at the stream's end.
    let (counts, _, _) =
        scan_alone(b"BI /W 6 /H 1 /BPC 8 /CS /G ID abcd EI BT /F1 12 Tf (a) Tj ET");
    assert_eq!(counts.text_ops, 1);
}

/// A colour space an inline image names from the resources gives its
/// samples' components: an `/ICCBased` space by its `/N`, a device space
/// by its name, `/Indexed` and `/Separation` as one, `/DeviceN` by its
/// names — so the data's length is known and an `EI` among its bytes ends
/// nothing; a name the resources do not bind leaves the length unknown,
/// and the data ends at that `EI` as filtered data does.
#[test]
fn an_inline_image_s_named_colour_space_is_resolved_in_the_resources() {
    use lopdf::dictionary;
    let (mut doc, page_id, _) = synthetic_page(false, false, &[]);
    let icc = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! { "N" => Object::Integer(3) },
        Vec::new(),
    )));
    let spaces = dictionary! {
        "CS0" => vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(icc)],
        "CS1" => Object::Name(b"DeviceCMYK".to_vec()),
        "CS2" => vec![
            Object::Name(b"Indexed".to_vec()),
            Object::Name(b"DeviceRGB".to_vec()),
            Object::Integer(1),
            Object::String(vec![0, 0, 0, 255, 255, 255], lopdf::StringFormat::Hexadecimal),
        ],
        "CS3" => vec![
            Object::Name(b"DeviceN".to_vec()),
            Object::Array(vec![Object::Name(b"A".to_vec()), Object::Name(b"B".to_vec())]),
            Object::Name(b"DeviceGray".to_vec()),
        ],
        "CS4" => vec![Object::Name(b"Separation".to_vec()), Object::Name(b"Spot".to_vec())],
    };
    doc.get_dictionary_mut(page_id)
        .unwrap()
        .get_mut(b"Resources")
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("ColorSpace", spaces);

    // Twelve bytes of samples spelling `  EI 3 Tr   `: four RGB samples,
    // three CMYK ones, twelve indexed or separation ones, six of two inks.
    let data = "  EI 3 Tr   ";
    for (space, width) in [("CS0", 4), ("CS1", 3), ("CS2", 12), ("CS3", 6), ("CS4", 12)] {
        let content =
            format!("BI /W {width} /H 1 /BPC 8 /CS /{space} ID {data} EI BT /F1 12 Tf (a) Tj ET");
        let (executed_ops, hidden, _) = executed(&doc, page_id, &[&content]);
        assert_eq!((executed_ops, hidden), (1, 0), "{space}");
    }
    let unbound = format!("BI /W 4 /H 1 /BPC 8 /CS /CS9 ID {data} EI BT /F1 12 Tf (a) Tj ET");
    let (executed_ops, hidden, _) = executed(&doc, page_id, &[&unbound]);
    assert_eq!(
        (executed_ops, hidden),
        (1, 1),
        "unbound: the `EI` among the bytes ends the data"
    );
}
