"""Tests for the pdf_inspector Python bindings."""

import os
from typing import Optional

import pytest
import pdf_inspector

FIXTURES_DIR = os.path.join(os.path.dirname(__file__), "fixtures")


def fixture_path(name: str) -> str:
    return os.path.join(FIXTURES_DIR, name)


def fixture_bytes(name: str) -> bytes:
    with open(fixture_path(name), "rb") as f:
        return f.read()


def three_weights_pdf() -> bytes:
    """A one-page PDF whose first line is set in three non-embedded faces that
    differ only in weight: ``Face-Lt`` and ``Face-Md`` name theirs, the third
    has an opaque name and ``/FontWeight 700`` in its descriptor. None of them
    is bold by the flags or name words the default extraction reads. A second
    line uses the light face twice."""
    widths = "[" + " ".join(["600"] * 256) + "]"

    def font(base_font: str, descriptor: int) -> str:
        return (
            f"<< /Type /Font /Subtype /TrueType /BaseFont /{base_font} /FirstChar 0"
            f" /LastChar 255 /Widths {widths} /FontDescriptor {descriptor} 0 R >>"
        )

    def descriptor(base_font: str, font_weight: Optional[int] = None) -> str:
        weight = f" /FontWeight {font_weight}" if font_weight else ""
        return (
            f"<< /Type /FontDescriptor /FontName /{base_font} /Flags 32"
            f" /ItalicAngle 0{weight} >>"
        )

    content = (
        "BT /F1 12 Tf 72 700 Td (Light ) Tj /F2 12 Tf (Medium ) Tj /F3 12 Tf (Heavy) Tj ET\n"
        "BT /F1 12 Tf 72 680 Td (Same ) Tj (weight) Tj ET"
    )
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]"
        " /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R >> >> /Contents 4 0 R >>",
        f"<< /Length {len(content)} >>\nstream\n{content}\nendstream",
        font("ABCDEF+Face-Lt", 8),
        font("ABCDEF+Face-Md", 9),
        font("ABCDEF+Opaque", 10),
        descriptor("ABCDEF+Face-Lt"),
        descriptor("ABCDEF+Face-Md"),
        descriptor("ABCDEF+Opaque", 700),
    ]
    pdf = b"%PDF-1.4\n"
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf += f"{number} 0 obj\n{body}\nendobj\n".encode("latin-1")
    xref = len(pdf)
    pdf += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode("latin-1")
    for offset in offsets:
        pdf += f"{offset:010d} 00000 n \n".encode("latin-1")
    pdf += (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
    ).encode("latin-1")
    return pdf


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

    def test_strip_headers_footers_keeps_running_furniture_when_off(self):
        # p1244-1996.pdf repeats its per-page form header; by default the
        # furniture pass removes the repeats, with the flag off they stay.
        path = fixture_path("p1244-1996.pdf")
        header = "Tips received"
        default = pdf_inspector.process_pdf(path).markdown
        kept = pdf_inspector.process_pdf(path, strip_headers_footers=False).markdown
        assert kept.count(header) > default.count(header)

    def test_remove_page_numbers_keeps_page_number_text_when_off(self):
        path = fixture_path("multiline_indent_cell_rect_grid.pdf")
        default = pdf_inspector.process_pdf(path).markdown
        kept = pdf_inspector.process_pdf(path, remove_page_numbers=False).markdown
        assert "Page 101" not in default
        assert "Page 101" in kept

    def test_markdown_kwargs_default_to_current_behavior(self):
        path = fixture_path("thermo-freon12.pdf")
        default = pdf_inspector.process_pdf(path)
        explicit = pdf_inspector.process_pdf(
            path, strip_headers_footers=True, remove_page_numbers=True
        )
        assert explicit.markdown == default.markdown

    def test_markdown_kwargs_are_keyword_only(self):
        with pytest.raises(TypeError):
            pdf_inspector.process_pdf(
                fixture_path("thermo-freon12.pdf"), None, False
            )

    def test_bytes_variant_accepts_markdown_kwargs(self):
        data = fixture_bytes("p1244-1996.pdf")
        kept = pdf_inspector.process_pdf_bytes(data, strip_headers_footers=False)
        from_file = pdf_inspector.process_pdf(
            fixture_path("p1244-1996.pdf"), strip_headers_footers=False
        )
        assert kept.markdown == from_file.markdown


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
        assert isinstance(item.rotation, float)
        assert isinstance(item.advance_known, bool)
        assert isinstance(item.font, str)
        assert isinstance(item.font_size, float)
        assert isinstance(item.page, int)
        assert isinstance(item.is_bold, bool)
        assert isinstance(item.is_italic, bool)
        assert item.font_weight is None or isinstance(item.font_weight, int)
        sources = (None, "font_name", "font_flags", "weight_class", "painted")
        assert all(i.bold_source in sources for i in items)
        assert all(i.is_bold == (i.bold_source is not None) for i in items)
        assert all(i.fixed_pitch is None or isinstance(i.fixed_pitch, bool) for i in items)
        assert isinstance(item.item_type, str)

    def test_bold_from_weight_defaults_off(self):
        path = fixture_path("thermo-freon12.pdf")
        plain = pdf_inspector.extract_text_with_positions(path)
        explicit = pdf_inspector.extract_text_with_positions(path, bold_from_weight=False)
        styles = lambda items: [(i.text, i.is_bold, i.font_weight) for i in items]
        assert styles(explicit) == styles(plain)
        # The fixture's faces name their weight ("Verdana,Bold" and "Arial,Bold"
        # read 700, "Verdana" and "Arial" nothing), and the runs of different
        # weight already differ in is_bold, so the option leaves every item as
        # it was; every weight it reports is on the 100..900 scale.
        weighted = pdf_inspector.extract_text_with_positions(path, bold_from_weight=True)
        assert styles(weighted) == styles(plain)
        assert any(i.font_weight == 700 and i.is_bold for i in weighted)
        assert all(i.font_weight is None or 100 <= i.font_weight <= 900 for i in weighted)
        positioned = pdf_inspector.extract_text_with_positions_and_rotations(
            path, bold_from_weight=True
        )
        assert styles(positioned.items) == styles(plain)

    def test_bold_from_weight_splits_runs_and_reads_bold_from_600(self, tmp_path):
        data = three_weights_pdf()
        path = tmp_path / "three_weights.pdf"
        path.write_bytes(data)
        styles = lambda items: [(i.text, i.is_bold, i.font_weight) for i in items]
        # Default: the three runs merge into one item as they always did, none
        # is bold, and the item carries its first run's weight class.
        plain = pdf_inspector.extract_text_with_positions_bytes(data)
        assert styles(plain) == [
            ("Light Medium Heavy", False, 300),
            ("Same weight", False, 300),
        ]
        # Option on: the 700 face is bold on the weight class's account, the
        # 300 and 500 faces are not and, agreeing, still merge; the bold run
        # is its own item.
        weighted = pdf_inspector.extract_text_with_positions_bytes(
            data, bold_from_weight=True
        )
        assert styles(weighted) == [
            ("Light Medium ", False, 300),
            ("Heavy", True, 700),
            ("Same weight", False, 300),
        ]
        assert next(i for i in weighted if i.text == "Heavy").bold_source == "weight_class"
        assert next(i for i in weighted if i.text.startswith("Light")).bold_source is None
        positioned = pdf_inspector.extract_text_with_positions_and_rotations_bytes(
            data, bold_from_weight=True
        )
        assert styles(positioned.items) == styles(weighted)
        # A threshold of 500 reads the medium face as bold too, and the runs
        # merge by the verdict; 800 makes nothing bold; the threshold alone,
        # without the option, changes nothing.
        assert styles(
            pdf_inspector.extract_text_with_positions_bytes(
                data, bold_from_weight=True, bold_weight_threshold=500
            )
        ) == [
            ("Light ", False, 300),
            ("Medium Heavy", True, 500),
            ("Same weight", False, 300),
        ]
        assert styles(
            pdf_inspector.extract_text_with_positions_bytes(
                data, bold_from_weight=True, bold_weight_threshold=800
            )
        ) == styles(plain)
        assert styles(
            pdf_inspector.extract_text_with_positions_bytes(data, bold_weight_threshold=100)
        ) == styles(plain)
        # Outside 100..900 the threshold is a ValueError, on every function
        # that takes it and whether or not the option is on; values that do
        # not fit the crate's 16-bit class are the same error, not a
        # conversion failure. Both ends of the scale are valid.
        for bad in (-1, 0, 99, 901, 1000, 65536):
            with pytest.raises(ValueError, match="bold_weight_threshold"):
                pdf_inspector.extract_text_with_positions_bytes(
                    data, bold_from_weight=True, bold_weight_threshold=bad
                )
        with pytest.raises(ValueError, match="bold_weight_threshold"):
            pdf_inspector.extract_text_with_positions(str(path), bold_weight_threshold=50)
        assert styles(
            pdf_inspector.extract_text_with_positions_bytes(
                data, bold_from_weight=True, bold_weight_threshold=100
            )
        ) == [("Light Medium Heavy", True, 300), ("Same weight", True, 300)]
        assert styles(
            pdf_inspector.extract_text_with_positions_bytes(
                data, bold_from_weight=True, bold_weight_threshold=900
            )
        ) == styles(plain)
        with pytest.raises(ValueError, match="bold_weight_threshold"):
            pdf_inspector.extract_text_with_positions_and_rotations(
                str(path), bold_weight_threshold=1000
            )
        with pytest.raises(ValueError, match="bold_weight_threshold"):
            pdf_inspector.extract_text_with_positions_and_rotations_bytes(
                data, bold_from_weight=True, bold_weight_threshold=901
            )
        with pytest.raises(ValueError, match="bold_weight_threshold"):
            pdf_inspector.extract_text_in_regions(
                str(path), [(0, [[0.0, 0.0, 612.0, 792.0]])], bold_weight_threshold=99
            )
        with pytest.raises(ValueError, match="bold_weight_threshold"):
            pdf_inspector.extract_text_in_regions_bytes(
                data, [(0, [[0.0, 0.0, 612.0, 792.0]])], bold_weight_threshold=0
            )
        # The file-based functions read the same page the same way.
        assert styles(pdf_inspector.extract_text_with_positions(str(path))) == styles(plain)
        assert styles(
            pdf_inspector.extract_text_with_positions(str(path), bold_from_weight=True)
        ) == styles(weighted)
        assert styles(
            pdf_inspector.extract_text_with_positions(
                str(path), pages=[1], bold_from_weight=True
            )
        ) == styles(weighted)
        assert styles(
            pdf_inspector.extract_text_with_positions_and_rotations(
                str(path), bold_from_weight=True
            ).items
        ) == styles(weighted)

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


