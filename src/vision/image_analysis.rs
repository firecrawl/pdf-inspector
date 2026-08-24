//! Cheap bitmap signals used to decide whether OCR work is warranted.

use crate::types::PdfRect;

use super::{RenderPixelFormat, RenderedPage};

/// Returns whether a PDF-space region has a document-like mix of white
/// background, dark foreground ink, content rows, and blank separators.
pub(super) fn region_has_document_like_ink(page: &RenderedPage, region: &PdfRect) -> bool {
    let (x, y, width, height) = page.pdf_rect_to_pixel(region);
    let x0 = x.floor().max(0.0).min(f64::from(page.width())) as usize;
    let y0 = y.floor().max(0.0).min(f64::from(page.height())) as usize;
    let x1 = (x + width).ceil().max(0.0).min(f64::from(page.width())) as usize;
    let y1 = (y + height).ceil().max(0.0).min(f64::from(page.height())) as usize;
    if x1 <= x0 + 4 || y1 <= y0 + 4 {
        return false;
    }

    let pixel_count = (x1 - x0).saturating_mul(y1 - y0);
    let step = ((pixel_count / 150_000).max(1) as f64).sqrt().ceil() as usize;
    let bytes_per_pixel = page.format().bytes_per_pixel();
    let mut sampled = 0usize;
    let mut near_white = 0usize;
    let mut dark = 0usize;
    let mut content_rows = 0usize;
    let mut blank_rows = 0usize;
    for sample_y in (y0..y1).step_by(step.max(1)) {
        let mut row_sampled = 0usize;
        let mut row_white = 0usize;
        let mut row_dark = 0usize;
        for sample_x in (x0..x1).step_by(step.max(1)) {
            let offset = sample_y * page.stride() + sample_x * bytes_per_pixel;
            let luminance = match page.format() {
                RenderPixelFormat::Gray8 => u16::from(page.pixels()[offset]),
                RenderPixelFormat::Rgb8 | RenderPixelFormat::Rgba8 => {
                    let red = u16::from(page.pixels()[offset]);
                    let green = u16::from(page.pixels()[offset + 1]);
                    let blue = u16::from(page.pixels()[offset + 2]);
                    (red * 3 + green * 6 + blue) / 10
                }
            };
            sampled += 1;
            row_sampled += 1;
            if luminance >= 235 {
                near_white += 1;
                row_white += 1;
            }
            if luminance <= 110 {
                dark += 1;
                row_dark += 1;
            }
        }
        if row_sampled > 0 {
            let dark_ratio = row_dark as f32 / row_sampled as f32;
            let white_ratio = row_white as f32 / row_sampled as f32;
            if (0.01..=0.65).contains(&dark_ratio) {
                content_rows += 1;
            } else if row_dark == 0 && white_ratio >= 0.9 {
                blank_rows += 1;
            }
        }
    }

    if sampled == 0 {
        return false;
    }
    let white_ratio = near_white as f32 / sampled as f32;
    let dark_ratio = dark as f32 / sampled as f32;
    white_ratio >= 0.45
        && (0.002..=0.35).contains(&dark_ratio)
        && content_rows >= 3
        && blank_rows >= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::PageTransform;

    fn rendered_gray_page(pixels: Vec<u8>) -> RenderedPage {
        let transform =
            PageTransform::from_corners(20, 20, (0.0, 20.0), (20.0, 20.0), (0.0, 0.0)).unwrap();
        RenderedPage::new(
            1,
            20.0,
            20.0,
            20,
            20,
            20,
            RenderPixelFormat::Gray8,
            pixels,
            transform,
        )
        .unwrap()
    }

    #[test]
    fn requires_document_like_ink_rows() {
        let region = PdfRect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
            page: 1,
        };
        let mut document = vec![255; 400];
        for y in [4usize, 10, 16] {
            for x in 4usize..16 {
                document[y * 20 + x] = 0;
            }
        }
        assert!(region_has_document_like_ink(
            &rendered_gray_page(document),
            &region
        ));
        assert!(!region_has_document_like_ink(
            &rendered_gray_page(vec![128; 400]),
            &region
        ));
    }
}
