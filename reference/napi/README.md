# PDF Inspector

Fast PDF classification and region-based text extraction for Node.js/Bun. Native Rust performance via [napi-rs](https://napi.rs).

Built by [Firecrawl](https://firecrawl.dev) for hybrid OCR pipelines — extract text from PDF structure where possible, fall back to OCR only when needed.

## Features

- **Smart classification** — text-based / scanned / image-based / mixed in ~10–50ms, with a confidence score and per-page OCR routing.
- **Region-based extraction** — pull text from bounding boxes with per-region quality checks (`needsOcr`).
- **Layout-aware** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Lightweight** — native Rust core via napi-rs, no ML models, no external services; ~5–6 MB platform binary, TypeScript definitions included.

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
npm install @firecrawl/pdf-inspector
# or
bun add @firecrawl/pdf-inspector
```

Prebuilt binaries for **Linux x64**, **macOS ARM64**, and **Windows x64** — npm installs only the one matching your platform. No Rust toolchain needed.

## API

### `classifyPdf(buffer: Buffer): PdfClassification`

Classify a PDF as TextBased, Scanned, Mixed, or ImageBased (~10-50ms). Returns which pages need OCR.

```typescript
import { classifyPdf } from '@firecrawl/pdf-inspector'
import { readFileSync } from 'fs'

const pdf = readFileSync('document.pdf')
const result = classifyPdf(pdf)

console.log(result.pdfType)        // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
console.log(result.pageCount)      // 42
console.log(result.pagesNeedingOcr) // [5, 12, 15] (0-indexed)
console.log(result.confidence)     // 0.875
```

### `extractTextInRegions(buffer: Buffer, pageRegions: PageRegions[]): PageRegionTexts[]`

Extract text within bounding-box regions from a PDF. Designed for hybrid OCR pipelines where a layout model detects regions in rendered page images, and this function extracts text from the PDF structure for text-based pages — skipping GPU OCR.

Each region result includes a `needsOcr` flag that signals unreliable extraction (empty text, GID-encoded fonts, garbage text, encoding issues). When the cause is a suspected garbled text layer, `ocrReason` is set to `"suspected_garbled_text"`.

```typescript
import { extractTextInRegions } from '@firecrawl/pdf-inspector'

const result = extractTextInRegions(pdf, [
  {
    page: 0, // 0-indexed
    regions: [
      [0, 0, 300, 400],    // [x1, y1, x2, y2] in PDF points, top-left origin
      [300, 0, 612, 400],
    ]
  }
])

for (const region of result[0].regions) {
  if (region.needsOcr) {
    // Unreliable text — send this region to OCR instead
  } else {
    console.log(region.text) // Extracted text in reading order
  }
}
```

## Types

```typescript
interface PdfClassification {
  pdfType: string          // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
  pageCount: number
  pagesNeedingOcr: number[] // 0-indexed page numbers
  confidence: number        // 0.0 - 1.0
}

interface PageRegions {
  page: number              // 0-indexed
  regions: number[][]       // [[x1, y1, x2, y2], ...] in PDF points, top-left origin
}

interface PageRegionTexts {
  page: number
  regions: RegionText[]
}

interface RegionText {
  text: string
  needsOcr: boolean         // true when text is unreliable
  ocrReason?: string        // "suspected_garbled_text" when known
}
```

## Platforms

Prebuilt binaries ship as platform-specific packages installed automatically via `optionalDependencies`:

| Platform | Architecture | Package |
|----------|-------------|---------|
| Linux    | x64 (glibc) | `@firecrawl/pdf-inspector-linux-x64-gnu` |
| macOS    | ARM64       | `@firecrawl/pdf-inspector-darwin-arm64` |
| Windows  | x64         | `@firecrawl/pdf-inspector-win32-x64-msvc` |

## License

MIT
