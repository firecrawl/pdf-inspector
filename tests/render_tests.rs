#![cfg(feature = "render")]

use pdf_inspector::{
    render_pages_mem, RenderError, RenderOptions, RenderWarning, DEFAULT_RENDER_DPI,
    MAX_RENDER_DPI, MAX_RENDER_OUTPUT_BYTES, MAX_RENDER_PAGES_PER_REQUEST,
    MAX_RENDER_PIXELS_PER_PAGE,
};

#[path = "support/render_fixture.rs"]
mod render_fixture;

fn make_solid_page_pdf(width: f32, height: f32, colors: &[[u8; 3]]) -> Vec<u8> {
    make_solid_page_pdf_with_page_options(width, height, colors, "")
}

fn make_solid_page_pdf_with_page_options(
    width: f32,
    height: f32,
    colors: &[[u8; 3]],
    page_options: &str,
) -> Vec<u8> {
    let mut object_bodies = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), {
        let kids = (0..colors.len())
            .map(|index| format!("{} 0 R", 3 + index * 2))
            .collect::<Vec<_>>()
            .join(" ");
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", colors.len()).into_bytes()
    }];

    for [red, green, blue] in colors.iter().copied() {
        let content_id = object_bodies.len() + 2;
        object_bodies.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 {page_options} /Resources << >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        );

        let content = format!(
            "{} {} {} rg 0 0 {width} {height} re f",
            f32::from(red) / 255.0,
            f32::from(green) / 255.0,
            f32::from(blue) / 255.0
        );
        object_bodies.push(
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        );
    }

    render_fixture::build_pdf_from_objects(&object_bodies)
}

fn center_pixel(pixels: &[u8], width: u32, height: u32) -> [u8; 4] {
    let offset = ((height as usize / 2) * width as usize + width as usize / 2) * 4;
    pixels[offset..offset + 4].try_into().unwrap()
}

