use magnus::{prelude::*, r_hash::RHash, Error, RArray, RString, Ruby};

/// Builds a Ruby hash from a flat list of `"key" => value` pairs, replacing
/// the repeated `hash.aset(ruby.to_symbol(key), value)?` boilerplate.
macro_rules! rhash {
    ($ruby:expr, { $($key:literal => $val:expr),+ $(,)? }) => {{
        let hash = $ruby.hash_new();
        $( hash.aset($ruby.to_symbol($key), $val)?; )+
        hash
    }};
}
pub(crate) use rhash;

/// Copies a Ruby string's bytes into an owned `Vec<u8>`.
///
/// # Safety
/// `as_slice` is unsafe because the Ruby GC could otherwise move or free the
/// underlying buffer while borrowed; copying immediately into an owned `Vec`
/// avoids holding that borrow across any GC-triggering operation.
pub(crate) fn bytes_of(bytes: RString) -> Vec<u8> {
    unsafe { bytes.as_slice() }.to_vec()
}

/// Converts a `PdfType` classification into its Ruby symbol representation
/// (e.g. `:text_based`, `:scanned`, `:image_based`, `:mixed`).
pub(crate) fn pdf_type_symbol(ruby: &Ruby, pdf_type: pdf_inspector::PdfType) -> magnus::Symbol {
    ruby.to_symbol(match pdf_type {
        pdf_inspector::PdfType::TextBased => "text_based",
        pdf_inspector::PdfType::Scanned => "scanned",
        pdf_inspector::PdfType::ImageBased => "image_based",
        pdf_inspector::PdfType::Mixed => "mixed",
    })
}

/// Converts a machine-readable OCR reason string into a Ruby symbol.
pub(crate) fn ocr_reason_symbol(ruby: &Ruby, reason: Option<String>) -> Option<magnus::Symbol> {
    reason.map(|reason| ruby.to_symbol(reason.as_str()))
}

/// Converts a list of per-page OCR reasons into a Ruby array of hashes,
/// each with a `:page` number and a `:reasons` array of symbols.
pub(crate) fn page_ocr_reasons_to_hashes(
    ruby: &Ruby,
    reasons: Vec<pdf_inspector::PageOcrReasons>,
) -> Result<RArray, Error> {
    ruby.ary_try_from_iter(reasons.into_iter().map(|page_reasons| {
        let syms = ruby.ary_from_iter(
            page_reasons
                .reasons
                .iter()
                .map(|reason| ruby.to_symbol(reason.as_str())),
        );
        Ok(rhash!(ruby, {
            "page" => page_reasons.page,
            "reasons" => syms,
        }))
    }))
}

/// Sets the classification fields shared by the full result hash
/// ([`pdf_result_to_hash`]) and the lightweight classify-only hash, so the
/// two stay in sync as fields change.
pub(crate) fn set_classification_fields(
    ruby: &Ruby,
    hash: RHash,
    pdf_type: pdf_inspector::PdfType,
    page_count: u32,
    pages_needing_ocr: Vec<u32>,
    confidence: f32,
) -> Result<(), Error> {
    hash.aset(ruby.to_symbol("pdf_type"), pdf_type_symbol(ruby, pdf_type))?;
    hash.aset(ruby.to_symbol("page_count"), page_count)?;
    hash.aset(ruby.to_symbol("pages_needing_ocr"), pages_needing_ocr)?;
    hash.aset(ruby.to_symbol("confidence"), confidence)?;
    Ok(())
}

/// Converts a full `PdfProcessResult` (markdown, page/table/column metadata,
/// OCR reasons, encoding issues, etc.) into a Ruby hash keyed by symbol.
pub(crate) fn pdf_result_to_hash(
    ruby: &Ruby,
    result: pdf_inspector::PdfProcessResult,
) -> Result<RHash, Error> {
    let ocr_reasons_by_page = page_ocr_reasons_to_hashes(ruby, result.ocr_reasons_by_page)?;
    let hash = rhash!(ruby, {
        "markdown" => result.markdown,
        "processing_time_ms" => result.processing_time_ms,
        "ocr_reasons_by_page" => ocr_reasons_by_page,
        "title" => result.title,
        "is_complex_layout" => result.layout.is_complex,
        "pages_with_tables" => result.layout.pages_with_tables,
        "pages_with_columns" => result.layout.pages_with_columns,
        "has_encoding_issues" => result.has_encoding_issues,
    });
    set_classification_fields(
        ruby,
        hash,
        result.pdf_type,
        result.page_count,
        result.pages_needing_ocr,
        result.confidence,
    )?;

    Ok(hash)
}

