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


# Fixtures that need a user password to open. `process_pdf` has no password
# parameter, so these are exercised through `process_pdf_with_ocr`, which does.
ENCRYPTED_FIXTURE_PASSWORDS = {"encrypted-secret123.pdf": "secret123"}


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

    def test_encrypted_pdf_without_password_raises(self):
        with pytest.raises(ValueError, match="encrypted"):
            pdf_inspector.process_pdf(fixture_path("encrypted-secret123.pdf"))

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
# process_pdf_with_ocr / process_pdf_with_ocr_bytes
# ---------------------------------------------------------------------------


class TestProcessPdfWithOcr:
    def test_off_mode_has_full_provenance_without_external_runtimes(self):
        result = pdf_inspector.process_pdf_with_ocr(
            fixture_path("thermo-freon12.pdf"), mode="off"
        )
        assert result.page_count == 3
        assert len(result.pages) == 3
        assert result.pages_routed_to_ocr == []
        assert all(page.provenance.source == "native" for page in result.pages)
        assert all(page.provenance.ocr_model is None for page in result.pages)
        assert result.markdown
        assert "OcrPdfResult" in repr(result)

    def test_password_opens_encrypted_fixture(self):
        filename = "encrypted-secret123.pdf"
        password = ENCRYPTED_FIXTURE_PASSWORDS[filename]
        result = pdf_inspector.process_pdf_with_ocr(
            fixture_path(filename), mode="off", password=password
        )
        assert result.page_count > 0
        assert "Procurement" in result.markdown

        result = pdf_inspector.process_pdf_with_ocr_bytes(
            fixture_bytes(filename), mode="off", password=password
        )
        assert "Procurement" in result.markdown

    def test_auto_mode_skips_external_runtimes_for_clean_text(self):
        result = pdf_inspector.process_pdf_with_ocr_bytes(
            fixture_bytes("thermo-freon12.pdf")
        )
        assert result.pages_routed_to_ocr == []
        assert result.render_time_ms == 0
        assert result.ocr_time_ms == 0

    def test_selected_pages_are_one_indexed(self):
        result = pdf_inspector.process_pdf_with_ocr(
            fixture_path("thermo-freon12.pdf"), mode="off", page_numbers=[2]
        )
        assert [page.page_number for page in result.pages] == [2]

    def test_rejects_invalid_options(self):
        with pytest.raises(ValueError, match="mode must be"):
            pdf_inspector.process_pdf_with_ocr(
                fixture_path("thermo-freon12.pdf"), mode="sometimes"
            )
        with pytest.raises(ValueError, match="page 0"):
            pdf_inspector.process_pdf_with_ocr(
                fixture_path("thermo-freon12.pdf"),
                mode="off",
                page_numbers=[0],
            )

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

    def test_mcid(self):
        # Untagged fixture: mcid is None or int, never anything else
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("thermo-freon12.pdf")
        )
        assert all(item.mcid is None or isinstance(item.mcid, int) for item in items)
        # Tagged fixture: marked content carries MCIDs
        tagged = pdf_inspector.extract_text_with_positions(
            fixture_path("firecrawl_docs_tagged.pdf")
        )
        assert any(item.mcid is not None for item in tagged)


# ---------------------------------------------------------------------------
# extract_structure_elements / extract_structure_elements_bytes
# ---------------------------------------------------------------------------


class TestExtractStructureElements:
    def test_tagged_file(self):
        elements = pdf_inspector.extract_structure_elements(
            fixture_path("firecrawl_docs_tagged.pdf")
        )
        assert len(elements) > 0
        assert all(isinstance(e.page, int) for e in elements)
        assert all(isinstance(e.mcid, int) for e in elements)
        assert all(isinstance(e.role, str) and len(e.role) > 0 for e in elements)
        assert any(e.role == "H1" for e in elements)

    def test_join_with_text_items(self):
        # (page, mcid) joins against extract_text_with_positions to recover
        # heading text
        path = fixture_path("firecrawl_docs_tagged.pdf")
        elements = pdf_inspector.extract_structure_elements(path)
        items = pdf_inspector.extract_text_with_positions(path)
        h1_refs = {(e.page, e.mcid) for e in elements if e.role == "H1"}
        h1_text = "".join(
            item.text
            for item in items
            if item.mcid is not None and (item.page, item.mcid) in h1_refs
        )
        assert len(h1_text.strip()) > 0

    def test_with_pages(self):
        # pages filter is 1-indexed, matching TextItem.page
        elements = pdf_inspector.extract_structure_elements(
            fixture_path("firecrawl_docs_tagged.pdf"), pages=[1]
        )
        assert len(elements) > 0
        assert all(e.page == 1 for e in elements)

    def test_bytes(self):
        data = fixture_bytes("firecrawl_docs_tagged.pdf")
        elements = pdf_inspector.extract_structure_elements_bytes(data)
        assert len(elements) > 0
        assert any(e.role == "H1" for e in elements)

    def test_untagged_returns_empty(self):
        elements = pdf_inspector.extract_structure_elements(
            fixture_path("thermo-freon12.pdf")
        )
        assert elements == []

    def test_repr(self):
        elements = pdf_inspector.extract_structure_elements(
            fixture_path("firecrawl_docs_tagged.pdf")
        )
        assert "StructureElement" in repr(elements[0])

    def test_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.extract_structure_elements_bytes(b"not a pdf")