#[test]
fn renders_selected_pages_in_caller_order_with_duplicates() {
    let pdf = make_solid_page_pdf(100.0, 80.0, &[[255, 0, 0], [0, 0, 255]]);
    let rendered = render_pages_mem(&pdf, &[1, 0, 1], RenderOptions::new().dpi(72.0))
        .expect("render selected pages");

    assert_eq!(
        rendered.iter().map(|page| page.page).collect::<Vec<_>>(),
        [1, 0, 1]
    );
    for page in &rendered {
        assert_eq!((page.width, page.height), (100, 80));
        assert_eq!(page.pixels.len(), 100 * 80 * 4);
        assert!(page.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }
    assert_eq!(center_pixel(&rendered[0].pixels, 100, 80), [0, 0, 255, 255]);
    assert_eq!(center_pixel(&rendered[1].pixels, 100, 80), [255, 0, 0, 255]);
    assert_eq!(rendered[0], rendered[2]);
}

#[test]
fn renders_an_image_xobject_to_opaque_color_pixels() {
    let pdf = render_fixture::synthetic_image_pdf();
    let rendered =
        render_pages_mem(&pdf, &[0], RenderOptions::new().dpi(72.0)).expect("render image page");
    let mut pixels = rendered[0].pixels.chunks_exact(4);

    assert_eq!((rendered[0].width, rendered[0].height), (64, 64));
    assert!(rendered[0].warnings.is_empty());
    assert!(pixels.clone().all(|pixel| pixel[3] == 255));
    assert!(
        pixels.clone().any(|pixel| pixel[0] > 180 && pixel[2] < 80),
        "red checker cells were not decoded"
    );
    assert!(
        pixels.any(|pixel| pixel[2] > 180 && pixel[0] < 80),
        "blue checker cells were not decoded"
    );
}

#[test]
fn exposes_image_decode_failures_instead_of_silently_returning_pixels() {
    let pdf = render_fixture::synthetic_broken_image_pdf();
    let rendered =
        render_pages_mem(&pdf, &[0], RenderOptions::new().dpi(72.0)).expect("render image page");

    assert_eq!(rendered[0].warnings, [RenderWarning::ImageDecodeFailure]);
}

#[test]
fn uses_bounded_200_dpi_default() {
    let pdf = make_solid_page_pdf(100.0, 80.0, &[[0, 0, 0]]);
    let rendered = render_pages_mem(&pdf, &[0], RenderOptions::new()).expect("render page");

    assert_eq!(DEFAULT_RENDER_DPI, 200.0);
    assert_eq!((rendered[0].width, rendered[0].height), (277, 222));
    assert_eq!(
        rendered[0].pixels.len(),
        rendered[0].width as usize * rendered[0].height as usize * 4
    );
}

#[test]
fn applies_crop_box_and_page_rotation() {
    let pdf = make_solid_page_pdf_with_page_options(
        100.0,
        80.0,
        &[[0, 255, 0]],
        "/CropBox [0 0 40 30] /Rotate 90",
    );
    let rendered =
        render_pages_mem(&pdf, &[0], RenderOptions::new().dpi(72.0)).expect("render page");

    assert_eq!((rendered[0].width, rendered[0].height), (30, 40));
    assert_eq!(center_pixel(&rendered[0].pixels, 30, 40), [0, 255, 0, 255]);
}

#[test]
fn rejects_invalid_dpi_values() {
    let pdf = make_solid_page_pdf(100.0, 80.0, &[[0, 0, 0]]);

    for dpi in [0.0, -1.0, f32::NAN, f32::INFINITY, MAX_RENDER_DPI + 1.0] {
        assert!(matches!(
            render_pages_mem(&pdf, &[0], RenderOptions::new().dpi(dpi)),
            Err(RenderError::InvalidDpi { .. })
        ));
    }
}

#[test]
fn rejects_invalid_pdf_bytes() {
    assert_eq!(
        render_pages_mem(b"not a PDF", &[0], RenderOptions::new()),
        Err(RenderError::Parse)
    );
}

#[test]
fn rejects_out_of_range_pages_before_rendering() {
    let pdf = make_solid_page_pdf(100.0, 80.0, &[[0, 0, 0]]);

    assert_eq!(
        render_pages_mem(&pdf, &[0, 1], RenderOptions::new().dpi(72.0)),
        Err(RenderError::PageOutOfRange {
            page: 1,
            page_count: 1,
        })
    );
}

#[test]
fn accepts_an_empty_selection_after_parsing() {
    let pdf = make_solid_page_pdf(100.0, 80.0, &[[0, 0, 0]]);

    assert_eq!(
        render_pages_mem(&pdf, &[], RenderOptions::new()).unwrap(),
        []
    );
    assert_eq!(
        render_pages_mem(b"not a PDF", &[], RenderOptions::new()),
        Err(RenderError::Parse)
    );
}

#[test]
fn rejects_dimension_and_pixel_limits_before_allocation() {
    let too_wide = make_solid_page_pdf(17_000.0, 1.0, &[[0, 0, 0]]);
    assert!(matches!(
        render_pages_mem(&too_wide, &[0], RenderOptions::new().dpi(72.0)),
        Err(RenderError::PageDimensionsTooLarge { page: 0, .. })
    ));

    let too_many_pixels = make_solid_page_pdf(6_000.0, 5_000.0, &[[0, 0, 0]]);
    assert!(matches!(
        render_pages_mem(&too_many_pixels, &[0], RenderOptions::new().dpi(72.0)),
        Err(RenderError::PagePixelsTooLarge {
            page: 0,
            pixels: 30_000_000,
            max: MAX_RENDER_PIXELS_PER_PAGE,
        })
    ));
}

#[test]
fn rejects_total_output_limit_before_allocation() {
    let pdf = make_solid_page_pdf(6_000.0, 4_000.0, &[[0, 0, 0]]);

    assert_eq!(
        render_pages_mem(&pdf, &[0, 0], RenderOptions::new().dpi(72.0)),
        Err(RenderError::OutputTooLarge {
            bytes: 192_000_000,
            max: MAX_RENDER_OUTPUT_BYTES,
        })
    );
}

#[test]
fn rejects_excessive_page_entry_count() {
    let pdf = make_solid_page_pdf(1.0, 1.0, &[[0, 0, 0]]);
    let pages = vec![0; MAX_RENDER_PAGES_PER_REQUEST + 1];

    assert_eq!(
        render_pages_mem(&pdf, &pages, RenderOptions::new().dpi(72.0)),
        Err(RenderError::TooManyPages {
            requested: MAX_RENDER_PAGES_PER_REQUEST + 1,
            max: MAX_RENDER_PAGES_PER_REQUEST,
        })
    );
}

#[test]
fn render_options_debug_redacts_password() {
    let debug = format!("{:?}", RenderOptions::new().password("secret123"));

    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret123"));
}

#[test]
fn decrypts_with_the_supplied_password() {
    let pdf = include_bytes!("fixtures/encrypted-secret123.pdf");

    assert_eq!(
        render_pages_mem(pdf, &[0], RenderOptions::new().dpi(72.0)),
        Err(RenderError::Encrypted)
    );
    assert_eq!(
        render_pages_mem(pdf, &[0], RenderOptions::new().dpi(72.0).password("wrong")),
        Err(RenderError::Encrypted)
    );

    let rendered = render_pages_mem(
        pdf,
        &[0],
        RenderOptions::new().dpi(72.0).password("secret123"),
    )
    .expect("render encrypted PDF with its password");
    assert_eq!(rendered.len(), 1);
    assert!(!rendered[0].pixels.is_empty());
}
