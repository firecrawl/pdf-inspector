#![cfg(all(feature = "ocr-oar", not(target_arch = "wasm32")))]

#[cfg(feature = "ocr")]
use pdf_inspector::vision::{
    process_pdf_with_ocr_mem, ModelDownloadPolicy, OcrPdfOptions, OcrPipelineError,
    PageContentSource,
};
use pdf_inspector::vision::{
    ModelStore, OarOcrEngine, OcrEngine, OcrMode, OcrOptions, PageTransform, RenderPixelFormat,
    RenderedPage, PP_OCR_V6_SMALL,
};
#[cfg(feature = "render-pdfium")]
use pdf_inspector::vision::{PdfiumRenderer, RenderError, RenderOptions};

const MODEL_DIRECTORY_ENV: &str = "PDF_INSPECTOR_OCR_TEST_MODELS";
const IMAGE_ENV: &str = "PDF_INSPECTOR_OCR_TEST_IMAGE";
const EXPECTED_TEXT_ENV: &str = "PDF_INSPECTOR_OCR_TEST_EXPECTED";

#[cfg(feature = "render-pdfium")]
fn load_renderer() -> Option<PdfiumRenderer> {
    match PdfiumRenderer::load() {
        Ok(renderer) => Some(renderer),
        Err(RenderError::PdfiumLoad { .. }) => {
            eprintln!("skipping OCR runtime test because no native PDFium library is installed");
            None
        }
        Err(error) => panic!("failed to load PDFium: {error}"),
    }
}

#[test]
fn recognizes_an_rgb_image_with_verified_models() {
    let Some(model_directory) = std::env::var_os(MODEL_DIRECTORY_ENV) else {
        eprintln!("skipping OCR runtime test because {MODEL_DIRECTORY_ENV} is not set");
        return;
    };
    let Some(image_path) = std::env::var_os(IMAGE_ENV) else {
        eprintln!("skipping OCR runtime test because {IMAGE_ENV} is not set");
        return;
    };

    let image = image::open(image_path).unwrap().into_rgb8();
    let (width, height) = image.dimensions();
    let transform = PageTransform::from_corners(
        width,
        height,
        (0.0, f64::from(height)),
        (f64::from(width), f64::from(height)),
        (0.0, 0.0),
    )
    .unwrap();
    let page = RenderedPage::new(
        1,
        width as f32,
        height as f32,
        width,
        height,
        width as usize * 3,
        RenderPixelFormat::Rgb8,
        image.into_raw(),
        transform,
    )
    .unwrap();
    let results = recognize(&model_directory, &[page]);
    assert_usable_result(&results);

    let text = results[0]
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("recognized: {text}");
    if let Ok(expected) = std::env::var(EXPECTED_TEXT_ENV) {
        assert!(
            text.to_lowercase().contains(&expected.to_lowercase()),
            "expected OCR output to contain {expected:?}, got {text:?}"
        );
    }
}

#[cfg(feature = "render-pdfium")]
#[test]
fn recognizes_a_pdfium_rendered_fixture_with_verified_models() {
    let Some(model_directory) = std::env::var_os(MODEL_DIRECTORY_ENV) else {
        eprintln!("skipping OCR runtime test because {MODEL_DIRECTORY_ENV} is not set");
        return;
    };
    let Some(renderer) = load_renderer() else {
        return;
    };

    let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();
    let pages = renderer
        .render_pages(
            &bytes,
            &[1],
            None,
            &RenderOptions::new().dpi(150.0).form_fields(false),
        )
        .unwrap();
    let results = recognize(&model_directory, &pages);
    assert_usable_result(&results);
}

