//! JSON result shapes returned by the C ABI (see `go/include/pdf_inspector.h`).
//! Every exported function in `lib.rs` builds one of these, wraps it in an
//! `{"ok":true,...}` / `{"ok":false,"error":"..."}` envelope, and hands it
//! to `to_json_cstring`. Field names are `snake_case` to match the core
//! crate and the Python binding, rather than napi's `camelCase` — there is
//! no single "native" casing to inherit here, so this picks the one Go's
//! own `encoding/json` struct tags (also written by hand) read most
//! naturally against.

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum PdfType {
    TextBased,
    Scanned,
    ImageBased,
    Mixed,
}

impl From<pdf_inspector::PdfType> for PdfType {
    fn from(t: pdf_inspector::PdfType) -> Self {
        match t {
            pdf_inspector::PdfType::TextBased => PdfType::TextBased,
            pdf_inspector::PdfType::Scanned => PdfType::Scanned,
            pdf_inspector::PdfType::ImageBased => PdfType::ImageBased,
            pdf_inspector::PdfType::Mixed => PdfType::Mixed,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PageOcrReasons {
    pub page: u32,
    pub reasons: Vec<String>,
}

impl From<pdf_inspector::PageOcrReasons> for PageOcrReasons {
    fn from(r: pdf_inspector::PageOcrReasons) -> Self {
        Self {
            page: r.page,
            reasons: r.reasons,
        }
    }
}

fn page_ocr_reasons(v: Vec<pdf_inspector::PageOcrReasons>) -> Vec<PageOcrReasons> {
    v.into_iter().map(Into::into).collect()
}

#[derive(Serialize)]
pub(crate) struct Classification {
    pub pdf_type: PdfType,
    pub page_count: u32,
    /// 0-indexed, matching `classify_pdf_mem`'s caller-convenience convention.
    pub pages_needing_ocr: Vec<u32>,
    pub confidence: f32,
}

impl From<pdf_inspector::PdfClassification> for Classification {
    fn from(c: pdf_inspector::PdfClassification) -> Self {
        Self {
            pdf_type: c.pdf_type.into(),
            page_count: c.page_count,
            pages_needing_ocr: c.pages_needing_ocr,
            confidence: c.confidence,
        }
    }
}

/// Full processing result: matches napi's `PdfResult` / Python's
/// `PdfResult` field-for-field (modulo casing).
#[derive(Serialize)]
pub(crate) struct PdfResult {
    pub pdf_type: PdfType,
    pub markdown: Option<String>,
    pub page_count: u32,
    pub processing_time_ms: u64,
    /// 1-indexed, matching the core crate's convention for this type
    /// (unlike `Classification.pages_needing_ocr`, which is 0-indexed).
    pub pages_needing_ocr: Vec<u32>,
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    pub title: Option<String>,
    pub confidence: f32,
    pub is_complex_layout: bool,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub has_encoding_issues: bool,
}

impl From<pdf_inspector::PdfProcessResult> for PdfResult {
    fn from(r: pdf_inspector::PdfProcessResult) -> Self {
        Self {
            pdf_type: r.pdf_type.into(),
            markdown: r.markdown,
            page_count: r.page_count,
            processing_time_ms: r.processing_time_ms,
            pages_needing_ocr: r.pages_needing_ocr,
            ocr_reasons_by_page: page_ocr_reasons(r.ocr_reasons_by_page),
            title: r.title,
            confidence: r.confidence,
            is_complex_layout: r.layout.is_complex,
            pages_with_tables: r.layout.pages_with_tables,
            pages_with_columns: r.layout.pages_with_columns,
            has_encoding_issues: r.has_encoding_issues,
        }
    }
}

fn item_type_parts(t: &pdf_inspector::types::ItemType) -> (&'static str, Option<String>) {
    use pdf_inspector::types::ItemType;
    match t {
        ItemType::Text => ("text", None),
        ItemType::Image => ("image", None),
        ItemType::Link(url) => ("link", Some(url.clone())),
        ItemType::FormField => ("form_field", None),
    }
}

#[derive(Serialize)]
pub(crate) struct TextItem {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub font: String,
    pub font_size: f32,
    /// 1-indexed.
    pub page: u32,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_underline: bool,
    pub is_strikeout: bool,
    pub item_type: &'static str,
    /// URL for `item_type == "link"`, `null` otherwise.
    pub link_url: Option<String>,
    /// Marked Content ID linking this item to a tagged PDF's structure
    /// tree, when present.
    pub mcid: Option<i64>,
}

impl From<pdf_inspector::TextItem> for TextItem {
    fn from(item: pdf_inspector::TextItem) -> Self {
        let (item_type, link_url) = item_type_parts(&item.item_type);
        Self {
            text: item.text,
            x: item.x,
            y: item.y,
            width: item.width,
            height: item.height,
            font: item.font,
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            is_underline: item.is_underline,
            is_strikeout: item.is_strikeout,
            item_type,
            link_url,
            mcid: item.mcid,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct StructureElement {
    pub page: u32,
    pub mcid: i64,
    pub role: String,
}

impl From<pdf_inspector::StructureElement> for StructureElement {
    fn from(e: pdf_inspector::StructureElement) -> Self {
        Self {
            page: e.page,
            mcid: e.mcid,
            role: e.role,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PageMarkdown {
    pub page: u32,
    pub markdown: String,
    pub needs_ocr: bool,
    pub ocr_reason: Option<String>,
}

impl From<pdf_inspector::PageMarkdown> for PageMarkdown {
    fn from(p: pdf_inspector::PageMarkdown) -> Self {
        Self {
            page: p.page,
            markdown: p.markdown,
            needs_ocr: p.needs_ocr,
            ocr_reason: p.ocr_reason,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PagesExtractionResult {
    pub pages: Vec<PageMarkdown>,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub pages_needing_ocr: Vec<u32>,
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    pub is_complex: bool,
}

impl From<pdf_inspector::PagesExtractionResult> for PagesExtractionResult {
    fn from(r: pdf_inspector::PagesExtractionResult) -> Self {
        Self {
            pages: r.pages.into_iter().map(Into::into).collect(),
            pages_with_tables: r.pages_with_tables,
            pages_with_columns: r.pages_with_columns,
            pages_needing_ocr: r.pages_needing_ocr,
            ocr_reasons_by_page: page_ocr_reasons(r.ocr_reasons_by_page),
            is_complex: r.is_complex,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RegionText {
    pub text: String,
    pub needs_ocr: bool,
    pub ocr_reason: Option<String>,
}

impl From<pdf_inspector::RegionText> for RegionText {
    fn from(r: pdf_inspector::RegionText) -> Self {
        Self {
            text: r.text,
            needs_ocr: r.needs_ocr,
            ocr_reason: r.ocr_reason,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PageRegionTexts {
    pub page: u32,
    pub regions: Vec<RegionText>,
}

impl From<pdf_inspector::PageRegionResult> for PageRegionTexts {
    fn from(r: pdf_inspector::PageRegionResult) -> Self {
        Self {
            page: r.page,
            regions: r.regions.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct VectorGridDetection {
    pub structure_tokens: Vec<String>,
    pub cell_bboxes: Vec<Vec<f32>>,
}

impl From<pdf_inspector::VectorGridDetection> for VectorGridDetection {
    fn from(r: pdf_inspector::VectorGridDetection) -> Self {
        Self {
            structure_tokens: r.structure_tokens,
            cell_bboxes: r.cell_bboxes,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct StructuredCell {
    pub row: usize,
    pub col: usize,
    pub rowspan: usize,
    pub colspan: usize,
    pub is_header: bool,
    pub text: String,
    pub page_pt_bbox: [f32; 4],
}

impl From<pdf_inspector::tables::structured::StructuredCell> for StructuredCell {
    fn from(c: pdf_inspector::tables::structured::StructuredCell) -> Self {
        Self {
            row: c.row,
            col: c.col,
            rowspan: c.rowspan,
            colspan: c.colspan,
            is_header: c.is_header,
            text: c.text,
            page_pt_bbox: c.page_pt_bbox,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct OcrModelIdentity {
    pub name: String,
    pub revision: String,
}

impl From<pdf_inspector::vision::ModelIdentity> for OcrModelIdentity {
    fn from(m: pdf_inspector::vision::ModelIdentity) -> Self {
        Self {
            name: m.name,
            revision: m.revision,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct OcrTimings {
    pub render_ms: u64,
    pub ocr_ms: u64,
    pub assembly_ms: u64,
}

impl From<pdf_inspector::vision::VisionTimings> for OcrTimings {
    fn from(t: pdf_inspector::vision::VisionTimings) -> Self {
        Self {
            render_ms: t.render_ms,
            ocr_ms: t.ocr_ms,
            assembly_ms: t.assembly_ms,
        }
    }
}

fn page_content_source_str(s: pdf_inspector::vision::PageContentSource) -> &'static str {
    use pdf_inspector::vision::PageContentSource;
    match s {
        PageContentSource::Native => "native",
        PageContentSource::Ocr => "ocr",
        PageContentSource::Fused => "fused",
        // `#[non_exhaustive]`, mirroring napi's `to_napi`/`convert_page_content_source`
        // fallback: an unrecognized future variant degrades to "native" rather
        // than panicking or silently miscompiling.
        _ => "native",
    }
}

#[derive(Serialize)]
pub(crate) struct OcrPageProvenance {
    pub page_number: u32,
    pub source: &'static str,
    pub ocr_model: Option<OcrModelIdentity>,
    pub render_dpi: Option<f32>,
    pub ocr_confidence: Option<f32>,
    pub timings: OcrTimings,
    pub warnings: Vec<String>,
    pub hosted_recommended: bool,
}

impl From<pdf_inspector::vision::PageProvenance> for OcrPageProvenance {
    fn from(p: pdf_inspector::vision::PageProvenance) -> Self {
        Self {
            page_number: p.page_number,
            source: page_content_source_str(p.source),
            ocr_model: p.ocr_model.map(Into::into),
            render_dpi: p.render_dpi,
            ocr_confidence: p.ocr_confidence,
            timings: p.timings.into(),
            warnings: p.warnings,
            hosted_recommended: p.hosted_recommended,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct OcrPageResult {
    pub page_number: u32,
    pub markdown: String,
    pub provenance: OcrPageProvenance,
}

impl From<pdf_inspector::vision::FusedPageMarkdown> for OcrPageResult {
    fn from(p: pdf_inspector::vision::FusedPageMarkdown) -> Self {
        Self {
            page_number: p.page_number,
            markdown: p.markdown,
            provenance: p.provenance.into(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct OcrPdfResult {
    pub markdown: String,
    pub pages: Vec<OcrPageResult>,
    pub page_count: u32,
    pub pages_recommended_for_ocr: Vec<u32>,
    pub pages_routed_to_ocr: Vec<u32>,
    pub pages_recommending_hosted: Vec<u32>,
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub is_complex: bool,
    pub processing_time_ms: u64,
    pub render_time_ms: u64,
    pub ocr_time_ms: u64,
}

impl From<pdf_inspector::vision::OcrPdfResult> for OcrPdfResult {
    fn from(r: pdf_inspector::vision::OcrPdfResult) -> Self {
        Self {
            markdown: r.markdown,
            pages: r.pages.into_iter().map(Into::into).collect(),
            page_count: r.page_count,
            pages_recommended_for_ocr: r.pages_recommended_for_ocr,
            pages_routed_to_ocr: r.pages_routed_to_ocr,
            pages_recommending_hosted: r.pages_recommending_hosted,
            ocr_reasons_by_page: page_ocr_reasons(r.ocr_reasons_by_page),
            pages_with_tables: r.pages_with_tables,
            pages_with_columns: r.pages_with_columns,
            is_complex: r.is_complex,
            processing_time_ms: r.processing_time_ms,
            render_time_ms: r.render_time_ms,
            ocr_time_ms: r.ocr_time_ms,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TableExtractionResult {
    pub markdown: String,
    pub fallback_reason: Option<String>,
}

impl From<pdf_inspector::TableExtractionResult> for TableExtractionResult {
    fn from(r: pdf_inspector::TableExtractionResult) -> Self {
        Self {
            markdown: r.markdown,
            fallback_reason: r.fallback_reason,
        }
    }
}

// ---------------------------------------------------------------------------
// Envelopes: one per ABI function, always `{"ok":bool, ...}`.
// ---------------------------------------------------------------------------

macro_rules! envelope {
    ($name:ident, $field:ident: $ty:ty) => {
        #[derive(Serialize)]
        pub(crate) struct $name {
            pub ok: bool,
            pub $field: Option<$ty>,
            pub error: Option<String>,
        }

        impl $name {
            pub(crate) fn ok(value: $ty) -> Self {
                Self {
                    ok: true,
                    $field: Some(value),
                    error: None,
                }
            }

            pub(crate) fn err(message: impl Into<String>) -> Self {
                Self {
                    ok: false,
                    $field: None,
                    error: Some(message.into()),
                }
            }
        }
    };
}

envelope!(ClassifyEnvelope, result: Classification);
envelope!(TextEnvelope, text: String);
envelope!(PdfResultEnvelope, result: PdfResult);
envelope!(TextItemsEnvelope, items: Vec<TextItem>);
envelope!(StructureElementsEnvelope, elements: Vec<StructureElement>);
envelope!(PagesExtractionEnvelope, result: PagesExtractionResult);
envelope!(PageRegionTextsEnvelope, results: Vec<PageRegionTexts>);
envelope!(MarkdownStringsEnvelope, results: Vec<String>);
envelope!(StructuredCellsEnvelope, results: Vec<Vec<StructuredCell>>);
envelope!(TableExtractionEnvelope, results: Vec<TableExtractionResult>);
envelope!(OcrPdfEnvelope, result: OcrPdfResult);

/// Hand-written rather than generated by the `envelope!` macro: the "no
/// grid found" case is a *successful* `None`, distinct from the "call
/// failed" case — collapsing both into one `Option<Option<T>>` field would
/// serialize identically (`"result": null` either way) and rely entirely
/// on `ok`/`error` to disambiguate, which is easy to misread on the Go
/// side. Splitting `found` out makes the three states unambiguous in the
/// JSON itself: `{ok:true, found:true, result:{...}}`,
/// `{ok:true, found:false, result:null}`, `{ok:false, error:"..."}`.
#[derive(Serialize)]
pub(crate) struct VectorGridEnvelope {
    pub ok: bool,
    pub found: bool,
    pub result: Option<VectorGridDetection>,
    pub error: Option<String>,
}

impl VectorGridEnvelope {
    pub(crate) fn ok(value: Option<VectorGridDetection>) -> Self {
        Self {
            ok: true,
            found: value.is_some(),
            result: value,
            error: None,
        }
    }

    pub(crate) fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            found: false,
            result: None,
            error: Some(message.into()),
        }
    }
}
