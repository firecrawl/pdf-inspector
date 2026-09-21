//! Tests of the executed-content scan, and of the invisible-text-layer
//! signal `analyze_page_content` builds on it.

use super::super::content_mask::inline_image_data_bound;
use super::super::{analyze_page_content, page_ocr_reasons, page_ocr_signals};
use super::*;

/// A scan of `content` on its own — no page, nothing followed through
/// `Do` — as its counts and executed tallies: (counts, text ops,
/// hidden text ops).
fn scan_alone(content: &[u8]) -> (ContentCounts, u32, u32) {
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

/// A Form XObject of [`synthetic_page`]: the page's font as `F1`, and
/// the `/BBox` and `/Matrix` given.
#[derive(Clone, Copy)]
struct TestForm<'a> {
    name: &'a str,
    content: &'a str,
    matrix: Option<[i64; 6]>,
    bbox: &'a [i64],
}

/// A page-sized form with nothing in it, to fill in.
const PAGE_FORM: TestForm<'static> = TestForm {
    name: "",
    content: "",
    matrix: None,
    bbox: &[0, 0, 612, 792],
};

/// A pattern of [`synthetic_page_with_patterns`]: a tiling pattern whose
/// cell runs `content`, with the page's image as `Im0` and font as `F1`
/// in its resources — or a shading pattern, when `shading`.
#[derive(Clone, Copy)]
struct TestPattern<'a> {
    name: &'a str,
    content: &'a str,
    shading: bool,
}

/// A one-page 612×792 document whose content stream is set with
/// [`set_page_content`]: a 2×2 gray image `Im0` when `image`; a
/// 1500×2383 image `ImBig`, bound whether or not the content draws it,
/// when `large_image`; and the given forms, bound whether or not the
/// content invokes them.
fn synthetic_page(
    image: bool,
    large_image: bool,
    forms: &[TestForm<'_>],
) -> (Document, ObjectId, ObjectId) {
    synthetic_page_with_patterns(image, large_image, forms, &[])
}

/// [`synthetic_page`] with the given patterns bound as well.
fn synthetic_page_with_patterns(
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
        let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Image".to_vec()),
                "Width" => Object::Integer(width),
                "Height" => Object::Integer(height),
                "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                "BitsPerComponent" => Object::Integer(8),
            },
            data,
        )));
        xobjects.set(name, Object::Reference(image_id));
        image_id
    };
    let mut page_image = None;
    if image {
        page_image = Some(add_image(&mut doc, "Im0", 2, 2, vec![200, 60, 60, 200]));
    }
    if large_image {
        add_image(&mut doc, "ImBig", 1500, 2383, Vec::new());
    }
    for form in forms {
        let mut dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => Object::Name(b"Form".to_vec()),
            "BBox" => form
                .bbox
                .iter()
                .map(|&value| Object::Integer(value))
                .collect::<Vec<_>>(),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
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
    (doc, page_id, content_id)
}

fn set_page_content(doc: &mut Document, content_id: ObjectId, content: &str) {
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
fn executed(doc: &Document, page_id: ObjectId, streams: &[&str]) -> (u32, u32, f64) {
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
    (
        state.executed_text_ops,
        state.executed_hidden_text_ops,
        state.covered_image_area(),
    )
}

const PAGE_AREA: f64 = 612.0 * 792.0;
const FULL_PAGE_IMAGE: &str = "q 612 0 0 792 0 0 cm /Im0 Do Q\n";

/// 120 one-glyph `Tj` blocks under `mode`, as a producer writes a text
/// layer.
fn glyph_layer(mode: u8) -> String {
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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn clip_only_text_painted_through_is_visible() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    // A title filled with an image: the glyphs clip the image painted
    // through them, so they are visible. The image under the page,
    // drawn before the text, shows nothing of it.
    let (executed_ops, hidden, covered) = executed(
        &doc,
        page_id,
        &["q 612 0 0 792 0 0 cm /Im0 Do Q \
           q BT 7 Tr (L) Tj (O) Tj ET q 612 0 0 792 0 0 cm /Im0 Do Q Q"],
    );
    assert_eq!((executed_ops, hidden), (2, 0));
    assert!(close(covered, PAGE_AREA));

    // Shadings, inline images, path painting and visible text show it
    // too; mode 3 is never shown, whatever is painted after it.
    for painting in [
        "sh",
        "BI /W 1 /H 1 ID x EI",
        "0 0 1 1 re f",
        "0 0 m 1 1 l S",
        "b",
        "BT 0 Tr (v) Tj ET",
    ] {
        let content = format!("q BT 7 Tr (a) Tj ET {painting} Q BT 3 Tr (b) Tj ET {painting}");
        let (executed_ops, hidden, _) = executed(&doc, page_id, &[&content]);
        let visible_text = if painting.contains("Tj") { 2 } else { 0 };
        assert_eq!(executed_ops, 2 + visible_text, "{painting}");
        assert_eq!(hidden, 1, "{painting}");
    }

    // A clip whose level closes unpainted hides its text for good: the
    // image painted afterwards, in a later stream, is outside it.
    let (executed_ops, hidden, _) = executed(
        &doc,
        page_id,
        &[
            "q BT 7 Tr (a) Tj ET Q BT 7 Tr (b) Tj ET",
            "q 612 0 0 792 0 0 cm /Im0 Do Q",
        ],
    );
    assert_eq!(
        (executed_ops, hidden),
        (2, 1),
        "(b) was painted through, (a) was not"
    );
}

/// Within a tenth of the expected area — the grid's resolution costs a
/// cell along each edge — and nothing but zero for none.
fn about(covered: f64, expected: f64) -> bool {
    if expected == 0.0 {
        covered == 0.0
    } else {
        (covered - expected).abs() <= expected * 0.1
    }
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

    assert_eq!(inline_image_data_bound(b"/W 3 /H 2 /BPC 1 /CS /G"), 2);
    assert_eq!(inline_image_data_bound(b"/W 2 /H 2 /BPC 8 /CS /RGB"), 12);
    assert_eq!(inline_image_data_bound(b"/W 8 /H 1 /IM true"), 1);
    assert_eq!(inline_image_data_bound(b"/W 2 /H 2 /F /AHx"), 4096);
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

/// A one-page document: a 2×2 gray image drawn over the whole page when
/// `covering_image`; a layer of 120 one-glyph `Tj` blocks under
/// `layer_mode` when given, in the page's content or, when
/// `layer_in_form`, in a Form XObject the page invokes; and a visible
/// caption line when given.
fn layered_scan_page(
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
