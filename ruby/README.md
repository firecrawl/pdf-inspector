# pdf-inspector-rb

Fast PDF classification and text extraction for Ruby, without OCR. Native Rust performance via [magnus](https://github.com/matsadler/magnus)/[rb_sys](https://github.com/oxidize-rb/rb-sys).

## Features

- Classifies PDFs as text-based, scanned, image-based, or mixed, with a confidence score and per-page OCR routing.
- Extracts text from bounding-box regions, with per-region quality checks (`needs_ocr`).
- Multi-column reading order, with position and font info attached to each text item.
- Decodes CID/Type0 fonts via ToUnicode CMaps and flags broken encodings so callers can fall back to OCR.
- Returns immutable `Data.define` value objects, symbol enums, and exceptions instead of error codes. Full API reference in [docs/ruby.md](../docs/ruby.md).

## Install

```bash
gem install pdf-inspector-rb
```

Or in a Gemfile:

```ruby
gem "pdf-inspector-rb"
```

This is a **source-only gem**: `extconf.rb` and `rb_sys` compile the native extension at install time, so a Rust toolchain is required on the installing machine. Minimum Ruby version is **3.2**.

## API

### `PdfInspector.process(source, pages: nil) -> PdfResult`

Full processing: detect + extract + convert to Markdown.

```ruby
require "pdf_inspector"

result = PdfInspector.process("document.pdf")
result.pdf_type   # :text_based, :scanned, :image_based, or :mixed
result.confidence # 0.0 - 1.0
result.markdown   # Markdown string or nil

# Process specific pages only
result = PdfInspector.process("document.pdf", pages: [1, 3, 5])

# Process from an IO, no filesystem path needed
File.open("document.pdf", "rb") { |f| PdfInspector.process(f) }
```

### `PdfInspector.extract_text_in_regions(source, page_regions) -> Array<PageRegionTexts>`

Extract text within bounding-box regions from a PDF. Designed for hybrid OCR pipelines where a layout model detects regions in rendered page images and this method extracts text from the PDF structure for text-based pages, skipping GPU OCR.

Each region result includes a `needs_ocr` flag that signals unreliable extraction (empty text, GID-encoded fonts, garbage text, encoding issues).

```ruby
result = PdfInspector.extract_text_in_regions("document.pdf", [
  { page: 0, regions: [[0, 0, 300, 400], [300, 0, 612, 400]] } # [x1, y1, x2, y2] in PDF points, top-left origin
])

result[0].regions.each do |region|
  if region.needs_ocr
    # Unreliable text, send this region to OCR instead
  else
    puts region.text # Extracted text in reading order
  end
end
```

### All methods

| Method | Returns | Description |
|---|---|---|
| `PdfInspector.process(source, pages: nil)` | `PdfResult` | Full processing (detect + extract + markdown) |
| `PdfInspector.detect(source)` | `PdfResult` | Fast detection only (`markdown` is `nil`) |
| `PdfInspector.classify(source)` | `PdfClassification` | Lightweight classification |
| `PdfInspector.extract_text(source)` | `String` | Plain text extraction |
| `PdfInspector.extract_text_with_positions(source, pages: nil)` | `Array<TextItem>` | Text with X/Y coords and font info |
| `PdfInspector.extract_text_in_regions(source, page_regions)` | `Array<PageRegionTexts>` | Extract text in bounding-box regions |
| `PdfInspector.extract_pages_markdown(source, pages: nil)` | `PagesExtractionResult` | Per-page Markdown + layout metadata |
| `PdfInspector.resolve_bytes(source)` | `String` (binary) | The shared path/IO resolver every method above uses internally |

`source` is always a path (`String`/`Pathname`) or an IO-like object (`IO`/`StringIO`). `page_regions` is `Array<{page:, regions:}>`, where `regions` is an array of `[x1, y1, x2, y2]` PDF-point bounding boxes (top-left origin).

## Types

All result types are immutable `Data.define` value objects:

```ruby
PdfResult = Data.define(
  :pdf_type, :markdown, :page_count, :processing_time_ms,
  :pages_needing_ocr, :ocr_reasons_by_page, :title, :confidence,
  :is_complex_layout, :pages_with_tables, :pages_with_columns, :has_encoding_issues
)

PageOcrReasons = Data.define(:page, :reasons)              # reasons: Array<Symbol>

PdfClassification = Data.define(:pdf_type, :page_count, :pages_needing_ocr, :confidence)

TextItem = Data.define(
  :text, :x, :y, :width, :height, :font, :font_size, :page,
  :is_bold, :is_italic, :is_underline, :is_strikeout, :item_type, :link_url
)

RegionText = Data.define(:text, :needs_ocr, :ocr_reason)    # ocr_reason: Symbol | nil
PageRegionTexts = Data.define(:page, :regions)              # regions: Array<RegionText>

PageMarkdown = Data.define(:page, :markdown, :needs_ocr, :ocr_reason)
PagesExtractionResult = Data.define(
  :pages, :pages_with_tables, :pages_with_columns,
  :pages_needing_ocr, :ocr_reasons_by_page, :is_complex
)
```

## Errors

Errors are exceptions, not error codes, rooted in a gem-specific hierarchy:

```ruby
begin
  PdfInspector.process("secret.pdf")
rescue PdfInspector::EncryptedError => e
  puts e.message
end
```

| Class | Meaning |
|---|---|
| `PdfInspector::Error` | Base class |
| `PdfInspector::EncryptedError` | Password-protected PDF |
| `PdfInspector::InvalidPdfError` | Malformed / not a PDF |
| `PdfInspector::ParseError` | Parse failure, with the underlying message |

IO failures (e.g. missing file) raise the native `Errno::ENOENT`, not a custom class.

See [docs/ruby.md](../docs/ruby.md) for the full API reference.

## License

MIT
