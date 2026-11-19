# OCR routing guide

`pdf-inspector` is a classifier and text extractor, not an OCR engine. Use its
classification result to decide which documents or pages should be sent to an
OCR service. The classifier does not perform OCR, and its `confidence` value is
a routing signal rather than a probability calibrated for every document
corpus.

## The routing signals

The high-level result exposes four complementary signals:

- `pdf_type`: `TextBased`, `Scanned`, `ImageBased`, or `Mixed` (lowercase in
  the Python API).
- `confidence`: a `0.0`–`1.0` score produced by the detector's heuristics.
- `pages_needing_ocr`: page numbers that do not have a reliable extractable
  text layer. Full processing results in Rust, Node.js, and Python use
  1-indexed pages for this field. The lightweight Python `PdfClassification`
  and native Node.js `classifyPdf` results use 0-indexed page numbers. Check
  the binding-specific API below.
- `ocr_reasons_by_page`: machine-readable reasons for pages in
  `pages_needing_ocr`, such as `scanned`, `no_text`, `vector_text`, and
  `suspected_garbled_text`. This diagnostic field is available on full
  processing results; lightweight classification results expose page numbers
  and confidence only.

`ocr_recommended` is also available in the low-level Rust detector result. It
is `true` when OCR is recommended for extraction, including cases where images
provide essential context even though a text layer exists.

## Recommended decision policy

Start with a conservative policy and tune it against representative documents
from your own corpus. Do not treat the example thresholds below as universal
accuracy guarantees.

| Detector result | Recommended action |
|---|---|
| `TextBased` with confidence `>= 0.90`, no pages needing OCR, and no encoding issues | Extract locally. |
| `TextBased` with confidence `0.75`–`0.90` | Extract locally if the application can validate the result; otherwise use OCR as a fallback. |
| `TextBased` below `0.75`, or with encoding issues | Prefer OCR, or run a validation/second-pass policy before accepting extracted text. |
| `Mixed` | Extract pages that do not need OCR and route only `pages_needing_ocr` to OCR. |
| `Scanned` or `ImageBased` with high confidence | Route all pages to OCR. |
| Any result with a parse, encryption, or I/O error | Handle the error explicitly. Do not silently treat it as text or scanned content. |

These ranges are application policy examples. The library's detector currently
returns a heuristic score and does not expose a calibrated corpus-specific
threshold. If false negatives are expensive, lower the threshold at which your
application chooses OCR. If OCR cost is the dominant concern, validate a
sample of locally extracted results before accepting a more aggressive policy.

For a `Mixed` document, page-level routing is normally preferable to sending
the entire document to OCR. If the OCR provider cannot accept individual pages,
fall back to whole-document OCR rather than silently dropping the affected
pages.

## Rust

Use `PdfProcessResult` for full processing, or `PdfTypeResult` for detection
only. The Rust detector's `pages_needing_ocr` and `ocr_reasons_by_page` values
are 1-indexed.

```rust
use pdf_inspector::{detect_pdf, PdfType};

let result = detect_pdf("document.pdf")?;

match result.pdf_type {
    PdfType::TextBased
        if result.confidence >= 0.90
            && result.pages_needing_ocr.is_empty() => {
        // Extract locally.
    }
    PdfType::Mixed => {
        for page in &result.pages_needing_ocr {
            // Route this 1-indexed page to OCR.
            println!("OCR page {page}");
        }
    }
    PdfType::Scanned | PdfType::ImageBased => {
        // Route the document to OCR.
    }
    PdfType::TextBased => {
        // Borderline or otherwise unsuitable for blind local extraction.
        // Validate the text or use OCR according to application policy.
    }
}
```

For large documents, choose the scan strategy deliberately. `Sample(n)`
reduces classification work but can miss a page outside the sample; `Full`
provides the most complete document-level view; `Pages(vec)` is useful when the
caller already knows which pages need inspection.

```rust
use pdf_inspector::{
    process_pdf_with_options, DetectionConfig, PdfOptions, ProcessMode,
    ScanStrategy,
};

let result = process_pdf_with_options(
    "large.pdf",
    PdfOptions::new()
        .mode(ProcessMode::DetectOnly)
        .detection(DetectionConfig {
            strategy: ScanStrategy::Sample(8),
            ..Default::default()
        }),
)?;
```

## Python

