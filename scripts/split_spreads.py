#!/usr/bin/env python3
"""
Split 2-up facing page spreads in a PDF into sequential single portrait pages.

Usage:
    ./venv/bin/python scripts/split_spreads.py [input_pdf] [output_pdf]
Default:
    input_pdf:  MasteEllioWav_OCR.pdf
    output_pdf: MasteEllioWav_single.pdf
"""

import sys
import time
from pathlib import Path
import pypdf
from pypdf.generic import RectangleObject


def split_spreads(input_path: Path, output_path: Path):
    print(f"Opening input PDF: {input_path}")
    start_time = time.time()
    reader = pypdf.PdfReader(str(input_path))
    writer = pypdf.PdfWriter()

    total_spreads = len(reader.pages)
    print(f"Total input pages: {total_spreads}")

    split_count = 0
    single_count = 0

    for idx, page in enumerate(reader.pages):
        w = float(page.mediabox.width)
        h = float(page.mediabox.height)

        # First page (cover) or any page that is already portrait
        if idx == 0 or w < h:
            p_single = writer.add_page(page)
            p_single.mediabox = RectangleObject([0, 0, w, h])
            p_single.cropbox = RectangleObject([0, 0, w, h])
            single_count += 1
        else:
            mid_x = w / 2.0

            # Left page: [0, 0, mid_x, h]
            p_left = writer.add_page(page)
            p_left.mediabox = RectangleObject([0, 0, mid_x, h])
            p_left.cropbox = RectangleObject([0, 0, mid_x, h])

            # Right page: [mid_x, 0, w, h]
            p_right = writer.add_page(page)
            p_right.mediabox = RectangleObject([mid_x, 0, w, h])
            p_right.cropbox = RectangleObject([mid_x, 0, w, h])

            split_count += 1

        if (idx + 1) % 20 == 0 or idx + 1 == total_spreads:
            print(f"  Processed {idx + 1}/{total_spreads} pages...")

    print(f"Writing single-page PDF to: {output_path} ...")
    with open(output_path, "wb") as f:
        writer.write(f)

    elapsed = time.time() - start_time
    total_output_pages = len(writer.pages)
    print(
        f"Done in {elapsed:.2f}s! "
        f"{total_spreads} source pages ({single_count} portrait, {split_count} spreads) "
        f"-> {total_output_pages} single pages."
    )


def main():
    input_file = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("MasteEllioWav_OCR.pdf")
    output_file = (
        Path(sys.argv[2]) if len(sys.argv) > 2 else Path("MasteEllioWav_single.pdf")
    )

    if not input_file.exists():
        print(f"Error: input file '{input_file}' does not exist.", file=sys.stderr)
        sys.exit(1)

    split_spreads(input_file, output_file)


if __name__ == "__main__":
    main()
