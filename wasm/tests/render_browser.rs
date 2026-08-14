#![cfg(all(target_arch = "wasm32", feature = "render"))]

use wasm_bindgen_test::*;

#[path = "../../tests/support/render_fixture.rs"]
mod render_fixture;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn renders_an_image_xobject_in_a_browser() {
    let pdf = render_fixture::synthetic_image_pdf();
    let rendered =
        pdf_inspector::render_pages_mem(&pdf, &[0], pdf_inspector::RenderOptions::new().dpi(72.0))
            .expect("render image page");

    assert_eq!(rendered.len(), 1);
    assert_eq!((rendered[0].width, rendered[0].height), (64, 64));
    assert!(rendered[0].warnings.is_empty());
    assert_eq!(rendered[0].pixels.len(), 64 * 64 * 4);
    assert!(rendered[0]
        .pixels
        .chunks_exact(4)
        .all(|pixel| pixel[3] == 255));
    assert!(rendered[0]
        .pixels
        .chunks_exact(4)
        .any(|pixel| pixel[0] > 180 && pixel[2] < 80));
    assert!(rendered[0]
        .pixels
        .chunks_exact(4)
        .any(|pixel| pixel[2] > 180 && pixel[0] < 80));
}
