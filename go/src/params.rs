//! JSON parameter shapes accepted by the C ABI functions that need more
//! than raw PDF bytes (see `go/include/pdf_inspector.h`). Every ABI
//! function that takes options accepts them as a single JSON string so the
//! ABI surface stays at "one function per operation" rather than growing a
//! new C parameter for every option any operation ever needs.

use serde::Deserialize;

/// Shared by every ABI call that supports an optional page filter
/// (`process_pdf`, `extract_pages_markdown`, `extract_text_with_positions`,
/// `extract_structure_elements`). Absent or `null` means "every page, in
/// document order".
///
/// Indexing is **not** uniform across these four callers — each function
/// forwards the value straight to its own core API with no conversion, so
/// each one's indexing matches whichever core function it calls, not a
/// convention chosen by this ABI:
/// - `extract_pages_markdown`, `extract_text_in_regions`/`extract_tables_in_regions`'s
///   page fields: 0-indexed (`extract_pages_markdown_mem`'s own convention).
/// - `process_pdf`: 1-indexed (`PdfOptions::pages`'s convention, matching
///   Python's `process_pdf(path, pages=[1, 3, 5])` and napi's `processPdf`).
/// - `extract_text_with_positions`: 1-indexed, matching the 1-indexed
///   `TextItem.page` field the results carry (matches napi's tested
///   behavior: `extractTextWithPositions(buf, [1])` returns items with
///   `page === 1`).
/// - `extract_structure_elements`: 1-indexed, matching `TextItem.page` for
///   the same reason (see `extract_structure_elements_mem`'s own doc).
///
/// See each function's doc comment in `lib.rs` (and the Go-side function
/// doc comments in `pdfinspector.go`) for the specific convention.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct PagesParams {
    pub pages: Option<Vec<u32>>,
}

#[derive(Deserialize)]
pub(crate) struct PageRegionsEntry {
    pub page: u32,
    pub regions: Vec<[f32; 4]>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct PageRegionsParams {
    pub page_regions: Vec<PageRegionsEntry>,
}

pub(crate) fn into_page_regions(entries: Vec<PageRegionsEntry>) -> Vec<(u32, Vec<[f32; 4]>)> {
    entries.into_iter().map(|e| (e.page, e.regions)).collect()
}

#[derive(Deserialize)]
pub(crate) struct VectorGridParams {
    pub page_idx: u32,
    pub region_pdf_pt_bbox: [f32; 4],
    pub render_dpi: f32,
}

/// Mirrors `pdf_inspector::TsrTableInput`, minus the `From` impl a foreign
/// crate can't provide for a foreign type — see the `impl From` below
/// instead.
#[derive(Deserialize)]
pub(crate) struct TsrTableInput {
    pub page: u32,
    pub crop_pdf_pt_bbox: [f32; 4],
    pub render_dpi: f32,
    pub structure_tokens: Vec<String>,
    pub cell_bboxes: Vec<Vec<f32>>,
}

impl From<TsrTableInput> for pdf_inspector::TsrTableInput {
    fn from(i: TsrTableInput) -> Self {
        pdf_inspector::TsrTableInput {
            page: i.page,
            crop_pdf_pt_bbox: i.crop_pdf_pt_bbox,
            render_dpi: i.render_dpi,
            structure_tokens: i.structure_tokens,
            cell_bboxes: i.cell_bboxes,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct TsrInputsParams {
    pub inputs: Vec<TsrTableInput>,
}

/// Mirrors napi's `OcrOptions` object: every field optional, defaulting to
/// `OcrPdfOptions::auto()` the same way `to_core_ocr_options` does for the
/// Node binding. `mode` is a bare string (`"off" | "auto" | "force"`,
/// case-insensitive) rather than a JSON enum, matching how the rest of this
/// ABI represents strings-with-a-fixed-set-of-values (see `results.rs`'s
/// `item_type` field for the same convention).
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct OcrOptions {
    pub mode: Option<String>,
    pub page_numbers: Option<Vec<u32>>,
    pub password: Option<String>,
    pub dpi: Option<f32>,
    pub minimum_confidence: Option<f32>,
    pub hosted_recommendation_confidence: Option<f32>,
    pub model_directory: Option<String>,
    pub offline: Option<bool>,
}

impl OcrOptions {
    pub(crate) fn into_core(self) -> Result<pdf_inspector::vision::OcrPdfOptions, String> {
        let mut result = pdf_inspector::vision::OcrPdfOptions::auto();

        if let Some(mode) = self.mode {
            result.ocr.mode = match mode.to_ascii_lowercase().as_str() {
                "off" => pdf_inspector::vision::OcrMode::Off,
                "auto" => pdf_inspector::vision::OcrMode::Auto,
                "force" => pdf_inspector::vision::OcrMode::Force,
                other => return Err(format!("invalid mode {other:?}: want off, auto, or force")),
            };
        }
        if let Some(pages) = self.page_numbers {
            result = result.page_numbers(pages);
        }
        if let Some(password) = self.password {
            result = result.password(password);
        }
        if let Some(dpi) = self.dpi {
            result.render.dpi = dpi;
        }
        if let Some(minimum_confidence) = self.minimum_confidence {
            result.ocr.minimum_confidence = minimum_confidence;
        }
        if let Some(confidence) = self.hosted_recommendation_confidence {
            result.hosted_recommendation_confidence = confidence;
        }
        if let Some(directory) = self.model_directory {
            result.ocr.model_directory = Some(directory.into());
        }
        if self.offline.unwrap_or(false) {
            result.ocr.model_downloads = pdf_inspector::vision::ModelDownloadPolicy::Offline;
        }
        Ok(result)
    }
}
