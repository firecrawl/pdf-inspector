# pdf-inspector Go binding

Go bindings for [pdf-inspector](https://github.com/firecrawl/pdf-inspector)'s PDF classification, text extraction, Markdown conversion, and table structure recovery, via [cgo](https://pkg.go.dev/cmd/cgo) against the same native Rust core the [Node.js](../napi) and Python bindings use.

## Scope

This binding's document-processing surface matches Node's (and exceeds Python's — Python doesn't expose `extract_tables_in_regions`, `detect_vector_grid_in_region`, or the `extract_tables_with_structure` family): classify, detect, and fully process a PDF (Markdown included); extract plain text, positioned text, per-page Markdown, or region-scoped text/tables; read a tagged PDF's structure tree; recover table structure from an externally-supplied TSR model's output; and selective OCR (`ProcessPdfWithOcr`).

### OCR

`go/Cargo.toml` builds against the core crate's `ocr` feature, the same way `napi/Cargo.toml` and `pyproject.toml`'s `python` feature do. That doesn't pull in a heavier build or link a native OCR library by default: PDFium and the ONNX Runtime backend are loaded *dynamically at runtime* (see the root `Cargo.toml`'s `firecrawl-pdfium`/`ort` comments), so `cargo build --release` in `go/` succeeds with or without either present. They're only required on the host **at runtime** when `OcrMode` is `"auto"` or `"force"` and at least one page is actually routed to OCR:

- **PDFium** (page rasterization): point `PDFIUM_LIB_PATH` at `libpdfium.{so,dylib,dll}`, same as the core crate's own tests/CI (see `.github/workflows/ci.yml`'s `ocr-runtime` job for how it's fetched).
- **ONNX Runtime** (OCR inference): point `ORT_DYLIB_PATH` at `libonnxruntime.{so,dylib,dll}`.
- **Model artifacts**: downloaded automatically to a local cache by default; set `OcrOptions.Offline: true` plus `OcrOptions.ModelDirectory` to run without network access, or `PDF_INSPECTOR_MODEL_CACHE` to relocate the cache.

