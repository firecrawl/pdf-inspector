use pdf_inspector::{
    LayoutComplexity, MarkdownProfile, PageOcrReasons, PdfOptions, PdfProcessResult, PdfType,
    ProcessMode,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT_TYPES: &str = r#"
export type PdfType = "TextBased" | "Scanned" | "ImageBased" | "Mixed";
export type MarkdownProfile = "fidelity" | "compact";

export interface ProcessOptions {
  /** Restrict extraction to these 1-indexed page numbers. */
  pages?: number[];
  /** Password for an encrypted PDF. */
  password?: string;
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
export function detectPdf(data: Uint8Array, options?: Pick<ProcessOptions, "password">): PdfProcessResult;
export function classifyPdf(data: Uint8Array): PdfClassification;
export function extractText(data: Uint8Array): string;
export function version(): string;
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct WasmProcessOptions {
    pages: Option<Vec<u32>>,
    password: Option<String>,
    profile: Option<WasmMarkdownProfile>,
    include_page_markers: Option<bool>,
    include_images: Option<bool>,
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

fn deserialize_options(value: JsValue) -> Result<WasmProcessOptions, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(WasmProcessOptions::default());
    }

    serde_wasm_bindgen::from_value(value).map_err(|error| js_error("invalid options", error))
}

fn build_options(value: JsValue, mode: ProcessMode) -> Result<PdfOptions, JsValue> {
    let options = deserialize_options(value)?;
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

    let mut result = PdfOptions::new().mode(mode);
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
pub fn classify_pdf(data: &[u8]) -> Result<JsValue, JsValue> {
    initialize();
    let result =
        pdf_inspector::classify_pdf_mem(data).map_err(|error| js_error("classify PDF", error))?;
    serialize(&WasmPdfClassification {
        pdf_type: pdf_type_name(result.pdf_type),
        page_count: result.page_count,
        pages_needing_ocr: result.pages_needing_ocr,
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

    #[wasm_bindgen_test]
    fn processes_pdf_to_markdown() {
        let result = process_pdf(TEXT_PDF, JsValue::UNDEFINED).expect("process PDF");
        let pdf_type = Reflect::get(&result, &JsValue::from_str("pdfType"))
            .expect("pdfType")
            .as_string()
            .expect("pdfType string");
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
        let classification = classify_pdf(TEXT_PDF).expect("classify PDF");
        let pdf_type = Reflect::get(&classification, &JsValue::from_str("pdfType"))
            .expect("pdfType")
            .as_string()
            .expect("pdfType string");
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
}
