# pdf-inspector-rb

Fast PDF classification and text extraction for Ruby. Detects whether a PDF is text-based or scanned, extracts text with position awareness, and converts to Markdown, all without OCR. Ruby bindings via [magnus](https://github.com/matsadler/magnus)/[rb_sys](https://github.com/oxidize-rb/rb-sys) for the [pdf-inspector](https://github.com/firecrawl/pdf-inspector) Rust library.

Lives in this repo as a `ruby/` subdirectory, matching the existing `napi/` and `wasm/` bindings, not a separate repo.

## Features

- Classification: text-based / scanned / image-based / mixed, with a confidence score and per-page OCR routing.
- Markdown conversion: headings, lists, code blocks, bold/italic, URL linking, and dual-mode table detection.
- Region-based extraction: pull text from bounding boxes with per-region quality checks (`needs_ocr`), for hybrid OCR pipelines where a layout model detects regions in rendered page images.
- Layout-aware extraction: multi-column reading order, position and font info per text item.
- Text decoding: CID/Type0 fonts via ToUnicode CMaps, with automatic flagging of broken encodings so callers can fall back to OCR.
- Idiomatic Ruby API: immutable `Data.define` value objects, symbol enums for closed-vocabulary fields, exceptions instead of error codes.

## Install

```bash
gem install pdf-inspector-rb
```

Or in a Gemfile:

```ruby
gem "pdf-inspector-rb"
```

The gem name is `pdf-inspector-rb` (`pdf-inspector` was unavailable on RubyGems); `require "pdf_inspector"` and the top-level module `PdfInspector` are unaffected. The gem-name workaround is invisible to callers.

This ships today as a **source-only gem**: `extconf.rb` + `rb_sys` compile the extension at `gem install` time, so a Rust toolchain is required on the installing machine. Precompiled cross-platform gems (mirroring napi's target matrix) and CI wiring for the `ruby/` job are deferred to a follow-up. Minimum Ruby version is **3.2**, forced by magnus's own supported range.

## Usage

```ruby
require "pdf_inspector"

# Full processing: detect + extract + convert to Markdown
result = PdfInspector.process("document.pdf")
result.pdf_type   # :text_based, :scanned, :image_based, or :mixed
result.confidence # 0.0 - 1.0
result.markdown   # Markdown string or nil

# Process specific pages only
result = PdfInspector.process("document.pdf", pages: [1, 3, 5])

# Process from an IO, no filesystem path needed
File.open("document.pdf", "rb") { |f| PdfInspector.process(f) }

# Fast detection only (no text extraction)
result = PdfInspector.detect("document.pdf")

# Plain text extraction
text = PdfInspector.extract_text("document.pdf")

# Positioned text items with font info
items = PdfInspector.extract_text_with_positions("document.pdf")

# Extract text within bounding-box regions (skips OCR for text-based pages)
result = PdfInspector.extract_text_in_regions("document.pdf", [
  { page: 0, regions: [[0, 0, 300, 400], [300, 0, 612, 400]] } # [x1, y1, x2, y2] in PDF points, top-left origin
])
result[0].regions.each do |region|
  region.needs_ocr ? nil : puts(region.text)
end

# Per-page markdown, plus layout metadata
result = PdfInspector.extract_pages_markdown("document.pdf")
result.pages.each { |page| puts "page #{page.page}: #{page.markdown.length} chars" }

# Errors are exceptions
begin
  PdfInspector.process("secret.pdf")
rescue PdfInspector::EncryptedError => e
  puts e.message
end
```

## API reference

| Method | Returns | Description |
|---|---|---|
| `PdfInspector.process(source, pages: nil)` | `PdfResult` | Full processing (detect + extract + markdown) |
| `PdfInspector.detect(source)` | `PdfResult` | Fast detection only (`markdown` is `nil`) |
| `PdfInspector.classify(source)` | `PdfClassification` | Lightweight classification |
| `PdfInspector.extract_text(source)` | `String` | Plain text extraction |
| `PdfInspector.extract_text_with_positions(source, pages: nil)` | `Array<TextItem>` | Text with X/Y coords and font info |
| `PdfInspector.extract_text_in_regions(source, page_regions)` | `Array<PageRegionTexts>` | Extract text in bounding-box regions |
| `PdfInspector.extract_pages_markdown(source, pages: nil)` | `PagesExtractionResult` | Per-page Markdown + layout metadata |
| `PdfInspector.resolve_bytes(source)` | `String` (binary) | The shared path/IO resolver every method above uses internally; public so it's independently testable and reusable |

`source` is always a path (`String`/`Pathname`) or an IO-like object (`IO`/`StringIO`) — passing anything else raises `TypeError`. There's no `_bytes` method split like the Python bindings (`process_pdf` / `process_pdf_bytes`); each method dispatches on the argument type instead. `page_regions` is `Array<{page:, regions:}>`, where `regions` is an array of `[x1, y1, x2, y2]` PDF-point bounding boxes (top-left origin).

## Types

All result types are immutable `Data.define` value objects, keyword-init, with the same field names as the equivalent [Python dataclass](python.md#types) unless noted:

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

Closed-vocabulary fields are symbols rather than strings: `pdf_type` (`:text_based` / `:scanned` / `:image_based` / `:mixed`), `item_type` (`:text` / `:image` / `:link` / `:form_field`), and machine-readable OCR/error reason identifiers (`reasons`, `ocr_reason`). Free text (`markdown`, `title`, extracted `text`) stays a `String`.

## Errors

Errors are exceptions, not error codes, rooted in a gem-specific hierarchy that preserves the distinctions the Rust core's `PdfError` enum (`src/lib.rs`) already makes:

| Class | Meaning |
|---|---|
| `PdfInspector::Error` | Base class |
| `PdfInspector::EncryptedError` | Password-protected PDF |
| `PdfInspector::InvalidPdfError` | Malformed / not a PDF (`NotAPdf`, `InvalidStructure`) |
| `PdfInspector::ParseError` | Parse failure, with the underlying message |

IO failures (e.g. missing file) raise the native `Errno::ENOENT` a Rubyist already expects from `File.read`, not a custom class.

## Testing & tooling

- Tests: RSpec (`rake spec`, or `bundle exec rspec`).
- Linting: RuboCop (`rake rubocop`), with `rubocop-performance`, `rubocop-rspec`, and `rubocop-thread_safety`.
- `rake` (no args) runs compile → spec → rubocop, matching the core crate's "fmt/clippy/test must all pass" convention.