# ---------------------------------------------------------------------------
# extract_layout_blocks / extract_layout_blocks_bytes
# ---------------------------------------------------------------------------


ALLOWED_BLOCK_TYPES = {
    "title",
    "section_header",
    "text",
    "list_item",
    "caption",
    "code",
    "table",
    "picture",
}


class TestExtractLayoutBlocks:
    def test_file(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        assert result.pdf_type == "text_based"
        assert result.page_count == 3
        assert len(result.markdown) > 0
        assert len(result.blocks) > 0
        assert all(b.block_type in ALLOWED_BLOCK_TYPES for b in result.blocks)

    def test_spans_ground_the_markdown(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        md = result.markdown.encode("utf-8")
        prev_end = 0
        for block in result.blocks:
            start, end = block.markdown_span
            assert 0 <= start < end <= len(md)
            assert start >= prev_end  # non-overlapping, ascending
            assert md[start:end].decode("utf-8").strip()
            prev_end = end

    def test_bbox_normalized(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        assert any(b.bbox is not None for b in result.blocks)
        for block in result.blocks:
            assert 1 <= block.page <= result.page_count
            if block.bbox is None:
                continue
            x0, y0, x1, y1 = block.bbox
            assert 0.0 <= x0 <= x1 <= 1.0
            assert 0.0 <= y0 <= y1 <= 1.0

    def test_provenance(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        assert all(b.source == "native_text" for b in result.blocks)
        assert all(b.layout_confidence is None for b in result.blocks)
        assert all(b.ocr_confidence is None for b in result.blocks)
        headers = [b for b in result.blocks if b.block_type == "section_header"]
        assert all(
            b.label is not None and b.label.startswith("H") for b in headers
        )

    def test_table_blocks_detected(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        tables = [b for b in result.blocks if b.block_type == "table"]
        assert len(tables) > 0
        start, end = tables[0].markdown_span
        assert "|" in result.markdown.encode("utf-8")[start:end].decode("utf-8")

    def test_picture_blocks_included(self):
        # The layout payload enables image placeholders, so figures surface
        # as picture blocks (the default process_pdf markdown omits them).
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("text_page_with_watermark_image.pdf")
        )
        pictures = [b for b in result.blocks if b.block_type == "picture"]
        assert len(pictures) > 0
        start, end = pictures[0].markdown_span
        md = result.markdown.encode("utf-8")
        assert md[start:end].decode("utf-8").startswith("![Image:")

    def test_bytes(self):
        data = fixture_bytes("thermo-freon12.pdf")
        result = pdf_inspector.extract_layout_blocks_bytes(data)
        assert len(result.blocks) > 0

    def test_repr(self):
        result = pdf_inspector.extract_layout_blocks(
            fixture_path("thermo-freon12.pdf")
        )
        assert "LayoutBlocksResult" in repr(result)
        assert "LayoutBlock" in repr(result.blocks[0])

    def test_not_a_pdf(self):
        with pytest.raises(ValueError):
            pdf_inspector.extract_layout_blocks_bytes(b"not a pdf")


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
        password = ENCRYPTED_FIXTURE_PASSWORDS.get(filename)
        if password is not None:
            # `process_pdf` cannot open encrypted files; use the password-aware
            # entry point with OCR disabled so no external runtime is needed.
            result = pdf_inspector.process_pdf_with_ocr(
                fixture_path(filename), mode="off", password=password
            )
            assert result.page_count > 0
            assert result.markdown
            return

        result = pdf_inspector.process_pdf(fixture_path(filename))
        assert result.pdf_type in (
            "text_based",
            "scanned",
            "image_based",
            "mixed",
        )
        assert result.page_count > 0
        assert result.confidence >= 0.0
