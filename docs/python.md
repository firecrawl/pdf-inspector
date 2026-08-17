# pdf-inspector

Fast PDF classification, text extraction, and selective OCR. Detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts clean native and OCR results to Markdown. Python bindings via [PyO3](https://pyo3.rs) for the [pdf-inspector](https://github.com/firecrawl/pdf-inspector) Rust library.

Built by [Firecrawl](https://firecrawl.dev) to handle text-based PDFs locally in under 200ms, skipping expensive OCR services for the ~54% of PDFs that don't need them.

## Features

- **Smart classification** — `text_based` / `scanned` / `image_based` / `mixed` in ~10–50ms, with a confidence score and per-page OCR routing.
- **Markdown conversion** — headings, lists, code blocks, bold/italic, URL linking, and dual-mode table detection (PDF drawing ops + text-alignment heuristics).
- **Layout-aware extraction** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Selective OCR** — `auto` routes only pages rejected by native extraction; `force` OCRs every selected page; `off` keeps the result/provenance contract without external runtime work.
- **External artifacts** — the wheel embeds no OCR models, PDFium, or ONNX Runtime; clean `auto` requests never load or download them.

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
pip install pdf-inspector
```

Prebuilt wheels cover CPython ≥3.8 on Linux (x86_64, aarch64), macOS (Intel, Apple Silicon), and Windows (x64). Other platforms build from source, which requires a Rust toolchain. For local development in a repo checkout:

```bash
pip install maturin
maturin develop --release
```

OCR calls that route work require compatible PDFium and ONNX Runtime shared
libraries. Set `PDFIUM_LIB_PATH` and `ORT_DYLIB_PATH` when they are not on the
platform library search path. The pinned OCR model set is downloaded and
checksum-verified on the first routed page; use `offline=True` with a warm
cache or `model_directory` to prohibit network access. See the
[OCR runtime setup guide](https://github.com/firecrawl/pdf-inspector/blob/main/docs/ocr-runtime.md)
for pinned downloads, supported platforms, and hosted-fallback behavior.

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

# One-call selective OCR. This releases the GIL while processing.
ocr = pdf_inspector.process_pdf_with_ocr("document.pdf")
for page in ocr.pages:
    print(page.page_number, page.provenance.source)

# Restrict OCR processing to 1-indexed PDF pages and prohibit downloads.
ocr = pdf_inspector.process_pdf_with_ocr(
    "document.pdf",
    page_numbers=[1, 3],
    model_directory="/opt/models/pp-ocrv6-small",
    offline=True,
)

# Structure-tree elements from tagged PDFs (empty list when untagged).
# Pages are 1-indexed to match TextItem.page, so (page, mcid) joins directly
# against extract_text_with_positions — e.g. to recover real heading levels:
elements = pdf_inspector.extract_structure_elements("tagged.pdf")
roles = {(e.page, e.mcid): e.role for e in elements}
headings = [
    item.text
    for item in pdf_inspector.extract_text_with_positions("tagged.pdf")
    if item.mcid is not None and roles.get((item.page, item.mcid), "").startswith("H")
]
```

## API reference

| Function | Description |
|---|---|
| `process_pdf(path, pages=None)` | Full processing (detect + extract + markdown) |
| `process_pdf_bytes(data, pages=None)` | Full processing from bytes |
| `process_pdf_with_ocr(path, **options)` | Native extraction + selective OCR with provenance |
| `process_pdf_with_ocr_bytes(data, **options)` | Native extraction + selective OCR from bytes |
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
| `extract_structure_elements(path, pages=None)` | Structure-tree elements from tagged PDFs (page, mcid, role) |
| `extract_structure_elements_bytes(data, pages=None)` | Structure-tree elements from bytes |

## Types

Type stubs (`pdf_inspector.pyi`) ship with the package. Result types at a glance:

```python
class PdfResult:                     # process_pdf / detect_pdf
    pdf_type: str                    # "text_based" | "scanned" | "image_based" | "mixed"
    markdown: str | None             # extracted Markdown (None for detect_pdf)
    page_count: int
    processing_time_ms: int
    pages_needing_ocr: list[int]     # 1-indexed
    ocr_reasons_by_page: list[PageOcrReasons]
    title: str | None
    confidence: float                # 0.0 - 1.0
    is_complex_layout: bool
    pages_with_tables: list[int]
    pages_with_columns: list[int]
    has_encoding_issues: bool        # broken font encodings — consider OCR fallback

class PageOcrReasons:                # per-page OCR diagnostics
    page: int                        # 1-indexed
    reasons: list[str]               # machine-readable reason identifiers

class OcrModelIdentity:
    name: str                        # model family/name
    revision: str                    # immutable artifact-set revision

class OcrTimings:                    # per-page processing stages
    render_ms: int
    ocr_ms: int
    assembly_ms: int

class OcrPageProvenance:
    page_number: int                 # 1-indexed
    source: Literal["native", "ocr", "fused"]
    ocr_model: OcrModelIdentity | None
    render_dpi: float | None
    ocr_confidence: float | None
    timings: OcrTimings
    warnings: list[str]
    hosted_recommended: bool

class OcrPageResult:
    page_number: int                 # 1-indexed
    markdown: str
    provenance: OcrPageProvenance

class OcrPdfResult:                  # process_pdf_with_ocr / bytes
    markdown: str
    pages: list[OcrPageResult]
    page_count: int
    pages_recommended_for_ocr: list[int]
    pages_routed_to_ocr: list[int]
    pages_recommending_hosted: list[int]
    ocr_reasons_by_page: list[PageOcrReasons]
    pages_with_tables: list[int]
    pages_with_columns: list[int]
    is_complex: bool
    processing_time_ms: int
    render_time_ms: int
    ocr_time_ms: int

class PdfClassification:             # classify_pdf
    pdf_type: str
    page_count: int
    pages_needing_ocr: list[int]     # 0-indexed
    confidence: float

class TextItem:                      # extract_text_with_positions
    text: str
    x: float
    y: float
    width: float
    height: float
    font: str
    font_size: float
    page: int
    is_bold: bool
    is_italic: bool
    is_underline: bool
    is_strikeout: bool
    item_type: str
    mcid: int | None                 # marked-content ID for tagged PDFs (None otherwise)

class StructureElement:              # extract_structure_elements
    page: int                        # 1-indexed (matches TextItem.page)
    mcid: int
    role: str                        # "H1".."H6", "P", "Table", ... (resolved via /RoleMap)

class RegionText:                    # extract_text_in_regions
    text: str
    needs_ocr: bool
    ocr_reason: str | None           # machine-readable OCR reason

class PageRegionTexts:               # extract_text_in_regions
    page: int                        # 0-indexed
    regions: list[RegionText]

class PagesExtractionResult:         # extract_pages_markdown
    pages: list[PageMarkdown]        # PageMarkdown: page (0-indexed), markdown, needs_ocr, ocr_reason
    pages_with_tables: list[int]     # 1-indexed
    pages_with_columns: list[int]    # 1-indexed
    pages_needing_ocr: list[int]     # 1-indexed
    ocr_reasons_by_page: list[PageOcrReasons]
    is_complex: bool                 # any page has tables or multi-column layout
```
