use pdf_inspector::{
    LayoutComplexity, MarkdownProfile, PageOcrReasons, PdfOptions, PdfProcessResult, PdfType,
    ProcessMode,
};
use serde::{Deserialize, Serialize};
use std::panic::{self, AssertUnwindSafe};
use std::slice;

const ABI_VERSION: u32 = 1;
const STATUS_SUCCESS: i32 = 0;
const STATUS_INVALID_ARGUMENT: i32 = 1;
const STATUS_PROCESSING_ERROR: i32 = 2;
const STATUS_SERIALIZATION_ERROR: i32 = 3;
const STATUS_PANIC: i32 = 4;

#[repr(C)]
pub struct PdfInspectorResult {
    status: i32,
    data: *mut u8,
    len: usize,
}

impl PdfInspectorResult {
    fn from_bytes(status: i32, bytes: Vec<u8>) -> Self {
        let mut bytes = bytes.into_boxed_slice();
        let result = Self {
            status,
            data: bytes.as_mut_ptr(),
            len: bytes.len(),
        };
        std::mem::forget(bytes);
        result
    }

    fn success(bytes: Vec<u8>) -> Self {
        Self::from_bytes(STATUS_SUCCESS, bytes)
    }

    fn error(status: i32, context: &str, error: impl std::fmt::Display) -> Self {
        let message = format!("{context}: {error}");
        Self::from_bytes(status, message.into_bytes())
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ProcessOptions {
    pages: Option<Vec<u32>>,
    password: Option<String>,
    profile: Option<BindingMarkdownProfile>,
    include_page_markers: Option<bool>,
    include_images: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum BindingMarkdownProfile {
    #[serde(alias = "Fidelity")]
    Fidelity,
    #[serde(alias = "Compact")]
    Compact,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingPageOcrReasons {
    page: u32,
    reasons: Vec<String>,
}

impl From<PageOcrReasons> for BindingPageOcrReasons {
    fn from(value: PageOcrReasons) -> Self {
        Self {
            page: value.page,
            reasons: value.reasons,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingLayoutComplexity {
    is_complex: bool,
    pages_with_tables: Vec<u32>,
    pages_with_columns: Vec<u32>,
}

impl From<LayoutComplexity> for BindingLayoutComplexity {
    fn from(value: LayoutComplexity) -> Self {
        Self {
            is_complex: value.is_complex,
            pages_with_tables: value.pages_with_tables,
            pages_with_columns: value.pages_with_columns,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingPdfProcessResult {
    pdf_type: &'static str,
    markdown: Option<String>,
    page_count: u32,
    processing_time_ms: u64,
    pages_needing_ocr: Vec<u32>,
    ocr_reasons_by_page: Vec<BindingPageOcrReasons>,
    title: Option<String>,
    confidence: f64,
    layout: BindingLayoutComplexity,
    has_encoding_issues: bool,
}

impl From<PdfProcessResult> for BindingPdfProcessResult {
    fn from(value: PdfProcessResult) -> Self {
        Self {
            pdf_type: pdf_type_name(value.pdf_type),
            markdown: value.markdown,
            page_count: value.page_count,
            processing_time_ms: value.processing_time_ms,
            pages_needing_ocr: value.pages_needing_ocr,
            ocr_reasons_by_page: value
                .ocr_reasons_by_page
                .into_iter()
                .map(Into::into)
                .collect(),
            title: value.title,
            confidence: f64::from(value.confidence),
            layout: value.layout.into(),
            has_encoding_issues: value.has_encoding_issues,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingPdfClassification {
    pdf_type: &'static str,
    page_count: u32,
    pages_needing_ocr: Vec<u32>,
    confidence: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
enum BindingOcrMode {
    Off,
    Auto,
    Force,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct BindingOcrOptions {
    mode: Option<BindingOcrMode>,
    page_numbers: Option<Vec<u32>>,
    password: Option<String>,
    dpi: Option<f64>,
    minimum_confidence: Option<f64>,
    hosted_recommendation_confidence: Option<f64>,
    model_directory: Option<String>,
    offline: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingOcrModelIdentity {
    name: String,
    revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingOcrTimings {
    render_ms: u64,
    ocr_ms: u64,
    assembly_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingOcrPageProvenance {
    page_number: u32,
    source: &'static str,
    ocr_model: Option<BindingOcrModelIdentity>,
    render_dpi: Option<f64>,
    ocr_confidence: Option<f64>,
    timings: BindingOcrTimings,
    warnings: Vec<String>,
    hosted_recommended: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingOcrPageResult {
    page_number: u32,
    markdown: String,
    provenance: BindingOcrPageProvenance,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingOcrPdfResult {
    markdown: String,
    pages: Vec<BindingOcrPageResult>,
    page_count: u32,
    pages_recommended_for_ocr: Vec<u32>,
    pages_routed_to_ocr: Vec<u32>,
    pages_recommending_hosted: Vec<u32>,
    ocr_reasons_by_page: Vec<BindingPageOcrReasons>,
    pages_with_tables: Vec<u32>,
    pages_with_columns: Vec<u32>,
    is_complex: bool,
    processing_time_ms: u64,
    render_time_ms: u64,
    ocr_time_ms: u64,
}

fn pdf_type_name(pdf_type: PdfType) -> &'static str {
    match pdf_type {
        PdfType::TextBased => "TextBased",
        PdfType::Scanned => "Scanned",
        PdfType::ImageBased => "ImageBased",
        PdfType::Mixed => "Mixed",
    }
}

unsafe fn required_bytes<'a>(data: *const u8, len: usize) -> Result<&'a [u8], &'static str> {
    if data.is_null() {
        return Err("PDF data pointer is null");
    }
    if len == 0 {
        return Err("PDF data is empty");
    }
    Ok(slice::from_raw_parts(data, len))
}

unsafe fn optional_bytes<'a>(data: *const u8, len: usize) -> Result<&'a [u8], &'static str> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err("options pointer is null while options length is non-zero");
    }
    Ok(slice::from_raw_parts(data, len))
}

fn parse_options<T>(bytes: &[u8]) -> Result<T, serde_json::Error>
where
    T: Default + for<'de> Deserialize<'de>,
{
    if bytes.is_empty() {
        Ok(T::default())
    } else {
        serde_json::from_slice(bytes)
    }
}

fn build_options(options: ProcessOptions, mode: ProcessMode) -> Result<PdfOptions, &'static str> {
    if options
        .pages
        .as_ref()
        .is_some_and(|pages| pages.contains(&0))
    {
        return Err("pages are 1-indexed; page 0 is invalid");
    }

    let mut result = PdfOptions::new().mode(mode);
    if let Some(pages) = options.pages {
        result = result.pages(pages);
    }
    if let Some(password) = options.password {
        result = result.password(password);
    }
    if let Some(profile) = options.profile {
        result.markdown.profile = match profile {
            BindingMarkdownProfile::Fidelity => MarkdownProfile::Fidelity,
            BindingMarkdownProfile::Compact => MarkdownProfile::Compact,
        };
    }
    if let Some(include_page_markers) = options.include_page_markers {
        result.markdown.include_page_numbers = include_page_markers;
    }
    if let Some(include_images) = options.include_images {
        result.markdown.include_images = include_images;
    }
    Ok(result)
}

fn build_ocr_options(
    options: BindingOcrOptions,
) -> Result<pdf_inspector::vision::OcrPdfOptions, &'static str> {
    if options
        .page_numbers
        .as_ref()
        .is_some_and(|pages| pages.contains(&0))
    {
        return Err("pageNumbers are 1-indexed; page 0 is invalid");
    }

    let mut result = pdf_inspector::vision::OcrPdfOptions::auto();
    if let Some(mode) = options.mode {
        result.ocr.mode = match mode {
            BindingOcrMode::Off => pdf_inspector::vision::OcrMode::Off,
            BindingOcrMode::Auto => pdf_inspector::vision::OcrMode::Auto,
            BindingOcrMode::Force => pdf_inspector::vision::OcrMode::Force,
        };
    }
    if let Some(pages) = options.page_numbers {
        result = result.page_numbers(pages);
    }
    if let Some(password) = options.password {
        result = result.password(password);
    }
    if let Some(dpi) = options.dpi {
        result.render.dpi = dpi as f32;
    }
    if let Some(minimum_confidence) = options.minimum_confidence {
        result.ocr.minimum_confidence = minimum_confidence as f32;
    }
    if let Some(confidence) = options.hosted_recommendation_confidence {
        result.hosted_recommendation_confidence = confidence as f32;
    }
    if let Some(directory) = options.model_directory {
        result.ocr.model_directory = Some(directory.into());
    }
    if options.offline.unwrap_or(false) {
        result.ocr.model_downloads = pdf_inspector::vision::ModelDownloadPolicy::Offline;
    }
    Ok(result)
}

fn serialize<T: Serialize>(value: &T) -> PdfInspectorResult {
    match serde_json::to_vec(value) {
        Ok(bytes) => PdfInspectorResult::success(bytes),
        Err(error) => {
            PdfInspectorResult::error(STATUS_SERIALIZATION_ERROR, "serialize result", error)
        }
    }
}

fn guarded(
    context: &'static str,
    action: impl FnOnce() -> PdfInspectorResult + panic::UnwindSafe,
) -> PdfInspectorResult {
    match panic::catch_unwind(action) {
        Ok(result) => result,
        Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_owned()
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else {
                "unknown panic".to_owned()
            };
            PdfInspectorResult::error(STATUS_PANIC, context, format!("Rust panic: {message}"))
        }
    }
}

fn process_impl(
    data: *const u8,
    len: usize,
    options_data: *const u8,
    options_len: usize,
    mode: ProcessMode,
    context: &'static str,
) -> PdfInspectorResult {
    guarded(
        context,
        AssertUnwindSafe(|| unsafe {
            let bytes = match required_bytes(data, len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(STATUS_INVALID_ARGUMENT, context, error)
                }
            };
            let options_bytes = match optional_bytes(options_data, options_len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(STATUS_INVALID_ARGUMENT, context, error)
                }
            };
            let options = match parse_options::<ProcessOptions>(options_bytes) {
                Ok(options) => options,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "invalid options",
                        error,
                    )
                }
            };
            let options = match build_options(options, mode) {
                Ok(options) => options,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "invalid options",
                        error,
                    )
                }
            };
            match pdf_inspector::process_pdf_mem_with_options(bytes, options) {
                Ok(result) => serialize(&BindingPdfProcessResult::from(result)),
                Err(error) => PdfInspectorResult::error(STATUS_PROCESSING_ERROR, context, error),
            }
        }),
    )
}

#[no_mangle]
pub extern "C" fn pdf_inspector_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
/// # Safety
///
/// `data` must reference `len` readable bytes. When `options_len` is non-zero,
/// `options_data` must reference that many readable UTF-8 JSON bytes.
pub unsafe extern "C" fn pdf_inspector_process_pdf(
    data: *const u8,
    len: usize,
    options_data: *const u8,
    options_len: usize,
) -> PdfInspectorResult {
    process_impl(
        data,
        len,
        options_data,
        options_len,
        ProcessMode::Full,
        "process PDF",
    )
}

#[no_mangle]
/// # Safety
///
/// `data` must reference `len` readable bytes. When `options_len` is non-zero,
/// `options_data` must reference that many readable UTF-8 JSON bytes.
pub unsafe extern "C" fn pdf_inspector_detect_pdf(
    data: *const u8,
    len: usize,
    options_data: *const u8,
    options_len: usize,
) -> PdfInspectorResult {
    process_impl(
        data,
        len,
        options_data,
        options_len,
        ProcessMode::DetectOnly,
        "detect PDF",
    )
}

#[no_mangle]
/// # Safety
///
/// `data` must reference `len` readable bytes.
pub unsafe extern "C" fn pdf_inspector_classify_pdf(
    data: *const u8,
    len: usize,
) -> PdfInspectorResult {
    guarded(
        "classify PDF",
        AssertUnwindSafe(|| unsafe {
            let bytes = match required_bytes(data, len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "classify PDF",
                        error,
                    )
                }
            };
            match pdf_inspector::classify_pdf_mem(bytes) {
                Ok(result) => serialize(&BindingPdfClassification {
                    pdf_type: pdf_type_name(result.pdf_type),
                    page_count: result.page_count,
                    pages_needing_ocr: result.pages_needing_ocr,
                    confidence: f64::from(result.confidence),
                }),
                Err(error) => {
                    PdfInspectorResult::error(STATUS_PROCESSING_ERROR, "classify PDF", error)
                }
            }
        }),
    )
}

#[no_mangle]
/// # Safety
///
/// `data` must reference `len` readable bytes.
pub unsafe extern "C" fn pdf_inspector_extract_text(
    data: *const u8,
    len: usize,
) -> PdfInspectorResult {
    guarded(
        "extract text",
        AssertUnwindSafe(|| unsafe {
            let bytes = match required_bytes(data, len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "extract text",
                        error,
                    )
                }
            };
            match pdf_inspector::extractor::extract_text_with_positions_mem(bytes) {
                Ok(items) => PdfInspectorResult::success(
                    pdf_inspector::extractor::group_into_lines_preserving_all_text(items)
                        .into_iter()
                        .map(|line| line.text())
                        .filter(|line| !line.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .into_bytes(),
                ),
                Err(error) => {
                    PdfInspectorResult::error(STATUS_PROCESSING_ERROR, "extract text", error)
                }
            }
        }),
    )
}

#[no_mangle]
/// # Safety
///
/// `data` must reference `len` readable bytes. When `options_len` is non-zero,
/// `options_data` must reference that many readable UTF-8 JSON bytes.
pub unsafe extern "C" fn pdf_inspector_process_pdf_with_ocr(
    data: *const u8,
    len: usize,
    options_data: *const u8,
    options_len: usize,
) -> PdfInspectorResult {
    guarded(
        "process PDF with OCR",
        AssertUnwindSafe(|| unsafe {
            let bytes = match required_bytes(data, len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "process PDF with OCR",
                        error,
                    )
                }
            };
            let options_bytes = match optional_bytes(options_data, options_len) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "process PDF with OCR",
                        error,
                    )
                }
            };
            let options = match parse_options::<BindingOcrOptions>(options_bytes) {
                Ok(options) => options,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "invalid OCR options",
                        error,
                    )
                }
            };
            let options = match build_ocr_options(options) {
                Ok(options) => options,
                Err(error) => {
                    return PdfInspectorResult::error(
                        STATUS_INVALID_ARGUMENT,
                        "invalid OCR options",
                        error,
                    )
                }
            };

