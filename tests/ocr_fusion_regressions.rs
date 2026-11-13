#![cfg(all(feature = "ocr", not(target_arch = "wasm32")))]

use pdf_inspector::vision::{
    fuse_ocr_pages, FusedPageMarkdown, ImagePoint, ImageQuad, ModelIdentity, OcrFusionOptions,
    OcrPage, OcrRun, OcrSpan, PageContentSource, PageTransform, RenderPixelFormat, RenderedPage,
    RoutedOcrPage,
};
use pdf_inspector::PageMarkdown;

fn fuse(native: &str, recognized: &str) -> FusedPageMarkdown {
    let transform =
        PageTransform::from_corners(800, 1000, (0.0, 1000.0), (800.0, 1000.0), (0.0, 0.0)).unwrap();
    let rendered = RenderedPage::new(
        1,
        800.0,
        1000.0,
        800,
        1000,
        2400,
        RenderPixelFormat::Rgb8,
        vec![255; 2_400_000],
        transform,
    )
    .unwrap();
    let ocr = OcrPage {
        page_number: 1,
        spans: vec![OcrSpan {
            text: recognized.to_string(),
            polygon: ImageQuad::new([
                ImagePoint::new(10.0, 10.0),
                ImagePoint::new(780.0, 10.0),
                ImagePoint::new(780.0, 25.0),
                ImagePoint::new(10.0, 25.0),
            ]),
            confidence: 0.99,
            orientation_degrees: None,
        }],
        mean_confidence: Some(0.99),
        model: ModelIdentity::new("synthetic-ocr", "v1"),
        processing_time_ms: 1,
        warnings: Vec::new(),
    };
    let native_pages = [PageMarkdown {
        page: 0,
        markdown: native.to_string(),
        needs_ocr: native.is_empty(),
        ocr_reason: None,
    }];
    let run = OcrRun {
        pages: vec![RoutedOcrPage { rendered, ocr }],
        render_time_ms: 1,
        ocr_time_ms: 1,
    };
    fuse_ocr_pages(&native_pages, &run, 1, &OcrFusionOptions::new())
        .unwrap()
        .pages
        .remove(0)
}

#[test]
fn fusion_regression_discards_fragments_already_inside_native_paragraphs_and_tables() {
    let native = "Thermal Projection SA Customer Name Belgique\n\n\
        | Description | Quantité | Prix unitaire | TVA |\n\
        | --- | --- | --- | --- |\n\
        | ASPIRATION SUPPORT AVANT PROJECTION | 70 | 1,50 | 6% |\n";
    for recognized in ["Belgique", "unitaire", "PROJECTION70", "Prix unitaire"] {
        let page = fuse(native, recognized);
        assert_eq!(page.markdown, native, "OCR fragment: {recognized}");
        assert_eq!(page.provenance.source, PageContentSource::Native);
    }
}

#[test]
fn fusion_regression_matches_joined_and_split_words_without_rewriting_native() {
    let native = "Thermal Projection SA Customer Name Rue des étangs Belgique\n";
    for recognized in [
        "Thermal Projection SACustomer Name Rue des étangs",
        "Ther mal Projec tion SA Customer Name",
    ] {
        assert_eq!(fuse(native, recognized).markdown, native);
    }
}

#[test]
fn fusion_regression_matches_fragments_across_markdown_blocks() {
    let native = "# Date\n\n6 jours suivant la date de facturation\n\nPayment terms\n";
    assert_eq!(
        fuse(native, "Date6 jours suivant la date de facturation").markdown,
        native
    );
}

#[test]
fn fusion_regression_rejects_high_confidence_garbage_when_native_is_usable() {
    let native = "Invoice total 420.00\nPayment received\n";
    let page = fuse(native, &"a@@b%%c&&d==e~~".repeat(12));
    assert_eq!(page.markdown, native);
    assert_eq!(page.provenance.source, PageContentSource::Native);
    assert!(!page.provenance.warnings.is_empty());
}

#[test]
fn fusion_regression_keeps_financial_conflicts_with_their_context() {
    for (native, recognized) in [
        ("Refund 42.00\n", "Refund -42.00"),
        ("Total 42.00\n", "Total 4200"),
        ("Total 42.00\n", "Total 43.00"),
    ] {
        let page = fuse(native, recognized);
        assert!(page.markdown.starts_with(native));
        assert!(
            page.markdown.contains(recognized),
            "conflict lost: {}",
            page.markdown
        );
        assert_eq!(page.provenance.source, PageContentSource::Fused);
    }
}

#[test]
fn fusion_regression_preserves_new_content_and_scan_only_ocr() {
    for (native, recognized, expected) in [
        (
            "Invoice total 420.00\n",
            "Bank account BE12 3456 7890",
            "BE12 3456 7890",
        ),
        ("Internet services\n", "Net", "Net"),
        ("Status ready\n", "Status ready 42", "42"),
        ("", "Scanned invoice total 420.00", "420.00"),
    ] {
        let page = fuse(native, recognized);
        assert!(
            page.markdown.contains(expected),
            "new OCR lost: {}",
            page.markdown
        );
        if !native.is_empty() {
            assert!(
                page.markdown.len() > native.len(),
                "addition lost: {recognized}"
            );
            assert!(page.markdown.starts_with(native));
        }
    }
}

#[test]
fn fusion_regression_does_not_mix_different_table_rows_or_join_numeric_fields() {
    for (native, recognized) in [
        (
            "| Label | Value |\n| --- | --- |\n| Tax | 10 |\n| Fee | 20 |\n",
            "Tax 20",
        ),
        ("Reference 12 34\n", "Reference 1234"),
        ("Reference 1 234\n", "Reference 12 34"),
    ] {
        let page = fuse(native, recognized);
        assert!(
            page.markdown.contains(recognized),
            "distinct content lost: {}",
            page.markdown
        );
        assert!(page.markdown.starts_with(native));
    }
}
