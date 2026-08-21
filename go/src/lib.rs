//! C ABI shim for the Go binding (see `go/pdfinspector`).
//!
//! Mirrors the `napi/` crate's approach (result structs + manual
//! conversion, panic-catching around every entry point) but returns a
//! single JSON-encoded envelope per call instead of mapping fields into a
//! language-native object graph, since plain C has no equivalent of NAPI's
//! object marshaling. Every exported function returns an owned,
//! NUL-terminated C string that the caller must release with
//! `pdfinspector_free_string` — that pairing is the entire
//! memory-ownership contract of this ABI.
//!
//! Two argument shapes cover every operation:
//! - `(data, len)` for operations with no options (`classify`, `extract_text`).
//! - `(data, len, params_json)` for everything else, where `params_json` is
//!   a UTF-8, NUL-terminated JSON string (see `params.rs` for the accepted
//!   shape per function; a null pointer means "use every default").
//!
//! This keeps the ABI at "one C function per operation" — no per-option C
//! parameters to add or reorder later — at the cost of a JSON encode/decode
//! per call, negligible next to PDF parsing itself. See `results.rs` for
//! the mirrored envelope/result shapes and `params.rs` for the request
//! shapes.
//!
//! Scope: this now covers the same document-processing surface as the
//! `napi`/Python bindings (process/detect/classify, per-page markdown,
//! positioned text, structure-tree elements, region-based extraction, and
//! TSR-hybrid table structure recovery). It does **not** cover OCR
//! (`vision`/`process_pdf_with_ocr`): that feature loads PDFium and an ONNX
//! Runtime backend dynamically at runtime rather than bundling them, which
//! is a meaningfully larger distribution surface for a cgo binding (see
//! go/README.md's "Scope" section) and is left as a deliberate follow-up.

mod params;
mod results;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic;

use serde::de::DeserializeOwned;
use serde::Serialize;

use params::{PageRegionsParams, PagesParams, TsrInputsParams, VectorGridParams};
use results::{
    ClassifyEnvelope, MarkdownStringsEnvelope, PageRegionTextsEnvelope, PagesExtractionEnvelope,
    PdfResultEnvelope, StructureElementsEnvelope, StructuredCellsEnvelope, TableExtractionEnvelope,
    TextEnvelope, TextItemsEnvelope, VectorGridEnvelope,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run `f`, catching any Rust panic so it can't unwind across the FFI
/// boundary (unwinding into C/Go frames is undefined behavior). Mirrors
/// `napi/src/lib.rs`'s `catch_panic`.
fn catch_panic<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + panic::UnwindSafe,
{
    match panic::catch_unwind(f) {
        Ok(result) => result,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(format!("Rust panic: {msg}"))
        }
    }
}

fn to_json_cstring<T: Serialize>(value: &T) -> *mut c_char {
    // Serializing our own fixed-shape structs to JSON cannot fail, and none
    // of our fields can contain an interior NUL (JSON strings never do), so
    // both unwraps below are infallible in practice.
    let json = serde_json::to_string(value).expect("envelope serialization is infallible");
    CString::new(json)
        .expect("JSON output never contains an interior NUL")
        .into_raw()
}

/// Reconstruct a byte slice from a caller-owned buffer.
///
/// # Safety
/// `data` must be valid for reads of `len` bytes, or `len` must be 0.
unsafe fn slice_from_raw<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len)
    }
}

/// Parse a nullable, NUL-terminated JSON parameter string. A null pointer
/// deserializes as an empty JSON object (`{}`) so every params type's
/// `#[serde(default)]`/required fields decide what "no params given" means
/// on a per-function basis.
///
/// # Safety
/// `params_json`, if non-null, must point to a valid, NUL-terminated,
/// UTF-8 C string.
unsafe fn parse_params<T: DeserializeOwned>(params_json: *const c_char) -> Result<T, String> {
    let json = if params_json.is_null() {
        "{}".to_string()
    } else {
        CStr::from_ptr(params_json)
            .to_str()
            .map_err(|e| format!("params_json is not valid UTF-8: {e}"))?
            .to_string()
    };
    serde_json::from_str(&json).map_err(|e| format!("invalid params_json: {e}"))
}

fn page_set(pages: Option<Vec<u32>>) -> Option<std::collections::HashSet<u32>> {
    pages.map(|p| p.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Public C ABI: classification & plain text (original v1 surface)
// ---------------------------------------------------------------------------

/// Classify a PDF's bytes: type, page count, which pages need OCR, and a
/// confidence score. Fast — skips text/markdown extraction entirely.
///
/// Returns an owned JSON string: `{"ok":true,"result":{...}}` or
/// `{"ok":false,"error":"..."}`. Never returns null. The caller must release
/// the result with `pdfinspector_free_string`.
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes (or
/// `len` must be 0).
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_classify(data: *const u8, len: usize) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        pdf_inspector::classify_pdf_mem(bytes)
            .map(Into::into)
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(result) => ClassifyEnvelope::ok(result),
        Err(error) => ClassifyEnvelope::err(error),
    })
}

