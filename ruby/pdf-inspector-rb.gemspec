# frozen_string_literal: true

require_relative "lib/pdf_inspector/version"

Gem::Specification.new do |spec|
  spec.name = "pdf-inspector-rb"
  spec.version      = PdfInspector::VERSION
  spec.authors      = ["Henry Maddocks"]
  spec.summary      = "Fast PDF classification and text extraction, without OCR"
  spec.description  = "Ruby bindings for pdf-inspector: detect text-based vs scanned PDFs, " \
                      "extract text with position awareness, and convert to Markdown."
  spec.homepage     = "https://github.com/firecrawl/pdf-inspector"
  spec.license      = "MIT"
  spec.required_ruby_version = ">= 3.2"

  spec.metadata["homepage_uri"]    = spec.homepage
  spec.metadata["source_code_uri"] = "#{spec.homepage}/tree/main/ruby"
  spec.metadata["rubygems_mfa_required"] = "true"

  root_files = %w[README.md LICENSE].freeze
  spec.files = `git -C #{__dir__} ls-files -z`.split("\x0").select do |f|
    (f.start_with?("lib/") && f.end_with?(".rb")) ||
      (f.start_with?("ext/") && f.end_with?(".rs", ".toml", ".rb")) ||
      root_files.include?(f)
  end
  spec.require_paths = ["lib"]
  spec.extensions    = ["ext/pdf_inspector_native/extconf.rb"]

  spec.add_dependency "rb_sys", "~> 0.9"
end
