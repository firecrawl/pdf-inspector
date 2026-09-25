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

### `extractTextWithPositions(buffer: Buffer, pages?: number[], options?: FrameOptions): TextItem[]`

An async variant, `extractTextWithPositionsAsync`, takes the same arguments and
resolves to the same items (see [Async variants](#async-variants)).

Every text item (plus image placeholders, links and form fields) with its font
and position. `x`/`y` are PDF points relative to the page's **visible page
box** (`CropBox ∩ MediaBox`, else the MediaBox), origin at the box's lower-left
corner with `y` growing upward. `extractTextInRegions` reads its regions
relative to the same box but from its top-left corner with `y` growing
downward, so flip with the box height: `boxHeight - y`. For text items `y` is
the baseline and `height` the font size, so
`[x, boxHeight - y - height, x + width, boxHeight - y]` covers the glyph band
above the baseline (descenders fall below it); for image, link and form-field
items `y` is the rect bottom and that box is exact. Pages whose CropBox equals
the MediaBox at `(0, 0)` are unaffected.

By default the page `/Rotate` is not applied and a page whose text is
predominantly rotated is turned so that text reads left-to-right (this is the
`"sheet"` frame; `extractTextWithPositionsAndRotations` reports which pages
were turned). Pass `{ frame: "display" }` to get every item in the rendered
page's frame instead — the visible page box turned clockwise by the page's
inheritable `/Rotate`, lower-left origin, `y` up, with the turn of a rotated
page undone — so `x`/`y`/`width`/`height` and `rotation` describe the item as
a renderer draws it. Pages with `/Rotate 0` whose text is not predominantly
rotated are identical in both frames.

`fontWeight` is the font's weight class on the 100..900 scale shared by CSS
`font-weight` and the OS/2 `usWeightClass` field (400 regular, 700 bold), read
from the embedded font program's OS/2 table, else the FontDescriptor's
`/FontWeight`, else a weight word in the font name ("Light", "Medium", "-Md",
"Black", "W6"); it is omitted when none of them says. `isBold` is unchanged
and independent of it, so a medium face reports `fontWeight: 500` with
`isBold: false`. Pass `{ boldFromWeight: true }` to also read `isBold` from a
weight class of 600 (SemiBold) or more — or of `boldWeightThreshold`, any
class on the 100..900 scale — and to merge adjacent runs by that verdict, so a
heavier run inside a lighter paragraph keeps its own item while runs whose
weights differ but agree on bold merge as usual.

`boldSource` says where `isBold` came from — `"FontName"` (a bold word or
style abbreviation in the font name), `"FontFlags"` (the FontDescriptor's
ForceBold flag or the embedded program's bold selection), `"WeightClass"`
(the weight class, with `boldFromWeight`) or `"Painted"` (text filled and
stroked to look heavier), the first of them in that order when more than one
says bold — so a face whose name says Bold over a `fontWeight` of 400 can be
told from one whose weight class says so. It is omitted when `isBold` is
`false`.

`fixedPitch` is `true` when the FontDescriptor's FixedPitch flag or the
embedded program's `post` table says the font is monospaced, else measured
from the font's width table: `true` when a dozen or more of its glyphs share
one advance, `false` when two differ. It is omitted when the font declares
nothing and no two advances differ but fewer than a dozen share one. Many
producers write `/Flags 4`
whatever the face, so the flag is only ever read as a yes.

`fillColor` and `strokeColor` are the colours a run was shown with, as sRGB
`[red, green, blue]` arrays of 0..255: its glyphs are filled with the first in
render modes 0, 2, 4 and 6 and outlined with the second in modes 1, 2, 5 and
6. DeviceRGB is read as sRGB, DeviceGray as three equal components and
DeviceCMYK converted the way the PDF specification converts it to DeviceRGB;
ICCBased spaces are read by their component count and Indexed spaces through
their palette. A colour is omitted for any other colour space (Separation,
DeviceN, Pattern, CalRGB, Lab, ...). `renderMode` is the text render mode
(`Tr`) the run was shown with, 0..7: runs in mode 3 (invisible, the mode of OCR
text layers) and mode 7 (clipping only) put no glyphs on the page, so a caller
can tell visible text from invisible text. The colours and the mode are
graphics state — they hold across text objects, `q`/`Q` save and restore them,
and a Form XObject starts with the paint it was invoked under — and reporting
them changes nothing about which runs are extracted. All three are omitted for
image, link and form-field items, and a merged item keeps its first run's.

`legacySymbolRewrite: true` marks items whose decoded text includes a character
changed by legacy symbol cleanup. Merged items retain this evidence from either
source, and split items conservatively inherit it. The field is omitted when
that cleanup did not change a character; absence is not a general guarantee of
decoding accuracy. Consumers correcting other text can use the marker to avoid
treating a rewritten symbol as an authoritative Unicode value.

```typescript
import { extractTextWithPositions } from '@firecrawl/pdf-inspector'

for (const item of extractTextWithPositions(pdf, [1])) { // pages are 1-indexed
  console.log(item.page, item.text, item.x, item.y, item.fontSize)
}

// Boxes as a renderer draws the page (`/Rotate` applied)
const rendered = extractTextWithPositions(pdf, undefined, { frame: 'display' })

// Bold also from the weight class, SemiBold (600) and heavier
const weighted = extractTextWithPositions(pdf, undefined, { boldFromWeight: true })

// ... or from Bold (700) and heavier
const heavier = extractTextWithPositions(pdf, undefined, { boldFromWeight: true, boldWeightThreshold: 700 })
```

### `extractTextWithPositionsAndRotations(buffer: Buffer, pages?: number[], options?: FrameOptions): PositionedText`

`extractTextWithPositions` plus `pageRotations`, one `{ page, rotation: 'ccw' | 'cw' }`
entry per page whose text was predominantly rotated and therefore turned in the
`"sheet"` frame. With `{ frame: "display" }` the items are in the rendered
page's frame and the entries only report which pages were turned.

### `extractTextInRegions(buffer: Buffer, pageRegions: PageRegions[], options?: FrameOptions): PageRegionTexts[]`

Extract text within bounding-box regions from a PDF. Designed for hybrid OCR pipelines where a layout model detects regions in rendered page images, and this function extracts text from the PDF structure for text-based pages — skipping GPU OCR.

Region bboxes are `[x1, y1, x2, y2]` in PDF points with a top-left origin,
relative to the visible page box. By default they are read in the `"sheet"`
frame: the box as laid out in the content stream, `/Rotate` not applied, the
frame `extractTextWithPositions` reports items in flipped to a top-left
origin. The sheet frame matches a rendered page image only when both hold:
the page has `/Rotate 0`, and its text is not predominantly rotated. A page
whose text is predominantly rotated is turned in the sheet frame so that text
reads left-to-right (`extractTextWithPositionsAndRotations` reports which
pages were turned), so its sheet-frame bboxes do not match the rendered image
even with `/Rotate 0`. Pass `{ frame: "display" }` to give bboxes on the
rendered page (the visible box turned clockwise by the page's inheritable
`/Rotate`; the page-level turn of a predominantly rotated page is undone
first, while individual runs keep their own `rotation`), as a layout model
working on page images reports them, whatever the page's `/Rotate` or text
direction. `extractTablesInRegions` takes the same options, `boldFromWeight`
included (see `extractTextWithPositions`).

Each region result includes a `needsOcr` flag that signals unreliable extraction (empty text, GID-encoded fonts, garbage text, encoding issues). When the cause is a suspected garbled text layer, `ocrReason` is set to `"suspected_garbled_text"`.

```typescript
import { extractTextInRegions } from '@firecrawl/pdf-inspector'

const result = extractTextInRegions(pdf, [
  {
    page: 0, // 0-indexed
    regions: [
      [0, 0, 300, 400],    // [x1, y1, x2, y2] in PDF points, top-left origin of the visible page box (CropBox)
      [300, 0, 612, 400],
    ]
  }
])

// The same call with bboxes taken from a rendered page image
const onRendered = extractTextInRegions(
  pdf,
  [{ page: 0, regions: [[0, 0, 300, 400]] }],
  { frame: 'display' },
)

for (const region of result[0].regions) {
  if (region.needsOcr) {
    // Unreliable text — send this region to OCR instead
  } else {
    console.log(region.text) // Extracted text in reading order
  }
}
```

### Async variants

`processPdf`, `classifyPdf`, `extractPagesMarkdown`, and `extractTextWithPositions` are synchronous and parse on the calling thread — in Node, that's the event loop. For a one-off call in a script that's fine, but in a server a large document can hold the loop for tens to hundreds of milliseconds, and an unusual one for much longer.

`processPdfAsync`, `classifyPdfAsync`, `extractPagesMarkdownAsync`, and `extractTextWithPositionsAsync` take the same arguments and produce the same results, but run the parse on the libuv thread pool and return a promise, keeping the event loop free. Invalid options still throw when the call is made. The input buffer is copied before the call returns, so it's safe to reuse or mutate immediately:

```typescript
import { classifyPdfAsync, extractPagesMarkdownAsync, extractTextWithPositionsAsync } from '@firecrawl/pdf-inspector'

const classification = await classifyPdfAsync(pdf)
if (classification.pdfType === 'TextBased') {
  const { pages } = await extractPagesMarkdownAsync(pdf)
  // ...
}

const items = await extractTextWithPositionsAsync(pdf, [1, 2], { frame: 'display' })
```

## Types

```typescript
interface PdfClassification {
  pdfType: string          // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
  pageCount: number
  pagesNeedingOcr: number[] // 0-indexed page numbers
  confidence: number        // 0.0 - 1.0
}

interface PdfResult {       // processPdf / detectPdf (excerpt)
  pdfType: string
  markdown?: string         // omitted by detectPdf
  pageCount: number
  // The document information dictionary's entries, decoded as PDF text
  // strings (UTF-16 or UTF-8 after a byte order mark, PDFDocEncoding
  // otherwise); each omitted when missing or not a string.
  title?: string
  author?: string
  subject?: string
  keywords?: string
  creator?: string          // the application the document was authored in
  producer?: string         // the application that wrote the PDF
  creationDate?: string     // as written, e.g. "D:20240115103000+01'00'"
  modDate?: string
  // ...
}

interface PageRegions {
  page: number              // 0-indexed
  regions: number[][]       // [[x1, y1, x2, y2], ...] in PDF points, top-left origin of the visible page box
                            // (sheet frame by default; the rendered page with { frame: "display" })
}

interface FrameOptions {
  frame?: "sheet" | "display" // coordinate frame of items and region bboxes; "sheet" by default
  boldFromWeight?: boolean    // also read isBold from fontWeight >= boldWeightThreshold and merge runs by that verdict; false by default
  boldWeightThreshold?: number // the weight class boldFromWeight reads bold from, 100..900; 600 by default
}

interface PositionedText {
  items: TextItem[]            // as returned by extractTextWithPositions
  pageRotations: PageRotation[] // one entry per page whose text was predominantly rotated
}

interface PageRotation {
  page: number              // 1-indexed, matching TextItem.page
  rotation: string          // "ccw" | "cw": how the page was turned in the "sheet" frame
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
