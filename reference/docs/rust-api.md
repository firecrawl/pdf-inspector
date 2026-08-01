# pdf-inspector

Fast PDF classification and text extraction. Detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts to clean Markdown — all without OCR. Pure Rust, no ML models, no external services; the only PDF dependency is [lopdf](https://crates.io/crates/lopdf). Also available for [Python](https://pypi.org/project/pdf-inspector/) and [Node.js](https://www.npmjs.com/package/@firecrawl/pdf-inspector).

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
| `TextItem` | Text with position, font info, and page number |
| `MarkdownOptions` | Configuration for Markdown formatting (page numbers, etc.) |
| `PageMarkdown` | Per-page result: page (0-indexed), markdown, needs_ocr |
| `PagesExtractionResult` | Per-page output + 1-indexed pages_with_tables / pages_with_columns / pages_needing_ocr, is_complex |
| `PdfError` | `Io`, `Parse`, `Encrypted`, `InvalidStructure`, `NotAPdf` |