class TestFontMetadata:
    """tests/fixtures/font_metadata_faces.pdf: embedded subsets whose names,
    OS/2 tables, descriptor flags and width tables each make one point (see
    scripts/make_font_metadata_fixtures.py)."""

    @staticmethod
    def face_style(items, text):
        item = next(i for i in items if i.text == text)
        return (item.is_bold, item.bold_source, item.font_weight, item.fixed_pitch)

    @staticmethod
    def mixed_line(items):
        return [
            (i.text, i.is_bold, i.bold_source, i.font_weight)
            for i in items
            if i.page == 1 and abs(i.y - 580) < 0.5
        ]

    def test_bold_source_names_where_the_default_verdict_came_from(self):
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("font_metadata_faces.pdf")
        )
        # A Demi face is bold by its name, whatever its weight class; a Bold
        # name over a regular program too, with font_weight showing the
        # conflict; a heavy weight class alone is not bold by default.
        assert self.face_style(items, "Demi name, weight class 600") == (
            True, "font_name", 600, False,
        )
        assert self.face_style(items, "Bold name, weight class 400") == (
            True, "font_name", 400, False,
        )
        assert self.face_style(items, "Plain name, weight class 600") == (
            False, None, 600, False,
        )
        # The program's bold selection behind an opaque name, and text
        # filled and stroked to look heavier.
        assert self.face_style(
            items, "Opaque name, bold selection, weight class 700"
        ) == (True, "font_flags", 700, False)
        assert self.face_style(items, "Painted heavier") == (True, "painted", 400, False)
        # The runs that are not bold merge whatever their weight classes.
        assert self.mixed_line(items) == [
            ("Light regular heavier ", False, None, 200),
            ("bold", True, "font_name", 400),
        ]

    def test_fixed_pitch_is_declared_or_measured(self):
        items = pdf_inspector.extract_text_with_positions_bytes(
            fixture_bytes("font_metadata_faces.pdf")
        )
        # Declared by the program's post table or the descriptor's flag,
        # else measured from the advances of the glyphs in use; ten tabular
        # digits are too few to say.
        assert self.face_style(items, "Mono declared by the program")[3] is True
        assert self.face_style(items, "Mono measured from advances")[3] is True
        assert self.face_style(items, "Mo")[3] is True
        assert self.face_style(items, "Proportional by advances")[3] is False
        assert self.face_style(items, "0123456789")[3] is None

    def test_bold_from_weight_credits_the_weight_class_and_takes_a_threshold(self):
        path = fixture_path("font_metadata_faces.pdf")
        plain = pdf_inspector.extract_text_with_positions(path)
        weighted = pdf_inspector.extract_text_with_positions(path, bold_from_weight=True)
        # The plain-named 600 face is bold on the weight class's account;
        # the name and flags keep theirs.
        assert self.face_style(weighted, "Plain name, weight class 600") == (
            True, "weight_class", 600, False,
        )
        assert self.face_style(weighted, "Demi name, weight class 600")[1] == "font_name"
        assert self.face_style(weighted, "Bold name, weight class 400")[1] == "font_name"
        assert (
            self.face_style(weighted, "Opaque name, bold selection, weight class 700")[1]
            == "font_flags"
        )
        assert self.face_style(weighted, "Painted heavier")[1] == "painted"
        # The mixed line merges by the verdict: the 600 run joins its
        # bold-named neighbour, the two lighter runs stay one item.
        assert self.mixed_line(weighted) == [
            ("Light regular ", False, None, 200),
            ("heavier bold", True, "weight_class", 600),
        ]
        # A threshold of 700 puts the plain 600 face back with the regular
        # ones; the fixed-pitch verdict is the font's, not the option's.
        at_700 = pdf_inspector.extract_text_with_positions(
            path, bold_from_weight=True, bold_weight_threshold=700
        )
        assert self.face_style(at_700, "Plain name, weight class 600") == (
            False, None, 600, False,
        )
        assert self.face_style(at_700, "Demi name, weight class 600")[1] == "font_name"
        assert self.mixed_line(at_700) == self.mixed_line(plain)
        assert [i.fixed_pitch for i in weighted] == [i.fixed_pitch for i in plain]


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
# Coordinate frame: positions and regions share the visible page box
# ---------------------------------------------------------------------------


