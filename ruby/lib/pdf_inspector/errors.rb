# frozen_string_literal: true

module PdfInspector
  class Error < StandardError; end
  class EncryptedError < Error; end
  class InvalidPdfError < Error; end
  class ParseError < Error; end
end
