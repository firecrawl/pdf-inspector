# OCR routing decision guide

pdf-inspector can classify PDFs locally and return page-level OCR hints. Use this guide to decide when to extract text locally, when to send a whole document to OCR, and when to OCR only selected pages.

## How to interpret classifications

| Classification | Meaning | Recommended action |
| --- | --- | --- |
| `TextBased` | The document has enough native text to extract locally. | Extract locally when confidence is high. |
| `Scanned` | Most pages look like scans or image-only pages. | Send the full document to OCR when confidence is high. |
| `ImageBased` | Pages are dominated by images; native text is absent or unreliable. | Treat like OCR candidates, but consider business value before paying for OCR. |
| `Mixed` | Some pages are text-based and some pages need OCR. | Use page-level routing. |

Confidence is a routing heuristic, not a calibrated probability. Validate thresholds on a representative corpus.

## Recommended default thresholds

| Scenario | Default rule | Suggested status |
| --- | --- | --- |
| `TextBased` and confidence >= 0.90 | Extract locally without OCR. | `local_extract` |
| `Scanned` and confidence >= 0.80 | Send the full document to OCR. | `ocr_full_document` |
| `Mixed` | OCR only pages reported by `pages_needing_ocr` / `pagesNeedingOcr`. | `partial_ocr` |
| Per-page confidence is exposed | OCR pages with page confidence < 0.70. | `partial_ocr` |
| Confidence is below the threshold for the classification | Route to review or an asynchronous retry queue. | `review` |
| Corrupt or unreadable PDF | Preserve the original file and route to manual review. | `failed_corrupt` |
| Encrypted PDF without password | Do not retry automatically; request a password or manual handling. | `need_password` |

Start with these defaults and tune them from production metrics: OCR cost, missed text, garbled output, and review volume.

## Routing policy

1. Attempt classification.
2. If the file cannot be opened, route to manual review and preserve the original bytes.
3. If the file is encrypted, route to password collection or manual review.
4. If `TextBased` has high confidence, extract locally.
5. If `Scanned` has high confidence, send the whole document to OCR.
6. If the result is `Mixed`, or if any page is flagged as needing OCR, use page-level OCR.
7. If confidence is low, route to review or asynchronous retry.
8. If OCR is unavailable, use a circuit breaker and queue the work; do not silently drop the document.

## Page-level routing

Use the per-page list returned by the inspector as the primary page selection:

- Rust/Python field: `pages_needing_ocr`
- Node.js/WASM field: `pagesNeedingOcr`
- Page indexes are 0-based.

If your binding exposes per-page confidence, combine both signals:

- OCR pages already present in `pages_needing_ocr`.
- OCR pages whose page confidence is below `0.70`.
- Keep successfully extracted pages even if some pages fail OCR.
- Retry or review only the failed pages.

## Fallback behavior

Make fallback behavior explicit instead of relying on ad-hoc exception handling.

| Failure or low-confidence case | Recommended fallback | Suggested status |
| --- | --- | --- |
| PDF cannot be opened | Save the original file and route to manual review. | `failed_corrupt` |
| Encrypted PDF without password | Ask for the password or route to manual processing. | `need_password` |
| Classification unavailable | If local text exists, extract locally; otherwise queue for OCR. | `degraded_rule_based` |
| OCR timeout or 5xx | Circuit-break, queue asynchronously, or use a local OCR fallback. | `ocr_fallback` |
| Low confidence or `Mixed` | OCR only suspicious pages or route to review. | `partial_ocr` / `review` |
| Some OCR pages fail | Keep successful pages and retry only failed pages. | `partial_success` |

## Binding examples

The examples below show routing only. Replace the OCR calls with your provider or queue integration.

### Rust

