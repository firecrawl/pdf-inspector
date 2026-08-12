# pdf-inspector

Fast PDF classification and text extraction. Detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts to clean Markdown — all without OCR. Pure Rust, no ML models, and no external services; the default feature set uses [lopdf](https://crates.io/crates/lopdf), while an opt-in feature adds selected-page rasterization. Also available for [Python](https://pypi.org/project/pdf-inspector/) and [Node.js](https://www.npmjs.com/package/@firecrawl/pdf-inspector).

Built by [Firecrawl](https://firecrawl.dev) to handle text-based PDFs locally in under 200ms, skipping expensive OCR services for the ~54% of PDFs that don't need them.

## Features

- **Smart classification** — TextBased / Scanned / ImageBased / Mixed in ~10–50ms, with a confidence score and per-page OCR routing.
- **Markdown conversion** — headings, lists, code blocks, bold/italic, URL linking, and dual-mode table detection (PDF drawing ops + text-alignment heuristics).
- **Layout-aware extraction** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Optional rendering and image extraction** — selected pages or positioned image occurrences can be returned as bounded RGBA8 buffers, including for WebAssembly callers.
- **Lightweight by default** — pure Rust, no ML models, no external services; the default feature set keeps [lopdf](https://crates.io/crates/lopdf) as its only PDF dependency.

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

### Optional selected-page rendering

Enable the non-default `render` feature when an OCR pipeline needs pixels for
pages already identified by `pages_needing_ocr`:

```bash
cargo add pdf-inspector --features render
```

```rust
use pdf_inspector::{classify_pdf_mem, render_pages_mem, RenderOptions, RenderWarning};

let bytes = std::fs::read("scan.pdf")?;
let classification = classify_pdf_mem(&bytes)?;

// PdfClassification uses the renderer's zero-based page indexes. Only pages
// routed to OCR are rasterized, in caller-supplied order.
if !classification.pages_needing_ocr.is_empty() {
    let pages = render_pages_mem(
        &bytes,
        &classification.pages_needing_ocr,
        RenderOptions::new().dpi(200.0),
    )?;

    for page in pages {
        if page.warnings.contains(&RenderWarning::ImageDecodeFailure) {
            // Do not OCR pixels from an image resource that failed to decode.
            continue;
        }
        // Opaque, row-major RGBA8 pixels on a white background.
        run_ocr(page.width, page.height, &page.pixels);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
# fn run_ocr(_: u32, _: u32, _: &[u8]) {}
```

Do not pass `PdfProcessResult::pages_needing_ocr` directly: that high-level
field is one-based for display, while `PdfClassification::pages_needing_ocr`,
`PageMarkdown::page`, and `render_pages_mem` are zero-based.

Even with an empty page list, `render_pages_mem` still copies and parses the
PDF, so integrators must skip the call entirely on the no-OCR fast path.

The renderer is CPU-only and byte-oriented, with no filesystem access, and
compiles for `wasm32-unknown-unknown`. It is intentionally not enabled in the
default feature set. The `render` feature currently requires Rust 1.92 because
of its rendering backend; default builds keep the existing compiler behavior.

Every request's output viewport and returned buffers are validated before the
first page is rendered:

| Limit | Value |
|---|---:|
| Default DPI | 200 |
| Maximum DPI | 300 |
| Maximum width or height | 16,384 px |
| Maximum pixels per page | 25,000,000 |
| Maximum combined RGBA8 output | 100,000,000 bytes |
| Maximum page entries per call | 1,024 |

For large scanned documents, render small batches and release each page buffer
after OCR instead of retaining the whole document in memory. Rendering handles
common page crops, rotations, transforms, images, and soft masks before
returning pixels. Each page also carries typed warnings for unsupported fonts
and failed image decoding; an image-decode warning means its pixels must not be
silently accepted as OCR input.

The optional backend is still evolving. Hayro 0.7.1 documents missing or
incomplete support for blending and isolation, knockout groups, and color-key
masking. Preserve a fallback for documents it cannot render faithfully, even
when no interpreter warning is available for that unsupported construct.

These limits bound output dimensions and returned RGBA memory. The current
backend can still allocate internal image-decoder and compositing scratch
buffers from PDF resources. Until it exposes comprehensive internal resource
limits, isolate untrusted rendering in a process or Web Worker with its own
memory/input limits; output preflight alone is not an adversarial-PDF sandbox.

The same feature can preserve an image's position in Markdown and return the
pixels rendered at that position. Both outputs carry the same reference, so a
caller does not need to match PDF resource names:

```rust
use pdf_inspector::{
    extract_images_mem, process_pdf_mem_with_options, MarkdownOptions,
    PdfOptions, RenderOptions,
};

let bytes = std::fs::read("report.pdf")?;
let result = process_pdf_mem_with_options(
    &bytes,
    PdfOptions::new().markdown(MarkdownOptions {
        include_images: true,
        ..MarkdownOptions::default()
    }),
)?;
let markdown = result.markdown.unwrap_or_default();

for image in extract_images_mem(&bytes, RenderOptions::new().dpi(200.0))? {
    // Markdown contains, for example:
    // ![Image: Im0](pdf-image:p1_i1)
    assert!(markdown.contains(&image.reference));

    // Opaque, row-major RGBA8 pixels for this image occurrence.
    save_png(image.width, image.height, &image.pixels);
}
# Ok::<(), Box<dyn std::error::Error>>(())
# fn save_png(_: u32, _: u32, _: &[u8]) {}
```

`RenderedImage::page` is zero-based and `occurrence` is one-based within that
page. Its `bbox` uses the source PDF's bottom-left coordinate space; `width`,
`height`, and `pixels` describe the rendered crop after applying the page crop
box and rotation. Repeated uses of one image resource are returned separately
because each occurrence can have a different position or transform.

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
| `render_pages_mem(bytes, pages, options)` | Selected pages as bounded RGBA8 buffers (`render` feature) |
| `extract_images_mem(bytes, options)` | Positioned image occurrences as bounded RGBA8 buffers joined to opt-in Markdown references (`render` feature) |

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
| `RenderOptions` | DPI and optional password for selected-page rendering (`render` feature) |
| `RenderedPage` | Zero-based source page, dimensions, and opaque RGBA8 pixels (`render` feature) |
| `RenderedImage` | Stable Markdown reference, source page/bbox, dimensions, and rendered RGBA8 pixels (`render` feature) |
| `RenderError` | Typed parse, password, page-selection, and resource-limit failures (`render` feature) |
| `RenderWarning` | Per-page unsupported-font or image-decode fidelity warning (`render` feature) |
