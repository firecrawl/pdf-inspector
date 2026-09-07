# frozen_string_literal: true

RSpec.describe PdfInspector::Native do
  describe ".native_version" do
    subject { described_class.native_version }

    it { is_expected.to match(/\A\d+\.\d+\.\d+\z/) }
  end
end