`OcrMode: "off"` (the value used in this package's own tests) never touches any of the above — native extraction always runs first regardless of mode, and `"off"` just skips rendering and inference, which is why it's safe to exercise `ProcessPdfWithOcr`'s full result/provenance contract in CI without provisioning PDFium/ONNX Runtime at all.

## Building

### Option 1: fetch a prebuilt native library (no Rust toolchain needed)

On darwin/arm64, darwin/amd64, linux/amd64, or linux/arm64, `go generate` downloads a prebuilt `libpdf_inspector_go` from this repo's GitHub Releases (published by `.github/workflows/publish-go.yml`, tagged `go/vX.Y.Z`), checksum-verified against a `checksums.txt` release asset:

```bash
cd go/pdfinspector
go generate ./...
go test ./...
```

It's a no-op if a native library is already present at `go/target/release` (e.g. from Option 2), and it never overwrites one. On unsupported platforms, or if the download fails, it exits with a clear message pointing at Option 2.

### Option 2: build from source

Requires a Rust toolchain:

```bash
cd go
make native   # or: cargo build --release
```

This produces `go/target/release/libpdf_inspector_go.{dylib,so,a}`. Either option leaves the native library in the same place, so the rest of the workflow is identical:

```bash
cd go/pdfinspector
go test ./...
```

The Go package's cgo directives (`go/pdfinspector/pdfinspector.go`) find the library via `${SRCDIR}`-relative flags, so no environment variables or system install step are needed. `${SRCDIR}` resolves relative to the Go source file, so this works from any working directory as long as the two directories stay in this relative layout. The `-Wl,-rpath` flag embeds that same path into the built test/binary so it can find the dynamic library at runtime without `LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH`. This has been verified on macOS (arm64) and Linux (x64/arm64, via CI); Windows DLL search-path handling is untested and not covered by the prebuilt-library workflow.

CI (`.github/workflows/ci.yml`'s `go` job) builds and tests this binding from source on every push/PR, so both paths are exercised: source builds continuously, prebuilt downloads whenever a release is cut.

## Testing

`go test -race ./...` in `go/pdfinspector` (as CI runs it) covers, beyond one-shot happy-path calls per function:

- **Every fixture PDF in `tests/fixtures/`** (`TestCorpus_*`): `Classify`/`ExtractText` against all of them, not just the couple of curated fixtures the per-function tests use — a panic crossing the cgo boundary fails loudly here even on a fixture no unit test happens to touch.
- **Concurrent callers** (`TestConcurrentCalls`): several goroutines calling into the library at once, under the race detector — cgo calls release the calling goroutine's OS thread, so this genuinely exercises the underlying Rust library in parallel, not just interleaved Go-side scheduling.
- **Password-protected PDFs** (`TestProcessPdfWithOcr_EncryptedDocument_*`): wrong/missing password rejected, correct password decrypts — via `ProcessPdfWithOcr(..., Mode: OcrOff)`, since that's the only entry point in this package that accepts a password (see the OCR API section).

## API

All functions take the PDF as `[]byte` — there is no path-based API; read the file yourself (`os.ReadFile`) first. Every function returns a Go `error` built from the Rust side's message on failure.

```go
import "github.com/firecrawl/pdf-inspector/go/pdfinspector"

data, _ := os.ReadFile("document.pdf")
```

### `Classify(data []byte) (*Classification, error)`

Classify a PDF as TextBased, Scanned, Mixed, or ImageBased (~10-50ms). Returns which pages need OCR. This is the fastest call in the package — it skips text/Markdown extraction entirely — and is the right first call for an OCR-routing decision.

```go
result, err := pdfinspector.Classify(data)
if err != nil {
    log.Fatal(err)
}

fmt.Println(result.PdfType)          // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
fmt.Println(result.PageCount)        // 42
fmt.Println(result.PagesNeedingOCR)  // [5, 12, 15] (0-indexed)
fmt.Println(result.Confidence)       // 0.875
```

### `ExtractText(data []byte) (string, error)`

Extract a PDF's plain text (no layout or Markdown formatting). Pair with `Classify` to decide first whether the text layer is worth trusting.

```go
text, err := pdfinspector.ExtractText(data)
```

### `ProcessPdf(data []byte, pages []uint32) (*PdfResult, error)`

Full extraction: detect type, extract text, and convert to Markdown, in one document parse. `pages` is 0-indexed; pass `nil` for the whole document.

```go
result, err := pdfinspector.ProcessPdf(data, nil)
fmt.Println(result.PdfType, result.PageCount, *result.Markdown)
fmt.Println(result.IsComplexLayout, result.PagesWithTables, result.PagesWithColumns)
```

### `DetectPdf(data []byte) (*PdfResult, error)`

Fast metadata-only detection — same result shape as `ProcessPdf`, with `Markdown` always `nil`. Use this over `Classify` when you also want `Title`, `ProcessingTimeMs`, or 1-indexed `PagesNeedingOCR`/`OcrReasonsByPage`.

### `ExtractPagesMarkdown(data []byte, pages []uint32) (*PagesExtractionResult, error)`

Per-page Markdown plus layout classification (tables, columns, OCR needs) from a single parse, letting callers mix direct extraction for simple pages with OCR for complex/scanned ones. `pages` is 0-indexed; `nil` returns every page in document order, otherwise results follow the order you pass in.

```go
result, err := pdfinspector.ExtractPagesMarkdown(data, []uint32{2, 0}) // caller order preserved
for _, page := range result.Pages {
    fmt.Println(page.Page, page.NeedsOCR, page.Markdown)
}
```

### `ExtractTextWithPositions(data []byte, pages []uint32) ([]TextItem, error)`

Text with position, font, and style metadata (bold/italic/underline/strikeout, bounding box). `pages` is 0-indexed; `nil` for every page. `TextItem.Mcid` is non-nil when the item is linked to a tagged PDF's structure tree (join against `ExtractStructureElements` on `(Page, Mcid)`).

### `ExtractStructureElements(data []byte, pages []uint32) ([]StructureElement, error)`

Structure-tree element references (page, MCID, role — `"H1"`.."H6", `"P"`, `"Table"`, `"TD"`, ...) from a tagged PDF. Returns an empty slice for untagged PDFs. `pages` is **1-indexed**, matching `TextItem.Page` (unlike every other `pages` parameter in this package, which is 0-indexed).

### `ExtractTextInRegions` / `ExtractTablesInRegions(data []byte, pageRegions []PageRegions) ([]PageRegionTexts, error)`

For hybrid OCR pipelines: a layout model detects regions in a rendered page image, and these extract the PDF's own text (or, for the tables variant, a Markdown pipe-table) from within each region — skipping OCR for text-based pages. Regions are 0-indexed pages, PDF points, top-left origin. Each result carries `NeedsOCR`, set when the extraction is unreliable (empty, GID-encoded fonts, garbage/encoding issues, or — for the tables variant — no detectable table).

```go
results, err := pdfinspector.ExtractTextInRegions(data, []pdfinspector.PageRegions{
    {Page: 0, Regions: [][4]float32{{0, 0, 600, 100}}},
})
```

### `DetectVectorGridInRegion(data []byte, pageIdx uint32, regionPdfPtBbox [4]float32, renderDpi float32) (*VectorGridDetection, error)`

Detects a vector ruled-line / rectangle grid inside one page region without any external model — useful for tables whose structure is recoverable straight from the PDF's own drawing operators. Returns `(nil, nil)` when the region has no valid grid (as opposed to an `error`, which means the call itself failed).

### `ExtractTablesWithStructure` / `ExtractTablesWithStructureCells` / `ExtractTablesWithStructureAuto(data []byte, inputs []TsrTableInput) (…, error)`

For hybrid pipelines that already run an external table-structure-recognition model (e.g. SLANet) on rendered page crops: pass its structure tokens and cell bboxes in, and pdf-inspector lays out the cells and pulls each cell's text from the native PDF — no OCR involved.

- `ExtractTablesWithStructure` returns one Markdown pipe-table string per input.
- `ExtractTablesWithStructureCells` returns the resolved `[]StructuredCell` per input (row/col/span/header/text/bbox) for callers that want to drive their own rendering.
- `ExtractTablesWithStructureAuto` is the auto-fallback variant: it detects known TSR pathologies (phantom rows, multi-row content merged into one cell) and falls back to heuristic table extraction on flagged inputs, reporting which path produced each result via `TableExtractionResult.FallbackReason`.

### `ProcessPdfWithOcr(data []byte, options *OcrOptions) (*OcrPdfResult, error)`

Processes a PDF through native extraction with selective OCR — pass `nil` for `options` to run with every default (`OcrAuto`, 150 DPI, online model downloads). Native extraction always runs first; `OcrAuto` renders and runs OCR only on pages flagged as needing it, `OcrForce` runs it on every selected page, and `OcrOff` never touches the renderer or OCR engine (see "OCR" under Scope for what `OcrAuto`/`OcrForce` need available at runtime).

```go
result, err := pdfinspector.ProcessPdfWithOcr(data, &pdfinspector.OcrOptions{
    Mode: pdfinspector.OcrAuto,
})
fmt.Println(result.PagesRoutedToOCR)         // 1-indexed
fmt.Println(result.Pages[0].Provenance.Source) // "native" | "ocr" | "fused"
```

`OcrOptions.PageNumbers` and every result page number are **1-indexed** — the one place in this package that departs from the 0-indexed convention everything else uses, because it mirrors `OcrPdfOptions`'s own convention on the Rust side rather than `PdfOptions`'s. `OcrOptions.Password` decrypts an encrypted PDF; it's the only place in this package a password can be supplied at all, matching Node/Python (their plain `process_pdf` doesn't accept one either — only the OCR entry point does).

## Design notes

- **Why JSON envelopes instead of a napi/UniFFI-style typed FFI layer?** Plain C has no object-marshaling story, so every exported function returns one owned, NUL-terminated JSON string (`{"ok":true,"result":{...}}` / `{"ok":false,"error":"..."}`, or the analogous shape for that function's result field — see each function's doc comment in `go/src/lib.rs`) that the caller releases with `pdfinspector_free_string`. Two C argument shapes cover all thirteen operations — `(data, len)` for the two that take no options, `(data, len, params_json)` for everything else, where `params_json` is a single JSON-encoded parameter blob (see `go/src/params.rs`) — rather than growing a bespoke C parameter list per function. The cost is a JSON encode/decode per call, negligible next to PDF parsing itself.
- **DTOs are duplicated** (once in `go/src/{params,results}.rs`, once in `go/pdfinspector/pdfinspector.go`) rather than generated. `pdfinspector_test.go` exercises every operation against real fixtures through the full cgo round trip on every CI run, so a field-shape mismatch between the two sides surfaces immediately as a decode error or a failing assertion rather than silently drifting.
- **Why not reuse the in-flight UniFFI work (#255)?** That PR targets Swift/Kotlin specifically and isn't merged yet. This binding doesn't depend on it or conflict with it — if UniFFI's Go generator (`uniffi-bindgen-go`) ends up being the project's preferred multi-language story once #255/#182 settle, this hand-written ABI can be swapped out later without changing the Go-facing API surface.
- Every entry point in `go/src/lib.rs` wraps its body in `catch_unwind`, mirroring `napi/src/lib.rs`'s `catch_panic` — a Rust panic unwinding across the FFI boundary is undefined behavior, so it's converted to an error result instead.
- **cgo is a deliberate, inherent trade-off, not something this binding works around.** There's no pure-Go way to call into a Rust cdylib, so cross-compilation friction and slower builds are the cost of any Rust-backed Go binding. Some Go shops avoid cgo on principle; that's a fact of this approach, not a defect to fix here.
- **Distribution and CI** are handled by `.github/workflows/ci.yml` (build-and-test on every push/PR, from source, on Linux and macOS) and `.github/workflows/publish-go.yml` (cross-builds prebuilt native libraries and attaches them to a `go/vX.Y.Z` GitHub Release whenever `go/Cargo.toml`'s version changes). There is still no registry-distributed, zero-toolchain `go get` experience the way Python/Node have per-platform packages — Go's module system has no binary-artifact mechanism to hook into — but `go generate` closes most of that gap on the four platforms CI publishes for.