class TestVisiblePageBoxFrame:
    """Positions and regions are relative to the visible page box (CropBox)."""

    FIXTURE = "cropbox_offset_origin.pdf"

    @staticmethod
    def _glyph(items):
        glyph = next(
            (item for item in items if item.text.strip() == "Visible glyph"), None
        )
        assert glyph is not None, (
            f"fixture glyph missing from {[item.text for item in items]}"
        )
        return glyph

    def test_positions_are_relative_to_cropbox_origin(self):
        # MediaBox [0 0 400 500], CropBox [50 60 350 460]; the glyph is
        # written at raw (120, 300), so a CropBox render puts it at (70, 240)
        # from the box's lower-left corner.
        glyph = self._glyph(
            pdf_inspector.extract_text_with_positions(fixture_path(self.FIXTURE))
        )
        assert glyph.x == pytest.approx(70.0, abs=0.01)
        assert glyph.y == pytest.approx(240.0, abs=0.01)
        from_bytes = self._glyph(
            pdf_inspector.extract_text_with_positions_bytes(
                fixture_bytes(self.FIXTURE)
            )
        )
        assert (from_bytes.x, from_bytes.y) == (glyph.x, glyph.y)

    def test_regions_read_the_same_frame(self):
        glyph = self._glyph(
            pdf_inspector.extract_text_with_positions(fixture_path(self.FIXTURE))
        )
        visible_height = 400.0  # the CropBox is 300 x 400
        region = [
            glyph.x,
            visible_height - glyph.y - glyph.height,
            glyph.x + glyph.width,
            visible_height - glyph.y,
        ]
        results = pdf_inspector.extract_text_in_regions(
            fixture_path(self.FIXTURE), [(0, [region])]
        )
        text = results[0].regions[0].text
        assert "Visible glyph" in text
        assert "Second line" not in text


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

    def test_bold_from_weight_keeps_the_words_of_a_region(self, tmp_path):
        # The option splits runs of different weight into separate items; a
        # region's text is the words on the page and reads the same either
        # way (the item-level effect is covered by
        # TestExtractTextWithPositions).
        data = three_weights_pdf()
        path = tmp_path / "three_weights.pdf"
        path.write_bytes(data)
        regions = [(0, [[60.0, 80.0, 400.0, 116.0]])]
        plain = pdf_inspector.extract_text_in_regions_bytes(data, regions)
        weighted = pdf_inspector.extract_text_in_regions_bytes(
            data, regions, bold_from_weight=True
        )
        assert plain[0].regions[0].text.splitlines()[0].strip() == "Light Medium Heavy"
        assert weighted[0].regions[0].text == plain[0].regions[0].text
        from_file = pdf_inspector.extract_text_in_regions(
            str(path), regions, bold_from_weight=True
        )
        assert from_file[0].regions[0].text == plain[0].regions[0].text

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