/// Extract plain text from a PDF's bytes (no layout/markdown formatting).
///
/// Returns an owned JSON string: `{"ok":true,"text":"..."}` or
/// `{"ok":false,"error":"..."}`. Never returns null. The caller must release
/// the result with `pdfinspector_free_string`.
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes (or
/// `len` must be 0).
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_text(data: *const u8, len: usize) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        pdf_inspector::extractor::extract_text_mem(bytes).map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(text) => TextEnvelope::ok(text),
        Err(error) => TextEnvelope::err(error),
    })
}

// ---------------------------------------------------------------------------
// Public C ABI: full document processing
// ---------------------------------------------------------------------------

/// Process a PDF's bytes with full extraction: detect type, extract text,
/// and convert to Markdown. `params_json`: `{"pages": [0, 2]}` (0-indexed,
/// omit or pass `{}`/null for every page).
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json`, if non-null, must point to a valid
/// NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_process_pdf(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PagesParams = parse_params(params_json)?;
        let mut opts = pdf_inspector::PdfOptions::new();
        if let Some(pages) = params.pages {
            opts = opts.pages(pages);
        }
        pdf_inspector::process_pdf_mem_with_options(bytes, opts)
            .map(Into::into)
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(result) => PdfResultEnvelope::ok(result),
        Err(error) => PdfResultEnvelope::err(error),
    })
}

/// Fast detection only — no text extraction or markdown. Same result shape
/// as `pdfinspector_process_pdf` with `markdown` always `null`.
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0).
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_detect_pdf(data: *const u8, len: usize) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        pdf_inspector::detect_pdf_mem(bytes)
            .map(Into::into)
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(result) => PdfResultEnvelope::ok(result),
        Err(error) => PdfResultEnvelope::err(error),
    })
}

/// Extract per-page markdown with layout classification metadata.
/// `params_json`: `{"pages": [0, 2]}` (0-indexed, omit for every page).
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json`, if non-null, must point to a valid
/// NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_pages_markdown(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PagesParams = parse_params(params_json)?;
        pdf_inspector::extract_pages_markdown_mem(bytes, params.pages.as_deref())
            .map(Into::into)
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(result) => PagesExtractionEnvelope::ok(result),
        Err(error) => PagesExtractionEnvelope::err(error),
    })
}

/// Extract text with position/style information. `params_json`: `{"pages":
/// [0, 2]}` (0-indexed, omit for every page).
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json`, if non-null, must point to a valid
/// NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_text_with_positions(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PagesParams = parse_params(params_json)?;
        let items = match page_set(params.pages) {
            Some(pages) => {
                pdf_inspector::extractor::extract_text_with_positions_mem_pages(bytes, Some(&pages))
            }
            None => pdf_inspector::extractor::extract_text_with_positions_mem(bytes),
        }
        .map_err(|e| e.to_string())?;
        Ok(items.into_iter().map(Into::into).collect())
    });
    to_json_cstring(&match outcome {
        Ok(items) => TextItemsEnvelope::ok(items),
        Err(error) => TextItemsEnvelope::err(error),
    })
}

/// Extract structure-tree element references (page, MCID, role) from a
/// tagged PDF. Returns an empty list for untagged PDFs. `params_json`:
/// `{"pages": [1, 3]}` (1-indexed, matching `TextItem.page`; omit for every
/// page).
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json`, if non-null, must point to a valid
/// NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_structure_elements(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PagesParams = parse_params(params_json)?;
        pdf_inspector::extract_structure_elements_mem(bytes, params.pages.as_deref())
            .map(|elements| elements.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(elements) => StructureElementsEnvelope::ok(elements),
        Err(error) => StructureElementsEnvelope::err(error),
    })
}

// ---------------------------------------------------------------------------
// Public C ABI: region-based extraction (hybrid OCR / layout-model pipelines)
// ---------------------------------------------------------------------------

