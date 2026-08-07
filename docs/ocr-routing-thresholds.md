# OCR routing thresholds and confidence

`pdf-inspector` returns a document-level confidence score in `0.0-1.0` and a list of pages that should be sent to OCR. The score estimates how likely the document can be extracted as native text.

## Default behavior

The default routing thresholds are:

- `text_based_min_confidence: 0.85`
- `needs_ocr_max_confidence: 0.35`
- `mixed_page_threshold: 0.25`

A document with confidence at least `0.85` is normally treated as text-based. A document at or below `0.35` is treated as scanned/image-based. Values between the two are candidates for mixed routing, and per-page signals decide which pages need OCR. `mixed_page_threshold` is the fraction of pages that must disagree before the document is labeled mixed.

## Examples

Run the Python example:

```bash
python examples/ocr_routing_thresholds.py tests/fixtures/thermo-freon12.pdf
```

Run the Node example after building the NAPI package:

```bash
node examples/ocr_routing_thresholds.mjs tests/fixtures/thermo-freon12.pdf
```

Both examples print the document class, confidence, and pages needing OCR. They also pass threshold overrides where the binding supports them; older bindings fall back to default routing.

## Recommended presets

- Avoid OCR costs: keep `text_based_min_confidence` near `0.85` and `needs_ocr_max_confidence` near `0.35`.
- Maximize extraction reliability: raise `text_based_min_confidence` toward `0.95` so uncertain pages are routed to OCR.
- Reduce OCR for mostly scanned documents: lower `needs_ocr_max_confidence` toward `0.25` only if false text detection is acceptable.

## Compatibility

The `0.0-1.0` confidence range and the route names are stable. Documented default thresholds are a behavior contract; changing defaults should be handled as a behavior-breaking change.