#[cfg(all(feature = "ocr", feature = "render-pdfium"))]
#[test]
fn complete_ocr_pipeline_routes_and_assembles_a_scanned_fixture() {
    let Some(model_directory) = std::env::var_os(MODEL_DIRECTORY_ENV) else {
        eprintln!("skipping OCR runtime test because {MODEL_DIRECTORY_ENV} is not set");
        return;
    };
    let Some(_renderer) = load_renderer() else {
        return;
    };

    let bytes = std::fs::read("tests/fixtures/scan_with_native_header_text.pdf").unwrap();
    let ocr = OcrOptions::new()
        .mode(OcrMode::Auto)
        .minimum_confidence(0.3)
        .model_directory(model_directory)
        .model_downloads(ModelDownloadPolicy::Offline);
    let options = OcrPdfOptions::new().ocr(ocr);
    let result = process_pdf_with_ocr_mem(&bytes, options.clone()).unwrap();
    let repeated = process_pdf_with_ocr_mem(&bytes, options).unwrap();

    assert_eq!(result.pages_routed_to_ocr, vec![1]);
    assert!(!result.markdown.trim().is_empty());
    assert!(result
        .markdown
        .contains("Order Date Item Code Description Status Unit Cost\n\n03/14/2024"));
    assert!(result.markdown.contains("$482,110.40\n\n05/02/2024"));
    assert_eq!(result.pages[0].provenance.source, PageContentSource::Fused);
    assert!(result.pages[0]
        .provenance
        .warnings
        .iter()
        .any(|warning| warning.contains("complementary OCR")));
    assert_eq!(
        result.pages[0].provenance.ocr_model.as_ref().unwrap().name,
        PP_OCR_V6_SMALL.id
    );
    assert_eq!(repeated.markdown, result.markdown);
}

#[cfg(all(feature = "ocr", feature = "render-pdfium"))]
#[test]
fn auto_recovers_credible_native_text_before_loading_ocr_models() {
    let Some(_renderer) = load_renderer() else {
        return;
    };

    let bytes = std::fs::read("tests/fixtures/shinagawa_identity_h.pdf").unwrap();
    let ocr = OcrOptions::new()
        .mode(OcrMode::Auto)
        .model_directory("/models/must-not-be-read")
        .model_downloads(ModelDownloadPolicy::Offline);
    let result = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().ocr(ocr)).unwrap();

    assert_eq!(result.pages_recommended_for_ocr, vec![1]);
    assert!(result.pages_routed_to_ocr.is_empty());
    assert!(result.markdown.contains("羽田空港新飛行経路"));
    assert!(result.markdown.contains("|4月30日|有|81.0|"));
    assert!(result.markdown.contains("※１ 最大騒音レベル"));
    assert!(result.pages_with_tables.contains(&1));
    assert_eq!(result.pages[0].provenance.source, PageContentSource::Native);
    assert!(result.pages[0].provenance.ocr_model.is_none());
}

#[cfg(all(feature = "ocr", feature = "render-pdfium"))]
#[test]
fn auto_rejects_garbled_native_recovery_and_continues_to_ocr() {
    let Some(_renderer) = load_renderer() else {
        return;
    };

    let bytes = std::fs::read("tests/fixtures/shifted_cipher_tounicode.pdf").unwrap();
    let ocr = OcrOptions::new()
        .mode(OcrMode::Auto)
        .model_directory("/models/must-not-be-read")
        .model_downloads(ModelDownloadPolicy::Offline);
    let error = process_pdf_with_ocr_mem(&bytes, OcrPdfOptions::new().ocr(ocr)).unwrap_err();

    assert!(matches!(
        error,
        OcrPipelineError::ModelAcquire(_) | OcrPipelineError::ModelStore(_)
    ));
}

fn recognize(
    model_directory: &std::ffi::OsStr,
    pages: &[RenderedPage],
) -> Vec<pdf_inspector::vision::OcrPage> {
    let store = ModelStore::new(model_directory).override_root(model_directory);
    let models = store.resolve(&PP_OCR_V6_SMALL).unwrap();
    let engine = OarOcrEngine::from_models(&models).unwrap();
    engine
        .recognize(
            pages,
            &OcrOptions::new()
                .mode(OcrMode::Force)
                .minimum_confidence(0.3),
        )
        .unwrap()
}

fn assert_usable_result(results: &[pdf_inspector::vision::OcrPage]) {
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].page_number, 1);
    assert_eq!(results[0].model.name, PP_OCR_V6_SMALL.id);
    assert_eq!(results[0].model.revision, PP_OCR_V6_SMALL.revision);
    assert!(!results[0].spans.is_empty());
    assert!(results[0].spans.iter().all(|span| span.confidence >= 0.3));
}
