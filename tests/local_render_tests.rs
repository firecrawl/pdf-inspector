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

/// A page drawn through a Form XObject with the given `/BBox`, whose
/// content is a 24 pt line at the top of the page.
fn page_drawn_through_form(form_bbox: &str) -> Vec<u8> {
    let page_content = "q Q q 0 0 612 792 re W n /Fm1 Do Q";
    let form_content = "BT /F1 24 Tf 72 700 Td (Drawn through the form) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{page_content}\nendstream",
            page_content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [{form_bbox}] \
             /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{form_content}\nendstream",
            form_content.len()
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Dark pixels in the rendered first page, and the rows they span.
fn rendered_ink(renderer: &PdfiumRenderer, bytes: &[u8]) -> (usize, Option<(u32, u32)>) {
    let pages = renderer
        .render_pages(
            bytes,
            &[1],
            None,
            &RenderOptions::new()
                .dpi(72.0)
                .pixel_format(RenderPixelFormat::Rgb8),
        )
        .unwrap();
    let page = &pages[0];
    let pixels = page.pixels();
    let mut dark = 0usize;
    let mut rows: Option<(u32, u32)> = None;
    for y in 0..page.height() {
        for x in 0..page.width() {
            let i = y as usize * page.stride() + x as usize * 3;
            if pixels[i] < 128 && pixels[i + 1] < 128 && pixels[i + 2] < 128 {
                dark += 1;
                rows = Some(match rows {
                    Some((top, bottom)) => (top.min(y), bottom.max(y)),
                    None => (y, y),
                });
            }
        }
    }
    (dark, rows)
}

/// A Form XObject whose `/BBox` has no area clips its content to nothing
/// when rendered as written; the document repaired by
/// `widen_degenerate_form_bboxes_mem` renders the form's text where it is
/// painted, and a form with a real box keeps that box.
#[test]
fn zero_area_form_bbox_renders_after_the_repair() {
    let Some(renderer) = load_renderer() else {
        return;
    };
    let original = page_drawn_through_form("0 0 0 0");
    let (dark, _) = rendered_ink(&renderer, &original);
    assert_eq!(dark, 0, "the zero-area box clips the form as written");

    let repaired = pdf_inspector::widen_degenerate_form_bboxes_mem(&original)
        .unwrap()
        .expect("the zero-area box is repaired");
    let (dark, rows) = rendered_ink(&renderer, &repaired);
    assert!(
        dark > 100,
        "the repaired form paints its text: {dark} dark pixels"
    );
    // 24 pt text on the 700 pt baseline of a 792 pt page, at 72 dpi.
    let (top, bottom) = rows.unwrap();
    assert!(
        (70..=95).contains(&top) && (85..=100).contains(&bottom),
        "rows {top}..{bottom}"
    );

    let proper = page_drawn_through_form("0 690 612 792");
    assert!(pdf_inspector::widen_degenerate_form_bboxes_mem(&proper)
        .unwrap()
        .is_none());
    let (dark, _) = rendered_ink(&renderer, &proper);
    assert!(dark > 100, "a form with a real box renders as written");
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
