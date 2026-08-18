#![cfg(all(feature = "render-pdfium", not(target_arch = "wasm32")))]

use pdf_inspector::vision::{PdfiumRenderer, RenderError, RenderOptions, RenderPixelFormat};

fn load_renderer() -> Option<PdfiumRenderer> {
    match PdfiumRenderer::load() {
        Ok(renderer) => Some(renderer),
        Err(RenderError::PdfiumLoad { .. }) => {
            eprintln!("skipping PDFium runtime test because no native library is installed");
            None
        }
        Err(error) => panic!("failed to load PDFium: {error}"),
    }
}

#[test]
fn renders_owned_rgb_page_and_round_trips_coordinates() {
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

    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert_eq!(page.page(), 1);
    assert_eq!(page.format(), RenderPixelFormat::Rgb8);
    assert_eq!(page.stride(), page.width() as usize * 3);
    assert_eq!(page.pixels().len(), page.stride() * page.height() as usize);
    assert!((page.width() as f32 - page.page_width()).abs() > 1.0);

    let pdf_rect = page.pixel_rect_to_pdf_rect(10.0, 10.0, 20.0, 12.0);
    let pixel_rect = page.pdf_rect_to_pixel(&pdf_rect);
    assert!((pixel_rect.0 - 10.0).abs() < 0.01);
    assert!((pixel_rect.1 - 10.0).abs() < 0.01);
    assert!((pixel_rect.2 - 20.0).abs() < 0.01);
    assert!((pixel_rect.3 - 12.0).abs() < 0.01);
}

#[test]
fn rejects_zero_and_out_of_range_page_numbers() {
    let Some(renderer) = load_renderer() else {
        return;
    };
    let bytes = std::fs::read("tests/fixtures/thermo-freon12.pdf").unwrap();

    assert!(matches!(
        renderer.render_pages(&bytes, &[0], None, &RenderOptions::new()),
        Err(RenderError::InvalidPageNumber)
    ));

    assert!(matches!(
        renderer.render_pages(&bytes, &[u32::MAX], None, &RenderOptions::new()),
        Err(RenderError::PageOutOfBounds { .. })
    ));
}
