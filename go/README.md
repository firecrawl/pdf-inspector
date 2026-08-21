# pdf-inspector Go binding

Go bindings for [pdf-inspector](https://github.com/firecrawl/pdf-inspector)'s PDF classification and text extraction, via [cgo](https://pkg.go.dev/cmd/cgo) against the same native Rust core the [Node.js](../napi) and Python bindings use.

Built for OCR-routing pipelines — classify a PDF's text layer, extract locally when it's trustworthy, fall back to OCR only when it isn't.

## Scope

This is a v1: `Classify` and `ExtractText` only, covering the core "should I OCR this?" decision and the plain-text payload once the answer is no. The Node binding's fuller surface — `processPdf`/markdown extraction, region-based extraction, table structure recovery, vector-grid detection — is not exposed here yet. Adding it means extending `go/src/lib.rs` with the same JSON-envelope pattern used for the two functions below; happy to follow up if there's interest.

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

- **Why JSON envelopes instead of a napi/UniFFI-style typed FFI layer?** Plain C has no object-marshaling story, so every exported function returns one owned, NUL-terminated JSON string (`{"ok":true,"result":{...}}` or `{"ok":false,"error":"..."}`) that the caller releases with `pdfinspector_free_string`. This keeps the ABI to three functions total with no struct layout to keep in sync across the FFI boundary, at the cost of a JSON encode/decode per call — negligible next to PDF parsing itself. The DTOs are duplicated (once in `go/src/lib.rs`, once in `go/pdfinspector/pdfinspector.go`) rather than generated; at 2 operations / ~5 fields each, the drift risk is small and is caught by `pdfinspector_test.go`, which exercises real fixtures through the full cgo round trip on every CI run.
- **Why not reuse the in-flight UniFFI work (#255)?** That PR targets Swift/Kotlin specifically and isn't merged yet. This binding doesn't depend on it or conflict with it — if UniFFI's Go generator (`uniffi-bindgen-go`) ends up being the project's preferred multi-language story once #255/#182 settle, this hand-written ABI can be swapped out later without changing the Go-facing API surface.
- Every entry point in `go/src/lib.rs` wraps its body in `catch_unwind`, mirroring `napi/src/lib.rs`'s `catch_panic` — a Rust panic unwinding across the FFI boundary is undefined behavior, so it's converted to an error result instead.
- **cgo is a deliberate, inherent trade-off, not something this binding works around.** There's no pure-Go way to call into a Rust cdylib, so cross-compilation friction and slower builds are the cost of any Rust-backed Go binding. Some Go shops avoid cgo on principle; that's a reason to keep this binding's surface small (see Scope above), not a defect to fix here.
- **Distribution and CI** are handled by `.github/workflows/ci.yml` (build-and-test on every push/PR, from source, on Linux and macOS) and `.github/workflows/publish-go.yml` (cross-builds prebuilt native libraries and attaches them to a `go/vX.Y.Z` GitHub Release whenever `go/Cargo.toml`'s version changes). There is still no registry-distributed, zero-toolchain `go get` experience the way Python/Node have per-platform packages — Go's module system has no binary-artifact mechanism to hook into — but `go generate` closes most of that gap on the four platforms CI publishes for.
