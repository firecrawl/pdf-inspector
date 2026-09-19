#!/usr/bin/env python3
"""Generate the font-metadata fixture ``tests/fixtures/font_metadata_faces.pdf``.

Two pages of text set in embedded TrueType subsets whose names, OS/2 tables,
descriptor flags and width tables exercise the font metadata the
positioned-text APIs report: ``is_bold`` with ``bold_source``,
``font_weight`` and ``fixed_pitch``.

Page 1 (12 pt, left edge at 72 pt, baselines from 700 pt down in 20 pt
steps), one line per face and then one line set in several of them:

    F1  FixtureSans-Demi        Demi in the name, usWeightClass 600, no bold selection
    F2  FixtureSans-Bold        Bold in the name, usWeightClass 400 (a regular program)
    F3  FixtureSans-Plain       no weight word in the name, usWeightClass 600
    F4  FixtureSans-Opaque      no weight word in the name, usWeightClass 700, bold selection
    F5  FixtureSans-ExtraLight  usWeightClass 200
    F6  FixtureSans             regular face (usWeightClass 400), filled and stroked
    F5 F6 F3 F2                 "Light regular heavier bold", one run per face

Page 2, fixed pitch:

    M1  FixtureMono             monospaced program, descriptor Flags 4, post isFixedPitch 1
    M2  FixtureMonoPlain        monospaced program, Flags 4, post isFixedPitch 0
    M3  FixtureMonoFlag         monospaced program, Flags 5 (FixedPitch set), isFixedPitch 0, two glyphs
    F6  FixtureSans             proportional program, Flags 4
    P1  FixtureSansDigits       proportional program, Flags 4, only the ten digits

The subsets are cut from the DejaVu fonts, version 2.37, which are
redistributable under the Bitstream Vera Fonts licence (DejaVu's own changes
are in the public domain). That licence asks that modified fonts not carry
the Bitstream Vera names, so every subset is renamed to a "Fixture" face, and
that the copyright and permission notice accompany every copy of the fonts,
so each subset keeps the source font's copyright, licence description and
licence URL in its name table, the PDF's document information dictionary
repeats the copyright line, and the script writes the full licence to
``tests/fixtures/FONT_LICENSES.md``.

The release archive is downloaded and checked against a pinned SHA-256
unless ``--fonts-dir`` points at a directory holding the four ``.ttf`` files.
fontTools is the only dependency outside the standard library:

    uv run --no-project --with fonttools scripts/make_font_metadata_fixtures.py

The output is deterministic: running the script again on the same release
reproduces the committed files byte for byte.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import sys
import urllib.request
import zipfile
import zlib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "font_metadata_faces.pdf"
LICENSE_NOTE = ROOT / "tests" / "fixtures" / "FONT_LICENSES.md"

DEJAVU_URL = (
    "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/"
    "version_2_37/dejavu-fonts-ttf-2.37.zip"
)
DEJAVU_SHA256 = "7576310b219e04159d35ff61dd4a4ec4cdba4f35c00e002a136f00e96a908b0a"
DEJAVU_DIR = "dejavu-fonts-ttf-2.37"
DEJAVU_FILES = (
    "DejaVuSans.ttf",
    "DejaVuSans-Bold.ttf",
    "DejaVuSans-ExtraLight.ttf",
    "DejaVuSansMono.ttf",
)

FS_SELECTION_ITALIC = 1 << 0
FS_SELECTION_BOLD = 1 << 5
FS_SELECTION_REGULAR = 1 << 6
MAC_STYLE_BOLD = 1 << 0

# FontDescriptor /Flags bits (PDF 32000-1, table 123).
FLAG_FIXED_PITCH = 1
FLAG_SYMBOLIC = 4

FONT_SIZE = 12
LEFT = 72
TOP_BASELINE = 700
LINE_STEP = 20


@dataclass(frozen=True)
class Face:
    """One embedded subset: where it comes from and what its tables say."""

    tag: str
    source: str
    ps_name: str
    family: str
    subfamily: str
    weight_class: int
    bold: bool
    fixed_pitch_post: bool
    flags: int


FACES = (
    Face("F1", "DejaVuSans-Bold.ttf", "FixtureSans-Demi", "Fixture Sans", "Demi", 600, False, False, FLAG_SYMBOLIC),
    Face("F2", "DejaVuSans.ttf", "FixtureSans-Bold", "Fixture Sans", "Bold", 400, False, False, FLAG_SYMBOLIC),
    Face("F3", "DejaVuSans-Bold.ttf", "FixtureSans-Plain", "Fixture Sans", "Plain", 600, False, False, FLAG_SYMBOLIC),
    Face("F4", "DejaVuSans-Bold.ttf", "FixtureSans-Opaque", "Fixture Sans", "Opaque", 700, True, False, FLAG_SYMBOLIC),
    Face("F5", "DejaVuSans-ExtraLight.ttf", "FixtureSans-ExtraLight", "Fixture Sans", "ExtraLight", 200, False, False, FLAG_SYMBOLIC),
    Face("F6", "DejaVuSans.ttf", "FixtureSans", "Fixture Sans", "Regular", 400, False, False, FLAG_SYMBOLIC),
    Face("M1", "DejaVuSansMono.ttf", "FixtureMono", "Fixture Mono", "Regular", 400, False, True, FLAG_SYMBOLIC),
    Face("M2", "DejaVuSansMono.ttf", "FixtureMonoPlain", "Fixture Mono", "Plain", 400, False, False, FLAG_SYMBOLIC),
    Face("M3", "DejaVuSansMono.ttf", "FixtureMonoFlag", "Fixture Mono", "Flag", 400, False, False, FLAG_SYMBOLIC | FLAG_FIXED_PITCH),
    Face("P1", "DejaVuSans.ttf", "FixtureSansDigits", "Fixture Sans", "Digits", 400, False, False, FLAG_SYMBOLIC),
)

# Each page is a list of lines; a line is its runs, one (face tag, text) per
# run, and whether it is filled and stroked (text render mode 2 with a
# visible line width) to look heavier than its face.
PAGES = (
    (
        ((("F1", "Demi name, weight class 600"),), False),
        ((("F2", "Bold name, weight class 400"),), False),
        ((("F3", "Plain name, weight class 600"),), False),
        ((("F4", "Opaque name, bold selection, weight class 700"),), False),
        ((("F5", "Extra light, weight class 200"),), False),
        ((("F6", "Painted heavier"),), True),
        ((("F5", "Light "), ("F6", "regular "), ("F3", "heavier "), ("F2", "bold")), False),
    ),
    (
        ((("M1", "Mono declared by the program"),), False),
        ((("M2", "Mono measured from advances"),), False),
        ((("M3", "Mo"),), False),
        ((("F6", "Proportional by advances"),), False),
        ((("P1", "0123456789"),), False),
    ),
)


def text_of(tag: str) -> str:
    """Every character the face shows, across both pages."""
    return "".join(text for page in PAGES for runs, _ in page for run_tag, text in runs if run_tag == tag)


def pdf_escape(text: str) -> str:
    """The text as the body of a PDF literal string."""
    return text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def scale_advance(advance: int, units_per_em: int) -> int:
    """A glyph advance in thousandths of the em, as /Widths wants it."""
    return round(advance * 1000 / units_per_em)


def widths_array(widths: dict[int, int]) -> tuple[int, int, list[int]]:
    """``(FirstChar, LastChar, Widths)`` for a code-to-width map, with zero
    for the codes in between that the face does not show."""
    first, last = min(widths), max(widths)
    return first, last, [widths.get(code, 0) for code in range(first, last + 1)]


def content_stream(lines) -> bytes:
    """The page's content: one text object per line, every run in its face,
    the painted line wrapped in a saved graphics state."""
    ops = []
    for runs, painted in lines:
        y = TOP_BASELINE - LINE_STEP * len(ops)
        parts = ["BT"]
        for index, (tag, text) in enumerate(runs):
            parts.append(f"/{tag} {FONT_SIZE} Tf")
            if index == 0:
                parts.append(f"{LEFT} {y} Td")
            parts.append(f"({pdf_escape(text)}) Tj")
        parts.append("ET")
        line = " ".join(parts)
        ops.append(f"q 0.4 w 2 Tr {line} Q" if painted else line)
    return "\n".join(ops).encode("latin-1")


def to_unicode_cmap(codes) -> bytes:
    """A ToUnicode CMap mapping every single-byte code to the same character."""
    entries = "\n".join(f"<{code:02X}> <{code:04X}>" for code in sorted(codes))
    return (
        "/CIDInit /ProcSet findresource begin\n"
        "12 dict begin\n"
        "begincmap\n"
        "/CMapName /Adobe-Identity-UCS def\n"
        "/CMapType 2 def\n"
        "1 begincodespacerange\n<00> <FF>\nendcodespacerange\n"
        f"{len(codes)} beginbfchar\n{entries}\nendbfchar\n"
        "endcmap\n"
        "CMapName currentdict /CMap defineresource pop\n"
        "end\nend\n"
    ).encode("ascii")


class PdfWriter:
    """Objects in creation order, written with a classic cross-reference table."""

    def __init__(self) -> None:
        self.objects: list[bytes | None] = []

    def reserve(self) -> int:
        self.objects.append(None)
        return len(self.objects)

    def set(self, number: int, body: bytes) -> int:
        self.objects[number - 1] = body
        return number

    def add(self, body: bytes) -> int:
        return self.set(self.reserve(), body)

    def stream(self, entries: str, data: bytes, compress: bool = False) -> int:
        extra = ""
        if compress:
            extra = f" /Length1 {len(data)} /Filter /FlateDecode"
            data = zlib.compress(data, 9)
        head = f"<< {entries} /Length {len(data)}{extra} >>\nstream\n".encode("latin-1")
        return self.add(head + data + b"\nendstream")

    def build(self, root: int, info: int) -> bytes:
        out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for number, body in enumerate(self.objects, start=1):
            assert body is not None, f"object {number} was reserved but never set"
            offsets.append(len(out))
            out += f"{number} 0 obj\n".encode("latin-1") + body + b"\nendobj\n"
        xref = len(out)
        out += f"xref\n0 {len(self.objects) + 1}\n0000000000 65535 f \n".encode("latin-1")
        for offset in offsets:
            out += f"{offset:010d} 00000 n \n".encode("latin-1")
        out += (
            f"trailer\n<< /Size {len(self.objects) + 1} /Root {root} 0 R /Info {info} 0 R >>\n"
            f"startxref\n{xref}\n%%EOF\n"
        ).encode("latin-1")
        return bytes(out)


def subset_face(face: Face, source: Path):
    """The face's font program, cut down to the glyphs it shows and retabled
    as the fixture wants, with the metrics its dictionaries need."""
    from fontTools import subset
    from fontTools.ttLib import TTFont

    text = text_of(face.tag)
    font = TTFont(str(source), recalcTimestamp=False, recalcBBoxes=False)
    options = subset.Options()
    options.layout_features = []
    # The copyright (0), licence description (13) and licence URL (14) of
    # the source font stay, as its licence asks of every copy; a trademark
    # record (7) would too. The family and style names become the fixture's.
    options.name_IDs = [0, 1, 2, 3, 4, 6, 7, 13, 14]
    options.glyph_names = False
    options.notdef_outline = True
    options.recalc_bounds = False
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(text=text)
    subsetter.subset(font)

    os2 = font["OS/2"]
    os2.usWeightClass = face.weight_class
    os2.fsSelection &= ~(FS_SELECTION_ITALIC | FS_SELECTION_BOLD | FS_SELECTION_REGULAR)
    os2.fsSelection |= FS_SELECTION_BOLD if face.bold else FS_SELECTION_REGULAR
    font["head"].macStyle = MAC_STYLE_BOLD if face.bold else 0
    font["post"].isFixedPitch = 1 if face.fixed_pitch_post else 0
    names = font["name"]
    names.names = [record for record in names.names if record.nameID in (0, 7, 13, 14)]
    full_name = f"{face.family} {face.subfamily}".strip()
    for name_id, value in (
        (1, face.family),
        (2, face.subfamily),
        (3, f"{face.ps_name}:fixture"),
        (4, full_name),
        (6, face.ps_name),
    ):
        names.setName(value, name_id, 1, 0, 0)
        names.setName(value, name_id, 3, 1, 0x409)

    buffer = io.BytesIO()
    font.save(buffer)
    program = buffer.getvalue()

    units_per_em = font["head"].unitsPerEm
    cmap = font.getBestCmap()
    hmtx = font["hmtx"]
    widths = {
        ord(ch): scale_advance(hmtx[cmap[ord(ch)]][0], units_per_em) for ch in set(text)
    }
    head = font["head"]
    bbox = [scale_advance(v, units_per_em) for v in (head.xMin, head.yMin, head.xMax, head.yMax)]
    metrics = {
        "bbox": bbox,
        "ascent": scale_advance(font["hhea"].ascent, units_per_em),
        "descent": scale_advance(font["hhea"].descent, units_per_em),
        "cap_height": scale_advance(getattr(os2, "sCapHeight", 0) or head.yMax, units_per_em),
    }
    return program, widths, metrics


def font_objects(writer: PdfWriter, face: Face, program: bytes, widths: dict[int, int], metrics) -> int:
    """The face's font dictionary, with its descriptor, program and ToUnicode CMap."""
    file_ref = writer.stream("", program, compress=True)
    bbox = " ".join(str(v) for v in metrics["bbox"])
    descriptor = writer.add(
        (
            f"<< /Type /FontDescriptor /FontName /{face.ps_name} /Flags {face.flags}"
            f" /FontBBox [{bbox}] /ItalicAngle 0 /Ascent {metrics['ascent']}"
            f" /Descent {metrics['descent']} /CapHeight {metrics['cap_height']}"
            f" /StemV 80 /FontFile2 {file_ref} 0 R >>"
        ).encode("latin-1")
    )
    to_unicode = writer.stream("", to_unicode_cmap(widths))
    first, last, array = widths_array(widths)
    return writer.add(
        (
            f"<< /Type /Font /Subtype /TrueType /BaseFont /{face.ps_name}"
            f" /FirstChar {first} /LastChar {last} /Widths [{' '.join(map(str, array))}]"
            f" /Encoding /WinAnsiEncoding /FontDescriptor {descriptor} 0 R"
            f" /ToUnicode {to_unicode} 0 R >>"
        ).encode("latin-1")
    )


