/// Build a small, deterministic PDF whose only page contains an RGB image
/// XObject. Keeping this generated avoids adding a separately licensed binary
/// fixture and lets native and WebAssembly tests exercise the same bytes.
pub fn synthetic_image_pdf() -> Vec<u8> {
    let mut image = Vec::with_capacity(4 * 4 * 3);
    for y in 0..4 {
        for x in 0..4 {
            image.extend_from_slice(if (x + y) % 2 == 0 {
                &[255, 0, 0]
            } else {
                &[0, 0, 255]
            });
        }
    }

    synthetic_image_pdf_with_data(&image, "")
}

/// Build an image-backed PDF whose declared JPEG data cannot be decoded.
#[cfg(not(target_arch = "wasm32"))]
pub fn synthetic_broken_image_pdf() -> Vec<u8> {
    synthetic_image_pdf_with_data(b"not a JPEG stream", "/Filter /DCTDecode")
}

fn synthetic_image_pdf_with_data(image: &[u8], image_options: &str) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0_usize];

    fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &[u8]) {
        assert_eq!(id, offsets.len());
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    add_object(
        &mut pdf,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    add_object(
        &mut pdf,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 64] \
          /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );

    let content = b"q 64 0 0 64 0 0 cm /Im0 Do Q";
    let mut content_stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    content_stream.extend_from_slice(content);
    content_stream.extend_from_slice(b"\nendstream");
    add_object(&mut pdf, &mut offsets, 4, &content_stream);

    let mut image_stream = format!(
        "<< /Type /XObject /Subtype /Image /Width 4 /Height 4 \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 {image_options} /Length {} >>\nstream\n",
        image.len()
    )
    .into_bytes();
    image_stream.extend_from_slice(image);
    image_stream.extend_from_slice(b"\nendstream");
    add_object(&mut pdf, &mut offsets, 5, &image_stream);

    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF",
            offsets.len()
        )
        .as_bytes(),
    );
    pdf
}
