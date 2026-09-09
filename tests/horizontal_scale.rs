use lopdf::{dictionary, Document, Object, Stream};
use pdf_inspector::{
    extract_text_in_regions_mem, extract_text_with_positions_and_rotations_mem,
    extract_text_with_positions_mem, PageRotation, TextItem,
};

/// Keep every glyph's advance at 600/1000 em so the geometry assertions
/// follow the PDF operators directly, without depending on system fonts.
fn pdf(content: &str, form_depth: usize, prefix: &str, crop: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        "FirstChar" => 0, "LastChar" => 255,
        "Widths" => Object::Array((0..=255).map(|_| 600.into()).collect()),
    });
    let unknown_font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "SyntheticUnknown",
    });
    let mut resources = dictionary! {
        "Font" => dictionary! { "F1" => font_id, "F2" => unknown_font_id },
    };
    let mut stream = content.to_owned();
    for _ in 0..form_depth {
        let form_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 600.into(), 400.into()],
                "Resources" => resources,
            },
            stream.into_bytes(),
        ));
        resources = dictionary! { "XObject" => dictionary! { "Form" => form_id } };
        stream = "/Form Do".to_owned();
    }
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        format!("{prefix}\n{stream}").into_bytes(),
    ));
    let mut page = dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 600.into(), 400.into()],
        "Resources" => resources, "Contents" => content_id,
    };
    if crop {
        page.set(
            "CropBox",
            vec![50.into(), 70.into(), 550.into(), 370.into()],
        );
        // The public native-coordinate contract deliberately does not apply
        // /Rotate; text matrices and CropBox offsets still must compose.
        page.set("Rotate", 90);
    }
    let page_id = doc.add_object(page);
    doc.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages", "Count" => 1,
            "Kids" => vec![page_id.into()],
        }
        .into(),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn items(bytes: &[u8]) -> Vec<TextItem> {
    extract_text_with_positions_mem(bytes).unwrap()
}

fn find<'a>(items: &'a [TextItem], text: &str) -> &'a TextItem {
    items
        .iter()
        .find(|item| item.text == text)
        .unwrap_or_else(|| panic!("missing {text:?} in {items:?}"))
}

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.002,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn horizontal_scale_changes_width_without_changing_height() {
    for depth in [0, 1, 2] {
        for scale in [50, 100, 150, 200] {
            let bytes = pdf(
                &format!("BT /F1 12 Tf {scale} Tz 1 0 0 1 100 300 Tm (ABCDE) Tj ET"),
                depth,
                "",
                false,
            );
            let extracted = items(&bytes);
            let text = find(&extracted, "ABCDE");
            close(text.x, 100.0);
            close(text.width, 36.0 * scale as f32 / 100.0);
            close(text.y, 300.0);
            close(text.height, 12.0);
            close(text.font_size, 12.0);
        }
    }
}

#[test]
fn negative_font_size_and_scale_preserve_body_region_ownership() {
    for depth in [0, 1, 2] {
        let bytes = pdf(
            "q 1 0 0 -1 0 400 cm -100 Tz
             BT /F1 -12 Tf 1 0 0 1 250 100 Tm (Northstar survey recorded twelve seedlings.) Tj ET Q",
            depth,
            "",
            false,
        );
        let extracted = items(&bytes);
        let text = find(&extracted, "Northstar survey recorded twelve seedlings.");
        close(text.x, 250.0);
        close(text.width, text.text.len() as f32 * 7.2);
        close(text.y, 300.0);
        close(text.height, 12.0);
        close(text.rotation, 0.0);

        let regions = extract_text_in_regions_mem(
            &bytes,
            &[(
                0,
                vec![[10.0, 80.0, 50.0, 120.0], [245.0, 80.0, 560.0, 120.0]],
            )],
        )
        .unwrap();
        assert!(regions[0].regions[0].text.is_empty());
        assert_eq!(regions[0].regions[1].text, text.text);
        assert!(!regions[0].regions[1].needs_ocr);
    }
}