# ---------------------------------------------------------------------------
# rotated text-run geometry (fixture: rotated_margin_stamp.pdf)
# ---------------------------------------------------------------------------


ROTATED_STAMP_TEXT = "arXiv:2301.00001v1 [cs.CL] 1 Jan 2023"


class TestRotatedRunGeometry:
    def test_rotated_margin_run_has_tall_thin_box(self):
        items = pdf_inspector.extract_text_with_positions(
            fixture_path("rotated_margin_stamp.pdf")
        )
        stamp = next(i for i in items if i.text == ROTATED_STAMP_TEXT)
        # 90° counter-clockwise: reads bottom-to-top, one em wide, advance tall.
        assert abs(stamp.rotation - 90.0) < 1e-3
        assert stamp.height > 10 * stamp.width
        assert all(i.width > 0 for i in items if i.text.strip())
        assert all(i.rotation == 0.0 for i in items if i.text != ROTATED_STAMP_TEXT)
        assert all(i.advance_known for i in items)

    def test_rotated_margin_run_assigned_to_margin_region_only(self):
        results = pdf_inspector.extract_text_in_regions(
            fixture_path("rotated_margin_stamp.pdf"),
            [(0, [[0.0, 0.0, 50.0, 792.0], [60.0, 0.0, 612.0, 792.0]])],
        )
        margin, body = results[0].regions
        assert margin.text.strip() == ROTATED_STAMP_TEXT
        assert not margin.needs_ocr
        assert "arXiv" not in body.text
        assert "The quick brown fox" in body.text


