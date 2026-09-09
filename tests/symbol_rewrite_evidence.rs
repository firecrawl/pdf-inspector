use lopdf::{dictionary, Document, Object, Stream};
use pdf_inspector::{extract_text_in_regions_mem, extract_text_with_positions_mem, TextItem};

// All glyph advances are explicit; source mappings and content are synthetic.
fn pdf(content: &str, in_form: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let cmap = doc.add_object(Stream::new(
        dictionary! {},
        br#"
/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CMapName /ExampleMap def /CMapType 2 def
1 begincodespacerange <00> <FF> endcodespacerange
9 beginbfchar
<41> <0041> <42> <0042> <57> <F057> <31> <F031>
<4F> <03A9> <6D> <03BC> <44> <0024> <7A> <E123> <10> <F010>
endbfchar endcmap CMapName currentdict /CMap defineresource pop end end
"#
        .to_vec(),
    ));
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "ExampleFace",
        "FirstChar" => 0, "LastChar" => 255,
        "Widths" => Object::Array((0..=255).map(|_| 600.into()).collect()),
        "ToUnicode" => cmap,
    });
    let resources = dictionary! { "Font" => dictionary! { "F1" => font } };
    let (resources, content) = if in_form {
        let form = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 600.into(), 400.into()],
                "Resources" => resources,
            },
            content.as_bytes().to_vec(),
        ));
        (
            dictionary! { "XObject" => dictionary! { "Form" => form } },
            "/Form Do",
        )
    } else {
        (resources, content)
    };
    let stream = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 600.into(), 400.into()],
        "Resources" => resources, "Contents" => stream,
    });
    doc.objects.insert(
        pages,
        dictionary! {
            "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()],
        }
        .into(),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn extract(content: &str, in_form: bool) -> Vec<TextItem> {
    extract_text_with_positions_mem(&pdf(content, in_form)).unwrap()
}

#[test]
fn records_only_actual_legacy_cleanup_without_changing_text_or_geometry() {
    for in_form in [false, true] {
        for (encoded, expected, rewritten) in [
            ("414F6D44", "AΩμ$", false),
            ("57", "W", true),
            ("31", "1", true),
            ("7A", "\u{e123}", false),
            ("10", "\u{f010}", false),
        ] {
            let items = extract(
                &format!("BT /F1 10 Tf 50 300 Td <{encoded}> Tj ET"),
                in_form,
            );
            assert_eq!(items.len(), 1, "{items:?}");
            assert_eq!(items[0].text, expected);
            assert_eq!(items[0].legacy_symbol_rewrite, rewritten);
            assert_eq!(items[0].x, 50.0);
            assert_eq!(items[0].y, 300.0);
            assert_eq!(items[0].width, encoded.len() as f32 / 2.0 * 6.0);
            assert_eq!(items[0].baseline_shift, 0.0);
        }
    }
}

#[test]
fn tj_evidence_is_local_to_each_emitted_segment() {
    for in_form in [false, true] {
        let items = extract(
            "BT /F1 10 Tf 50 300 Td [(A) -3000 (W) -3000.0 (B)] TJ ET",
            in_form,
        );
        assert_eq!(
            items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["A", "W", "B"]
        );
        assert_eq!(
            items
                .iter()
                .map(|i| i.legacy_symbol_rewrite)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        let joined = extract("BT /F1 10 Tf 50 300 Td [(A) (W) (B)] TJ ET", in_form);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].text, "AWB");
        assert!(joined[0].legacy_symbol_rewrite);
    }
}

#[test]
fn separate_show_operators_preserve_evidence_when_their_items_merge() {
    for in_form in [false, true] {
        let items = extract("BT /F1 10 Tf 50 300 Td (A) Tj (W) Tj (B) Tj ET", in_form);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].text, "AWB");
        assert!(items[0].legacy_symbol_rewrite);
        assert_eq!(items[0].width, 18.0);
    }
}

#[test]
fn quote_show_operators_record_evidence_without_changing_line_motion() {
    for in_form in [false, true] {
        let second_show = if in_form { "0 0 (A) \"" } else { "(A) '" };
        let items = extract(
            &format!("BT /F1 10 Tf 20 TL 50 300 Td (W) ' {second_show} ET"),
            in_form,
        );
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(
            (
                items[0].text.as_str(),
                items[0].y,
                items[0].legacy_symbol_rewrite
            ),
            ("W", 280.0, true)
        );
        assert_eq!(
            (
                items[1].text.as_str(),
                items[1].y,
                items[1].legacy_symbol_rewrite
            ),
            ("A", 260.0, false)
        );
    }
}

#[test]
fn actual_text_replaces_only_its_own_decoding_evidence() {
    let content = "BT /F1 10 Tf 50 300 Td (W) Tj 0 -30 Td /Span << /ActualText (Clean) >> BDC (W) Tj EMC 0 -30 Td (W) Tj ET";
    let items = extract(content, false);
    assert_eq!(
        items
            .iter()
            .map(|i| (i.text.as_str(), i.legacy_symbol_rewrite))
            .collect::<Vec<_>>(),
        [("W", true), ("Clean", false), ("W", true)]
    );
    // Form extraction does not materialize ActualText today. It must not
    // clear evidence for glyph text that it continues to emit.
    let form = extract(content, true);
    assert!(form
        .iter()
        .all(|i| i.text == "W" && i.legacy_symbol_rewrite));
}

#[test]
fn region_extraction_keeps_current_text_and_ocr_verdict() {
    let bytes = pdf("BT /F1 10 Tf 50 300 Td (W) Tj 150 0 Td (A) Tj ET", false);
    let regions = extract_text_in_regions_mem(
        &bytes,
        &[(
            0,
            vec![[40.0, 80.0, 80.0, 120.0], [190.0, 80.0, 230.0, 120.0]],
        )],
    )
    .unwrap();
    assert_eq!(regions[0].regions[0].text, "W");
    assert_eq!(regions[0].regions[1].text, "A");
    assert!(!regions[0].regions[0].needs_ocr);
    assert!(!regions[0].regions[1].needs_ocr);
}