#[test]
fn negative_size_and_negative_scale_individually_still_run_left() {
    for depth in [0, 1] {
        for content in [
            "q 1 0 0 -1 0 400 cm BT /F1 -12 Tf 100 Tz 1 0 0 1 300 100 Tm (ABCDE) Tj ET Q",
            "BT /F1 12 Tf -100 Tz 1 0 0 1 300 300 Tm (ABCDE) Tj ET",
        ] {
            let extracted = items(&pdf(content, depth, "", false));
            let text = find(&extracted, "ABCDE");
            close(text.x, 264.0);
            close(text.width, 36.0);
            close(text.height, 12.0);
        }
    }
}

#[test]
fn horizontal_scale_applies_to_tj_spacing_and_tj_array_cursor() {
    for depth in [0, 1] {
        for (show, expected_advance) in [
            ("1 Tc 2 Tw (A B) Tj", 13.3),
            ("[(AA) -100 (BB)] TJ", 15.0),
            ("[(AA) -3000 (BB)] TJ", 32.4),
            ("3 Tr (AB) Tj 0 Tr", 7.2),
            ("3 Tr [(AA) -100 (BB)] TJ 0 Tr", 15.0),
        ] {
            let bytes = pdf(
                &format!("BT /F1 12 Tf 50 Tz 1 0 0 1 100 300 Tm {show} 0 Tc 0 Tw 40 Ts /F1 10 Tf (FOLLOW) Tj ET"),
                depth,
                "",
                false,
            );
            let extracted = items(&bytes);
            let following = find(&extracted, "FOLLOW");
            close(following.x, 100.0 + expected_advance);
            close(following.width, 18.0);
        }
    }
}

#[test]
fn squeezed_space_runs_keep_their_word_space_under_horizontal_scale() {
    // Word-per-operator producers paint the space as its own run, squeezed
    // with a negative `Tc` (600/1000 em space, `-6 Tc` leaves 1.19pt at
    // 99 Tz: under the 0.13 em word threshold). Forms take the same path.
    for depth in [0, 1] {
        for show in [
            "(for) Tj -6 Tc ( ) Tj 0 Tc (the) Tj",
            "[(for)] TJ -6 Tc [( )] TJ 0 Tc [(the)] TJ",
        ] {
            let bytes = pdf(
                &format!("BT /F1 12 Tf 99 Tz 1 0 0 1 100 300 Tm {show} ET"),
                depth,
                "",
                false,
            );
            let extracted = items(&bytes);
            let texts: Vec<_> = extracted.iter().map(|item| item.text.as_str()).collect();
            assert_eq!(texts, ["for the"], "depth {depth}: {show}");
        }
    }
}

#[test]
fn horizontal_scale_restores_with_graphics_state_and_survives_text_blocks() {
    let content = "50 Tz BT /F1 12 Tf 1 0 0 1 100 300 Tm (BEFORE) Tj ET
        q 200 Tz BT /F1 12 Tf 1 0 0 1 100 250 Tm (INSIDE) Tj ET Q
        BT /F1 12 Tf 1 0 0 1 100 200 Tm (AFTER) Tj ET";
    for depth in [0, 1, 2] {
        let extracted = items(&pdf(content, depth, "", false));
        close(find(&extracted, "BEFORE").width, 21.6);
        close(find(&extracted, "INSIDE").width, 86.4);
        close(find(&extracted, "AFTER").width, 18.0);
    }
}

#[test]
fn nested_forms_inherit_the_invoking_streams_horizontal_scale() {
    for depth in [1, 2, 3] {
        let extracted = items(&pdf(
            "BT /F1 12 Tf 1 0 0 1 100 300 Tm (INHERITED) Tj ET",
            depth,
            "50 Tz",
            false,
        ));
        close(find(&extracted, "INHERITED").width, 32.4);
    }
}