# ---------------------------------------------------------------------------
# extract_text_with_positions_and_rotations
# ---------------------------------------------------------------------------


class TestPositionedTextWithRotations:
    def test_upright_page_reports_no_rotation(self):
        positioned = pdf_inspector.extract_text_with_positions_and_rotations(
            fixture_path("rotated_margin_stamp.pdf")
        )
        assert len(positioned.items) > 0
        assert positioned.page_rotations == []

    def test_rotated_page_reports_its_frame(self):
        positioned = pdf_inspector.extract_text_with_positions_and_rotations_bytes(
            fixture_bytes("tnagriculture_06_12.pdf")
        )
        assert len(positioned.items) > 0
        frames = [(r.page, r.rotation) for r in positioned.page_rotations]
        assert frames == [(1, "ccw")]
        assert "PageRotation" in repr(positioned.page_rotations[0])


# ---------------------------------------------------------------------------
# Text paint and document information
# ---------------------------------------------------------------------------


def paint_pdf() -> bytes:
    """A one-page PDF with a red run, a stroked blue run, a run shown under a
    ``3 Tr`` set before its text object, a line shown with ``"``, a Form
    XObject's text under the page's green fill, an image, a link annotation, a
    filled-in form field and a few body lines. Its information dictionary
    holds a UTF-16BE title, a PDFDocEncoding author, a producer and a creation
    date, and no other entry."""
    widths = "[" + " ".join(["600"] * 256) + "]"
    content = (
        "1 0 0 rg BT /F1 12 Tf 72 700 Td (Red run) Tj ET\n"
        "0 0 1 RG 1 Tr BT /F1 12 Tf 72 680 Td (Outlined run) Tj ET\n"
        "0 g 3 Tr BT /F1 12 Tf 72 660 Td (Invisible run) Tj ET\n"
        '0 Tr BT /F1 12 Tf 14 TL 72 654 Td 2 0.5 (Quoted run) " ET\n'
        "0 1 0 rg q /X1 Do Q\n"
        "q 20 0 0 20 400 700 cm /Im1 Do Q\n"
        # Body lines: a page that draws an image needs ten text operators or
        # more to be read as a text page.
        "0 g BT /F1 12 Tf 12 TL 72 430 Td (Body line one) Tj (Body line two) ' (Body line three) '\n"
        "(Body line four) ' (Body line five) ' (Body line six) ' (Body line seven) ' ET"
    )
    form = "BT /F1 12 Tf 72 600 Td (Form run) Tj ET"
    title = "<FEFF" + "Quarterly – Q3".encode("utf-16-be").hex().upper() + ">"
    objects = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]"
        " /Resources << /Font << /F1 5 0 R >> /XObject << /X1 6 0 R /Im1 8 0 R >> >>"
        " /Contents 4 0 R /Annots [9 0 R 10 0 R] >>",
        f"<< /Length {len(content)} >>\nstream\n{content}\nendstream",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 0"
        f" /LastChar 255 /Widths {widths} >>",
        "<< /Type /XObject /Subtype /Form /BBox [0 0 612 792]"
        f" /Resources << /Font << /F1 5 0 R >> >> /Length {len(form)} >>\nstream\n{form}\nendstream",
        f"<< /Title {title} /Author (Jos\xe9) /Producer (Test Library)"
        " /CreationDate (D:20240115103000Z) >>",
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray"
        " /BitsPerComponent 8 /Length 1 >>\nstream\n\x80\nendstream",
        "<< /Type /Annot /Subtype /Link /Rect [72 500 200 520] /Border [0 0 0]"
        " /A << /S /URI /URI (https://example.com/report) >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (Name) /V (Jane Doe)"
        " /Rect [72 450 272 470] /P 3 0 R >>",
    ]
    pdf = b"%PDF-1.7\n"
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf += f"{number} 0 obj\n{body}\nendobj\n".encode("latin-1")
    xref = len(pdf)
    pdf += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode("latin-1")
    for offset in offsets:
        pdf += f"{offset:010d} 00000 n \n".encode("latin-1")
    pdf += (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R /Info 7 0 R >>"
        f"\nstartxref\n{xref}\n%%EOF"
    ).encode("latin-1")
    return pdf


