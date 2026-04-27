# PDF Inspector

Fast PDF classification and region-based text extraction for Node.js/Bun. Native Rust performance via [napi-rs](https://napi.rs).

Built by [Firecrawl](https://firecrawl.dev) for hybrid OCR pipelines — extract text from PDF structure where possible, fall back to OCR only when needed.

## Install

```bash
npm install @firecrawl/pdf-inspector
# or
bun add @firecrawl/pdf-inspector
```

Prebuilt binaries included for **linux-x64** and **macOS ARM64**. No Rust toolchain needed.

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

Each region result includes a `needsOcr` flag that signals unreliable extraction (empty text, GID-encoded fonts, garbage text, encoding issues).

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
}
```

## Platforms

| Platform | Architecture | Supported |
|----------|-------------|-----------|
| Linux    | x64         | Yes       |
| macOS    | ARM64       | Yes       |

## License

MIT
