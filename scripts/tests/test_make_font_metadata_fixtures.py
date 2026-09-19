import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from make_font_metadata_fixtures import (
    FACES,
    PAGES,
    PdfWriter,
    content_stream,
    pdf_escape,
    scale_advance,
    text_of,
    to_unicode_cmap,
    widths_array,
)


class FixtureScriptTests(unittest.TestCase):
    def test_every_run_names_a_face_and_every_face_is_shown(self):
        tags = {face.tag for face in FACES}
        self.assertEqual(len(tags), len(FACES))
        used = {tag for page in PAGES for runs, _ in page for tag, _ in runs}
        self.assertEqual(used, tags)
        for face in FACES:
            self.assertTrue(text_of(face.tag), face.tag)

    def test_pdf_escape_protects_string_delimiters(self):
        self.assertEqual(pdf_escape("a (b) \\ c"), "a \\(b\\) \\\\ c")

    def test_scale_advance_is_in_thousandths_of_the_em(self):
        self.assertEqual(scale_advance(1233, 2048), 602)
        self.assertEqual(scale_advance(1000, 1000), 1000)

    def test_widths_array_spans_the_codes_with_zero_gaps(self):
        first, last, widths = widths_array({65: 600, 67: 650, 32: 300})
        self.assertEqual((first, last), (32, 67))
        self.assertEqual(len(widths), 67 - 32 + 1)
        self.assertEqual(widths[0], 300)
        self.assertEqual(widths[65 - 32], 600)
        self.assertEqual(widths[66 - 32], 0)
        self.assertEqual(widths[-1], 650)

    def test_content_stream_sets_each_run_in_its_face(self):
        lines = (
            ((("F1", "One"),), False),
            ((("F2", "Two "), ("F3", "(three)")), True),
        )
        stream = content_stream(lines).decode("latin-1").split("\n")
        self.assertEqual(stream[0], "BT /F1 12 Tf 72 700 Td (One) Tj ET")
        self.assertEqual(
            stream[1],
            "q 0.4 w 2 Tr BT /F2 12 Tf 72 680 Td (Two ) Tj /F3 12 Tf (\\(three\\)) Tj ET Q",
        )

    def test_to_unicode_cmap_maps_every_code_to_itself(self):
        cmap = to_unicode_cmap({65: 600, 32: 300}).decode("ascii")
        self.assertIn("2 beginbfchar\n<20> <0020>\n<41> <0041>\nendbfchar", cmap)

    def test_pdf_writer_offsets_point_at_their_objects(self):
        writer = PdfWriter()
        catalog = writer.reserve()
        info = writer.add(b"<< /Title (t) >>")
        stream = writer.stream("", b"BT ET")
        packed = writer.stream("", b"x" * 64, compress=True)
        writer.set(catalog, f"<< /Type /Catalog /Pages {stream} 0 R >>".encode())
        pdf = writer.build(catalog, info)

        self.assertTrue(pdf.startswith(b"%PDF-1.5\n"))
        self.assertTrue(pdf.endswith(b"%%EOF\n"))
        startxref = int(re.search(rb"startxref\n(\d+)\n%%EOF", pdf).group(1))
        self.assertTrue(pdf[startxref:].startswith(b"xref\n0 5\n"))
        entries = re.findall(rb"(\d{10}) 00000 n \n", pdf[startxref:])
        self.assertEqual(len(entries), 4)
        for number, offset in enumerate(entries, start=1):
            self.assertTrue(pdf[int(offset):].startswith(f"{number} 0 obj\n".encode()))
        self.assertIn(b"/Length 5 >>\nstream\nBT ET\nendstream", pdf)
        self.assertIn(b"/Length1 64 /Filter /FlateDecode", pdf)
        self.assertIn(b"/Root 1 0 R /Info 2 0 R", pdf)
        self.assertEqual(writer.build(catalog, info), pdf)


if __name__ == "__main__":
    unittest.main()
