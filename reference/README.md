# pdf-inspector

[![Crates.io](https://img.shields.io/crates/v/pdf-inspector.svg)](https://crates.io/crates/pdf-inspector)
[![npm](https://img.shields.io/npm/v/@firecrawl/pdf-inspector.svg)](https://www.npmjs.com/package/@firecrawl/pdf-inspector)
[![PyPI](https://img.shields.io/pypi/v/pdf-inspector.svg)](https://pypi.org/project/pdf-inspector/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast Rust library for PDF classification and text extraction. Detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts to clean Markdown — all without OCR. Includes bindings for [Python](docs/python.md), [Node.js](napi/README.md), and [browser WebAssembly](wasm/README.md).

Built by [Firecrawl](https://firecrawl.dev) to handle text-based PDFs locally in under 200ms, skipping expensive OCR services for the ~54% of PDFs that don't need them.

## Features

- **Smart classification** — Detect TextBased, Scanned, ImageBased, or Mixed PDFs in ~10-50ms by sampling content streams. Returns a confidence score (0.0-1.0) and per-page OCR routing.
- **Text extraction** — Position-aware extraction with font info, X/Y coordinates, and automatic multi-column reading order.
- **Markdown conversion** — Headings (H1-H4 via font size ratios), bullet/numbered/letter lists, code blocks (monospace font detection), tables (rectangle-based and heuristic), bold/italic formatting, URL linking, and page breaks.
- **Table detection** — Dual-mode: rectangle-based detection from PDF drawing ops, plus heuristic detection from text alignment. Handles financial tables, footnotes, and continuation tables across pages.
- **CID font support** — ToUnicode CMap decoding for Type0/Identity-H fonts, UTF-16BE, UTF-8, and Latin-1 encodings.
- **Multi-column layout** — Automatic detection of newspaper-style columns, sequential reading order, and RTL text support.
- **Encoding issue detection** — Automatically flags broken font encodings so callers can fall back to OCR.
- **Single document load** — The document is parsed once and shared between detection and extraction, avoiding redundant I/O.
- **Browser WebAssembly** — Run the same Rust parser locally in browsers and Web Workers, with embedded CMaps and no server round trip.
- **Lightweight** — Pure Rust, no ML models, no external services. Single dependency on `lopdf` for PDF parsing.

## Benchmark

Evaluated on the [opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench) corpus (200 PDFs). Only local engines without model-based PDF parsing are shown; OCR was disabled. Scores are 0-1, higher is better.

| Engine | Overall | Reading Order (NID) | Tables (TEDS) | Headings (MHS) | Speed (200 docs) |
|---|---|---|---|---|---|
| pdf-inspector | **0.875** | **0.915** | **0.814** | 0.788 | **0.470s** |
| liteparse | 0.873 | 0.913 | 0.693 | **0.811** | 0.750s |
| opendataloader | 0.831 | 0.902 | 0.489 | 0.739 | 2.569s |
| pymupdf4llm | 0.735 | 0.886 | 0.401 | 0.424 | 17.117s |
| markitdown | 0.589 | 0.844 | 0.273 | 0.000 | 16.165s |

Results were refreshed on July 31, 2026, on an Apple M4 Pro. Engine versions were pdf-inspector 0.2.6, LiteParse 2.10.1, OpenDataLoader 2.2.1, PyMuPDF4LLM 0.2.0, and MarkItDown 0.1.5. Speed is the median of five alternating or rotating complete corpus runs after an excluded warm-up run, with each parser processing documents sequentially in a single process.

The complete parser configuration, per-document predictions, evaluator output, and generated charts are available in the [reproducible results branch](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results).

**Best fit:** Native-text PDFs where speed, reading order, and table structure matter. In this comparison, pdf-inspector delivered the higher overall, reading-order, and table scores, along with the fastest complete run. That makes it a strong local default for reports, research papers, financial documents, invoices, and legal PDFs that need clean, structured Markdown without adding OCR latency or infrastructure.

Use the [paired benchmark harness](docs/benchmarking.md) to compare two local builds against the exact same corpus and evaluator revision.

## Quick start

### Python

```bash
pip install maturin
maturin develop --release
```

```python
import pdf_inspector

result = pdf_inspector.process_pdf("document.pdf")
print(result.pdf_type)   # "text_based", "scanned", "image_based", "mixed"
print(result.markdown)   # Markdown string or None
```

> Full API reference: [docs/python.md](docs/python.md)

### Node.js

```bash
npm install @firecrawl/pdf-inspector
```

```javascript
import { readFileSync } from 'fs';
import { processPdf, classifyPdf } from '@firecrawl/pdf-inspector';

const result = processPdf(readFileSync('document.pdf'));
console.log(result.pdfType);   // "TextBased", "Scanned", "ImageBased", "Mixed"
console.log(result.markdown);  // Markdown string or null
```

> Full API reference: [napi/README.md](napi/README.md)

### Browser WebAssembly

```bash
npm install @firecrawl/pdf-inspector-wasm
```

```javascript
import init, { processPdf } from '@firecrawl/pdf-inspector-wasm';

await init();
const response = await fetch('/document.pdf');
const pdf = new Uint8Array(await response.arrayBuffer());
const result = processPdf(pdf);

console.log(result.pdfType);
console.log(result.markdown);
```

> Full API reference: [wasm/README.md](wasm/README.md)

### Rust

Install from [crates.io](https://crates.io/crates/pdf-inspector):

```bash
cargo add pdf-inspector
```

Or add it manually:

```toml
[dependencies]
pdf-inspector = "0.1"
```

```rust
use pdf_inspector::process_pdf;

let result = process_pdf("document.pdf")?;
println!("Type: {:?}", result.pdf_type);
if let Some(markdown) = &result.markdown {
    println!("{}", markdown);
}
```

> Full API reference: [docs/rust-api.md](docs/rust-api.md)

### CLI

```bash
# Install the CLI tools
cargo install pdf-inspector

# Convert PDF to Markdown
pdf2md document.pdf

# JSON output (for piping)
pdf2md document.pdf --json

# Positioned TextItem JSON, including is_underline metadata
pdf2md document.pdf --items-json

# Raw markdown only (no headers)
pdf2md document.pdf --raw

# Token-efficient output (collapses long dot leaders and similar source padding)
pdf2md document.pdf --compact

# Insert page break markers (<!-- Page N -->)
pdf2md document.pdf --pages

# Process only specific pages
pdf2md document.pdf --select-pages 1,3,5-10

# Detection only (no extraction)
detect-pdf document.pdf
detect-pdf document.pdf --json

# Detection + layout analysis (tables, columns)
detect-pdf document.pdf --analyze --json
```

From a source checkout, use `cargo run --bin pdf2md -- document.pdf` or `cargo run --bin detect-pdf -- document.pdf` instead.

## Architecture

```
PDF bytes
  │
  ├─► detector         → PdfType (TextBased / Scanned / ImageBased / Mixed)
  │
  └─► extractor
        ├─ fonts        → font widths, encodings
        ├─ content_stream → walk PDF operators → TextItems + PdfRects
        ├─ xobjects     → Form XObject text, image placeholders
        ├─ links        → hyperlinks, AcroForm fields
        └─ layout       → column detection → line grouping → reading order
              │
              ├─► tables
              │     ├─ detect_rects      → rectangle-based tables (union-find)
              │     ├─ detect_heuristic  → alignment-based tables
              │     ├─ grid              → column/row assignment → cells
              │     └─ format            → cells → Markdown table
              │
              └─► markdown
                    ├─ analysis     → font stats, heading tiers
                    ├─ preprocess   → merge headings, drop caps
                    ├─ convert      → line loop + table/image insertion
                    ├─ classify     → captions, lists, code
                    └─ postprocess  → cleanup → final Markdown
```

The document is loaded **once** via `load_document_from_path` / `load_document_from_mem` and shared between the detection and extraction stages, so there's no redundant parsing.

### Project structure

```
src/
  lib.rs                — Public API, PdfOptions builder, convenience functions
  python.rs             — PyO3 Python bindings
  types.rs              — Shared types: TextItem, TextLine, PdfRect, ItemType
  text_utils.rs         — Character/text helpers (CJK, RTL, ligatures, bold/italic)
  process_mode.rs       — ProcessMode enum (DetectOnly, Analyze, Full)
  detector.rs           — Fast PDF type detection without full document load
  glyph_names.rs        — Adobe Glyph List → Unicode mapping
  tounicode.rs          — ToUnicode CMap parsing for CID-encoded text
  extractor/            — Text extraction pipeline
  tables/               — Table detection and formatting
  markdown/             — Markdown conversion and structure detection
  bin/                  — CLI tools (pdf2md, detect_pdf)
napi/                   — Node.js/Bun bindings (napi-rs)
wasm/                   — Browser bindings (wasm-bindgen)
```

## How classification works

1. Parse the xref table and page tree (no full object load)
2. Select pages based on `ScanStrategy` (default: all pages with early exit)
3. Look for `Tj`/`TJ` (text operators) and `Do` (image operators) in content streams
4. Classify based on text operator presence across sampled pages

This detects 300+ page PDFs in milliseconds. The result includes `pages_needing_ocr` — a list of specific page numbers that lack text, enabling per-page OCR routing instead of all-or-nothing.

### Scan strategies

| Strategy | Behavior | Best for |
|---|---|---|
| `EarlyExit` (default) | Scan all pages, stop on first non-text page | Pipelines routing TextBased PDFs to fast extraction |
| `Full` | Scan all pages, no early exit | Accurate Mixed vs Scanned classification |
| `Sample(n)` | Sample `n` evenly distributed pages (first, last, middle) | Very large PDFs where speed matters more than precision |
| `Pages(vec)` | Only scan specific 1-indexed page numbers | When the caller knows which pages to check |

## Markdown output

The converter handles:

| Element | How it's detected |
|---|---|
| Headings (H1-H4) | Font size tiers relative to body text, with 0.5pt clustering |
| Bold/italic | Font name patterns (Bold, Italic, Oblique) |
| Bullet lists | `*`, `-`, `*`, `○`, `●`, `◦` prefixes |
| Numbered lists | `1.`, `1)`, `(1)` patterns |
| Letter lists | `a.`, `a)`, `(a)` patterns |
| Code blocks | Monospace fonts (Courier, Consolas, Monaco, Menlo, Fira Code, JetBrains Mono) and keyword detection |
| Tables | Rectangle-based detection from PDF drawing ops + heuristic detection from text alignment |
| Financial tables | Token splitting for consolidated numeric values |
| Captions | "Figure", "Table", "Source:" prefix detection |
| Sub/superscript | Font size and Y-offset relative to baseline |
| URLs | Converted to Markdown links |
| Hyphenation | Rejoins words broken across lines |
| Page numbers | Filtered from output |
| Drop caps | Large initial letters merged with following text |
| Dot leaders | TOC-style dots collapsed to " ... " |

## Use case: smart PDF routing

pdf-inspector was built for pipelines that process PDFs at scale. Instead of sending every PDF through OCR:

```
PDF arrives
  → pdf-inspector classifies it (~20ms)
  → TextBased + high confidence?
      YES → extract locally (~150ms), done
      NO  → send to OCR service (2-10s)
```

This saves cost and latency for the majority of PDFs that are already text-based (reports, papers, invoices, legal docs).

## Debugging

See [docs/debugging.md](docs/debugging.md) for `RUST_LOG` environment variable usage.

## License

[MIT](LICENSE)
