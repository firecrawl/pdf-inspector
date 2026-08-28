# OCR routing integration guide

pdf-inspector helps you decide whether a PDF can be processed locally or should be sent to OCR. This guide explains how to interpret classification confidence, how to handle Mixed PDFs, and how to design routing policies for cost, quality, and accessibility requirements.

## Core signals

In Node.js, `classifyPdf` returns `pdfType`, `confidence`, `pageCount`, and `pagesNeedingOcr`. In Python, the same concepts use snake_case field names. The routing patterns below apply to every binding.

| Signal | Meaning | Recommended use |
| --- | --- | --- |
| `pdfType` | `TextBased`, `Scanned`, `ImageBased`, or `Mixed` | Primary route selector. |
| `confidence` | Local confidence score from 0.0 to 1.0 | Threshold based on workflow risk. |
| `pagesNeedingOcr` | Zero-indexed pages that appear to need OCR | Route only those pages when splitting Mixed PDFs. |
| Page or region `needsOcr` | Extraction was unreliable for that page or region | Override local processing and send to OCR or review. |

## Interpreting confidence

A confidence score is a routing signal, not a universal guarantee that extracted text is perfect. Use it together with the classification result and your tolerance for missed text or incorrect structure.

Practical guidance:

- High-confidence `TextBased` documents can usually be processed locally.
- Low-confidence `TextBased` documents may contain broken fonts, garbled text, unusual encodings, or sparse text. Treat them as suspect even when some text is extracted.
- `Mixed` documents should usually not be treated as fully local unless you have validated the pages that do not need OCR.
- A score around 0.65 can be acceptable for low-risk search indexing, but it is usually too permissive for accessibility, legal, financial, or archival workflows.

### Suggested thresholds

| Workflow | Starting threshold | Mixed PDF handling | Notes |
| --- | ---: | --- | --- |
| Search indexing | 0.60 to 0.70 | Split and OCR only flagged pages | Accept some missing or imperfect text. |
| Cost-saving triage | 0.70 to 0.75 | Split and OCR only flagged pages | Good default for high-volume pipelines. |
| RAG and structured extraction | 0.75 to 0.85 | OCR Mixed pages or whole document | Tables, headings, and reading order matter. |
| Accessibility and user-facing text | 0.85 or higher | OCR with tagging or review | Prefer structured output and verification. |
| Legal or archival preservation | 0.90 or higher | OCR plus human verification | Keep the original and audit the output. |

Tune thresholds on a representative sample of your PDFs. Measure OCR cost, extraction quality, and downstream failure modes rather than choosing a threshold from confidence alone.

## Document-level routing

This example uses the Node.js API, but the same policy shape works in Python, Rust, or WebAssembly.

```ts
import { classifyPdf } from '@firecrawl/pdf-inspector';

type RoutingPolicy = {
  minConfidence: number;
  mixed: 'split' | 'ocr' | 'review';
  lowConfidenceText: 'ocr' | 'review';
};

const costOptimizedPolicy: RoutingPolicy = {
  minConfidence: 0.65,
  mixed: 'split',
  lowConfidenceText: 'review',
};

const qualityOptimizedPolicy: RoutingPolicy = {
  minConfidence: 0.8,
  mixed: 'ocr',
  lowConfidenceText: 'ocr',
};

function routeDocument(pdf: Buffer, policy: RoutingPolicy) {
  const classification = classifyPdf(pdf);
  const allPages = Array.from({ length: classification.pageCount }, (_, page) => page);

  if (
    classification.pdfType === 'TextBased' &&
    classification.confidence >= policy.minConfidence
  ) {
    return { classification, localPages: allPages, ocrPages: [], reviewPages: [] };
  }

  if (
    classification.pdfType === 'Scanned' ||
    classification.pdfType === 'ImageBased'
  ) {
    return { classification, localPages: [], ocrPages: allPages, reviewPages: [] };
  }

  if (classification.pdfType === 'Mixed') {
    const flaggedPages =
      classification.pagesNeedingOcr.length > 0
        ? classification.pagesNeedingOcr
        : allPages;

    if (policy.mixed === 'review') {
      return { classification, localPages: [], ocrPages: [], reviewPages: flaggedPages };
    }

    if (policy.mixed === 'split') {
      const ocrSet = new Set(flaggedPages);
      const localPages = allPages.filter(page => !ocrSet.has(page));
      return { classification, localPages, ocrPages: flaggedPages, reviewPages: [] };
    }

    return { classification, localPages: [], ocrPages: allPages, reviewPages: [] };
  }

  if (classification.pdfType === 'TextBased') {
    if (policy.lowConfidenceText === 'ocr') {
      return { classification, localPages: [], ocrPages: allPages, reviewPages: [] };
    }

    return { classification, localPages: [], ocrPages: [], reviewPages: allPages };
  }

  return { classification, localPages: [], ocrPages: allPages, reviewPages: [] };
}
```

### Mixed PDF strategies

- `split`: Process unflagged pages locally and send flagged pages to OCR. Best for cost-sensitive pipelines where occasional imperfect Mixed pages are acceptable.
- `ocr`: Send the whole Mixed document, or at least all Mixed pages, to OCR. Best for quality-sensitive pipelines because page context, headers, footnotes, and tables can span pages.
- `review`: Send Mixed pages to human or automated review instead of immediate OCR. Useful when OCR is expensive and the document may be low value.

