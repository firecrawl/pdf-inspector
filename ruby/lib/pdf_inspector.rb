# frozen_string_literal: true

require_relative "pdf_inspector/version"
require_relative "pdf_inspector/errors"
require_relative "pdf_inspector/types"
require_relative "pdf_inspector/pdf_inspector_native"

# Ruby bindings for pdf-inspector: detect text-based vs scanned PDFs,
# extract text with position awareness, and convert to Markdown.
module PdfInspector
  class << self
    def process(source, pages: nil)
      to_pdf_result(Native.process_bytes(resolve_bytes(source), pages))
    end

    def detect(source)
      to_pdf_result(Native.detect_bytes(resolve_bytes(source)))
    end

    def classify(source)
      PdfClassification.new(**Native.classify_bytes(resolve_bytes(source)))
    end

    def extract_text(source)
      Native.extract_text_bytes(resolve_bytes(source))
    end

    def extract_text_with_positions(source, pages: nil)
      Native.extract_text_with_positions_bytes(resolve_bytes(source), pages).map { |h| TextItem.new(**h) }
    end

    def extract_text_in_regions(source, page_regions)
      Native.extract_text_in_regions_bytes(resolve_bytes(source), page_regions).map do |page_hash|
        PageRegionTexts.new(
          page: page_hash[:page],
          regions: page_hash[:regions].map { |r| RegionText.new(**r) }
        )
      end
    end

    def extract_pages_markdown(source, pages: nil)
      hash = Native.extract_pages_markdown_bytes(resolve_bytes(source), pages)
      PagesExtractionResult.new(
        **hash,
        pages: hash[:pages].map { |h| PageMarkdown.new(**h) },
        ocr_reasons_by_page: wrap_ocr_reasons(hash)
      )
    end

    def resolve_bytes(source)
      case source
      when String, Pathname
        File.binread(source)
      when ->(s) { s.respond_to?(:read) }
        source.read
      else
        raise TypeError, "expected a path (String/Pathname) or an IO-like object, got #{source.class}"
      end
    end

    private

    def to_pdf_result(hash)
      PdfResult.new(**hash, ocr_reasons_by_page: wrap_ocr_reasons(hash))
    end

    def wrap_ocr_reasons(hash)
      hash[:ocr_reasons_by_page].map { |h| PageOcrReasons.new(**h) }
    end
  end
end
