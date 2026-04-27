# Python API

Python bindings via [PyO3](https://pyo3.rs). Requires Rust toolchain for building from source.

## Install

```bash
pip install maturin
maturin develop --release
```

## Usage

```python
import pdf_inspector

# Full processing: detect + extract + convert to Markdown
result = pdf_inspector.process_pdf("document.pdf")
print(result.pdf_type)      # "text_based", "scanned", "image_based", "mixed"
print(result.confidence)     # 0.0 - 1.0
print(result.page_count)     # number of pages
print(result.markdown)       # Markdown string or None

# Process specific pages only
result = pdf_inspector.process_pdf("document.pdf", pages=[1, 3, 5])

# Process from bytes (no filesystem needed)
with open("document.pdf", "rb") as f:
    result = pdf_inspector.process_pdf_bytes(f.read())

# Fast detection only (no text extraction)
result = pdf_inspector.detect_pdf("document.pdf")
if result.pdf_type == "text_based":
    print("Can extract locally!")
else:
    print(f"Pages needing OCR: {result.pages_needing_ocr}")

# Plain text extraction
text = pdf_inspector.extract_text("document.pdf")

# Positioned text items with font info
items = pdf_inspector.extract_text_with_positions("document.pdf")
for item in items[:5]:
    print(f"'{item.text}' at ({item.x:.0f}, {item.y:.0f}) size={item.font_size}")

# Per-page markdown (one Markdown string per page, plus layout metadata)
result = pdf_inspector.extract_pages_markdown("document.pdf")
for page in result.pages:
    print(f"Page {page.page}: {len(page.markdown)} chars, needs_ocr={page.needs_ocr}")

# Restrict to specific 0-indexed pages (preserves caller order)
result = pdf_inspector.extract_pages_markdown("document.pdf", pages=[0, 2])
```

## API reference

| Function | Description |
|---|---|
| `process_pdf(path, pages=None)` | Full processing (detect + extract + markdown) |
| `process_pdf_bytes(data, pages=None)` | Full processing from bytes |
| `detect_pdf(path)` | Fast detection only (returns PdfResult) |
| `detect_pdf_bytes(data)` | Fast detection from bytes |
| `classify_pdf(path)` | Lightweight classification (returns PdfClassification) |
| `classify_pdf_bytes(data)` | Lightweight classification from bytes |
| `extract_text(path)` | Plain text extraction |
| `extract_text_bytes(data)` | Plain text extraction from bytes |
| `extract_text_with_positions(path, pages=None)` | Text with X/Y coords and font info |
| `extract_text_with_positions_bytes(data, pages=None)` | Text with positions from bytes |
| `extract_text_in_regions(path, page_regions)` | Extract text in bounding-box regions |
| `extract_text_in_regions_bytes(data, page_regions)` | Region extraction from bytes |
| `extract_pages_markdown(path, pages=None)` | Per-page Markdown + layout metadata (all pages by default) |
| `extract_pages_markdown_bytes(data, pages=None)` | Per-page Markdown from bytes |

## Types

**`PdfResult` fields:** `pdf_type`, `markdown`, `page_count`, `processing_time_ms`, `pages_needing_ocr`, `title`, `confidence`, `is_complex_layout`, `pages_with_tables`, `pages_with_columns`, `has_encoding_issues`

**`PdfClassification` fields:** `pdf_type`, `page_count`, `pages_needing_ocr` (0-indexed), `confidence`

**`TextItem` fields:** `text`, `x`, `y`, `width`, `height`, `font`, `font_size`, `page`, `is_bold`, `is_italic`, `item_type`

**`RegionText` fields:** `text`, `needs_ocr`

**`PageRegionTexts` fields:** `page` (0-indexed), `regions` (list of RegionText)

**`PageMarkdown` fields:** `page` (0-indexed), `markdown`, `needs_ocr`

**`PagesExtractionResult` fields:** `pages` (list of PageMarkdown), `pages_with_tables` (1-indexed), `pages_with_columns` (1-indexed), `pages_needing_ocr` (1-indexed), `is_complex`
