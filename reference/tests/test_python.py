"""Tests for the pdf_inspector Python bindings."""

import os
import pytest
import pdf_inspector

FIXTURES_DIR = os.path.join(os.path.dirname(__file__), "fixtures")


def fixture_path(name: str) -> str:
    return os.path.join(FIXTURES_DIR, name)


def fixture_bytes(name: str) -> bytes:
    with open(fixture_path(name), "rb") as f:
        return f.read()


# ---------------------------------------------------------------------------
# process_pdf
# ---------------------------------------------------------------------------


class TestProcessPdf:
    def test_basic(self):
        result = pdf_inspector.process_pdf(fixture_path("thermo-freon12.pdf"))
        assert result.pdf_type == "text_based"
        assert result.page_count == 3
        assert result.confidence > 0.0
        assert result.markdown is not None
        assert len(result.markdown) > 0

    def test_result_repr(self):
        result = pdf_inspector.process_pdf(fixture_path("thermo-freon12.pdf"))
        r = repr(result)
        assert "PdfResult" in r
        assert "text_based" in r

    def test_with_pages(self):
        result = pdf_inspector.process_pdf(
            fixture_path("thermo-freon12.pdf"), pages=[1]
        )
        assert result.page_count == 3  # total pages in doc
        assert result.markdown is not None

    def test_result_fields(self):
        result = pdf_inspector.process_pdf(fixture_path("thermo-freon12.pdf"))
        # All fields should be accessible
        assert isinstance(result.pdf_type, str)
        assert isinstance(result.page_count, int)
        assert isinstance(result.processing_time_ms, int)
        assert isinstance(result.pages_needing_ocr, list)
        assert isinstance(result.confidence, float)
        assert isinstance(result.is_complex_layout, bool)
        assert isinstance(result.pages_with_tables, list)
        assert isinstance(result.pages_with_columns, list)
        assert isinstance(result.has_encoding_issues, bool)
        # title can be None or str
        assert result.title is None or isinstance(result.title, str)


# ---------------------------------------------------------------------------
# process_pdf_bytes
# ---------------------------------------------------------------------------