#[test]
fn horizontal_scale_and_painted_bold_inherit_and_restore_together() {
    for depth in [0, 1, 2, 3] {
        for scale in [50, -100] {
            let extracted = items(&pdf(
                "BT /F1 12 Tf 1 0 0 1 100 300 Tm (ALPHA) Tj ET
                 q 200 Tz 0 Tr 0 w 0.5 g 1 G [1] 0 d /Unknown gs
                 BT /F1 12 Tf 1 0 0 1 100 250 Tm (BRAVO) Tj ET Q
                 BT /F1 12 Tf 1 0 0 1 100 200 Tm [(DELTA)] TJ ET",
                depth,
                &format!("0.36 w BT 2 Tr {scale} Tz ET"),
                false,
            ));
            for text in ["ALPHA", "DELTA"] {
                let item = find(&extracted, text);
                close(item.x, if scale < 0 { 64.0 } else { 100.0 });
                close(item.width, 36.0 * (scale as f32 / 100.0).abs());
                close(item.height, 12.0);
                assert!(item.is_bold, "depth {depth}, scale {scale}, text {text}");
            }
            let plain = find(&extracted, "BRAVO");
            close(plain.x, 100.0);
            close(plain.width, 72.0);
            assert!(!plain.is_bold);
        }
    }
}

#[test]
fn horizontal_scale_composes_with_rotated_runs_and_visible_page_offsets() {
    // Three horizontal shows keep this a page with marginalia rather than
    // a predominantly rotated page whose coordinate frame is rebased.
    let content = "BT /F1 12 Tf 50 Tz 0 1 -1 0 300 150 Tm (SIDE) Tj ET
        BT /F1 12 Tf 100 Tz 1 0 0 1 100 300 Tm (FIRST) Tj ET
        BT /F1 12 Tf 1 0 0 1 100 250 Tm (SECOND) Tj ET
        BT /F1 12 Tf 1 0 0 1 100 200 Tm (THIRD) Tj ET";
    for depth in [0, 1] {
        let extracted = items(&pdf(content, depth, "", true));
        let side = find(&extracted, "SIDE");
        close(side.x, 238.0);
        close(side.y, 80.0);
        close(side.width, 12.0);
        close(side.height, 14.4);
        close(side.rotation, 90.0);
    }
}

#[test]
fn actual_text_uses_the_scale_at_the_painted_runs_not_at_emc() {
    for show in ["(ABCDE) Tj", "[(AB) -100 (CDE)] TJ"] {
        let bytes = pdf(
            &format!("q 1 0 0 -1 0 400 cm BT /F1 -12 Tf -100 Tz 1 0 0 1 250 100 Tm /Span << /ActualText (REPLACEMENT) >> BDC {show} 50 Tz EMC ET Q"),
            0,
            "",
            false,
        );
        let extracted = items(&bytes);
        let text = find(&extracted, "REPLACEMENT");
        close(text.x, 250.0);
        close(text.width, if show.contains("TJ") { 37.2 } else { 36.0 });
        close(text.rotation, 0.0);
    }
}

#[test]
fn horizontal_scale_applies_to_metricless_estimates_and_zero_scale() {
    for depth in [0, 1] {
        for (font_size, scale, expected_width) in [(12, 50, 15.0), (-12, -100, 30.0), (12, 0, 0.0)]
        {
            let extracted = items(&pdf(
                &format!("BT /F2 {font_size} Tf {scale} Tz 1 0 0 1 100 300 Tm (ABCDE) Tj 40 Ts /F1 {font_size} Tf (FOLLOW) Tj ET"),
                depth,
                "",
                false,
            ));
            let text = find(&extracted, "ABCDE");
            close(text.x, 100.0);
            close(text.width, expected_width);
            assert!(!text.advance_known);
            close(find(&extracted, "FOLLOW").x, 100.0 + expected_width);
        }
    }
}

#[test]
fn metricless_tj_array_scales_kerning_and_following_cursor() {
    for depth in [0, 1] {
        let extracted = items(&pdf(
            "BT /F2 12 Tf 50 Tz 1 0 0 1 100 300 Tm [(AA) -100 (BB)] TJ 40 Ts /F1 12 Tf (FOLLOW) Tj ET",
            depth,
            "",
            false,
        ));
        close(find(&extracted, "FOLLOW").x, 112.6);
    }
}

