# pdf-inspector Go binding

Go bindings for [pdf-inspector](https://github.com/firecrawl/pdf-inspector)'s PDF classification and text extraction, via [cgo](https://pkg.go.dev/cmd/cgo) against the same native Rust core the [Node.js](../napi) and Python bindings use.

Built for OCR-routing pipelines — classify a PDF's text layer, extract locally when it's trustworthy, fall back to OCR only when it isn't.

## Scope

This is a v1: `Classify` and `ExtractText` only, covering the core "should I OCR this?" decision and the plain-text payload once the answer is no. The Node binding's fuller surface — `processPdf`/markdown extraction, region-based extraction, table structure recovery, vector-grid detection — is not exposed here yet. Adding it means extending `go/src/lib.rs` with the same JSON-envelope pattern used for the two functions below; happy to follow up if there's interest.

## Building

Unlike the Node/Python bindings, there's no prebuilt-binary distribution yet (no equivalent of napi's per-platform npm packages) — you need a Rust toolchain to build the native library once:

```bash
cd go
cargo build --release
```

This produces `go/target/release/libpdf_inspector_go.{dylib,so,a}`. The Go package's cgo directives (`go/pdfinspector/pdfinspector.go`) find it via `${SRCDIR}`-relative flags, so no environment variables or system install step are needed:

```bash
cd go/pdfinspector
go test ./...
```

`${SRCDIR}` resolves relative to the Go source file, so this works from any working directory as long as the two directories stay in this relative layout. The `-Wl,-rpath` flag embeds that same path into the built test/binary so it can find the dynamic library at runtime without `LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH`. This has been verified on macOS (arm64) and should work equivalently on Linux; Windows DLL search-path handling is untested.

## API

### `Classify(data []byte) (*Classification, error)`

Classify a PDF as TextBased, Scanned, Mixed, or ImageBased (~10-50ms). Returns which pages need OCR.

```go
import "github.com/firecrawl/pdf-inspector/go/pdfinspector"

data, _ := os.ReadFile("document.pdf")
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

Extract a PDF's plain text (no layout or markdown formatting). Pair with `Classify` to decide first whether the text layer is worth trusting.

```go
text, err := pdfinspector.ExtractText(data)
```

## Design notes

- **Why JSON envelopes instead of a napi/UniFFI-style typed FFI layer?** Plain C has no object-marshaling story, so every exported function returns one owned, NUL-terminated JSON string (`{"ok":true,"result":{...}}` or `{"ok":false,"error":"..."}`) that the caller releases with `pdfinspector_free_string`. This keeps the ABI to three functions total with no struct layout to keep in sync across the FFI boundary, at the cost of a JSON encode/decode per call — negligible next to PDF parsing itself.
- **Why not reuse the in-flight UniFFI work (#255)?** That PR targets Swift/Kotlin specifically and isn't merged yet. This binding doesn't depend on it or conflict with it — if UniFFI's Go generator (`uniffi-bindgen-go`) ends up being the project's preferred multi-language story once #255/#182 settle, this hand-written ABI can be swapped out later without changing the Go-facing API surface.
- Every entry point in `go/src/lib.rs` wraps its body in `catch_unwind`, mirroring `napi/src/lib.rs`'s `catch_panic` — a Rust panic unwinding across the FFI boundary is undefined behavior, so it's converted to an error result instead.
