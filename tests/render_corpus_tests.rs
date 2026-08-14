#![cfg(all(feature = "render", not(target_arch = "wasm32")))]

use pdf_inspector::{
    classify_pdf_mem, render_pages_mem, RenderOptions, RenderWarning, RenderedPage,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct CorpusCase {
    relative_path: &'static str,
    sha256: &'static str,
    pages: &'static [u32],
    dpi: f32,
}

const CASES: &[CorpusCase] = &[
    CorpusCase {
        relative_path: "001-trivial/minimal-document.pdf",
        sha256: "f723638db6e763cf4ccadad38a3d38a02d9ecab95dab1f0bbf00e801991b5f92",
        pages: &[0],
        dpi: 72.0,
    },
    CorpusCase {
        relative_path: "007-imagemagick-images/imagemagick-images.pdf",
        sha256: "0f2076573bfed1107300a2383b88bbbbc2b85a57f06b3ff478a0faa7ded57b4e",
        pages: &[5, 0, 3],
        dpi: 72.0,
    },
    CorpusCase {
        relative_path: "018-base64-image/base64image.pdf",
        sha256: "aaad90df16fce40ec768629d2135479b98f65b39bb27c7f80fb106393187d619",
        pages: &[0],
        dpi: 200.0,
    },
    CorpusCase {
        relative_path: "023-cmyk-image/cmyk-image.pdf",
        sha256: "5a5f76a951e403a5b357992789afc5164fd6c2914583741de7a1dd08ec029ab2",
        pages: &[0],
        dpi: 72.0,
    },
    CorpusCase {
        relative_path: "027-cropped-rotated-scaled/cropped-rotated-scaled.pdf",
        sha256: "cc195eac510b81123de4f55ac9f8185f2120975a7b75a106460b98615737aacd",
        pages: &[3, 0],
        dpi: 72.0,
    },
    CorpusCase {
        relative_path: "028-image-references-deduplication/wrong-references.pdf",
        sha256: "16cb8e10bd59e30b4d350f11d0ed9c7d0bd7e7bb962a46022c89836e2aa44f63",
        pages: &[2, 0],
        dpi: 72.0,
    },
];

/// Exercise an immutable, checksum-verified external compatibility corpus.
///
/// The PDF files are CC-BY-SA-4.0 and are deliberately not vendored into this
/// MIT repository. Check out py-pdf/sample-files at commit
/// `89039b6078fd0c9f98bf3d6fcb5583fac6b0ecaf`, set
/// `PDF_INSPECTOR_SAMPLE_FILES` to its root, then run:
///
/// `cargo test --features render --test render_corpus_tests renders_pinned -- --ignored --nocapture`
#[test]
#[ignore = "requires the external py-pdf/sample-files corpus"]
fn renders_pinned_py_pdf_sample_files() {
    let root = std::env::var_os("PDF_INSPECTOR_SAMPLE_FILES")
        .map(PathBuf::from)
        .expect("set PDF_INSPECTOR_SAMPLE_FILES to the pinned corpus checkout");

    for case in CASES {
        let path = root.join(case.relative_path);
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", path.display());
        });
        assert_sha256(&path, &bytes, case.sha256);

        let started = Instant::now();
        let rendered = render_pages_mem(&bytes, case.pages, RenderOptions::new().dpi(case.dpi))
            .unwrap_or_else(|error| panic!("failed to render {}: {error}", path.display()));
        let elapsed = started.elapsed();

        assert_eq!(
            rendered.iter().map(|page| page.page).collect::<Vec<_>>(),
            case.pages
        );
        for page in &rendered {
            assert_valid_rgba(page, &path);
        }

        eprintln!(
            "rendered {} page buffer(s) from {} at {} DPI in {:.3?}",
            rendered.len(),
            case.relative_path,
            case.dpi,
            elapsed
        );
    }
}

/// Print one release-profile data point for the OCR-positive image PDF.
///
/// Set `PDF_INSPECTOR_RENDER_DPI` to run the same pinned input at a specific
/// resolution. This is kept separate from the compatibility loop so an
/// external process monitor can capture peak memory for one DPI at a time.
#[test]
#[ignore = "requires the external py-pdf/sample-files corpus"]
fn measures_ocr_positive_page_at_configured_dpi() {
    let root = std::env::var_os("PDF_INSPECTOR_SAMPLE_FILES")
        .map(PathBuf::from)
        .expect("set PDF_INSPECTOR_SAMPLE_FILES to the pinned corpus checkout");
    let dpi = std::env::var("PDF_INSPECTOR_RENDER_DPI")
        .unwrap_or_else(|_| "200".to_string())
        .parse::<f32>()
        .expect("PDF_INSPECTOR_RENDER_DPI must be a number");
    let case = &CASES[2];
    let path = root.join(case.relative_path);
    let bytes = std::fs::read(&path).expect("read OCR-positive corpus PDF");
    assert_sha256(&path, &bytes, case.sha256);
    let classification = classify_pdf_mem(&bytes).expect("classify OCR-positive corpus PDF");
    assert_eq!(classification.pages_needing_ocr, [0]);

    let started = Instant::now();
    let rendered = render_pages_mem(&bytes, &[0], RenderOptions::new().dpi(dpi))
        .expect("render OCR-positive page");
    let elapsed = started.elapsed();
    assert_valid_rgba(&rendered[0], &path);

    eprintln!(
        "rendered {} at {} DPI to {}x{} ({} RGBA bytes) in {:.3?}",
        case.relative_path,
        dpi,
        rendered[0].width,
        rendered[0].height,
        rendered[0].pixels.len(),
        elapsed
    );
}

fn assert_sha256(path: &Path, bytes: &[u8], expected: &str) {
    let actual = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(
        actual,
        expected,
        "unexpected corpus revision for {}",
        path.display()
    );
}

fn assert_valid_rgba(page: &RenderedPage, path: &Path) {
    assert!(
        !page.warnings.contains(&RenderWarning::ImageDecodeFailure),
        "an image resource failed to decode for {}",
        path.display()
    );
    assert!(page.width > 0, "zero-width output for {}", path.display());
    assert!(page.height > 0, "zero-height output for {}", path.display());
    assert_eq!(
        page.pixels.len(),
        page.width as usize * page.height as usize * 4,
        "invalid RGBA length for {}",
        path.display()
    );
    assert!(
        page.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255),
        "non-opaque output for {}",
        path.display()
    );
    let non_white = page
        .pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] != [255, 255, 255])
        .count();
    assert!(
        non_white > 0,
        "rendered content is unexpectedly blank for {}",
        path.display()
    );
}