            match pdf_inspector::vision::process_pdf_with_ocr_mem(bytes, options) {
                Ok(result) => serialize(&BindingOcrPdfResult {
                    markdown: result.markdown,
                    pages: result
                        .pages
                        .into_iter()
                        .map(|page| {
                            let provenance = page.provenance;
                            BindingOcrPageResult {
                                page_number: page.page_number,
                                markdown: page.markdown,
                                provenance: BindingOcrPageProvenance {
                                    page_number: provenance.page_number,
                                    source: match provenance.source {
                                        pdf_inspector::vision::PageContentSource::Native => {
                                            "Native"
                                        }
                                        pdf_inspector::vision::PageContentSource::Ocr => "Ocr",
                                        pdf_inspector::vision::PageContentSource::Fused => "Fused",
                                        _ => "Native",
                                    },
                                    ocr_model: provenance.ocr_model.map(|model| {
                                        BindingOcrModelIdentity {
                                            name: model.name,
                                            revision: model.revision,
                                        }
                                    }),
                                    render_dpi: provenance.render_dpi.map(f64::from),
                                    ocr_confidence: provenance.ocr_confidence.map(f64::from),
                                    timings: BindingOcrTimings {
                                        render_ms: provenance.timings.render_ms,
                                        ocr_ms: provenance.timings.ocr_ms,
                                        assembly_ms: provenance.timings.assembly_ms,
                                    },
                                    warnings: provenance.warnings,
                                    hosted_recommended: provenance.hosted_recommended,
                                },
                            }
                        })
                        .collect(),
                    page_count: result.page_count,
                    pages_recommended_for_ocr: result.pages_recommended_for_ocr,
                    pages_routed_to_ocr: result.pages_routed_to_ocr,
                    pages_recommending_hosted: result.pages_recommending_hosted,
                    ocr_reasons_by_page: result
                        .ocr_reasons_by_page
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    pages_with_tables: result.pages_with_tables,
                    pages_with_columns: result.pages_with_columns,
                    is_complex: result.is_complex,
                    processing_time_ms: result.processing_time_ms,
                    render_time_ms: result.render_time_ms,
                    ocr_time_ms: result.ocr_time_ms,
                }),
                Err(error) => PdfInspectorResult::error(
                    STATUS_PROCESSING_ERROR,
                    "process PDF with OCR",
                    error,
                ),
            }
        }),
    )
}