class TestTextPaint:
    def paint_of(self, items, text):
        item = next((i for i in items if i.text == text), None)
        assert item is not None, [i.text for i in items]
        return (item.fill_color, item.stroke_color, item.render_mode)

    def test_items_report_fill_and_stroke_colour_and_render_mode(self):
        items = pdf_inspector.extract_text_with_positions_bytes(paint_pdf())
        assert self.paint_of(items, "Red run") == ((255, 0, 0), (0, 0, 0), 0)
        assert self.paint_of(items, "Outlined run") == ((255, 0, 0), (0, 0, 255), 1)
        # Extracted as it always was, and reported as painting nothing.
        assert self.paint_of(items, "Invisible run") == ((0, 0, 0), (0, 0, 255), 3)
        assert self.paint_of(items, "Quoted run") == ((0, 0, 0), (0, 0, 255), 0)
        assert self.paint_of(items, "Form run") == ((0, 255, 0), (0, 0, 255), 0)

    def test_non_text_items_carry_no_paint(self):
        items = pdf_inspector.extract_text_with_positions_bytes(paint_pdf())
        non_text = [i for i in items if i.item_type != "text"]
        kinds = {i.item_type.split(":", 1)[0] for i in non_text}
        assert kinds == {"image", "link", "form_field"}, [i.item_type for i in non_text]
        for item in non_text:
            assert item.fill_color is None, item.item_type
            assert item.stroke_color is None, item.item_type
            assert item.render_mode is None, item.item_type

    def test_fixture_text_reports_its_paint(self):
        items = pdf_inspector.extract_text_with_positions(fixture_path("thermo-freon12.pdf"))
        text = [i for i in items if i.item_type == "text"]
        assert text
        for item in text:
            for color in (item.fill_color, item.stroke_color):
                assert isinstance(color, tuple) and len(color) == 3
                assert all(0 <= c <= 255 for c in color)
            assert item.render_mode in range(8)


class TestDocumentInformation:
    def test_information_entries_are_decoded(self):
        result = pdf_inspector.process_pdf_bytes(paint_pdf())
        assert result.title == "Quarterly – Q3"
        assert result.author == "José"
        assert result.producer == "Test Library"
        assert result.creation_date == "D:20240115103000Z"
        assert result.subject is None
        assert result.keywords is None
        assert result.creator is None
        assert result.mod_date is None
        assert result.markdown is not None and "Quoted run" in result.markdown

    def test_detection_reads_them_too(self):
        tagged = pdf_inspector.detect_pdf(fixture_path("firecrawl_docs_tagged.pdf"))
        assert tagged.title == "Firecrawl Documentation - API Reference"
        assert tagged.author == "Firecrawl"
        assert tagged.creation_date == "D:20260318031744Z"
        plain = pdf_inspector.detect_pdf_bytes(fixture_bytes("thermo-freon12.pdf"))
        assert plain.producer == "pypdf"
        assert plain.title is None
