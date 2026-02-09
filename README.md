# pdf-inspector

Fast Rust library for PDF inspection, classification, and text extraction. Intelligently detects scanned vs text-based PDFs to enable smart routing decisions.

## Supported Features

| Category | Feature | Description |
|----------|---------|-------------|
| **Detection** | Fast Classification | ~10-50ms by sampling content streams |
| | PDF Types | TextBased, Scanned, ImageBased, Mixed |
| | Confidence Scoring | 0.0-1.0 scale for classification certainty |
| | Configurable Thresholds | Tune sampling depth and detection sensitivity |
| | Metadata Extraction | Document title from PDF Info dictionary |
| **Text Extraction** | Plain Text | Direct extraction from text-based PDFs |
| | Position-Aware | Text with X/Y coordinates, font info, page numbers |
| | Multi-Column Support | Automatic detection and proper reading order |
| | Text Encoding | UTF-16BE, UTF-8, and Latin-1 |
| | ToUnicode CMap | Proper decoding of CID-keyed fonts (Type0/Identity-H) |
| | Linearized PDFs | Raw stream extraction for optimized PDFs |
| **Headers** | Auto Detection | H1-H4 based on font size ratios |
| **Lists** | Bullet Points | `•`, `-`, `*`, `○`, `●`, `◦` |
| | Numbered Lists | `1.`, `1)`, `(1)` |
| | Letter Lists | `a.`, `a)`, `(a)` |
| **Code Blocks** | Monospace Fonts | Courier, Consolas, Monaco, Menlo, Fira Code, JetBrains Mono |
| | Keyword Detection | Language keywords and syntax patterns |
| **Tables** | Region Detection | Automatic table boundary identification |
| | Column/Row Detection | Position clustering for structure |
| | Markdown Output | Proper alignment and formatting |
| | Footnotes | Extraction and formatting |
| **Text Processing** | Subscript/Superscript | Font size and Y-offset detection |
| | Hyphenation Fixing | Rejoins words broken across lines |
| | Page Number Filtering | Removes isolated page numbers |
| | URL Formatting | Converts URLs to markdown links |
| | Drop Cap Merging | Handles large initial letters |

## Output Formats

| Format | Description |
|--------|-------------|
| Markdown | Headers, lists, code blocks, tables, page breaks |
| Plain Text | Basic text extraction |
| JSON | Metadata with type, confidence, page count, timing |
| Positioned Items | Low-level text with coordinates and font info |

## CLI Tools

| Tool | Description |
|------|-------------|
| `pdf2md` | Convert PDF to Markdown (supports `--json` output) |
| `detect-pdf` | Detect PDF type without conversion (supports `--json` output) |

## API Overview

### Functions

| Function | Description |
|----------|-------------|
| `process_pdf` / `process_pdf_mem` | Detect, extract, and convert to markdown |
| `detect_pdf_type` / `detect_pdf_type_mem` | Fast type detection only |
| `extract_text` / `extract_text_mem` | Plain text extraction |
| `extract_text_with_positions` | Text with coordinates |
| `to_markdown` | Convert text to markdown |

### Types

| Type | Description |
|------|-------------|
| `PdfType` | `TextBased`, `Scanned`, `ImageBased`, `Mixed` |
| `PdfProcessResult` | Full result with text, markdown, and metadata |
| `PdfTypeResult` | Detection result with type, confidence, page count |
| `TextItem` | Text with position, font info, and page number |
| `TextLine` | Grouped items on the same line |
| `MarkdownOptions` | Configuration for markdown conversion |
| `DetectionConfig` | Configuration for PDF type detection |
| `PdfError` | `Io`, `Parse`, `Encrypted`, `InvalidStructure` |

## How Detection Works

1. Load only metadata (xref table, trailer, page count)
2. Sample first ~5 pages' content streams
3. Scan raw bytes for `Tj`/`TJ` (text) and `Do` (image) operators
4. Classify based on text operator presence

This allows detecting 300+ page PDFs in milliseconds.

## License

MIT