#[no_mangle]
pub extern "C" fn pdf_inspector_version() -> PdfInspectorResult {
    PdfInspectorResult::success(env!("CARGO_PKG_VERSION").as_bytes().to_vec())
}

#[no_mangle]
/// # Safety
///
/// `data` and `len` must be the unchanged values returned by this library and
/// must not have been released previously.
pub unsafe extern "C" fn pdf_inspector_free_result(data: *mut u8, len: usize) {
    if data.is_null() {
        return;
    }
    drop(Vec::from_raw_parts(data, len, len));
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT_PDF: &[u8] = include_bytes!("../../../tests/fixtures/thermo-freon12.pdf");

    fn take(result: PdfInspectorResult) -> Result<Vec<u8>, (i32, String)> {
        let bytes = unsafe { slice::from_raw_parts(result.data, result.len).to_vec() };
        unsafe { pdf_inspector_free_result(result.data, result.len) };
        if result.status == STATUS_SUCCESS {
            Ok(bytes)
        } else {
            Err((
                result.status,
                String::from_utf8(bytes).expect("UTF-8 error"),
            ))
        }
    }

    #[test]
    fn exposes_wasm_compatible_process_contract() {
        let bytes = take(unsafe {
            pdf_inspector_process_pdf(TEXT_PDF.as_ptr(), TEXT_PDF.len(), std::ptr::null(), 0)
        })
        .expect("process PDF");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("result JSON");

        assert_eq!(value["pdfType"], "TextBased");
        assert!(value["markdown"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
        assert!(value["layout"]["isComplex"].is_boolean());
    }

    #[test]
    fn rejects_page_zero() {
        let options = br#"{"pages":[0]}"#;
        let error = take(unsafe {
            pdf_inspector_process_pdf(
                TEXT_PDF.as_ptr(),
                TEXT_PDF.len(),
                options.as_ptr(),
                options.len(),
            )
        })
        .expect_err("page zero must fail");

        assert_eq!(error.0, STATUS_INVALID_ARGUMENT);
        assert!(error.1.contains("1-indexed"));
    }

    #[test]
    fn ocr_off_does_not_require_external_runtime() {
        let options = br#"{"mode":"Off"}"#;
        let bytes = take(unsafe {
            pdf_inspector_process_pdf_with_ocr(
                TEXT_PDF.as_ptr(),
                TEXT_PDF.len(),
                options.as_ptr(),
                options.len(),
            )
        })
        .expect("native-only OCR pipeline");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("result JSON");

        assert!(value["pageCount"].as_u64().is_some_and(|count| count > 0));
        assert_eq!(value["pagesRoutedToOcr"], serde_json::json!([]));
    }
}
