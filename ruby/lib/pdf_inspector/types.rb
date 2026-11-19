# frozen_string_literal: true

module PdfInspector
  PageOcrReasons = Data.define(:page, :reasons)

  PdfResult = Data.define(
    :pdf_type,
    :markdown,
    :page_count,
    :processing_time_ms,
    :pages_needing_ocr,
    :ocr_reasons_by_page,
    :title,
    :confidence,
    :is_complex_layout,
    :pages_with_tables,
    :pages_with_columns,
    :has_encoding_issues
  )

  PdfClassification = Data.define(:pdf_type, :page_count, :pages_needing_ocr, :confidence)

  TextItem = Data.define(
    :text, :x, :y, :width, :height, :font, :font_size, :page,
    :is_bold, :is_italic, :is_underline, :is_strikeout,
    :item_type, :link_url
  )

  RegionText = Data.define(:text, :needs_ocr, :ocr_reason)

  PageRegionTexts = Data.define(:page, :regions)

  PageMarkdown = Data.define(:page, :markdown, :needs_ocr, :ocr_reason)

  PagesExtractionResult = Data.define(
    :pages, :pages_with_tables, :pages_with_columns,
    :pages_needing_ocr, :ocr_reasons_by_page, :is_complex
  )
end
