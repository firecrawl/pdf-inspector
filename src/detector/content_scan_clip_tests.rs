//! Tests of where clip-only (mode 7) text shows: paint that lands where
//! its glyphs lie, and nothing else, shows it.

use super::super::analyze_page_content;
use super::fixtures::*;
use super::*;

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

    // Shadings, inline images, path painting (of a path that lands on
    // the page: a painting operator with no path paints nothing) and
    // visible text show it too; mode 3 is never shown, whatever is
    // painted after it.
    for painting in [
        "sh",
        "BI /W 1 /H 1 ID x EI",
        "0 0 1 1 re f",
        "0 0 m 1 1 l S",
        "0 0 m 1 1 l 2 0 l b",
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

#[test]
fn a_draw_off_the_page_or_clipped_away_reveals_nothing() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let run = |content: &str| executed(&doc, page_id, &[content]);
    let clip_text = "BT /F1 10 Tf 7 Tr (a) Tj ET";

    // An image drawn off the page, an XObject or an inline one, and a
    // fill or a shading that the clip in force leaves nothing of: the
    // clip-only text stays hidden.
    for painting in [
        "q 612 0 0 792 700 0 cm /Im0 Do Q",
        "q 612 0 0 792 700 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q",
        "q 0 0 10 10 re W n 500 500 50 50 re f Q",
        "q W n sh Q",
        "q W n 0 0 612 792 re f Q",
    ] {
        let (executed_ops, hidden, _) = run(&format!("{clip_text} {painting}"));
        assert_eq!((executed_ops, hidden), (1, 1), "{painting}");
    }
    // Landing within the clip, each of them shows the text through.
    for painting in [
        "q 612 0 0 792 0 0 cm /Im0 Do Q",
        "q 612 0 0 792 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q",
        "q 0 0 10 10 re W n 5 5 50 50 re f Q",
        "q 0 0 10 10 re W n 0 0 m 100 100 l S Q",
        "sh",
    ] {
        let (executed_ops, hidden, _) = run(&format!("{clip_text} {painting}"));
        assert_eq!((executed_ops, hidden), (1, 0), "{painting}");
    }

    // A hidden layer over a covering image stays one when an off-page
    // draw follows.
    let (mut doc, page_id, content_id) = synthetic_page(true, false, &[]);
    set_page_content(
        &mut doc,
        content_id,
        &format!(
            "{FULL_PAGE_IMAGE}{}q 612 0 0 792 700 0 cm /Im0 Do Q",
            glyph_layer(7)
        ),
    );
    assert!(analyze_page_content(&doc, page_id).has_invisible_text_layer);
}

/// Clip-only text shows only where paint lands on its glyphs: a title in
/// the top band under a full-page image is visible, as under a small
/// image, a stroke or visible text over it; the same paint in a corner
/// the title does not reach shows nothing through it. A layer over the
/// body under a covering image stays a layer nobody sees when a small
/// image follows in a corner, and shows the glyphs under an image drawn
/// over its first lines.
#[test]
fn clip_only_text_shows_only_where_paint_lands_on_it() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let run = |content: &str| executed(&doc, page_id, &[content]);
    let title = "BT /F1 36 Tf 1 0 0 1 72 700 Tm 7 Tr (Title) Tj ET";

    let (executed_ops, hidden, covered) = run(&format!("{title} {FULL_PAGE_IMAGE}"));
    assert_eq!((executed_ops, hidden), (1, 0));
    assert!(close(covered, PAGE_AREA));

    let corner = "q 20 0 0 20 580 20 cm /Im0 Do Q";
    let over = "q 20 0 0 20 60 700 cm /Im0 Do Q";
    assert_eq!(run(&format!("{title} {corner}")).1, 1, "a corner image");
    assert_eq!(run(&format!("{title} {over}")).1, 0, "an image over it");
    assert_eq!(
        run(&format!("{title} 72 600 m 300 600 l S")).1,
        1,
        "a line below"
    );
    assert_eq!(
        run(&format!("{title} 72 700 m 300 700 l S")).1,
        0,
        "a line along the baseline"
    );
    let text_at = |x: u32, y: u32| format!("BT 0 Tr /F1 12 Tf 1 0 0 1 {x} {y} Tm (x) Tj ET");
    assert_eq!(
        run(&format!("{title} {}", text_at(400, 100))),
        (2, 1, 0.0),
        "text elsewhere"
    );
    assert_eq!(
        run(&format!("{title} {}", text_at(80, 705))),
        (2, 0, 0.0),
        "text over it"
    );

    let (mut doc, page_id, content_id) = synthetic_page(true, false, &[]);
    set_page_content(
        &mut doc,
        content_id,
        &format!("{FULL_PAGE_IMAGE}{}{corner}", glyph_layer(7)),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(analysis.has_invisible_text_layer);
    assert_eq!(analysis.invisible_text_operator_count, 120);

    // The image lands on the first eight glyphs of each of the three rows.
    set_page_content(
        &mut doc,
        content_id,
        &format!(
            "{FULL_PAGE_IMAGE}{}q 100 0 0 100 60 650 cm /Im0 Do Q",
            glyph_layer(7)
        ),
    );
    let analysis = analyze_page_content(&doc, page_id);
    assert!(!analysis.has_invisible_text_layer);
    assert_eq!(
        (
            analysis.executed_text_operator_count,
            analysis.invisible_text_operator_count
        ),
        (120, 96)
    );
}