#[test]
fn actual_text_estimate_accumulates_each_painted_runs_scale() {
    let extracted = items(&pdf(
        "BT /F2 12 Tf 50 Tz 1 0 0 1 100 300 Tm /Span << /ActualText (REPLACEMENT) >> BDC
         (ABC) Tj 200 Tz (DE) Tj -100 Tz EMC ET",
        0,
        "",
        false,
    ));
    let text = find(&extracted, "REPLACEMENT");
    close(text.x, 100.0);
    close(text.width, 33.0);
    assert!(!text.advance_known);
}

#[test]
fn actual_text_unions_painted_bounds_when_horizontal_scales_change_sign() {
    for (font, advance) in [("F1", 14.4), ("F2", 12.0)] {
        for (shows, x, y, width, height) in [
            (
                "100 Tz (AB) Tj -100 Tz (CD) Tj",
                100.0,
                300.0,
                advance,
                12.0,
            ),
            (
                "100 Tz [(AB) -100 (CD)] TJ -100 Tz (EFGH) Tj",
                100.0,
                300.0,
                advance * 2.0 + 1.2,
                12.0,
            ),
            (
                "100 Tz (AB) Tj -100 Tz 20 TL (CD) '",
                100.0 - advance,
                280.0,
                advance * 2.0,
                32.0,
            ),
            (
                "100 Tz (AB) Tj q -100 Tz (CD) Tj Q (EF) Tj",
                100.0,
                300.0,
                advance,
                12.0,
            ),
        ] {
            let extracted = items(&pdf(
                &format!("BT /{font} 12 Tf 1 0 0 1 100 300 Tm /Span << /ActualText (REPLACEMENT) >> BDC {shows} EMC ET"),
                0,
                "",
                false,
            ));
            let text = find(&extracted, "REPLACEMENT");
            close(text.x, x);
            close(text.y, y);
            close(text.width, width);
            close(text.height, height);
            assert_eq!(text.advance_known, font == "F1");
        }
    }
}

#[test]
fn actual_text_unions_painted_bounds_when_font_sizes_change_sign() {
    for (font, advance) in [("F1", 14.4), ("F2", 12.0)] {
        for (shows, x, y, width, height) in [
            (
                format!("(AB) Tj /{font} -12 Tf (CD) Tj"),
                100.0,
                288.0,
                advance,
                24.0,
            ),
            (
                format!("[(AB) -100 (CD)] TJ /{font} -12 Tf [(EFGH)] TJ"),
                100.0,
                288.0,
                advance * 2.0 + 1.2,
                24.0,
            ),
            (
                format!("(AB) Tj /{font} -12 Tf 20 TL (CD) '"),
                100.0 - advance,
                268.0,
                advance * 2.0,
                44.0,
            ),
            (
                format!("(AB) Tj q /{font} -12 Tf (CD) Tj Q (EF) Tj"),
                100.0,
                288.0,
                advance,
                24.0,
            ),
            // With a negative Tz, flipping Tf also reverses the advance.
            (
                format!("-100 Tz (AB) Tj /{font} -12 Tf (CD) Tj"),
                100.0 - advance,
                288.0,
                advance,
                24.0,
            ),
            // Flipping both signs preserves advance direction, but reverses
            // glyph-up: the existing vertical bounds union must survive.
            (
                format!("(AB) Tj /{font} -12 Tf -100 Tz (CD) Tj"),
                100.0,
                288.0,
                advance * 2.0,
                24.0,
            ),
        ] {
            let extracted = items(&pdf(
                &format!("BT /{font} 12 Tf 100 Tz 1 0 0 1 100 300 Tm /Span << /ActualText (REPLACEMENT) >> BDC {shows} EMC ET"),
                0,
                "",
                false,
            ));
            let text = find(&extracted, "REPLACEMENT");
            close(text.x, x);
            close(text.y, y);
            close(text.width, width);
            close(text.height, height);
            assert_eq!(text.advance_known, font == "F1");
        }
    }
}

