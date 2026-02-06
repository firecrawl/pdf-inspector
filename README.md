# pdf-to-markdown

Fast Rust library for PDF to Markdown conversion with smart scanned vs text-based detection.

## Features

- **Smart Detection** - Detects scanned vs text-based PDFs in ~10-50ms by sampling content streams for text operators (`Tj`/`TJ`) without loading the full document
- **Direct Extraction** - Text extraction using [lopdf](https://github.com/J-F-Liu/lopdf) with no external dependencies
- **Structure Detection** - Headers (by font size), lists, code blocks (monospace fonts)
- **CLI Tools** - `detect-pdf` and `pdf2md` binaries included

## How Detection Works

Instead of loading the entire PDF, we:

1. Load only metadata (xref table, trailer, page count)
2. Sample first ~5 pages' content streams
3. Scan raw bytes for `Tj`/`TJ` (text) and `Do` (image) operators
4. Classify based on text operator presence

This allows detecting 300+ page PDFs in milliseconds.