```rust
use std::path::Path;
use pdf_inspector::{classify_pdf, PdfType};

pub enum Route {
    LocalExtract,
    FullOcr,
    PageOcr { pages: Vec<u32> },
    Review,
}

pub fn route_pdf(path: &Path) -> Result<Route, Box<dyn std::error::Error>> {
    let inspection = classify_pdf(path)?;
    let pages: Vec<u32> = inspection
        .pages_needing_ocr
        .iter()
        .map(|page| *page as u32)
        .collect();

    Ok(match &inspection.pdf_type {
        PdfType::TextBased if inspection.confidence >= 0.90 => Route::LocalExtract,
        PdfType::Scanned if inspection.confidence >= 0.80 => Route::FullOcr,
        PdfType::Mixed => Route::PageOcr { pages: pages.clone() },
        _ if !pages.is_empty() => Route::PageOcr { pages: pages.clone() },
        _ => Route::Review,
    })
}
```

### Python

```python
import pdf_inspector

def route_pdf(pdf_source) -> dict:
    try:
        inspection = pdf_inspector.classify_pdf(pdf_source)
    except Exception as exc:
        return {'action': 'manual_review', 'reason': str(exc)}

    if inspection.pdf_type == 'text_based' and inspection.confidence >= 0.90:
        return {'action': 'local_extract', 'confidence': inspection.confidence}

    if inspection.pdf_type == 'scanned' and inspection.confidence >= 0.80:
        return {'action': 'ocr_full_document', 'confidence': inspection.confidence}

    pages = getattr(inspection, 'pages_needing_ocr', []) or []
    if inspection.pdf_type == 'mixed' or pages:
        return {'action': 'ocr_pages', 'pages': pages}

    return {'action': 'review', 'confidence': inspection.confidence}
```

### Node.js

```javascript
import { classifyPdf } from '@firecrawl/pdf-inspector';

export function routePdf(buffer) {
  let inspection;
  try {
    inspection = classifyPdf(buffer);
  } catch (error) {
    return { action: 'manual_review', reason: String(error) };
  }

  if (inspection.pdfType === 'TextBased' && inspection.confidence >= 0.90) {
    return { action: 'local_extract', confidence: inspection.confidence };
  }

  if (inspection.pdfType === 'Scanned' && inspection.confidence >= 0.80) {
    return { action: 'ocr_full_document', confidence: inspection.confidence };
  }

  const pages = inspection.pagesNeedingOcr ?? [];
  if (inspection.pdfType === 'Mixed' || pages.length > 0) {
    return { action: 'ocr_pages', pages };
  }

  return { action: 'review', confidence: inspection.confidence };
}
```

### Browser WASM

```javascript
import init, * as inspector from '@firecrawl/pdf-inspector-wasm';

await init();

const response = await fetch('/document.pdf');
const bytes = new Uint8Array(await response.arrayBuffer());
const classify = inspector.classifyPdf ?? inspector.processPdf;

let inspection;
try {
  inspection = classify(bytes);
} catch (error) {
  // Route corrupt or unreadable files to manual review.
  throw error;
}

const pages = inspection.pagesNeedingOcr ?? [];

const route =
  inspection.pdfType === 'TextBased' && inspection.confidence >= 0.90
    ? 'local_extract'
    : inspection.pdfType === 'Scanned' && inspection.confidence >= 0.80
      ? 'ocr_full_document'
      : inspection.pdfType === 'Mixed' || pages.length > 0
        ? 'ocr_pages'
        : 'review';

console.log(route, pages);
```

## Testing strategy

Prefer deterministic CI tests over live OCR calls. Use fixed PDF fixtures and mock the OCR dependency for the main routing paths.

Key cases to cover:

| Case | Expected behavior |
| --- | --- |
| Text-based fixture | Returns local extraction and does not call OCR. |
| Scanned fixture | Triggers full-document OCR. |
| Mixed fixture | Triggers OCR only for flagged pages. |
| Low-confidence fixture | Routes to review or async retry. |
| Corrupt fixture | Routes to manual review and preserves the input. |
| Encrypted fixture | Routes to password request or manual review. |
| OCR timeout or 5xx | Uses fallback queue or local OCR fallback. |
| Partial OCR failure | Keeps successful pages and retries failed pages. |

Run real OCR E2E tests in a nightly or manually triggered pipeline so network and model instability do not block trunk merges. Include at least one Windows CI runner if file paths or binary fixtures are part of the integration surface.