def build_pdf(programs) -> bytes:
    """The fixture, from ``face tag -> (program, widths, metrics)``."""
    writer = PdfWriter()
    catalog = writer.reserve()
    pages = writer.reserve()
    subject = (
        "Synthetic fixture: text set in renamed TrueType subsets of the DejaVu"
        " fonts 2.37. Fonts are (c) Bitstream (Copyright (c) 2003 by Bitstream,"
        " Inc. All Rights Reserved; glyphs imported from the Arev fonts are"
        " Copyright (c) 2006 by Tavmjong Bah. All Rights Reserved; DejaVu changes"
        " are in the public domain), redistributed under the Bitstream Vera Fonts"
        " licence reproduced in tests/fixtures/FONT_LICENSES.md; each subset keeps"
        " the notice in its name table."
    )
    info = writer.add(
        (
            "<< /Title (Font metadata faces)"
            f" /Subject ({pdf_escape(subject)})"
            " /Producer (scripts/make_font_metadata_fixtures.py) >>"
        ).encode("latin-1")
    )
    fonts = {
        face.tag: font_objects(writer, face, *programs[face.tag]) for face in FACES
    }
    page_refs = []
    for lines in PAGES:
        used = sorted({tag for runs, _ in lines for tag, _ in runs})
        resources = " ".join(f"/{tag} {fonts[tag]} 0 R" for tag in used)
        contents = writer.stream("", content_stream(lines))
        page_refs.append(
            writer.add(
                (
                    f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 612 792]"
                    f" /Resources << /Font << {resources} >> >> /Contents {contents} 0 R >>"
                ).encode("latin-1")
            )
        )
    kids = " ".join(f"{ref} 0 R" for ref in page_refs)
    writer.set(pages, f"<< /Type /Pages /Kids [{kids}] /Count {len(page_refs)} >>".encode("latin-1"))
    writer.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode("latin-1"))
    return writer.build(catalog, info)


