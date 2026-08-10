# frozen_string_literal: true

require "stringio"

RSpec.describe PdfInspector do
  let(:fixture_path) { File.join(PdfInspector::FIXTURES, "thermo-freon12.pdf") }
  let(:encrypted_path) { File.join(PdfInspector::FIXTURES, "encrypted-secret123.pdf") }

  describe ".process" do
    subject(:result) { described_class.process(source, **options) }

    let(:source) { fixture_path }
    let(:options) { {} }

    it { is_expected.to be_a(PdfInspector::PdfResult) }

    it "detects the PDF as text-based" do
      expect(result.pdf_type).to eq(:text_based)
    end

    it "reports the correct page count" do
      expect(result.page_count).to eq(3)
    end

    it "has positive confidence" do
      expect(result.confidence).to be > 0
    end

    it "extracts markdown as a String" do
      expect(result.markdown).to be_a(String)
    end

    it "extracts non-empty markdown" do
      expect(result.markdown).not_to be_empty
    end

    context "when given an IO instead of a path" do
      let(:source) { File.open(fixture_path, "rb") }

      after { source.close }

      it "reports the correct page count" do
        expect(result.page_count).to eq(3)
      end
    end

    context "when restricted to specific pages" do
      let(:options) { { pages: [1] } }

      it "extracts non-empty markdown" do
        expect(result.markdown).not_to be_empty
      end
    end

    context "when the PDF is encrypted" do
      let(:source) { encrypted_path }

      it "raises PdfInspector::EncryptedError" do
        expect { result }.to raise_error(PdfInspector::EncryptedError)
      end
    end

    context "when the bytes are not a PDF" do
      let(:source) { StringIO.new("not a pdf at all") }

      it "raises PdfInspector::InvalidPdfError" do
        expect { result }.to raise_error(PdfInspector::InvalidPdfError)
      end
    end

    context "when the path does not exist" do
      let(:source) { "/no/such/file.pdf" }

      it "raises Errno::ENOENT" do
        expect { result }.to raise_error(Errno::ENOENT)
      end
    end
  end

  describe ".detect" do
    subject(:result) { described_class.detect(fixture_path) }

    it "detects the PDF as text-based" do
      expect(result.pdf_type).to eq(:text_based)
    end

    it "does not extract markdown" do
      expect(result.markdown).to be_nil
    end
  end

  describe ".classify" do
    subject(:result) { described_class.classify(fixture_path) }

    it { is_expected.to be_a(PdfInspector::PdfClassification) }

    it "detects the PDF as text-based" do
      expect(result.pdf_type).to eq(:text_based)
    end

    it "reports the correct page count" do
      expect(result.page_count).to eq(3)
    end

    it "has positive confidence" do
      expect(result.confidence).to be > 0
    end

    it "reports no pages needing OCR" do
      expect(result.pages_needing_ocr).to eq([])
    end
  end

  describe ".extract_text" do
    subject(:text) { described_class.extract_text(fixture_path) }

    it { is_expected.to be_a(String) }

    it "is not empty" do
      expect(text).not_to be_empty
    end
  end

  describe ".extract_text_with_positions" do
    subject(:items) { described_class.extract_text_with_positions(fixture_path, **options) }

    let(:options) { {} }

    it "returns TextItems" do
      expect(items).to all(be_a(PdfInspector::TextItem))
    end

    it "returns a non-empty list" do
      expect(items).not_to be_empty
    end

    describe "the first item" do
      subject(:item) { items.first }

      it "has a Symbol item_type" do
        expect(item.item_type).to be_a(Symbol)
      end

      it "has an item_type from the known set" do
        expect(item.item_type).to(satisfy { |type| %i[text image link form_field].include?(type) })
      end

      it "has a boolean is_bold flag" do
        expect(item.is_bold).to be(true).or be(false)
      end
    end

    context "when restricted to specific pages" do
      let(:options) { { pages: [1] } }

      it "only returns items from the requested pages" do
        expect(items).to all(have_attributes(page: 1))
      end
    end
  end

  describe ".extract_text_in_regions" do
    subject(:results) { described_class.extract_text_in_regions(fixture_path, page_regions) }

    let(:page_regions) { [{ page: 0, regions: [[0.0, 0.0, 600.0, 800.0]] }] }

    it "returns one PageRegionTexts per requested page" do
      expect(results).to all(be_a(PdfInspector::PageRegionTexts))
    end

    it "reports the requested page number" do
      expect(results.first.page).to eq(0)
    end

    describe "the first region's result" do
      subject(:region) { results.first.regions.first }

      it { is_expected.to be_a(PdfInspector::RegionText) }

      it "does not need OCR" do
        expect(region.needs_ocr).to be(false)
      end

      it "has non-empty text" do
        expect(region.text).not_to be_empty
      end
    end
  end

  describe ".extract_pages_markdown" do
    subject(:result) { described_class.extract_pages_markdown(fixture_path, **options) }

    let(:options) { {} }

    it { is_expected.to be_a(PdfInspector::PagesExtractionResult) }

    it "returns one PageMarkdown per page" do
      expect(result.pages.size).to eq(3)
    end

    it "returns PageMarkdown instances" do
      expect(result.pages).to all(be_a(PdfInspector::PageMarkdown))
    end

    it "extracts non-empty markdown for the first page" do
      expect(result.pages.first.markdown).not_to be_empty
    end

    it "has a boolean is_complex flag" do
      expect(result.is_complex).to be(true).or be(false)
    end

    context "when restricted to specific pages, out of document order" do
      let(:options) { { pages: [2, 0] } }

      it "preserves the caller's requested order" do
        expect(result.pages.map(&:page)).to eq([2, 0])
      end
    end
  end
end