/// Extract text within bounding-box regions. `params_json`:
/// `{"page_regions": [{"page": 0, "regions": [[x1,y1,x2,y2], ...]}, ...]}`
/// (0-indexed pages, PDF points, top-left origin).
///
/// Each region result includes `needs_ocr`, set when the extracted text is
/// unreliable (empty, GID-encoded fonts, garbage, encoding issues).
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json` must point to a valid NUL-terminated
/// UTF-8 C string encoding the shape above.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_text_in_regions(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PageRegionsParams = parse_params(params_json)?;
        let regions = params::into_page_regions(params.page_regions);
        pdf_inspector::extract_text_in_regions_mem(bytes, &regions)
            .map(|results| results.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(results) => PageRegionTextsEnvelope::ok(results),
        Err(error) => PageRegionTextsEnvelope::err(error),
    })
}

/// Extract markdown tables within bounding-box regions. Same `params_json`
/// shape as `pdfinspector_extract_text_in_regions`. When a table is
/// detected, `text` is a markdown pipe-table and `needs_ocr` is `false`;
/// otherwise `text` is empty and `needs_ocr` is `true` so the caller can
/// fall back to OCR.
///
/// # Safety
/// Same as `pdfinspector_extract_text_in_regions`.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_tables_in_regions(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: PageRegionsParams = parse_params(params_json)?;
        let regions = params::into_page_regions(params.page_regions);
        pdf_inspector::extract_tables_in_regions_mem(bytes, &regions)
            .map(|results| results.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(results) => PageRegionTextsEnvelope::ok(results),
        Err(error) => PageRegionTextsEnvelope::err(error),
    })
}

/// Detect a vector ruled-line / rectangle grid inside one page region, for
/// callers doing their own TSR-hybrid pipeline. `params_json`:
/// `{"page_idx": 0, "region_pdf_pt_bbox": [x1,y1,x2,y2], "render_dpi": 200.0}`
/// (0-indexed page, PDF points with top-left origin, DPI of the crop image
/// that will consume the returned cell bboxes).
///
/// Returns `{"ok":true,"found":true,"result":{...}}` when a grid is found,
/// `{"ok":true,"found":false,"result":null}` when the region has no valid
/// vector grid, or `{"ok":false,"error":"..."}` on failure.
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json` must point to a valid NUL-terminated
/// UTF-8 C string encoding the shape above.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_detect_vector_grid_in_region(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: VectorGridParams = parse_params(params_json)?;
        pdf_inspector::detect_vector_grid_in_region_mem(
            bytes,
            params.page_idx,
            params.region_pdf_pt_bbox,
            params.render_dpi,
        )
        .map(|found| found.map(Into::into))
        .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(result) => VectorGridEnvelope::ok(result),
        Err(error) => VectorGridEnvelope::err(error),
    })
}

/// Extract markdown tables using externally-supplied structure recovery
/// (e.g. from an SLANet/TSR model run on rendered crops). `params_json`:
/// `{"inputs": [{"page":0, "crop_pdf_pt_bbox":[x1,y1,x2,y2],
/// "render_dpi":200.0, "structure_tokens":[...], "cell_bboxes":[[...],...]},
/// ...]}`. Returns one markdown string per input, in input order.
///
/// # Safety
/// `data` must point to a valid, readable buffer of at least `len` bytes
/// (or `len` must be 0). `params_json` must point to a valid NUL-terminated
/// UTF-8 C string encoding the shape above.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_tables_with_structure(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: TsrInputsParams = parse_params(params_json)?;
        let inputs: Vec<pdf_inspector::TsrTableInput> =
            params.inputs.into_iter().map(Into::into).collect();
        pdf_inspector::extract_tables_with_structure_mem(bytes, &inputs).map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(results) => MarkdownStringsEnvelope::ok(results),
        Err(error) => MarkdownStringsEnvelope::err(error),
    })
}

/// Lower-level sibling of `pdfinspector_extract_tables_with_structure`:
/// returns the resolved cells (row, col, span, header flag, text, bbox)
/// instead of rendered markdown, one `Vec` per input. Same `params_json`
/// shape.
///
/// # Safety
/// Same as `pdfinspector_extract_tables_with_structure`.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_tables_with_structure_cells(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: TsrInputsParams = parse_params(params_json)?;
        let inputs: Vec<pdf_inspector::TsrTableInput> =
            params.inputs.into_iter().map(Into::into).collect();
        pdf_inspector::extract_tables_with_structure_cells_mem(bytes, &inputs)
            .map(|per_input| {
                per_input
                    .into_iter()
                    .map(|cells| cells.into_iter().map(Into::into).collect())
                    .collect()
            })
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(results) => StructuredCellsEnvelope::ok(results),
        Err(error) => StructuredCellsEnvelope::err(error),
    })
}

/// Auto-fallback variant of `pdfinspector_extract_tables_with_structure`:
/// runs the TSR-hybrid path, detects known TSR detection pathologies
/// (phantom rows, multi-row-in-cell), and falls back to heuristic table
/// extraction on flagged inputs. Same `params_json` shape; each result
/// carries a `fallback_reason` (`null` when the TSR-hybrid path was used
/// directly).
///
/// # Safety
/// Same as `pdfinspector_extract_tables_with_structure`.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_extract_tables_with_structure_auto(
    data: *const u8,
    len: usize,
    params_json: *const c_char,
) -> *mut c_char {
    let bytes = slice_from_raw(data, len);
    let outcome = catch_panic(|| {
        let params: TsrInputsParams = parse_params(params_json)?;
        let inputs: Vec<pdf_inspector::TsrTableInput> =
            params.inputs.into_iter().map(Into::into).collect();
        pdf_inspector::extract_tables_with_structure_auto_mem(bytes, &inputs)
            .map(|results| results.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    });
    to_json_cstring(&match outcome {
        Ok(results) => TableExtractionEnvelope::ok(results),
        Err(error) => TableExtractionEnvelope::err(error),
    })
}

// ---------------------------------------------------------------------------
// Memory management
// ---------------------------------------------------------------------------

/// Release a string returned by any `pdfinspector_*` function above.
/// Passing null is a no-op. Passing anything else — a pointer not returned
/// by this crate, or a double-free — is undefined behavior, same as `free`.
///
/// # Safety
/// `s` must be either null or a pointer previously returned by one of this
/// crate's functions, not already freed.
#[no_mangle]
pub unsafe extern "C" fn pdfinspector_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}
