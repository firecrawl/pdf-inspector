# pdf-inspector

Fast PDF classification and text extraction. The default build detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts to clean Markdown without OCR. It is pure Rust, has no ML models or external services, and uses [lopdf](https://crates.io/crates/lopdf) for PDF parsing. Native Rust and CLI consumers can opt into selective OCR. Also available for [Python](https://pypi.org/project/pdf-inspector/) and [Node.js](https://www.npmjs.com/package/@firecrawl/pdf-inspector/).

Built by [Firecrawl](https://firecrawl.dev) to handle text-based PDFs locally in under 200ms, skipping expensive OCR services for the ~54% of PDFs that don't need them.

## Features

- **Smart classification** — TextBased / Scanned / ImageBased / Mixed in ~10–50ms, with a confidence score and per-page OCR routing.
- **Markdown conversion** — headings, lists, code blocks, bold/italic, URL linking, and dual-mode table detection (PDF drawing ops + text-alignment heuristics).
- **Layout-aware extraction** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Lightweight** — pure Rust, no ML models, no external services; single PDF dependency ([lopdf](https://crates.io/crates/lopdf)).

## Benchmark

[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench) corpus (200 PDFs), local engines without model-based PDF parsing; OCR disabled. Scores 0–1, higher is better:

| Engine | Overall | Reading order | Tables (TEDS) | Headings | Speed |
|---|---|---|---|---|---|
| **pdf-inspector** | **0.875** | **0.915** | **0.814** | 0.788 | **0.470s** |
| liteparse | 0.873 | 0.913 | 0.693 | **0.811** | 0.750s |
| opendataloader | 0.831 | 0.902 | 0.489 | 0.739 | 2.569s |
| pymupdf4llm | 0.735 | 0.886 | 0.401 | 0.424 | 17.117s |
| markitdown | 0.589 | 0.844 | 0.273 | 0.000 | 16.165s |

Refreshed July 31, 2026, on Apple M4 Pro; speed is the median of five complete corpus runs after an excluded warm-up. Full methodology and versions are in the [repo README](https://github.com/firecrawl/pdf-inspector#benchmark), with raw timings and artifacts in the [results branch](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results).

## Install

```bash
cargo add pdf-inspector
```

For the latest unreleased changes, use the git dependency instead:

```toml
[dependencies]
pdf-inspector = { git = "https://github.com/firecrawl/pdf-inspector" }
```

The crate also ships CLI binaries — `pdf2md` (PDF → Markdown, with `--json`, `--pages`, `--select-pages`, and the opt-in token-saving `--compact` profile) and `detect-pdf` (classification, with `--analyze --json`):

```bash
cargo install pdf-inspector
```

## Usage

Detect and extract in one call:

```rust
use pdf_inspector::process_pdf;

let result = process_pdf("document.pdf")?;

println!("Type: {:?}", result.pdf_type);       // TextBased, Scanned, ImageBased, Mixed
println!("Confidence: {:.0}%", result.confidence * 100.0);
println!("Pages: {}", result.page_count);

if let Some(markdown) = &result.markdown {
    println!("{}", markdown);
}
```

Fast metadata-only detection (no text extraction or markdown generation):

```rust
use pdf_inspector::detect_pdf;

let info = detect_pdf("document.pdf")?;

match info.pdf_type {
    pdf_inspector::PdfType::TextBased => {
        // Extract locally — fast and free
    }
    _ => {
        // Route to OCR service
        // info.pages_needing_ocr tells you exactly which pages
    }
}
```

Customize processing with `PdfOptions`:

```rust
use pdf_inspector::{process_pdf_with_options, PdfOptions, ProcessMode, DetectionConfig, ScanStrategy};

// Analyze layout without generating markdown
let result = process_pdf_with_options(
    "document.pdf",
    PdfOptions::new().mode(ProcessMode::Analyze),
)?;

// Full extraction with custom detection strategy
let result = process_pdf_with_options(
    "large.pdf",
    PdfOptions::new().detection(DetectionConfig {
        strategy: ScanStrategy::Sample(5),
        ..Default::default()
    }),
)?;

// Process only specific pages
let result = process_pdf_with_options(
    "document.pdf",
    PdfOptions::new().pages([1, 3, 5]),
)?;
```

Process from a byte buffer (no filesystem needed):

```rust
use pdf_inspector::process_pdf_mem;

let bytes = std::fs::read("document.pdf")?;
let result = process_pdf_mem(&bytes)?;
```

### Vision extension contracts

The native-only `vision` feature exposes the stable seam used by OCR
integrations without selecting or embedding an inference runtime. The
separate `model-cache` feature adds pinned artifact management:

- `PageRenderer` and `OcrEngine` traits;
- renderer-neutral owned page buffers and affine pixel↔PDF transforms;
- `OcrOptions` and opt-in `Off`/`Auto`/`Force` routing modes;
- positioned OCR results and per-page provenance types; and
- a versioned PP-OCRv6 Small manifest with checksum-verified, locked, atomic
  model-cache installation and explicit offline-directory overrides.

```toml
[dependencies]
pdf-inspector = { version = "1", features = ["vision", "model-cache"] }
```

The OCR contracts preserve existing behavior by default: OCR is `Off` and
model resolution is never reached. `ModelStore` itself does not access the
network. The optional `model-download` feature provides an
HTTPS downloader that streams pinned artifacts into the checksum-verified
cache only after routing has selected OCR work. Offline consumers set an
explicit model directory and `ModelDownloadPolicy::Offline`. Renderer-only
consumers do not enable `model-cache` or `model-download` and therefore do not
compile their filesystem, hashing, or HTTP dependencies.

```rust
use pdf_inspector::vision::{
    ModelDownloadPolicy, ModelStore, OcrMode, OcrOptions, PP_OCR_V6_SMALL,
};

let ocr = OcrOptions::new()
    .mode(OcrMode::Auto)
    .model_directory("/opt/firecrawl/models/pp-ocrv6-small")
    .model_downloads(ModelDownloadPolicy::Offline);
// Verifies exact sizes and SHA-256 digests before an engine opens the files.
let models = ModelStore::from_options(&ocr)?.resolve(&PP_OCR_V6_SMALL)?;
println!("using {} at {}", models.manifest_id(), models.revision());
```

### Optional native page rendering

The `render-pdfium` feature adds a native-only page renderer backed by
[`firecrawl-pdfium`](https://crates.io/crates/firecrawl-pdfium). It is the
rendering boundary for OCR pipelines; enabling it does not include an OCR
model or change the existing extraction functions. It implies `vision`,
and `PdfiumRenderer` implements the renderer-neutral `PageRenderer` trait.

```toml
[dependencies]
pdf-inspector = { version = "1", features = ["render-pdfium"] }
```

PDFium is loaded at runtime and is not bundled into the crate. Set
`PDFIUM_LIB_PATH` to the platform shared library, place that library next to
the executable, or use another discovery route supported by
`firecrawl-pdfium`. A load failure reports this prerequisite directly.

```rust
use pdf_inspector::vision::{PdfiumRenderer, RenderOptions};

let renderer = PdfiumRenderer::load()?;
let bytes = std::fs::read("document.pdf")?;
let pages = renderer.render_pages(
    &bytes,
    &[1, 3], // 1-indexed, matching pages_needing_ocr
    None,    // optional PDF password
    &RenderOptions::new().dpi(150.0),
)?;

for page in pages {
    // Owned RGB pixels can leave the PDFium critical section and be sent to
    // an OCR worker. OCR pixel boxes can be mapped back to PDF coordinates.
    let rect = page.pixel_rect_to_pdf_rect(20.0, 30.0, 100.0, 24.0);
    println!("page {}: {}x{}, rect={rect:?}", page.page(), page.width(), page.height());
}
```

Browser WASM remains on the default text-only path and does not expose native
PDFium rendering.

### Optional OCR engine

The native-only `ocr-oar` feature adds a CPU PP-OCRv6 Small implementation of
`OcrEngine` backed by OAR and ONNX Runtime. It implies `model-cache`, but does
not enable model auto-download, ONNX Runtime download, or PDF rendering. Model
files remain external, must match the pinned manifest, and are opened only
after `ModelStore` verifies their exact size and SHA-256 digest. Install an
ONNX Runtime shared library separately and set `ORT_DYLIB_PATH` to its full
path when it is not available through the platform library search path. The
runtime is resolved only when an OCR engine is first constructed; clean
`Auto` requests do not require it. The feature currently requires Rust 1.95
or newer, matching OAR 0.9.1's MSRV.

```toml
[dependencies]
pdf-inspector = { version = "1", features = ["ocr-oar", "render-pdfium"] }
```

Direct engine invocation is intentionally separate from extraction routing and
native/OCR fusion:

```rust
use pdf_inspector::vision::{
    ModelDownloadPolicy, ModelStore, OarOcrEngine, OcrEngine, OcrMode,
    OcrOptions, PdfiumRenderer, RenderOptions, PP_OCR_V6_SMALL,
};

let options = OcrOptions::new()
    .mode(OcrMode::Force)
    .minimum_confidence(0.45)
    .model_directory("/opt/firecrawl/models/pp-ocrv6-small")
    .model_downloads(ModelDownloadPolicy::Offline);
let models = ModelStore::from_options(&options)?.resolve(&PP_OCR_V6_SMALL)?;
let engine = OarOcrEngine::from_models(&models)?;

let renderer = PdfiumRenderer::load()?;
let bytes = std::fs::read("scan.pdf")?;
let pages = renderer.render_pages(&bytes, &[1], None, &RenderOptions::new())?;
let ocr_pages = engine.recognize(&pages, &options)?;

for span in &ocr_pages[0].spans {
    println!("{:.3}: {}", span.confidence, span.text);
}
```

The engine accepts renderer-neutral RGB, RGBA, and grayscale pages, preserves
OAR's positioned quadrilaterals in bitmap coordinates, filters spans using
`minimum_confidence`, and records the pinned model revision in every `OcrPage`.
`OcrMode::Off` is rejected at the engine boundary so default options cannot run
inference accidentally.

### Selective routing and lazy model acquisition

`route_ocr_pages` applies the existing detector/text-quality recommendations to
the configured mode. `Auto` processes only recommended pages, `Force` processes
all pages (or an explicit page selection), and `Off` always returns an empty
route. `run_ocr_pages` renders only that route, checks that both dependencies
preserve its order, and retains each bitmap's PDF transform for fusion.

```toml
[dependencies]
pdf-inspector = { version = "1", features = [
  "render-pdfium",
  "ocr-oar",
  "model-download",
] }
```

```rust
use pdf_inspector::vision::{
    route_ocr_pages, run_ocr_pages, HttpModelDownloader, ModelStore,
    OarOcrEngine, OcrMode, OcrOptions, PdfiumRenderer, RenderOptions,
    PP_OCR_V6_SMALL,
};

let bytes = std::fs::read("scan.pdf")?;
let extraction = pdf_inspector::extract_pages_markdown_mem(&bytes, None)?;
let options = OcrOptions::new().mode(OcrMode::Auto);
let routed = route_ocr_pages(
    options.mode,
    extraction.pages.len() as u32,
    &extraction.pages_needing_ocr,
    None,
)?;

if !routed.is_empty() {
    // No HTTP request or model initialization occurs before this point.
    let store = ModelStore::from_options(&options)?;
    let models = store.resolve_or_download(
        &PP_OCR_V6_SMALL,
        options.model_downloads,
        &HttpModelDownloader::default(),
    )?;
    let run = run_ocr_pages(
        &PdfiumRenderer::load()?,
        &OarOcrEngine::from_models(&models)?,
        &bytes,
        &routed,
        None,
        &RenderOptions::new(),
        &options,
    )?;
    println!("OCR processed {} pages", run.pages.len());
}
```

The downloader accepts HTTPS only, checks a declared content length, caps the
response stream to the pinned size plus one byte, and delegates final size and
SHA-256 verification to `ModelStore`. The store serializes installation across
processes and publishes completed artifacts atomically. Warm caches make no
network calls; offline mode and explicit model directories never download.

### OCR Markdown assembly and native fusion

`fuse_ocr_pages` maps OCR polygons back into PDF coordinates and sends the
result through pdf-inspector's existing deterministic reading-order, table,
and Markdown pipeline. Pages whose native extraction was rejected use OCR
output. When `Force` runs on a clean native page, normalized duplicate OCR
blocks are removed and only additional image-backed text is retained.

```rust
use pdf_inspector::vision::{fuse_ocr_pages, OcrFusionOptions};

let fused = fuse_ocr_pages(
    &extraction.pages,
    &run,
    extraction.pages.len() as u32,
    &OcrFusionOptions::new().render_dpi(150.0),
)?;

for page in &fused.pages {
    println!("{}", page.markdown);
    if page.provenance.hosted_recommended {
        eprintln!(
            "page {} needs the hosted document pipeline",
            page.page_number,
        );
    }
}
```

Each page carries `Native`, `Ocr`, or `Fused` provenance, the exact OCR model
revision, accepted-page confidence, local stage timings, and non-fatal
warnings. A page that required OCR recommends the hosted pipeline when local
OCR is missing, empty, or below the configurable page-confidence threshold.
This keeps the lightweight path explicit about cases it cannot finish well.

### Complete OCR API

The `ocr` convenience feature enables the renderer, OCR engine, verified
model acquisition, routing, and fusion layers together. It is the intended
downstream application integration boundary; lower-level features remain
available for consumers that bring their own renderer, model package manager,
or engine.

```toml
[dependencies]
pdf-inspector = { version = "1", features = ["ocr"] }
```

```rust
use pdf_inspector::vision::{process_pdf_with_ocr, OcrPdfOptions};

let result = process_pdf_with_ocr(
    "document.pdf",
    OcrPdfOptions::auto().page_numbers([1, 2, 3]),
)?;

println!("{}", result.markdown);
println!("OCR pages: {:?}", result.pages_routed_to_ocr);
println!(
    "Hosted fallback pages: {:?}",
    result.pages_recommending_hosted,
);
```

Native extraction always runs first. In `Auto`, a clean PDF returns before
PDFium loading, model-cache access, HTTP, or OAR initialization. Model files
remain external and the default crate feature set remains unchanged. `Off`
provides the same native-only behavior through the OCR result/provenance
shape; `Force` renders every selected page. OCR uses the existing deterministic
table, column, reading-order, and Markdown assembly path; no learned layout
model is included.

The [OCR runtime setup guide](https://github.com/firecrawl/pdf-inspector/blob/main/docs/ocr-runtime.md)
lists the pinned PDFium and ONNX Runtime builds, environment variables, model
cache behavior, and the error boundary downstream hosted fallbacks should use.

For ambiguous mixed pages, `Auto` privately retains clean native fragments
instead of discarding them when OCR is selected. After recognition it compares
script-agnostic text quality, OCR confidence, character overlap, and material
new coverage. Exact native text wins over a duplicate or weak OCR hypothesis;
complementary image-backed text is fused; and pages where both candidates are
weak recommend the hosted document pipeline. A page routed because native
coverage appeared incomplete also recommends hosted processing when confident
OCR only duplicates the retained fragment: the agreement preserves trustworthy
text, but neither hypothesis proves full-page coverage. Public native-only
extraction continues to suppress pages marked unreliable, and clean text
documents pay no renderer or model-initialization cost.

In `Auto`, pages routed only for suspicious font encoding or vectorized text
first get a bounded positioned-text probe through PDFium. A credible recovered
text layer with sufficient geometric page coverage skips rasterization and
model loading for that page; garbled, partial, or insubstantial recovery
continues through OCR. Recovered tables are reflected in the same document
metadata as tables found by the primary extractor.

The one-call API keeps the most recently used verified OCR engine in process.
Long-lived workers therefore verify the pinned artifacts and build the ONNX
sessions once, then reuse those loaded sessions across documents. The cache is
bounded to one model configuration and keyed by normalized model/runtime paths
plus the pinned manifest revision and artifact digests; switching the model
directory, runtime library, or compiled manifest replaces it. An active engine
owns the model data it already verified, so mutating artifacts in place does
not hot-reload a running process; restart the process when intentionally
replacing files at the same paths. CPU inference uses at most four intra-op
threads per ONNX session so a single small page does not oversubscribe larger
hosts, and recognizes variable-width line crops individually to avoid
padding-heavy CPU batches. The high-level pipeline renders and fuses at most
four routed pages at a time, bounding bitmap memory on long documents.

Build the CLI with the same opt-in feature:

```bash
cargo install pdf-inspector --features ocr --bin pdf2md
cargo build --release --features ocr --bin pdf2md
pdf2md document.pdf --ocr auto --raw
pdf2md document.pdf --ocr auto --json
pdf2md document.pdf --ocr auto --ocr-offline --ocr-model-dir /opt/models/pp-ocrv6-small
```

CLI controls include `--ocr-dpi`, `--ocr-min-confidence`,
`--ocr-hosted-threshold`, `--select-pages`, and the existing encrypted-PDF
`--password` option. JSON output has `schema_version: 1` and includes per-page Markdown, source/model
provenance, confidence, timings, warnings, routed pages, and hosted-fallback
recommendations. Page numbers in `OcrPdfResult` and its per-page provenance
are 1-indexed, matching the PDF page numbers accepted by
`OcrPdfOptions::page_numbers`.

Extract per-page Markdown (one string per page, plus document-wide layout
metadata):

```rust
use pdf_inspector::extract_pages_markdown;

// Pass `None` for every page in document order, or a slice of 0-indexed
// pages to restrict the output (caller-supplied order is preserved).
let result = extract_pages_markdown("document.pdf", None)?;

for page in &result.pages {
    if page.needs_ocr {
        // Route this page to OCR
    } else {
        println!("Page {}: {}", page.page, page.markdown);
    }
}

println!("Complex layout? {}", result.is_complex);
```

Extract structure-tree elements from tagged PDFs, and join them against
`extract_text_with_positions` to attach semantic roles (heading levels,
paragraphs, table cells) to extracted text:

```rust
use pdf_inspector::{extract_structure_elements, extract_text_with_positions};
use std::collections::HashMap;

// One entry per marked-content reference, sorted by (page, mcid); empty for
// untagged PDFs. Pages are 1-indexed to match `TextItem::page`, so the
// (page, mcid) pair is a direct join key.
let elements = extract_structure_elements("tagged.pdf", None)?;
let roles: HashMap<(u32, i64), &str> = elements
    .iter()
    .map(|e| ((e.page, e.mcid), e.role.as_str()))
    .collect();

for item in extract_text_with_positions("tagged.pdf")? {
    if let Some(mcid) = item.mcid {
        if let Some(role) = roles.get(&(item.page, mcid)) {
            if role.starts_with('H') {
                println!("{}: {}", role, item.text);
            }
        }
    }
}
```

## Processing modes

| Mode | What it does | Returns |
|---|---|---|
| `ProcessMode::Full` (default) | Detect + extract + convert to Markdown | Everything populated |
| `ProcessMode::Analyze` | Detect + extract + layout analysis (no Markdown) | `markdown` is `None`, `layout` is populated |
| `ProcessMode::DetectOnly` | Classification only (fastest) | `markdown` is `None`, `layout` is default |

## Functions

| Function | Description |
|---|---|
| `process_pdf(path)` | Full processing with defaults |
| `detect_pdf(path)` | Fast metadata-only detection (no extraction) |
| `process_pdf_with_options(path, options)` | Process with custom `PdfOptions` |
| `process_pdf_mem(bytes)` | Full processing from a byte buffer |
| `detect_pdf_mem(bytes)` | Fast detection from a byte buffer |
| `process_pdf_mem_with_options(bytes, options)` | Process from bytes with custom options |
| `extract_text(path)` | Plain text extraction |
| `extract_text_with_positions(path)` | Text with X/Y coordinates and font info |
| `to_markdown(text, options)` | Convert plain text to Markdown |
| `to_markdown_from_items(items, options)` | Markdown from pre-extracted `TextItem`s |
| `to_markdown_from_items_with_rects(items, options, rects)` | Markdown with rectangle-based table detection |
| `extract_pages_markdown(path, pages)` | Per-page Markdown + layout metadata (file) |
| `extract_pages_markdown_mem(bytes, pages)` | Per-page Markdown from bytes |
| `extract_structure_elements(path, pages)` | Structure-tree elements from tagged PDFs (page, mcid, role) |
| `extract_structure_elements_mem(bytes, pages)` | Structure-tree elements from bytes |

Low-level detection functions are also available via the `detector` module (`detect_pdf_type`, `detect_pdf_type_with_config`, etc.) for callers who need `PdfTypeResult` instead of `PdfProcessResult`.

## Types

| Type | Description |
|---|---|
| `PdfOptions` | Builder for processing configuration (mode, detection, markdown, page filter) |
| `ProcessMode` | `DetectOnly`, `Analyze`, `Full` |
| `PdfType` | `TextBased`, `Scanned`, `ImageBased`, `Mixed` |
| `PdfProcessResult` | Full result: pdf_type, markdown, page_count, confidence, layout, has_encoding_issues, timing |
| `PdfTypeResult` | Low-level detection result: type, confidence, page count, pages needing OCR |
| `DetectionConfig` | Configuration for detection: scan strategy, thresholds |
| `ScanStrategy` | `EarlyExit`, `Full`, `Sample(n)`, `Pages(vec)` |
| `LayoutComplexity` | Layout analysis: is_complex, pages_with_tables, pages_with_columns |
| `TextItem` | Text with position, font info, page number, and optional structure-tree `mcid` |
| `StructureElement` | Tagged-PDF structure reference: page (1-indexed), mcid, role (`"H1"`..`"H6"`, `"P"`, …) |
| `MarkdownOptions` | Configuration for Markdown formatting (page numbers, etc.) |
| `PageMarkdown` | Per-page result: page (0-indexed), markdown, needs_ocr |
| `PagesExtractionResult` | Per-page output + 1-indexed pages_with_tables / pages_with_columns / pages_needing_ocr, is_complex |
| `PdfError` | `Io`, `Parse`, `Encrypted`, `InvalidStructure`, `NotAPdf` |
