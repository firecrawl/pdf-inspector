# frozen_string_literal: true

require "stringio"
require "pathname"

RSpec.describe PdfInspector do
  describe ".resolve_bytes" do
    subject(:resolved) { described_class.resolve_bytes(source) }

    let(:fixture_path) { File.join(PdfInspector::FIXTURES, "thermo-freon12.pdf") }
    let(:fixture_bytes) { File.binread(fixture_path) }

    context "when given a String path" do
      let(:source) { fixture_path }

      it { is_expected.to eq(fixture_bytes) }
    end

    context "when given a Pathname" do
      let(:source) { Pathname.new(fixture_path) }

      it { is_expected.to eq(fixture_bytes) }
    end

    context "when given a StringIO" do
      let(:source) { StringIO.new(fixture_bytes) }

      it { is_expected.to eq(fixture_bytes) }
    end

    context "when given an open File" do
      subject(:resolved) do
        File.open(fixture_path, "rb") { |file| described_class.resolve_bytes(file) }
      end

      it { is_expected.to eq(fixture_bytes) }
    end

    context "when the path does not exist" do
      let(:source) { "/no/such/file.pdf" }

      it "raises Errno::ENOENT, like File.binread would" do
        expect { resolved }.to raise_error(Errno::ENOENT)
      end
    end

    context "when given an unsupported type" do
      let(:source) { 42 }

      it "raises TypeError" do
        expect { resolved }.to raise_error(TypeError, /expected a path.*or an IO-like object/)
      end
    end
  end
end
