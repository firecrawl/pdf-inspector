//! C ABI shim for the Go binding (see `go/pdfinspector`).
//!
//! Mirrors the `napi/` crate's approach (local result structs + manual
//! conversion, panic-catching around every entry point) but returns a single
//! JSON-encoded envelope per call instead of mapping fields into a
//! language-native object graph, since plain C has no equivalent of NAPI's
//! object marshaling. Every exported function returns an owned, NUL-terminated
//! C string that the caller must release with `pdfinspector_free_string` —
//! that pairing is the entire memory-ownership contract of this ABI.
//!
//! Scope: classification and plain-text extraction only (the two operations
//! DCE-style OCR-routing pipelines need). `detect_pdf`/`process_pdf`
//! (markdown), region/table extraction, and vector-grid detection are not
//! exposed here; see the napi crate for the full surface if a future PR
//! wants to extend this binding.

use std::ffi::CString;
use std::os::raw::c_char;
use std::panic;

use serde::Serialize;

// ---------------------------------------------------------------------------
// JSON result types (local to this crate; mirrors napi/src/lib.rs's pattern
// of defining its own binding-local structs rather than adding Serialize to
// the core crate's types).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
enum PdfType {
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
struct Classification {
    pdf_type: PdfType,
    page_count: u32,
    /// 0-indexed, matching classify_pdf_mem's caller-convenience convention.
    pages_needing_ocr: Vec<u32>,
    confidence: f32,
}

#[derive(Serialize)]
struct ClassifyEnvelope {
    ok: bool,
    result: Option<Classification>,
    error: Option<String>,
}

#[derive(Serialize)]
struct TextEnvelope {
    ok: bool,
    text: Option<String>,
    error: Option<String>,
}

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

// ---------------------------------------------------------------------------
// Public C ABI
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
            .map(|c| Classification {
                pdf_type: c.pdf_type.into(),
                page_count: c.page_count,
                pages_needing_ocr: c.pages_needing_ocr,
                confidence: c.confidence,
            })
            .map_err(|e| e.to_string())
    });

    let envelope = match outcome {
        Ok(result) => ClassifyEnvelope {
            ok: true,
            result: Some(result),
            error: None,
        },
        Err(error) => ClassifyEnvelope {
            ok: false,
            result: None,
            error: Some(error),
        },
    };
    to_json_cstring(&envelope)
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

    let envelope = match outcome {
        Ok(text) => TextEnvelope {
            ok: true,
            text: Some(text),
            error: None,
        },
        Err(error) => TextEnvelope {
            ok: false,
            text: None,
            error: Some(error),
        },
    };
    to_json_cstring(&envelope)
}

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
