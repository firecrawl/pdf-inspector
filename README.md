# pdf-to-markdown

Fast Rust library for PDF to Markdown conversion with smart scanned vs text-based detection.

## Features

- **Smart Detection** - Detects scanned vs text-based PDFs in ~10-50ms by sampling content streams for text operators (`Tj`/`TJ`) without loading the full document
- **Direct Extraction** - Text extraction using [lopdf](https://github.com/J-F-Liu/lopdf) with no external dependencies
- **Structure Detection** - Headers (by font size), lists, code blocks (monospace fonts)
- **CLI Tools** - `detect-pdf` and `pdf2md` binaries included

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
pdf-to-markdown = "0.1"
```

## Usage

### Quick Start

The simplest way to convert a PDF to Markdown:

```rust
use pdf_to_markdown::process_pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = process_pdf("document.pdf")?;

    match result.pdf_type {
        pdf_to_markdown::PdfType::TextBased => {
            println!("Markdown:\n{}", result.markdown.unwrap());
        }
        pdf_to_markdown::PdfType::Scanned => {
            println!("PDF is scanned - OCR required");
        }
        _ => {}
    }

    Ok(())
}
```

### PDF Type Detection

Quickly detect if a PDF is text-based or scanned without full extraction:

```rust
use pdf_to_markdown::{detect_pdf_type, PdfType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = detect_pdf_type("document.pdf")?;

    println!("Type: {:?}", result.pdf_type);
    println!("Pages: {}", result.page_count);
    println!("Confidence: {:.0}%", result.confidence * 100.0);

    if let Some(title) = result.title {
        println!("Title: {}", title);
    }

    match result.pdf_type {
        PdfType::TextBased => println!("Ready for text extraction"),
        PdfType::Scanned => println!("Needs OCR"),
        PdfType::ImageBased => println!("Mostly images"),
        PdfType::Mixed => println!("Mix of text and images"),
    }

    Ok(())
}
```

### Text Extraction

Extract plain text from a PDF:

```rust
use pdf_to_markdown::extract_text;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = extract_text("document.pdf")?;
    println!("{}", text);
    Ok(())
}
```

### Extract Text with Position Information

Get text items with position data for advanced processing:

```rust
use pdf_to_markdown::{extract_text_with_positions, TextItem};
use pdf_to_markdown::extractor::group_into_lines;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let items = extract_text_with_positions("document.pdf")?;

    for item in &items {
        println!("'{}' at ({}, {}) size={}",
            item.text, item.x, item.y, item.font_size);
    }

    // Group items into lines
    let lines = group_into_lines(items);
    for line in lines {
        println!("Line: {}", line.text());
    }

    Ok(())
}
```

### Custom Markdown Conversion

Convert text to Markdown with custom options:

```rust
use pdf_to_markdown::{to_markdown, MarkdownOptions};

fn main() {
    let text = "• First item\n• Second item\n\nconst x = 5;";

    // With all detection enabled (default)
    let md = to_markdown(text, MarkdownOptions::default());
    println!("{}", md);

    // Disable code detection
    let opts = MarkdownOptions {
        detect_headers: true,
        detect_lists: true,
        detect_code: false,
        base_font_size: None,
    };
    let md = to_markdown(text, opts);
    println!("{}", md);
}
```

### Processing from Memory

All functions have memory buffer variants for processing PDFs already in memory:

```rust
use pdf_to_markdown::{process_pdf_mem, detector::detect_pdf_type_mem};
use pdf_to_markdown::extractor::{extract_text_mem, extract_text_with_positions_mem};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let buffer = std::fs::read("document.pdf")?;

    // Process from memory
    let result = process_pdf_mem(&buffer)?;

    // Or detect only
    let detection = detect_pdf_type_mem(&buffer)?;

    // Or extract text
    let text = extract_text_mem(&buffer)?;

    Ok(())
}
```

### Custom Detection Configuration

Fine-tune the detection algorithm:

```rust
use pdf_to_markdown::detector::{detect_pdf_type_with_config, DetectionConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DetectionConfig {
        max_pages_to_sample: 10,        // Sample more pages
        min_text_ops_per_page: 5,       // Require more text operators
        text_page_ratio_threshold: 0.8, // Stricter text classification
    };

    let result = detect_pdf_type_with_config("document.pdf", config)?;
    println!("{:?}", result.pdf_type);

    Ok(())
}
```

## CLI Tools

### pdf2md

Convert a PDF to Markdown:

```bash
# Output to stdout
pdf2md document.pdf

# Output to file
pdf2md document.pdf output.md

# JSON output with metadata
pdf2md document.pdf --json
```

### detect-pdf

Detect PDF type without conversion:

```bash
# Human-readable output
detect-pdf document.pdf

# JSON output
detect-pdf document.pdf --json
```

## How Detection Works

Instead of loading the entire PDF, we:

1. Load only metadata (xref table, trailer, page count)
2. Sample first ~5 pages' content streams
3. Scan raw bytes for `Tj`/`TJ` (text) and `Do` (image) operators
4. Classify based on text operator presence

This allows detecting 300+ page PDFs in milliseconds.

## API Reference

### Types

| Type | Description |
|------|-------------|
| `PdfType` | Enum: `TextBased`, `Scanned`, `ImageBased`, `Mixed` |
| `PdfProcessResult` | Full processing result with text, markdown, and metadata |
| `PdfTypeResult` | Detection result with type, confidence, and page count |
| `TextItem` | Text with position (x, y), font info, and page number |
| `TextLine` | Group of `TextItem`s on the same line |
| `MarkdownOptions` | Configuration for markdown conversion |
| `DetectionConfig` | Configuration for PDF type detection |
| `PdfError` | Error type: `Io`, `Parse`, `Encrypted`, `InvalidStructure` |

### Functions

| Function | Description |
|----------|-------------|
| `process_pdf(path)` | High-level: detect, extract, and convert |
| `process_pdf_mem(buffer)` | Same as above, from memory |
| `detect_pdf_type(path)` | Fast type detection |
| `detect_pdf_type_mem(buffer)` | Type detection from memory |
| `extract_text(path)` | Extract plain text |
| `extract_text_mem(buffer)` | Extract text from memory |
| `extract_text_with_positions(path)` | Extract text with coordinates |
| `to_markdown(text, options)` | Convert text to markdown |

## License

MIT