/// The text-positioning operators place the glyphs: `Td` from the line
/// before, `TD` setting the leading `T*` moves by, `TL` setting it too,
/// `'` and `"` moving a line first, `Tm` outright; the font size comes
/// from `Tf`, scaled by `Tm` — a 1-point font under a twelvefold matrix
/// is 12 points tall, as producers write it; and the pen moves past what
/// is shown, so a second show sits to the right of the first.
#[test]
fn clip_only_text_is_placed_by_the_text_operators() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let hidden_after =
        |text: &str, paint: &str| executed(&doc, page_id, &[&format!("{text} {paint}")]).1;
    let image_at = |x: u32, y: u32| format!("q 10 0 0 10 {x} {y} cm /Im0 Do Q");

    let td = "BT /F1 10 Tf 7 Tr 72 700 Td (abc) Tj 0 -14 Td (def) Tj ET";
    assert_eq!(
        hidden_after(td, &image_at(75, 686)),
        0,
        "on the second line"
    );
    assert_eq!(hidden_after(td, &image_at(75, 650)), 2, "below both lines");

    let td_star = "BT /F1 10 Tf 7 Tr 72 700 Td 0 -14 TD (a) Tj T* (b) Tj ET";
    assert_eq!(
        hidden_after(td_star, &image_at(75, 672)),
        0,
        "T* a leading down"
    );
    assert_eq!(
        hidden_after(td_star, &image_at(75, 700)),
        2,
        "above the first line"
    );

    let quotes = "BT /F1 10 Tf 7 Tr 14 TL 72 700 Td (a) ' 0 0 (b) \" ET";
    assert_eq!(
        hidden_after(quotes, &image_at(75, 672)),
        0,
        "the second quoted line"
    );
    assert_eq!(
        hidden_after(quotes, &image_at(75, 700)),
        2,
        "above the first"
    );

    let tm = "BT /F1 1 Tf 7 Tr 12 0 0 12 72 700 Tm (abc) Tj ET";
    assert_eq!(
        hidden_after(tm, &image_at(80, 705)),
        0,
        "within the scaled glyphs"
    );
    assert_eq!(hidden_after(tm, &image_at(80, 720)), 1, "above them");

    let run_on = "BT /F1 10 Tf 7 Tr 72 700 Td (abcd) Tj (efgh) Tj ET";
    assert_eq!(
        hidden_after(run_on, &image_at(100, 700)),
        0,
        "on the second show"
    );
    assert_eq!(hidden_after(run_on, &image_at(200, 700)), 2, "past both");
}

/// Text placed nowhere the scan can tell — shown before any positioning
/// operator of its text object, or with no font size set — reaches
/// wherever paint lands in the clip, as all clip-only text did before it
/// was placed; text every glyph of which lies outside the clip in force
/// shows nothing, whatever is painted.
#[test]
fn unplaced_clip_only_text_is_shown_by_any_paint_in_the_clip() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let run = |content: &str| executed(&doc, page_id, &[content]);
    let corner = "q 20 0 0 20 580 20 cm /Im0 Do Q";
    let off_page = "q 20 0 0 20 700 20 cm /Im0 Do Q";
    for text in ["BT /F1 10 Tf 7 Tr (a) Tj ET", "BT 7 Tr 72 700 Td (a) Tj ET"] {
        let (executed_ops, hidden, _) = run(&format!("{text} {corner}"));
        assert_eq!((executed_ops, hidden), (1, 0), "{text}");
        let (executed_ops, hidden, _) = run(&format!("{text} {off_page}"));
        assert_eq!((executed_ops, hidden), (1, 1), "{text}");
    }
    let clipped_away =
        "q 0 0 100 100 re W n BT /F1 10 Tf 7 Tr 300 300 Td (a) Tj ET 0 0 612 792 re f Q";
    assert_eq!(run(clipped_away).1, 1);
    assert_eq!(run(&format!("{clipped_away} {FULL_PAGE_IMAGE}")).1, 1);
}

/// Past `CLIP_TEXT_ENTRIES_MAX` text objects at one level, the rest join
/// the last: paint on any of them shows them all, and nothing else.
#[test]
fn clip_only_text_objects_past_the_cap_are_judged_together() {
    let (doc, page_id, _) = synthetic_page(true, false, &[]);
    let total = CLIP_TEXT_ENTRIES_MAX + 44;
    let place = |n: usize| (10 + 12 * (n % 40), 780 - 14 * (n / 40));
    let mut layer = String::from("7 Tr\n");
    for n in 0..total {
        let (x, y) = place(n);
        layer.push_str(&format!("BT /F1 10 Tf {x} {y} Td (a) Tj ET\n"));
    }
    let paint_on = |n: usize| {
        let (x, y) = place(n);
        format!("q 1 0 0 1 {} {} cm /Im0 Do Q", x + 2, y + 2)
    };
    let hidden =
        |n: usize| executed(&doc, page_id, &[&format!("{layer}{}", paint_on(n))]).1 as usize;
    assert_eq!(hidden(0), total - 1, "the first alone is shown");
    assert_eq!(
        hidden(total - 1),
        CLIP_TEXT_ENTRIES_MAX - 1,
        "the last, and every one past the cap with it"
    );
}