class TestProcessPdfBytes:
    def test_basic(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.process_pdf_bytes(data)
        assert result.pdf_type == "text_based"
        assert result.markdown is not None

    def test_with_pages(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.process_pdf_bytes(data, pages=[1, 2])
        assert result.markdown is not None


# ---------------------------------------------------------------------------
# detect_pdf / detect_pdf_bytes
# ---------------------------------------------------------------------------


class TestDetectPdf:
    def test_detect_file(self):
        result = pdf_inspector.detect_pdf(fixture_path("thermo-freon12.pdf"))
        assert result.pdf_type == "text_based"
        assert result.markdown is None  # detect only — no markdown
        assert result.page_count == 3

    def test_detect_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.detect_pdf_bytes(data)
        assert result.pdf_type == "text_based"
        assert result.markdown is None


# ---------------------------------------------------------------------------
# classify_pdf / classify_pdf_bytes
# ---------------------------------------------------------------------------


class TestClassifyPdf:
    def test_classify_file(self):
        result = pdf_inspector.classify_pdf(fixture_path("thermo-freon12.pdf"))
        assert result.pdf_type == "text_based"
        assert result.page_count == 3
        assert result.confidence > 0.0
        assert isinstance(result.pages_needing_ocr, list)

    def test_classify_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.classify_pdf_bytes(data)
        assert result.pdf_type == "text_based"
        assert result.page_count == 3
        assert result.confidence > 0.0

    def test_classify_repr(self):
        result = pdf_inspector.classify_pdf(fixture_path("thermo-freon12.pdf"))
        r = repr(result)
        assert "PdfClassification" in r
        assert "text_based" in r

    def test_classify_fields(self):
        result = pdf_inspector.classify_pdf(fixture_path("thermo-freon12.pdf"))
        assert isinstance(result.pdf_type, str)
        assert isinstance(result.page_count, int)
        assert isinstance(result.pages_needing_ocr, list)
        assert isinstance(result.confidence, float)


# ---------------------------------------------------------------------------
# extract_text / extract_text_bytes
# ---------------------------------------------------------------------------


class TestExtractText:
    def test_basic(self):
        text = pdf_inspector.extract_text(fixture_path("thermo-freon12.pdf"))
        assert isinstance(text, str)
        assert len(text) > 0

    def test_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        text = pdf_inspector.extract_text_bytes(data)
        assert isinstance(text, str)
        assert len(text) > 0

    def test_bytes_matches_file(self):
        text_file = pdf_inspector.extract_text(fixture_path("thermo-freon12.pdf"))
        text_bytes = pdf_inspector.extract_text_bytes(fixture_bytes("thermo-freon12.pdf"))
        assert text_file == text_bytes


# ---------------------------------------------------------------------------
# extract_text_with_positions / extract_text_with_positions_bytes
# ---------------------------------------------------------------------------


class TestExtractTextWithPositions:
    def test_basic(self):
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("thermo-freon12.pdf")
        )
        assert len(items) > 0
        item = items[0]
        assert isinstance(item.text, str)
        assert isinstance(item.x, float)
        assert isinstance(item.y, float)
        assert isinstance(item.width, float)
        assert isinstance(item.height, float)
        assert isinstance(item.font, str)
        assert isinstance(item.font_size, float)
        assert isinstance(item.page, int)
        assert isinstance(item.is_bold, bool)
        assert isinstance(item.is_italic, bool)
        assert isinstance(item.item_type, str)

    def test_with_pages(self):
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("thermo-freon12.pdf"), pages=[1]
        )
        assert len(items) > 0
        assert all(item.page == 1 for item in items)

    def test_repr(self):
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("thermo-freon12.pdf")
        )
        r = repr(items[0])
        assert "TextItem" in r

    def test_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        items = pdf_inspector.extract_text_with_positions_bytes(data)
        assert len(items) > 0
        assert isinstance(items[0].text, str)

    def test_bytes_with_pages(self):
        data = fixture_bytes("thermo-freon12.pdf")
        items = pdf_inspector.extract_text_with_positions_bytes(data, pages=[1])
        assert len(items) > 0
        assert all(item.page == 1 for item in items)


# ---------------------------------------------------------------------------
# extract_text_in_regions / extract_text_in_regions_bytes
# ---------------------------------------------------------------------------


class TestExtractTextInRegions:
    def test_file(self):
        results = pdf_inspector.extract_text_in_regions(
            fixture_path("thermo-freon12.pdf"),
            [(0, [[0.0, 0.0, 600.0, 100.0]])],
        )
        assert len(results) == 1
        assert results[0].page == 0
        assert len(results[0].regions) == 1
        assert isinstance(results[0].regions[0].text, str)
        assert isinstance(results[0].regions[0].needs_ocr, bool)

    def test_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        results = pdf_inspector.extract_text_in_regions_bytes(
            data,
            [(0, [[0.0, 0.0, 600.0, 100.0]])],
        )
        assert len(results) == 1
        assert results[0].page == 0
        assert len(results[0].regions) == 1
        assert isinstance(results[0].regions[0].text, str)

    def test_repr(self):
        results = pdf_inspector.extract_text_in_regions(
            fixture_path("thermo-freon12.pdf"),
            [(0, [[0.0, 0.0, 600.0, 100.0]])],
        )
        r = repr(results[0])
        assert "PageRegionTexts" in r
        r2 = repr(results[0].regions[0])
        assert "RegionText" in r2

    def test_multiple_regions(self):
        results = pdf_inspector.extract_text_in_regions(
            fixture_path("thermo-freon12.pdf"),
            [(0, [[0.0, 0.0, 300.0, 100.0], [300.0, 0.0, 600.0, 100.0]])],
        )
        assert len(results) == 1
        assert len(results[0].regions) == 2

    def test_multiple_pages(self):
        results = pdf_inspector.extract_text_in_regions(
            fixture_path("thermo-freon12.pdf"),
            [
                (0, [[0.0, 0.0, 600.0, 100.0]]),
                (1, [[0.0, 0.0, 600.0, 100.0]]),
            ],
        )
        assert len(results) == 2
        assert results[0].page == 0
        assert results[1].page == 1

    def test_malformed_region_raises_value_error(self):
        with pytest.raises(ValueError, match="Invalid region"):
            pdf_inspector.extract_text_in_regions(
                fixture_path("thermo-freon12.pdf"),
                [(0, [[0.0, 0.0, 600.0]])],
            )


