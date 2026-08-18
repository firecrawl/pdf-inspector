# PDF Inspector

Fast PDF classification and region-based text extraction for Node.js/Bun. Native Rust performance via [napi-rs](https://napi.rs).

Built by [Firecrawl](https://firecrawl.dev) for hybrid OCR pipelines — extract text from PDF structure where possible, fall back to OCR only when needed.

## Features

- **Smart classification** — text-based / scanned / image-based / mixed in ~10–50ms, with a confidence score and per-page OCR routing.
- **Region-based extraction** — pull text from bounding boxes with per-region quality checks (`needsOcr`).
- **Layout-aware** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Selective OCR** — `Auto` routes only pages rejected by native extraction and returns source/model provenance plus hosted-fallback recommendations.
- **External artifacts** — the native package embeds no OCR models, PDFium, or ONNX Runtime; clean `Auto` requests never load or download them.

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

Prebuilt binaries for **Linux x64/ARM64** (glibc and musl/Alpine), **macOS ARM64**, and **Windows x64** — npm installs only the one matching your platform. No Rust toolchain needed.

OCR calls that route work require compatible PDFium and ONNX Runtime shared
libraries. Set `PDFIUM_LIB_PATH` and `ORT_DYLIB_PATH` when they are not on the
platform library search path. The pinned OCR model set is downloaded and
checksum-verified on the first routed page; use `offline: true` with a warm
cache or `modelDirectory` to prohibit network access. See the
[OCR runtime setup guide](https://github.com/firecrawl/pdf-inspector/blob/main/docs/ocr-runtime.md)
for pinned downloads, supported platforms, and hosted-fallback behavior.

## API

### `processPdfWithOcr(buffer: Buffer, options?: OcrOptions): Promise<OcrPdfResult>`

Run native extraction first and OCR only the pages selected by its quality
signals. The default mode is `Auto`; `Off` returns the same detailed result
shape without external runtime work, and `Force` OCRs every selected page.
The work runs on the libuv thread pool and never blocks Node's event loop.

```typescript
import { OcrMode, processPdfWithOcr } from '@firecrawl/pdf-inspector'

const result = await processPdfWithOcr(pdf, {
  mode: OcrMode.Auto,
  pageNumbers: [1, 3], // 1-indexed
})

for (const page of result.pages) {
  console.log(page.pageNumber, page.provenance.source)
}
console.log(result.pagesRoutedToOcr)
console.log(result.pagesRecommendingHosted)
```

For offline deployments, pass `modelDirectory` and `offline: true`. Other
controls include `dpi`, `minimumConfidence`,
`hostedRecommendationConfidence`, and `password`.

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

### Async variants

`processPdf`, `classifyPdf`, and `extractPagesMarkdown` are synchronous and parse on the calling thread — in Node, that's the event loop. For a one-off call in a script that's fine, but in a server a large document can hold the loop for tens to hundreds of milliseconds.

`processPdfAsync`, `classifyPdfAsync`, and `extractPagesMarkdownAsync` take the same arguments and produce the same results, but run the parse on the libuv thread pool and return a promise, keeping the event loop free. The input buffer is copied before the call returns, so it's safe to reuse or mutate immediately:

```typescript
import { classifyPdfAsync, extractPagesMarkdownAsync } from '@firecrawl/pdf-inspector'

const classification = await classifyPdfAsync(pdf)
if (classification.pdfType === 'TextBased') {
  const { pages } = await extractPagesMarkdownAsync(pdf)
  // ...
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

interface OcrPdfResult {
  markdown: string
  pages: OcrPageResult[]              // 1-indexed pages + provenance
  pageCount: number
  pagesRecommendedForOcr: number[]
  pagesRoutedToOcr: number[]
  pagesRecommendingHosted: number[]
  ocrReasonsByPage: PageOcrReasons[]
  pagesWithTables: number[]
  pagesWithColumns: number[]
  isComplex: boolean
  processingTimeMs: number
  renderTimeMs: number
  ocrTimeMs: number
}
```

## Platforms

Prebuilt binaries ship as platform-specific packages installed automatically via `optionalDependencies`:

| Platform | Architecture | Package |
|----------|-------------|---------|
| Linux    | x64 (glibc)         | `@firecrawl/pdf-inspector-linux-x64-gnu` |
| Linux    | x64 (musl/Alpine)   | `@firecrawl/pdf-inspector-linux-x64-musl` |
| Linux    | ARM64 (glibc)       | `@firecrawl/pdf-inspector-linux-arm64-gnu` |
| Linux    | ARM64 (musl/Alpine) | `@firecrawl/pdf-inspector-linux-arm64-musl` |
| macOS    | ARM64               | `@firecrawl/pdf-inspector-darwin-arm64` |
| Windows  | x64                 | `@firecrawl/pdf-inspector-win32-x64-msvc` |

## License

MIT