def license_note(license_text: str) -> str:
    return (
        "# Third-party font licences\n\n"
        "The fixtures under `tests/fixtures/` whose embedded font programs were cut\n"
        "from third-party fonts for the fixture are listed here with the licence those\n"
        "programs are redistributed under. Other fixtures embed the fonts of the\n"
        "documents they were made from and are not listed.\n\n"
        "## font_metadata_faces.pdf\n\n"
        "Generated by `scripts/make_font_metadata_fixtures.py` from the DejaVu fonts,\n"
        f"version 2.37 (<{DEJAVU_URL}>), as renamed\n"
        "and subset TrueType programs. DejaVu's changes to the fonts are in the public\n"
        "domain; the fonts are otherwise covered by the Bitstream Vera Fonts licence and\n"
        "the Arev Fonts licence below, reproduced from the release archive's `LICENSE`\n"
        "file. As that licence asks, no modified face carries the Bitstream Vera names,\n"
        "and every subset keeps the source font's copyright, licence description and\n"
        "licence URL in its name table; the PDF's document information repeats the\n"
        "copyright line.\n\n"
        "```text\n"
        f"{license_text.rstrip()}\n"
        "```\n"
    )


def fetch_release(cache_dir: Path) -> Path:
    """The extracted release directory, downloaded and checked once."""
    extracted = cache_dir / DEJAVU_DIR
    if all((extracted / "ttf" / name).is_file() for name in DEJAVU_FILES) and (extracted / "LICENSE").is_file():
        return extracted
    cache_dir.mkdir(parents=True, exist_ok=True)
    archive = cache_dir / Path(DEJAVU_URL).name
    if not archive.is_file():
        request = urllib.request.Request(DEJAVU_URL, headers={"User-Agent": "pdf-inspector-fixtures"})
        with urllib.request.urlopen(request, timeout=60) as response:
            archive.write_bytes(response.read())
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != DEJAVU_SHA256:
        # Leave nothing behind that the next run would trust.
        archive.unlink(missing_ok=True)
        raise SystemExit(
            f"{archive}: SHA-256 {digest} does not match the pinned {DEJAVU_SHA256};"
            " the archive was removed and will be downloaded again on the next run"
        )
    with zipfile.ZipFile(archive) as zipped:
        for member in zipped.namelist():
            if member.startswith(f"{DEJAVU_DIR}/ttf/") or member == f"{DEJAVU_DIR}/LICENSE":
                zipped.extract(member, cache_dir)
    return extracted


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--fonts-dir", type=Path, help="directory holding the DejaVu .ttf files (and LICENSE beside or above it)")
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "target" / "font-fixture-cache", help="where the release archive is downloaded and extracted")
    parser.add_argument("--output", type=Path, default=FIXTURE)
    parser.add_argument("--license-note", type=Path, default=LICENSE_NOTE)
    args = parser.parse_args(argv)

    if args.fonts_dir:
        fonts_dir = args.fonts_dir
        license_file = next((p for p in (fonts_dir / "LICENSE", fonts_dir.parent / "LICENSE") if p.is_file()), None)
    else:
        release = fetch_release(args.cache_dir)
        fonts_dir, license_file = release / "ttf", release / "LICENSE"
    if license_file is None:
        raise SystemExit(f"no LICENSE file next to or above {fonts_dir}")

    programs = {face.tag: subset_face(face, fonts_dir / face.source) for face in FACES}
    args.output.write_bytes(build_pdf(programs))
    args.license_note.write_text(license_note(license_file.read_text()))
    print(f"wrote {args.output} ({args.output.stat().st_size} bytes) and {args.license_note}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