# ---------------------------------------------------------------------------
# extract_pages_markdown / extract_pages_markdown_bytes
# ---------------------------------------------------------------------------


class TestExtractPagesMarkdown:
    def test_default_returns_all_pages(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf")
        )
        assert len(result.pages) == 3
        assert [p.page for p in result.pages] == [0, 1, 2]
        assert all(isinstance(p.markdown, str) for p in result.pages)

    def test_bytes_default_returns_all_pages(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.extract_pages_markdown_bytes(data)
        assert len(result.pages) == 3

    def test_selected_pages_preserve_order(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf"), pages=[2, 0]
        )
        assert [p.page for p in result.pages] == [2, 0]

    def test_bytes_selected_pages_preserve_order(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.extract_pages_markdown_bytes(data, pages=[1])
        assert len(result.pages) == 1
        assert result.pages[0].page == 1

    def test_page_fields(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf"), pages=[0]
        )
        page = result.pages[0]
        assert isinstance(page.page, int)
        assert isinstance(page.markdown, str)
        assert isinstance(page.needs_ocr, bool)
        assert not page.needs_ocr  # text-based fixture
        assert len(page.markdown) > 0

    def test_result_fields(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf")
        )
        assert isinstance(result.pages, list)
        assert isinstance(result.pages_with_tables, list)
        assert isinstance(result.pages_with_columns, list)
        assert isinstance(result.pages_needing_ocr, list)
        assert isinstance(result.is_complex, bool)

    def test_out_of_range_page_marks_needs_ocr(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf"), pages=[9999]
        )
        assert len(result.pages) == 1
        assert result.pages[0].needs_ocr
        assert result.pages[0].markdown == ""

    def test_repr(self):
        result = pdf_inspector.extract_pages_markdown(
            fixture_path("thermo-freon12.pdf"), pages=[0]
        )
        assert "PagesExtractionResult" in repr(result)
        assert "PageMarkdown" in repr(result.pages[0])

    def test_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.extract_pages_markdown_bytes(b"not a pdf")


# ---------------------------------------------------------------------------
# Error handling
# ---------------------------------------------------------------------------


class TestErrors:
    def test_nonexistent_file(self):
        with pytest.raises(ValueError):
            pdf_inspector.process_pdf("/nonexistent/file.pdf")

    def test_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.process_pdf_bytes(b"this is not a pdf")

    def test_empty_bytes(self):
        with pytest.raises(ValueError):
            pdf_inspector.process_pdf_bytes(b"")

    def test_classify_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.classify_pdf_bytes(b"not a pdf")

    def test_classify_nonexistent(self):
        with pytest.raises((ValueError, OSError)):
            pdf_inspector.classify_pdf("/nonexistent/file.pdf")

    def test_extract_text_bytes_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.extract_text_bytes(b"not a pdf")

    def test_regions_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.extract_text_in_regions_bytes(
                b"not a pdf", [(0, [[0.0, 0.0, 100.0, 100.0]])]
            )


# ---------------------------------------------------------------------------
# Multiple fixtures
# ---------------------------------------------------------------------------


class TestMultipleFixtures:
    """Run basic processing on all available test fixtures."""

    @pytest.mark.parametrize(
        "filename",
        [f for f in os.listdir(FIXTURES_DIR) if f.endswith(".pdf")],
    )
    def test_process_all_fixtures(self, filename):
        result = pdf_inspector.process_pdf(fixture_path(filename))
        assert result.pdf_type in (
            "text_based",
            "scanned",
            "image_based",
            "mixed",
        )
        assert result.page_count > 0
        assert result.confidence >= 0.0
