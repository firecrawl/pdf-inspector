# Classification and OCR routing knobs

pdf-inspector classifies PDFs and reports confidence plus per-page OCR routing. Tuning is expressed as a pure-data policy. Bindings can accept it as optional JSON-compatible input and forward it to the Rust core. When no policy is provided, built-in defaults remain unchanged.

All fields are optional; omitted fields keep built-in defaults.

The machine-readable contract is in schemas/classification-policy.schema.json.

## Policy fields

- sampling.max_pages: limit how many pages are inspected.
- sampling.max_content_streams: limit how many content streams are sampled.
- sampling.max_sample_bytes: limit how many content bytes are sampled.
- thresholds.min_text_confidence: minimum confidence for a text-based decision.
- thresholds.min_text_objects: minimum text objects per page.
- thresholds.min_text_density: minimum text density per page.
- thresholds.max_image_coverage: maximum image coverage before OCR routing.
- mixed: prefer_text, prefer_ocr, or route_per_page.

## Example

```python
policy = {
    'sampling': {'max_pages': 20},
    'thresholds': {'min_text_confidence': 0.55},
    'mixed': 'route_per_page',
}
```

```javascript
const policy = {
  sampling: { max_pages: 20 },
  thresholds: { min_text_confidence: 0.55 },
  mixed: 'route_per_page',
};
```

## Tuning guidance

- Lower text thresholds reduce OCR cost but may miss scanned pages.
- Raise thresholds for high-recall pipelines.
- Limit sampling for large documents to keep inspection fast.
- Use route_per_page for mixed documents that need page-level routing.