#[test]
fn actual_text_zero_scale_does_not_vote_for_a_reflection() {
    for (font, advance) in [("F1", 14.4), ("F2", 12.0)] {
        for zero_scale in ["0", "-0.0"] {
            let extracted = items(&pdf(
                &format!("BT /{font} 12 Tf 100 Tz 1 0 0 1 100 300 Tm /Span << /ActualText (REPLACEMENT) >> BDC
                    (AB) Tj q /{font} -12 Tf {zero_scale} Tz (CD) Tj Q (EF) Tj EMC ET"),
                0,
                "",
                false,
            ));
            let text = find(&extracted, "REPLACEMENT");
            close(text.x, 100.0);
            close(text.y, 300.0);
            close(text.width, advance * 2.0);
            close(text.height, 12.0);
            assert_eq!(text.advance_known, font == "F1");
        }
    }
}

#[test]
fn actual_text_zero_scale_strokes_keep_their_vertical_reflection() {
    for (font, advance) in [("F1", 14.4), ("F2", 12.0)] {
        for zero_scale in ["0", "-0.0"] {
            for render_mode in [1, 2, 5, 6] {
                for show in ["(CD) Tj", "[(CD)] TJ", "20 TL (CD) '"] {
                    let extracted = items(&pdf(
                        &format!("BT /{font} 12 Tf 100 Tz 1 0 0 1 100 300 Tm /Span << /ActualText (REPLACEMENT) >> BDC
                            (AB) Tj q /{font} -12 Tf {zero_scale} Tz {render_mode} Tr 2 w {show} Q (EF) Tj EMC ET"),
                        0,
                        "",
                        false,
                    ));
                    let text = find(&extracted, "REPLACEMENT");
                    let next_line = show.contains('\'');
                    close(text.x, 100.0);
                    close(text.y, if next_line { 268.0 } else { 288.0 });
                    // Quote resets the cursor to the next line's start;
                    // the other operators leave it after AB.
                    close(text.width, if next_line { advance } else { advance * 2.0 });
                    close(text.height, if next_line { 44.0 } else { 24.0 });
                    assert_eq!(text.advance_known, font == "F1");
                }
            }
        }
    }
}

#[test]
fn reflected_vertical_runs_choose_the_same_page_frame_inside_forms() {
    for scale in [-100, 100] {
        for font_size in [-12, 12] {
            for baseline_y in [-1, 1] {
                for show in ["(FIRST) Tj", "[(FIRST)] TJ"] {
                    let content = format!(
                        "BT /F1 {font_size} Tf {scale} Tz 0 {baseline_y} -1 0 300 300 Tm {show} ET
                         BT /F1 {font_size} Tf {scale} Tz 0 {baseline_y} -1 0 350 300 Tm (SECOND) Tj ET"
                    );
                    let (reference, frames) =
                        extract_text_with_positions_and_rotations_mem(&pdf(&content, 0, "", false))
                            .unwrap();
                    let expected_frame = if baseline_y * font_size * scale > 0 {
                        PageRotation::Ccw
                    } else {
                        PageRotation::Cw
                    };
                    assert_eq!(frames.get(&1), Some(&expected_frame));
                    for depth in [1, 2] {
                        let (extracted, frames) = extract_text_with_positions_and_rotations_mem(
                            &pdf(&content, depth, "", false),
                        )
                        .unwrap();
                        assert_eq!(frames.get(&1), Some(&expected_frame));
                        for text in ["FIRST", "SECOND"] {
                            let expected = find(&reference, text);
                            let actual = find(&extracted, text);
                            close(actual.x, expected.x);
                            close(actual.y, expected.y);
                            close(actual.width, expected.width);
                            close(actual.height, expected.height);
                            close(actual.rotation, expected.rotation);
                        }
                    }
                }
            }
        }
    }
}
