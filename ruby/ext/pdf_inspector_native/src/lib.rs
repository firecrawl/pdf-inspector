use convert::rhash;
use magnus::{function, prelude::*, Error, RArray, RHash, RString, Ruby};
use std::collections::HashSet;

mod convert;
mod errors;

/// Smoke-test function: proves the extension loaded and can call back into Ruby.
///
/// # Arguments
/// * `_ruby` - Ruby VM handle, unused here beyond proving the callback works.
fn native_version(_ruby: &Ruby) -> Result<String, Error> {
    Ok(env!("CARGO_PKG_VERSION").to_string())
}

/// Extension entry point, called by Ruby when the native library is loaded.
/// Registers the `PdfInspector::Native` module and its singleton methods.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to define modules and singleton methods.
#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    errors::init(ruby);

    let native = ruby
        .define_module("PdfInspector")?
        .define_module("Native")?;
    native.define_singleton_method("native_version", function!(native_version, 0))?;
    native.define_singleton_method("process_bytes", function!(process_bytes, 2))?;
    native.define_singleton_method("detect_bytes", function!(detect_bytes, 1))?;
    native.define_singleton_method("classify_bytes", function!(classify_bytes, 1))?;
    native.define_singleton_method("extract_text_bytes", function!(extract_text_bytes, 1))?;
    native.define_singleton_method(
        "extract_text_with_positions_bytes",
        function!(extract_text_with_positions_bytes, 2),
    )?;
    native.define_singleton_method(
        "extract_text_in_regions_bytes",
        function!(extract_text_in_regions_bytes, 2),
    )?;
    native.define_singleton_method(
        "extract_pages_markdown_bytes",
        function!(extract_pages_markdown_bytes, 2),
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Public Native API
// ---------------------------------------------------------------------------

/// Extracts a PDF (given as raw bytes) to Markdown, optionally restricted to
/// a subset of `pages` (1-based), and returns the full result hash.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result hash and map errors.
/// * `bytes` - Raw PDF file contents.
/// * `pages` - Optional 1-based page numbers to restrict extraction to; `None` processes all pages.
fn process_bytes(ruby: &Ruby, bytes: RString, pages: Option<Vec<u32>>) -> Result<RHash, Error> {
    let mut opts = pdf_inspector::PdfOptions::new();
    if let Some(page_numbers) = pages {
        opts = opts.pages(page_numbers);
    }

    let result = pdf_inspector::process_pdf_mem_with_options(&convert::bytes_of(bytes), opts)
        .map_err(|err| errors::to_magnus_err(ruby, err))?;

    convert::pdf_result_to_hash(ruby, result)
}

/// Runs full PDF-type detection (TextBased/Scanned/Mixed/ImageBased) on raw
/// PDF bytes, including markdown extraction, and returns the result hash.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result hash and map errors.
/// * `bytes` - Raw PDF file contents.
fn detect_bytes(ruby: &Ruby, bytes: RString) -> Result<RHash, Error> {
    let result = pdf_inspector::detect_pdf_mem(&convert::bytes_of(bytes))
        .map_err(|err| errors::to_magnus_err(ruby, err))?;

    convert::pdf_result_to_hash(ruby, result)
}

/// Lightweight classification of raw PDF bytes: returns just the PDF type,
/// page count, OCR-needed page count, and confidence, without extracting
/// markdown.
///
/// Note: `pages_needing_ocr` here is 0-indexed (matching `classify_pdf_mem`),
/// unlike the same-named field returned by `process_bytes`/`detect_bytes`,
/// which is 1-indexed.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result hash and map errors.
/// * `bytes` - Raw PDF file contents.
fn classify_bytes(ruby: &Ruby, bytes: RString) -> Result<RHash, Error> {
    let result = pdf_inspector::classify_pdf_mem(&convert::bytes_of(bytes))
        .map_err(|err| errors::to_magnus_err(ruby, err))?;

    let hash = ruby.hash_new();
    convert::set_classification_fields(
        ruby,
        hash,
        result.pdf_type,
        result.page_count,
        result.pages_needing_ocr,
        result.confidence,
    )?;

    Ok(hash)
}

/// Extracts plain text (no Markdown formatting) from raw PDF bytes.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to map errors.
/// * `bytes` - Raw PDF file contents.
fn extract_text_bytes(ruby: &Ruby, bytes: RString) -> Result<String, Error> {
    pdf_inspector::extractor::extract_text_mem(&convert::bytes_of(bytes))
        .map_err(|err| errors::to_magnus_err(ruby, err))
}

/// Extracts positioned text items (with bounding boxes, font info, and
/// style flags) from raw PDF bytes, optionally restricted to `pages`.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result array and map errors.
/// * `bytes` - Raw PDF file contents.
/// * `pages` - Optional 1-based page numbers to restrict extraction to; `None` processes all pages.
fn extract_text_with_positions_bytes(
    ruby: &Ruby,
    bytes: RString,
    pages: Option<Vec<u32>>,
) -> Result<RArray, Error> {
    let buf = convert::bytes_of(bytes);
    let items = match pages {
        Some(page_numbers) => {
            let page_set: HashSet<u32> = page_numbers.into_iter().collect();
            pdf_inspector::extractor::extract_text_with_positions_mem_pages(&buf, Some(&page_set))
                .map_err(|err| errors::to_magnus_err(ruby, err))?
        }
        None => pdf_inspector::extractor::extract_text_with_positions_mem(&buf)
            .map_err(|err| errors::to_magnus_err(ruby, err))?,
    };
    ruby.ary_try_from_iter(
        items
            .into_iter()
            .map(|item| convert::text_item_to_hash(ruby, item)),
    )
}

/// Extracts text confined to specific bounding-box regions per page, given
/// raw PDF bytes and a `page_regions` array of `{ page:, regions: }` hashes.
/// Returns one hash per page, each containing the extracted text per region.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result array and map errors.
/// * `bytes` - Raw PDF file contents.
/// * `page_regions` - Array of `{ page:, regions: }` hashes; `page` is a 0-indexed
///   page number (matching `pdf_inspector::extract_text_in_regions_mem`, unlike
///   the 1-indexed `pages:` used by `process`/`extract_text_with_positions`)
///   and `regions` is an array of `[x0, y0, x1, y1]` bounding boxes.
fn extract_text_in_regions_bytes(
    ruby: &Ruby,
    bytes: RString,
    page_regions: RArray,
) -> Result<RArray, Error> {
    let regions = convert::parse_page_regions(ruby, page_regions)?;
    let results = pdf_inspector::extract_text_in_regions_mem(&convert::bytes_of(bytes), &regions)
        .map_err(|err| errors::to_magnus_err(ruby, err))?;

    ruby.ary_try_from_iter(results.into_iter().map(|page_result| {
        let regions = ruby.ary_try_from_iter(
            page_result
                .regions
                .into_iter()
                .map(|region_text| convert::region_text_to_hash(ruby, region_text)),
        )?;
        Ok(rhash!(ruby, {
            "page" => page_result.page,
            "regions" => regions,
        }))
    }))
}

/// Extracts Markdown per-page (rather than one combined document) from raw
/// PDF bytes, optionally restricted to `pages`, along with table/column/OCR
/// metadata for the extracted set.
///
/// Note: `pages` and the returned `PageMarkdown#page` are 0-indexed (matching
/// `extract_pages_markdown_mem`), unlike the 1-indexed `pages_with_tables`,
/// `pages_with_columns`, `pages_needing_ocr`, and `ocr_reasons_by_page` in the
/// same result hash.
///
/// # Arguments
/// * `ruby` - Ruby VM handle used to build the result hash and map errors.
/// * `bytes` - Raw PDF file contents.
/// * `pages` - Optional 0-indexed page numbers to restrict extraction to; `None` processes all pages.
fn extract_pages_markdown_bytes(
    ruby: &Ruby,
    bytes: RString,
    pages: Option<Vec<u32>>,
) -> Result<RHash, Error> {
    let result =
        pdf_inspector::extract_pages_markdown_mem(&convert::bytes_of(bytes), pages.as_deref())
            .map_err(|err| errors::to_magnus_err(ruby, err))?;

    let page_hashes = ruby.ary_try_from_iter(
        result
            .pages
            .into_iter()
            .map(|page| convert::page_markdown_to_hash(ruby, page)),
    )?;
    let ocr_reasons_by_page =
        convert::page_ocr_reasons_to_hashes(ruby, result.ocr_reasons_by_page)?;

    Ok(rhash!(ruby, {
        "pages" => page_hashes,
        "pages_with_tables" => result.pages_with_tables,
        "pages_with_columns" => result.pages_with_columns,
        "pages_needing_ocr" => result.pages_needing_ocr,
        "ocr_reasons_by_page" => ocr_reasons_by_page,
        "is_complex" => result.is_complex,
    }))
}