/// Converts an `ItemType` into its Ruby symbol (`:text`, `:image`, `:link`,
/// `:form_field`), plus the link URL when the item is a `Link`.
pub(crate) fn item_type_symbol_and_link(
    ruby: &Ruby,
    item_type: &pdf_inspector::types::ItemType,
) -> (magnus::Symbol, Option<String>) {
    match item_type {
        pdf_inspector::types::ItemType::Text => (ruby.to_symbol("text"), None),
        pdf_inspector::types::ItemType::Image => (ruby.to_symbol("image"), None),
        pdf_inspector::types::ItemType::Link(url) => (ruby.to_symbol("link"), Some(url.clone())),
        pdf_inspector::types::ItemType::FormField => (ruby.to_symbol("form_field"), None),
    }
}

/// Converts a single `TextItem` (positioned text/image/link/form-field span)
/// into a Ruby hash with its geometry, font, style flags, and item type.
pub(crate) fn text_item_to_hash(
    ruby: &Ruby,
    item: pdf_inspector::TextItem,
) -> Result<RHash, Error> {
    let (item_type, link_url) = item_type_symbol_and_link(ruby, &item.item_type);

    Ok(rhash!(ruby, {
        "text" => item.text,
        "x" => item.x,
        "y" => item.y,
        "width" => item.width,
        "height" => item.height,
        "font" => item.font,
        "font_size" => item.font_size,
        "page" => item.page,
        "is_bold" => item.is_bold,
        "is_italic" => item.is_italic,
        "is_underline" => item.is_underline,
        "is_strikeout" => item.is_strikeout,
        "item_type" => item_type,
        "link_url" => link_url,
    }))
}

/// One page's worth of region bounding boxes: `(1-based page, [x0,y0,x1,y1]
/// boxes)`. Matches the tuple shape `pdf_inspector::extract_text_in_regions_mem`
/// takes directly, so no intermediate named type is introduced here.
pub(crate) type PageRegions = Vec<(u32, Vec<[f32; 4]>)>;

/// Parses a Ruby array of `{ page:, regions: }` hashes into `PageRegions`,
/// where each region is a 4-element `[x0, y0, x1, y1]` bounding box.
pub(crate) fn parse_page_regions(ruby: &Ruby, page_regions: RArray) -> Result<PageRegions, Error> {
    page_regions
        .into_iter()
        .map(|value| {
            let hash = RHash::try_convert(value)?;
            let page: u32 = hash.fetch(ruby.to_symbol("page"))?;

            let regions: Vec<Vec<f32>> = hash.fetch(ruby.to_symbol("regions"))?;
            let bboxes: Vec<[f32; 4]> = regions
                .into_iter()
                .map(|region| {
                    let len = region.len();
                    region.try_into().map_err(|_| {
                        Error::new(
                            ruby.exception_arg_error(),
                            format!(
                                "region must have exactly 4 elements [x0, y0, x1, y1], got {len}"
                            ),
                        )
                    })
                })
                .collect::<Result<_, Error>>()?;
            Ok((page, bboxes))
        })
        .collect()
}

/// Converts a `RegionText` result (text extracted from one bounding box)
/// into a Ruby hash with `:text`, `:needs_ocr`, and `:ocr_reason`.
pub(crate) fn region_text_to_hash(
    ruby: &Ruby,
    region_text: pdf_inspector::RegionText,
) -> Result<RHash, Error> {
    let ocr_reason = ocr_reason_symbol(ruby, region_text.ocr_reason);
    Ok(rhash!(ruby, {
        "text" => region_text.text,
        "needs_ocr" => region_text.needs_ocr,
        "ocr_reason" => ocr_reason,
    }))
}

/// Converts a single `PageMarkdown` (per-page markdown plus OCR status) into
/// a Ruby hash with `:page`, `:markdown`, `:needs_ocr`, and `:ocr_reason`.
pub(crate) fn page_markdown_to_hash(
    ruby: &Ruby,
    page: pdf_inspector::PageMarkdown,
) -> Result<RHash, Error> {
    let ocr_reason = ocr_reason_symbol(ruby, page.ocr_reason);
    Ok(rhash!(ruby, {
        "page" => page.page,
        "markdown" => page.markdown,
        "needs_ocr" => page.needs_ocr,
        "ocr_reason" => ocr_reason,
    }))
}