For accessibility, education, government, healthcare, and compliance workflows, avoid skipping Mixed content. Route it to OCR with tagging or to a remediation workflow.

## Page-level routing

If your pipeline already produces per-page inspection results, route pages independently. The exact field names may vary by binding, but the policy logic is the same.

```ts
type PageInspectionResult = {
  pageNumber: number;
  classification: 'Text' | 'Scanned' | 'Mixed';
  confidence: number;
  needsOcr?: boolean;
};

type PageRoute = 'local' | 'ocr' | 'review';

type PageRoutingPolicy = {
  minConfidence: number;
  mixed: PageRoute;
  lowConfidenceText: PageRoute;
};

function routePage(page: PageInspectionResult, policy: PageRoutingPolicy): PageRoute {
  if (page.needsOcr) {
    return 'ocr';
  }

  if (page.classification === 'Text' && page.confidence >= policy.minConfidence) {
    return 'local';
  }

  if (page.classification === 'Mixed') {
    return policy.mixed;
  }

  if (page.classification === 'Text') {
    return policy.lowConfidenceText;
  }

  return 'ocr';
}
```

## Async routing in production

Avoid awaiting OCR page by page. A better pattern is:

1. Inspect all pages first.
2. Bucket pages into local, OCR, and review routes.
3. Submit OCR and review jobs concurrently.
4. Use `Promise.allSettled` so one failed page does not fail the entire document.

```ts
async function routePagesForOcr(pages: PageInspectionResult[], policy: PageRoutingPolicy) {
  const routes = pages.map(page => ({ page, route: routePage(page, policy) }));

  const localPages = routes
    .filter(item => item.route === 'local')
    .map(item => item.page.pageNumber);

  const ocrJobs = routes
    .filter(item => item.route === 'ocr')
    .map(item => submitPageToOcr(item.page.pageNumber));

  const reviewJobs = routes
    .filter(item => item.route === 'review')
    .map(item => submitPageToReview(item.page.pageNumber));

  const [ocrResults, reviewResults] = await Promise.all([
    Promise.allSettled(ocrJobs),
    Promise.allSettled(reviewJobs),
  ]);

  return { localPages, ocrResults, reviewResults };
}
```

`submitPageToOcr` and `submitPageToReview` are placeholders for your OCR provider, queue, or human review system. Add concurrency limits, retries, and timeouts around them as needed.

## Accessibility and accessibility-enhanced routing

For accessibility, confidence is not only a cost signal. It also affects whether the final artifact can be consumed by screen readers, search, copy actions, and assistive technology.

Recommended accessibility rules:

- Do not rely on low-confidence local extraction, even if some text is returned. Route low-confidence pages to OCR or review.
- Treat Mixed PDFs conservatively. Prefer OCR with tagging, structured output, or human review over skipping OCR.
- Searchable PDF alone may not be enough. For accessible output, request tagged PDF, PDF/UA, or structured HTML/Markdown with headings, lists, tables, language information, and reading order.
- Charts, formulas, stamps, handwriting, and complex images may need OCR plus layout analysis, alternative text, or manual correction.
- Use a higher confidence threshold than you would for search indexing.

```ts
type AccessibilityPolicy = {
  minConfidence: number;
  mixed: 'ocr' | 'review';
  lowConfidenceText: 'ocr' | 'review';
};

const accessibilityPolicy: AccessibilityPolicy = {
  minConfidence: 0.85,
  mixed: 'ocr',
  lowConfidenceText: 'ocr',
};

function routeForAccessibility(page: PageInspectionResult, policy: AccessibilityPolicy) {
  if (page.classification === 'Text' && page.confidence >= policy.minConfidence) {
    return 'local-text';
  }

  if (page.classification === 'Mixed') {
    return policy.mixed === 'ocr' ? 'ocr-with-tagging' : 'review';
  }

  return policy.lowConfidenceText === 'ocr' ? 'ocr-with-review' : 'review';
}
```

A local-text result should still be checked for accessible structure. If headings, reading order, tables, or language metadata are missing, consider remediation even when confidence is high.

## Working with external OCR providers

- Convert `pagesNeedingOcr` from zero-indexed page numbers to the numbering convention expected by your OCR provider if needed.
- Send only flagged pages to reduce cost, but consider sending the whole document when cross-page context matters.
- Preserve document metadata, language hints, and page order when submitting OCR jobs.
- Merge local and OCR results by page number after all jobs settle.
- Keep failed pages separate so they can be retried, escalated, or marked for review.

## Read/write separation and operational safety

- Treat the PDF bytes given to pdf-inspector as immutable input.
- Perform routing before writing any output files or calling external services.
- Do not overwrite the original PDF with an OCR-enhanced or accessible version. Store derived artifacts separately.
- Use a content hash or document identifier as a cache key so routing decisions are reproducible.
- Make routing logic pure where possible: given the same inspection result and policy, it should return the same route.
- In distributed pipelines, ensure retries are idempotent and OCR outputs are written atomically.

## Decision checklist

Before choosing a threshold and Mixed strategy, answer these questions:

- What is the cost of sending too many pages to OCR?
- What is the cost of missing text or producing inaccessible output?
- Are documents used for search only, structured data extraction, human reading, compliance, or archival?
- Can Mixed pages be split safely, or do they need whole-document context?
- Do you need tagged PDF, PDF/UA, HTML, Markdown, or another accessible output format?
- What happens when OCR fails for a page?

Start with a conservative policy, monitor routing volume, and adjust thresholds using real documents from your workload.