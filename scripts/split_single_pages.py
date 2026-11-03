#!/usr/bin/env python3
"""
Split a PDF into individual single-page PDF files.

Usage:
    ./venv/bin/python scripts/split_single_pages.py [input_pdf] [output_dir]

Defaults:
    input_pdf:  MasteEllioWav_single.pdf
    output_dir: single_pages
"""

import argparse
import sys
import time
from pathlib import Path
import pypdf


def split_into_single_pages(input_pdf: Path, output_dir: Path):
    if not input_pdf.exists():
        print(f"Error: input file '{input_pdf}' does not exist.", file=sys.stderr)
        sys.exit(1)

    output_dir.mkdir(parents=True, exist_ok=True)
    print(f"Opening '{input_pdf}'...")
    start_time = time.time()
    reader = pypdf.PdfReader(str(input_pdf))
    total_pages = len(reader.pages)
    print(f"Total pages to split: {total_pages}")

    for idx, page in enumerate(reader.pages):
        page_num = idx + 1
        out_file = output_dir / f"page_{page_num:03d}.pdf"

        writer = pypdf.PdfWriter()
        writer.add_page(page)
        with open(out_file, "wb") as f:
            writer.write(f)

        if page_num % 50 == 0 or page_num == total_pages:
            print(f"  Exported {page_num}/{total_pages} single pages...")

    elapsed = time.time() - start_time
    print(f"Done in {elapsed:.2f}s! Exported {total_pages} files to '{output_dir}/'.")


def main():
    parser = argparse.ArgumentParser(description="Split PDF into standalone single-page PDF files")
    parser.add_argument(
        "--input",
        type=Path,
        default=Path("MasteEllioWav_single.pdf"),
        help="Input PDF file (default: MasteEllioWav_single.pdf)",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("single_pages"),
        help="Output directory (default: single_pages/)",
    )
    args = parser.parse_args()
    split_into_single_pages(args.input, args.output_dir)


if __name__ == "__main__":
    main()