The full `PdfResult` exposes 1-indexed `pages_needing_ocr` values and per-page
OCR reasons. The lightweight `PdfClassification` returned by
`classify_pdf()` exposes 0-indexed page values; do not mix the two result types
without converting the page numbering.

```python
import pdf_inspector

result = pdf_inspector.process_pdf("document.pdf")

if (
    result.pdf_type == "text_based"
    and result.confidence >= 0.90
    and not result.pages_needing_ocr
    and not result.has_encoding_issues
):
    text = result.markdown
elif result.pdf_type == "mixed":
    # These page numbers are 1-indexed on the full PdfResult.
    ocr_pages = set(result.pages_needing_ocr)
    local_pages = [page for page in range(1, result.page_count + 1)
                   if page not in ocr_pages]
    # Extract local_pages and send ocr_pages to the OCR provider.
else:
    # Scanned, image-based, low-confidence, or encoding-issue result.
    send_document_to_ocr()
```

When using `extract_pages_markdown()`, inspect each `PageMarkdown.needs_ocr`
value and the accompanying `ocr_reasons_by_page` entries. This is the preferred
path when the downstream OCR service can process individual pages.

## Node.js

The native Node binding's `classifyPdf()` result uses 0-indexed
`pagesNeedingOcr` values.

```ts
import { readFileSync } from "node:fs";
import { classifyPdf } from "@firecrawl/pdf-inspector";

const result = classifyPdf(readFileSync("document.pdf"));

if (
  result.pdfType === "TextBased" &&
  result.confidence >= 0.90 &&
  result.pagesNeedingOcr.length === 0
) {
  // Extract locally with the package's extraction API.
} else if (result.pdfType === "Mixed") {
  for (const page of result.pagesNeedingOcr) {
    // Route this 0-indexed page to OCR.
    console.log(`OCR page ${page}`);
  }
} else {
  // Route the whole document to OCR, or apply your fallback policy.
}
```

## Browser WebAssembly

The WASM binding runs locally in the browser and does not upload PDF bytes. For
large documents, run extraction in a Web Worker so the UI remains responsive.
The current WASM README documents `detectPdf(pdf)` and `processPdf(pdf, opts)`;
use the returned classification and page metadata to apply the same policy as
the native bindings.

```ts
import init, { detectPdf } from "@firecrawl/pdf-inspector-wasm";

await init();
const result = detectPdf(pdfBytes);

if (result.pdfType === "TextBased" && result.confidence >= 0.90) {
  // Continue with local extraction.
} else {
  // Route the document or the reported pages to OCR.
}
```

Image-only documents still require a separate OCR implementation. WASM does
not provide OCR itself.

## Fallbacks and operational guidance

- **Mixed PDFs:** prefer page-level routing; preserve page order when merging
  OCR output with locally extracted text.
- **Low confidence:** treat the result as uncertain, not as proof that the PDF
  is scanned. Validate the extracted text or use OCR according to your cost and
  recall requirements.
- **Encoding issues:** `has_encoding_issues` and
  `suspected_garbled_text` indicate that a technically present text layer may
  be unusable. OCR the affected page or document.
- **Encrypted PDFs:** provide the password through the supported API when the
  document is authorized for processing. Otherwise report an actionable error.
- **Corrupt or unsupported PDFs:** keep the original error and route to a
  recovery path. Do not infer a classification from a failed parse.
- **Sampling:** larger samples and `Full` improve coverage at the cost of more
  parsing work. Benchmark with representative PDFs before changing the
  default strategy.

## Measuring and tuning a policy

Use a labelled validation set containing text PDFs, scanned PDFs, mixed PDFs,
font-encoding failures, and encrypted/corrupt inputs. Measure at least:

- false local-extraction rate (documents accepted without OCR but needing it);
- unnecessary OCR rate (documents sent to OCR despite reliable local text);
- page-level routing accuracy for `Mixed` documents;
- classification latency and OCR cost.

Change one policy variable at a time, keep the classifier version fixed during
a comparison, and retain the corpus and results so threshold changes remain
reproducible. A threshold that works for reports may not work for invoices,
forms, or PDFs generated by a different toolchain.

## Related API references

- [Rust API](rust-api.md)
- [Python API](python.md)
- [Node.js API](../napi/README.md)
- [WebAssembly API](../wasm/README.md)
- [Benchmarking](benchmarking.md)

This guide describes routing policy at the application boundary. It does not
change detector defaults or claim that the confidence score is calibrated as a
probability.
