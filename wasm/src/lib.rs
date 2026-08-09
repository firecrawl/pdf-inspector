use pdf_inspector::{
    DetectionConfig, LayoutComplexity, MarkdownProfile, PageOcrReasons, PdfOptions,
    PdfProcessResult, PdfType, ProcessMode, ScanStrategy,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT_TYPES: &str = r#"
export type PdfType = "TextBased" | "Scanned" | "ImageBased" | "Mixed";
export type MarkdownProfile = "fidelity" | "compact";

export type ScanStrategy =
  | "earlyExit"
  | "full"
  | { sample: number }
  | { pages: number[] };

export interface DetectionOptions {
  /** Which pages detection inspects. Defaults to `{ sample: 8 }`. */
  strategy?: ScanStrategy;
  /** Minimum text operators required for a page to count as text-based. */
  minTextOpsPerPage?: number;
  /** Text-page ratio required for TextBased classification (0.0–1.0). */
  textPageRatioThreshold?: number;
}

export interface DetectOptions extends DetectionOptions {
  /** Password for an encrypted PDF. */
  password?: string;
}

export interface ProcessOptions extends DetectOptions {
  /** Restrict extraction to these 1-indexed page numbers. */
  pages?: number[];
  /** Source-faithful output by default, or compact output for fewer tokens. */
  profile?: MarkdownProfile;
  /** Insert `<!-- Page N -->` markers between pages. */
  includePageMarkers?: boolean;
  /** Include image placeholders in Markdown output. */
  includeImages?: boolean;
}

export interface PageOcrReasons {
  /** 1-indexed page number. */
  page: number;
  reasons: string[];
}

export interface LayoutComplexity {
  isComplex: boolean;
  /** 1-indexed page numbers. */
  pagesWithTables: number[];
  /** 1-indexed page numbers. */
  pagesWithColumns: number[];
}

export interface PdfProcessResult {
  pdfType: PdfType;
  markdown?: string;
  pageCount: number;
  processingTimeMs: number;
  /** 1-indexed page numbers. */
  pagesNeedingOcr: number[];
  ocrReasonsByPage: PageOcrReasons[];
  title?: string;
  confidence: number;
  layout: LayoutComplexity;
  hasEncodingIssues: boolean;
}

export interface PdfClassification {
  pdfType: PdfType;
  pageCount: number;
  /** 0-indexed page numbers, matching the native Node.js API. */
  pagesNeedingOcr: number[];
  confidence: number;
}

export function processPdf(data: Uint8Array, options?: ProcessOptions): PdfProcessResult;
export function detectPdf(data: Uint8Array, options?: DetectOptions): PdfProcessResult;
export function classifyPdf(data: Uint8Array, options?: DetectOptions): PdfClassification;
export function extractText(data: Uint8Array): string;
export function version(): string;
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct WasmProcessOptions {
    strategy: Option<WasmScanStrategy>,
    min_text_ops_per_page: Option<u32>,
    text_page_ratio_threshold: Option<f32>,
    pages: Option<Vec<u32>>,
    password: Option<String>,
    profile: Option<WasmMarkdownProfile>,
    include_page_markers: Option<bool>,
    include_images: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WasmScanStrategy {
    Named(WasmNamedScanStrategy),
    Sample(WasmSampleStrategy),
    Pages(WasmPagesStrategy),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum WasmNamedScanStrategy {
    EarlyExit,
    Full,
}

#[derive(Debug, Deserialize)]
struct WasmSampleStrategy {
    sample: u32,
}

#[derive(Debug, Deserialize)]
struct WasmPagesStrategy {
    pages: Vec<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WasmMarkdownProfile {
    Fidelity,
    Compact,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WasmPageOcrReasons {
    page: u32,
    reasons: Vec<String>,
}

impl From<PageOcrReasons> for WasmPageOcrReasons {
    fn from(value: PageOcrReasons) -> Self {
        Self {
            page: value.page,
            reasons: value.reasons,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WasmLayoutComplexity {
    is_complex: bool,
    pages_with_tables: Vec<u32>,
    pages_with_columns: Vec<u32>,
}

impl From<LayoutComplexity> for WasmLayoutComplexity {
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
struct WasmPdfProcessResult {
    pdf_type: &'static str,
    markdown: Option<String>,
    page_count: u32,
    processing_time_ms: f64,
    pages_needing_ocr: Vec<u32>,
    ocr_reasons_by_page: Vec<WasmPageOcrReasons>,
    title: Option<String>,
    confidence: f64,
    layout: WasmLayoutComplexity,
    has_encoding_issues: bool,
}

impl From<PdfProcessResult> for WasmPdfProcessResult {
    fn from(value: PdfProcessResult) -> Self {
        Self {
            pdf_type: pdf_type_name(value.pdf_type),
            markdown: value.markdown,
            page_count: value.page_count,
            processing_time_ms: value.processing_time_ms as f64,
            pages_needing_ocr: value.pages_needing_ocr,
            ocr_reasons_by_page: value
                .ocr_reasons_by_page
                .into_iter()
                .map(Into::into)
                .collect(),
            title: value.title,
            confidence: value.confidence as f64,
            layout: value.layout.into(),
            has_encoding_issues: value.has_encoding_issues,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WasmPdfClassification {
    pdf_type: &'static str,
    page_count: u32,
    pages_needing_ocr: Vec<u32>,
    confidence: f64,
}

fn pdf_type_name(pdf_type: PdfType) -> &'static str {
    match pdf_type {
        PdfType::TextBased => "TextBased",
        PdfType::Scanned => "Scanned",
        PdfType::ImageBased => "ImageBased",
        PdfType::Mixed => "Mixed",
    }
}

fn js_error(context: &str, error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&format!("{context}: {error}")).into()
}

fn validate_object_fields(
    value: &JsValue,
    allowed_fields: &[&str],
    object_name: &str,
) -> Result<(), JsValue> {
    // serde-wasm-bindgen reads the fields named by the Rust struct but does
    // not enumerate other JavaScript object keys, so Serde's
    // deny_unknown_fields cannot catch them by itself.
    value.dyn_ref::<js_sys::Object>().ok_or_else(|| {
        js_error(
            "invalid options",
            format!("{object_name} must be an object"),
        )
    })?;

    let keys = js_sys::Reflect::own_keys(value).map_err(|_| {
        js_error(
            "invalid options",
            format!("could not inspect {object_name} fields"),
        )
    })?;
    for key in keys.iter() {
        let Some(key) = key.as_string() else {
            return Err(js_error(
                "invalid options",
                format!("{object_name} fields must use string keys"),
            ));
        };
        if !allowed_fields.contains(&key.as_str()) {
            return Err(js_error(
                "invalid options",
                format!("unknown {object_name} field `{key}`"),
            ));
        }
    }

    Ok(())
}

fn validate_strategy_fields(value: &JsValue) -> Result<(), JsValue> {
    if value.is_undefined() || value.is_null() || value.as_string().is_some() {
        return Ok(());
    }

    value.dyn_ref::<js_sys::Object>().ok_or_else(|| {
        js_error(
            "invalid options",
            "strategy must be \"earlyExit\", \"full\", { sample: number }, or { pages: number[] }",
        )
    })?;
    let keys = js_sys::Reflect::own_keys(value)
        .map_err(|_| js_error("invalid options", "could not inspect strategy fields"))?;
    if keys.length() != 1 {
        return Err(js_error(
            "invalid options",
            "strategy objects must contain exactly one of `sample` or `pages`",
        ));
    }

    let key = keys
        .get(0)
        .as_string()
        .ok_or_else(|| js_error("invalid options", "strategy fields must use string keys"))?;
    if key != "sample" && key != "pages" {
        return Err(js_error(
            "invalid options",
            format!("unknown strategy field `{key}`"),
        ));
    }

    Ok(())
}

fn validate_option_fields(value: &JsValue, mode: &ProcessMode) -> Result<(), JsValue> {
    const DETECTION_FIELDS: &[&str] = &[
        "strategy",
        "minTextOpsPerPage",
        "textPageRatioThreshold",
        "password",
    ];
    const PROCESS_FIELDS: &[&str] = &[
        "strategy",
        "minTextOpsPerPage",
        "textPageRatioThreshold",
        "password",
        "pages",
        "profile",
        "includePageMarkers",
        "includeImages",
    ];

    let allowed_fields = match mode {
        ProcessMode::Full => PROCESS_FIELDS,
        ProcessMode::DetectOnly => DETECTION_FIELDS,
        ProcessMode::Analyze => PROCESS_FIELDS,
    };
    validate_object_fields(value, allowed_fields, "option")?;

    let strategy = js_sys::Reflect::get(value, &JsValue::from_str("strategy"))
        .map_err(|_| js_error("invalid options", "could not read `strategy`"))?;
    validate_strategy_fields(&strategy)
}

fn deserialize_options(value: JsValue, mode: &ProcessMode) -> Result<WasmProcessOptions, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(WasmProcessOptions::default());
    }

    validate_option_fields(&value, mode)?;
    serde_wasm_bindgen::from_value(value).map_err(|error| js_error("invalid options", error))
}

fn build_options(value: JsValue, mode: ProcessMode) -> Result<PdfOptions, JsValue> {
    let options = deserialize_options(value, &mode)?;
    if options
        .pages
        .as_ref()
        .is_some_and(|pages| pages.contains(&0))
    {
        return Err(js_error(
            "invalid options",
            "pages are 1-indexed; page 0 is invalid",
        ));
    }

    let mut detection = DetectionConfig::default();
    if let Some(strategy) = options.strategy {
        detection.strategy = match strategy {
            WasmScanStrategy::Named(WasmNamedScanStrategy::EarlyExit) => ScanStrategy::EarlyExit,
            WasmScanStrategy::Named(WasmNamedScanStrategy::Full) => ScanStrategy::Full,
            WasmScanStrategy::Sample(WasmSampleStrategy { sample }) => {
                if sample == 0 {
                    return Err(js_error(
                        "invalid options",
                        "strategy.sample must be at least 1",
                    ));
                }
                ScanStrategy::Sample(sample)
            }
            WasmScanStrategy::Pages(WasmPagesStrategy { pages }) => {
                if pages.is_empty() {
                    return Err(js_error(
                        "invalid options",
                        "strategy.pages must not be empty",
                    ));
                }
                if pages.contains(&0) {
                    return Err(js_error(
                        "invalid options",
                        "strategy.pages are 1-indexed; page 0 is invalid",
                    ));
                }
                ScanStrategy::Pages(pages)
            }
        };
    }
    if let Some(min_text_ops_per_page) = options.min_text_ops_per_page {
        if min_text_ops_per_page == 0 {
            return Err(js_error(
                "invalid options",
                "minTextOpsPerPage must be at least 1",
            ));
        }
        detection.min_text_ops_per_page = min_text_ops_per_page;
    }
    if let Some(text_page_ratio_threshold) = options.text_page_ratio_threshold {
        if !text_page_ratio_threshold.is_finite()
            || !(0.0..=1.0).contains(&text_page_ratio_threshold)
        {
            return Err(js_error(
                "invalid options",
                "textPageRatioThreshold must be between 0.0 and 1.0",
            ));
        }
        detection.text_page_ratio_threshold = text_page_ratio_threshold;
    }

    let mut result = PdfOptions::new().mode(mode);
    result = result.detection(detection);
    if let Some(pages) = options.pages {
        result = result.pages(pages);
    }
    if let Some(password) = options.password {
        result = result.password(password);
    }
    if let Some(profile) = options.profile {
        result.markdown.profile = match profile {
            WasmMarkdownProfile::Fidelity => MarkdownProfile::Fidelity,
            WasmMarkdownProfile::Compact => MarkdownProfile::Compact,
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

fn serialize<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|error| js_error("serialize result", error))
}

fn initialize() {
    console_error_panic_hook::set_once();
}

/// Process PDF bytes entirely inside WebAssembly.
#[wasm_bindgen(js_name = processPdf, skip_typescript)]
pub fn process_pdf(data: &[u8], options: JsValue) -> Result<JsValue, JsValue> {
    initialize();
    let options = build_options(options, ProcessMode::Full)?;
    let started = js_sys::Date::now();
    let mut result = pdf_inspector::process_pdf_mem_with_options(data, options)
        .map_err(|error| js_error("process PDF", error))?;
    result.processing_time_ms = (js_sys::Date::now() - started).max(0.0) as u64;
    serialize(&WasmPdfProcessResult::from(result))
}

/// Classify PDF bytes without extracting text or producing Markdown.
#[wasm_bindgen(js_name = detectPdf, skip_typescript)]
pub fn detect_pdf(data: &[u8], options: JsValue) -> Result<JsValue, JsValue> {
    initialize();
    let options = build_options(options, ProcessMode::DetectOnly)?;
    let started = js_sys::Date::now();
    let mut result = pdf_inspector::process_pdf_mem_with_options(data, options)
        .map_err(|error| js_error("detect PDF", error))?;
    result.processing_time_ms = (js_sys::Date::now() - started).max(0.0) as u64;
    serialize(&WasmPdfProcessResult::from(result))
}

/// Return the lightweight classification shape used by the native Node API.
#[wasm_bindgen(js_name = classifyPdf, skip_typescript)]
pub fn classify_pdf(data: &[u8], options: JsValue) -> Result<JsValue, JsValue> {
    initialize();
    let options = build_options(options, ProcessMode::DetectOnly)?;
    let result = pdf_inspector::process_pdf_mem_with_options(data, options)
        .map_err(|error| js_error("classify PDF", error))?;
    serialize(&WasmPdfClassification {
        pdf_type: pdf_type_name(result.pdf_type),
        page_count: result.page_count,
        pages_needing_ocr: result
            .pages_needing_ocr
            .into_iter()
            .map(|page| page - 1)
            .collect(),
        confidence: result.confidence as f64,
    })
}

/// Extract plain text from PDF bytes without Markdown conversion.
#[wasm_bindgen(js_name = extractText, skip_typescript)]
pub fn extract_text(data: &[u8]) -> Result<String, JsValue> {
    initialize();
    let items = pdf_inspector::extractor::extract_text_with_positions_mem(data)
        .map_err(|error| js_error("extract text", error))?;
    Ok(
        pdf_inspector::extractor::group_into_lines_preserving_all_text(items)
            .into_iter()
            .map(|line| line.text())
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Return the WebAssembly package version.
#[wasm_bindgen(skip_typescript)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use js_sys::Reflect;
    use wasm_bindgen_test::*;

    const TEXT_PDF: &[u8] = include_bytes!("../../tests/fixtures/thermo-freon12.pdf");
    const ENCRYPTED_PDF: &[u8] = include_bytes!("../../tests/fixtures/encrypted-secret123.pdf");

    fn string_property(value: &JsValue, name: &str) -> String {
        Reflect::get(value, &JsValue::from_str(name))
            .unwrap_or_else(|_| panic!("read {name}"))
            .as_string()
            .unwrap_or_else(|| panic!("{name} string"))
    }

    fn error_message(error: &JsValue) -> String {
        Reflect::get(error, &JsValue::from_str("message"))
            .expect("read error message")
            .as_string()
            .expect("error message string")
    }

    fn define_non_enumerable_property(object: &js_sys::Object, name: &str, value: &JsValue) {
        let descriptor = js_sys::Object::new();
        Reflect::set(&descriptor, &JsValue::from_str("value"), value)
            .expect("set descriptor value");
        Reflect::set(
            &descriptor,
            &JsValue::from_str("enumerable"),
            &JsValue::FALSE,
        )
        .expect("set descriptor enumerable");
        js_sys::Object::define_property(object, &JsValue::from_str(name), &descriptor);
    }

    fn synthetic_korea1_pdf() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];

        fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &str) {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body.as_bytes());
            pdf.extend_from_slice(b"\nendobj\n");
        }

        add_object(
            &mut pdf,
            &mut offsets,
            1,
            "<< /Type /Catalog /Pages 2 0 R >>",
        );
        add_object(
            &mut pdf,
            &mut offsets,
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        );
        add_object(
            &mut pdf,
            &mut offsets,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );

        // Adobe-Korea1 CID 1086 (0x043E) maps to U+AC00 (Korean syllable GA).
        // There is deliberately no ToUnicode stream: decoding must use the
        // embedded predefined CMap rather than lopdf's plain-text fallback.
        // Korea1 CIDs 21 and 19 map to ASCII "4" and "2". Place them near
        // the bottom edge so they look exactly like a numeric page footer.
        let content = "BT /F0 12 Tf 50 100 Td <043E> Tj 0 -60 Td <00150013> Tj ET";
        add_object(
            &mut pdf,
            &mut offsets,
            4,
            &format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
        );
        add_object(
            &mut pdf,
            &mut offsets,
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /SyntheticKorea1 /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
        );
        add_object(
            &mut pdf,
            &mut offsets,
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /SyntheticKorea1 /CIDSystemInfo << /Registry (Adobe) /Ordering (Korea1) /Supplement 2 >> /FontDescriptor 7 0 R /DW 1000 >>",
        );
        add_object(
            &mut pdf,
            &mut offsets,
            7,
            "<< /Type /FontDescriptor /FontName /SyntheticKorea1 /Flags 4 /FontBBox [-100 -200 1000 900] /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 >>",
        );

        let xref_start = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                offsets.len(),
                xref_start
            )
            .as_bytes(),
        );
        pdf
    }

    /// A 20-page document where the eight pages selected by the default
    /// Sample(8) strategy are text, while every other page is image-backed.
    fn synthetic_heterogeneous_pdf() -> Vec<u8> {
        const PAGE_COUNT: usize = 20;
        const TEXT_CONTENT_ID: usize = 23;
        const IMAGE_CONTENT_ID: usize = 24;
        const FONT_ID: usize = 25;
        const IMAGE_ID: usize = 26;

        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];

        fn add_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &str) {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body.as_bytes());
            pdf.extend_from_slice(b"\nendobj\n");
        }

        add_object(
            &mut pdf,
            &mut offsets,
            1,
            "<< /Type /Catalog /Pages 2 0 R >>",
        );
        let kids = (3..3 + PAGE_COUNT)
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        add_object(
            &mut pdf,
            &mut offsets,
            2,
            &format!("<< /Type /Pages /Kids [{kids}] /Count {PAGE_COUNT} >>"),
        );

        // distribute_pages(8, 20) selects exactly these page numbers.
        let default_sample = [1usize, 3, 5, 7, 9, 11, 13, 20];
        for page in 1..=PAGE_COUNT {
            let page_id = page + 2;
            let body = if default_sample.contains(&page) {
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                     /Resources << /Font << /F1 {FONT_ID} 0 R >> >> \
                     /Contents {TEXT_CONTENT_ID} 0 R >>"
                )
            } else {
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                     /Resources << /XObject << /Im0 {IMAGE_ID} 0 R >> >> \
                     /Contents {IMAGE_CONTENT_ID} 0 R >>"
                )
            };
            add_object(&mut pdf, &mut offsets, page_id, &body);
        }

        let text_content =
            "BT /F1 12 Tf 10 100 Td (Hello World) Tj (More Text) Tj (Sample Page) Tj ET";
        add_object(
            &mut pdf,
            &mut offsets,
            TEXT_CONTENT_ID,
            &format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                text_content.len(),
                text_content
            ),
        );
        let image_content = "/Im0 Do";
        add_object(
            &mut pdf,
            &mut offsets,
            IMAGE_CONTENT_ID,
            &format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                image_content.len(),
                image_content
            ),
        );
        add_object(
            &mut pdf,
            &mut offsets,
            FONT_ID,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        add_object(
            &mut pdf,
            &mut offsets,
            IMAGE_ID,
            "<< /Type /XObject /Subtype /Image /Width 1000 /Height 1000 \
             /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n0\nendstream",
        );

        let xref_start = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                offsets.len(),
                xref_start
            )
            .as_bytes(),
        );
        pdf
    }

    #[wasm_bindgen_test]
    fn processes_pdf_to_markdown() {
        let result = process_pdf(TEXT_PDF, JsValue::UNDEFINED).expect("process PDF");
        let pdf_type = string_property(&result, "pdfType");
        let markdown = Reflect::get(&result, &JsValue::from_str("markdown"))
            .expect("markdown")
            .as_string()
            .expect("markdown string");

        assert_eq!(pdf_type, "TextBased");
        assert!(!markdown.is_empty());
    }

    #[wasm_bindgen_test]
    fn rejects_non_pdf_bytes() {
        assert!(process_pdf(b"not a PDF", JsValue::UNDEFINED).is_err());
    }

    #[wasm_bindgen_test]
    fn classifies_and_extracts_plain_text() {
        let classification = classify_pdf(TEXT_PDF, JsValue::UNDEFINED).expect("classify PDF");
        let pdf_type = string_property(&classification, "pdfType");
        let text = extract_text(TEXT_PDF).expect("extract text");

        assert_eq!(pdf_type, "TextBased");
        assert!(!text.is_empty());
    }

    #[wasm_bindgen_test]
    fn extracts_cjk_and_preserves_numeric_page_footer() {
        let text = extract_text(&synthetic_korea1_pdf()).expect("extract predefined CMap text");

        assert_eq!(text, "가\n42");
    }

    #[wasm_bindgen_test]
    fn opens_encrypted_pdf_with_password() {
        assert!(process_pdf(ENCRYPTED_PDF, JsValue::UNDEFINED).is_err());

        let options = js_sys::Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("password"),
            &JsValue::from_str("secret123"),
        )
        .expect("set password");
        let result = process_pdf(ENCRYPTED_PDF, options.into()).expect("process encrypted PDF");
        let markdown = Reflect::get(&result, &JsValue::from_str("markdown"))
            .expect("markdown")
            .as_string()
            .expect("markdown string");

        assert!(!markdown.is_empty());
    }

    #[wasm_bindgen_test]
    fn full_strategy_scans_pages_missed_by_default_sample() {
        let pdf = synthetic_heterogeneous_pdf();
        let sampled = detect_pdf(&pdf, JsValue::UNDEFINED).expect("sampled detection");
        assert_eq!(string_property(&sampled, "pdfType"), "TextBased");

        let options = js_sys::Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("strategy"),
            &JsValue::from_str("full"),
        )
        .expect("set full strategy");

        let full = detect_pdf(&pdf, options.clone().into()).expect("full detection");
        assert_eq!(string_property(&full, "pdfType"), "Mixed");

        let classification = classify_pdf(&pdf, options.into()).expect("full classification");
        assert_eq!(string_property(&classification, "pdfType"), "Mixed");
        let pages = js_sys::Array::from(
            &Reflect::get(&classification, &JsValue::from_str("pagesNeedingOcr"))
                .expect("pagesNeedingOcr"),
        );
        assert!(
            pages.includes(&JsValue::from_f64(1.0), 0),
            "classifyPdf should report image-backed page 2 as zero-indexed page 1"
        );
    }

    #[wasm_bindgen_test]
    fn accepts_every_scan_strategy_variant() {
        let sample = js_sys::Object::new();
        Reflect::set(
            &sample,
            &JsValue::from_str("sample"),
            &JsValue::from_f64(1.0),
        )
        .expect("set sample count");

        let pages = js_sys::Object::new();
        Reflect::set(
            &pages,
            &JsValue::from_str("pages"),
            &js_sys::Array::of1(&JsValue::from_f64(1.0)),
        )
        .expect("set strategy pages");

        for strategy in [
            JsValue::from_str("earlyExit"),
            JsValue::from_str("full"),
            sample.into(),
            pages.into(),
        ] {
            let options = js_sys::Object::new();
            Reflect::set(&options, &JsValue::from_str("strategy"), &strategy)
                .expect("set strategy");
            detect_pdf(TEXT_PDF, options.into()).expect("supported strategy");
        }
    }

    #[wasm_bindgen_test]
    fn rejects_unknown_and_unsupported_detection_options() {
        let unknown = js_sys::Object::new();
        Reflect::set(&unknown, &JsValue::from_str("fullScan"), &JsValue::TRUE)
            .expect("set unknown option");
        let error = detect_pdf(TEXT_PDF, unknown.into()).expect_err("unknown option must fail");
        assert!(error_message(&error).contains("unknown option field `fullScan`"));

        let hidden = js_sys::Object::new();
        define_non_enumerable_property(&hidden, "hiddenOption", &JsValue::TRUE);
        let error =
            detect_pdf(TEXT_PDF, hidden.into()).expect_err("non-enumerable option must fail");
        assert!(error_message(&error).contains("unknown option field `hiddenOption`"));

        let symbol_keyed = js_sys::Object::new();
        Reflect::set(
            &symbol_keyed,
            &js_sys::Symbol::for_("unsupportedOption").into(),
            &JsValue::TRUE,
        )
        .expect("set symbol option");
        let error = detect_pdf(TEXT_PDF, symbol_keyed.into()).expect_err("symbol option must fail");
        assert!(error_message(&error).contains("option fields must use string keys"));

        let process_only = js_sys::Object::new();
        Reflect::set(
            &process_only,
            &JsValue::from_str("profile"),
            &JsValue::from_str("compact"),
        )
        .expect("set process-only option");
        let error = detect_pdf(TEXT_PDF, process_only.into())
            .expect_err("unsupported detection option must fail");
        assert!(error_message(&error).contains("unknown option field `profile`"));

        let malformed_strategy = js_sys::Object::new();
        Reflect::set(
            &malformed_strategy,
            &JsValue::from_str("sample"),
            &JsValue::from_f64(8.0),
        )
        .expect("set sample");
        define_non_enumerable_property(&malformed_strategy, "hiddenTypo", &JsValue::TRUE);
        let options = js_sys::Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("strategy"),
            &malformed_strategy,
        )
        .expect("set malformed strategy");
        let error = detect_pdf(TEXT_PDF, options.into()).expect_err("malformed strategy must fail");
        assert!(error_message(&error).contains("exactly one of `sample` or `pages`"));
    }

    #[wasm_bindgen_test]
    fn rejects_strategy_pages_when_none_are_in_range() {
        let pages = js_sys::Object::new();
        Reflect::set(
            &pages,
            &JsValue::from_str("pages"),
            &js_sys::Array::of1(&JsValue::from_f64(9999.0)),
        )
        .expect("set out-of-range pages");
        let options = js_sys::Object::new();
        Reflect::set(&options, &JsValue::from_str("strategy"), &pages).expect("set pages strategy");

        let error = detect_pdf(TEXT_PDF, options.into()).expect_err("out-of-range pages must fail");
        assert!(error_message(&error).contains("contains no in-range page numbers"));
    }
}
