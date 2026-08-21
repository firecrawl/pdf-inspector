//! JSON parameter shapes accepted by the C ABI functions that need more
//! than raw PDF bytes (see `go/include/pdf_inspector.h`). Every ABI
//! function that takes options accepts them as a single JSON string so the
//! ABI surface stays at "one function per operation" rather than growing a
//! new C parameter for every option any operation ever needs.

use serde::Deserialize;

/// Shared by every ABI call that supports an optional 0-indexed page
/// filter (`process_pdf`, `extract_pages_markdown`,
/// `extract_text_with_positions`, `extract_structure_elements`). Absent or
/// `null` means "every page, in document order" — matching the core
/// crate's own `Option<&[u32]>` convention.
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
