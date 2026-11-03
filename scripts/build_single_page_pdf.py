#!/usr/bin/env python3
"""
build_single_page_pdf.py

Splits a 2-up facing spread PDF into sequential single portrait pages,
and injects a complete, hierarchical Table of Contents (PDF Bookmarks/Outlines),
completely without OCR and 100% losslessly.

Usage:
    ./venv/bin/python scripts/build_single_page_pdf.py [options]

Examples:
    ./venv/bin/python scripts/build_single_page_pdf.py --input MasteEllioWav.pdf --output MasteEllioWav_single.pdf
"""

import argparse
import re
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple, Any

import pypdf
from pypdf.generic import RectangleObject


# Standard base page numbers for each Chapter in the 335-page single layout
DEFAULT_CHAPTER_BASES = {
    1: 17,
    2: 29,
    3: 43,
    4: 113,
    5: 123,
    6: 167,
    7: 173,
    8: 189,
    9: 221,
    10: 232,
    11: 245,
    12: 276,
}


def parse_toc_from_markdown(md_path: Path) -> List[Dict[str, Any]]:
    """Parse hierarchical bookmarks from the formatted TOC in Markdown."""
    if not md_path.exists():
        print(f"Warning: Markdown TOC file '{md_path}' not found. Using default chapter outlines.", file=sys.stderr)
        return get_default_chapter_bookmarks()

    lines = md_path.read_text(encoding="utf-8", errors="ignore").splitlines()

    start_idx = None
    end_idx = None
    for i, line in enumerate(lines):
        if "## Table of Contents" in line:
            start_idx = i
        if start_idx is not None and i > start_idx + 50 and line.startswith("# Chapter 1: Elementary Discussions"):
            end_idx = i
            break

    if start_idx is None:
        print("Warning: '## Table of Contents' not found in markdown. Using default chapter outlines.", file=sys.stderr)
        return get_default_chapter_bookmarks()

    if end_idx is None:
        end_idx = min(len(lines), start_idx + 600)

    toc_lines = lines[start_idx:end_idx]

    re_ch = re.compile(r"^#+\s+Chapter\s+(\d+):\s*(.*)", re.IGNORECASE)
    re_appendix = re.compile(r"^#+\s+Appendix", re.IGNORECASE)
    re_bullet = re.compile(r"^(\s*)[-*]\s+(.*)")
    re_page_ref = re.compile(r"\((?:Page\s*)?([0-9]+)[-–.\s\"'^]+([0-9]+)\)", re.IGNORECASE)
    re_single_num = re.compile(r"\((?:Page\s*)?([0-9]+)\)", re.IGNORECASE)

    bookmarks: List[Dict[str, Any]] = [
        {"level": 1, "title": "Cover", "page": 1},
        {"level": 1, "title": "Title Page", "page": 2},
        {"level": 1, "title": "Acknowledgments", "page": 3},
        {"level": 1, "title": "Table of Contents", "page": 5},
        {"level": 1, "title": "Dedication", "page": 14},
    ]

    curr_ch: Any = 1
    curr_base = DEFAULT_CHAPTER_BASES.get(1, 17)
    prev_page = 1

    for line in toc_lines:
        sline = line.strip()
        if not sline:
            continue

        # Pre-clean known OCR typos in page refs
        sline = re.sub(r"\(140\)", "(1-10)", sline)
        sline = re.sub(r"\(141\)", "(1-11)", sline)
        sline = re.sub(r"\(14\)", "(1-4)", sline)
        sline = re.sub(r"\(1 5\)", "(1-5)", sline)
        sline = re.sub(r"\(1\"8\)", "(1-8)", sline)
        sline = re.sub(r"\(Page 41\)", "(4-1)", sline)
        sline = re.sub(r"\(Page 44\)", "(4-4)", sline)
        sline = re.sub(r"\(Page 51\)", "(5-1)", sline)
        sline = re.sub(r"\(Page 54\)", "(5-4)", sline)

        m_ch = re_ch.match(sline)
        if m_ch:
            curr_ch = int(m_ch.group(1))
            curr_base = DEFAULT_CHAPTER_BASES.get(curr_ch, 17)
            raw_title = m_ch.group(2).strip()
            ch_title = f"Chapter {curr_ch}: {raw_title}" if raw_title else f"Chapter {curr_ch}"

            # Chapter outline pages
            if curr_ch <= 3:
                ch_page = curr_base - 1
            elif curr_ch == 5:
                ch_page = 121
            elif curr_ch == 7:
                ch_page = 172
            elif curr_ch == 8:
                ch_page = 188
            else:
                ch_page = curr_base

            bookmarks.append({"level": 1, "title": ch_title, "page": ch_page})
            prev_page = ch_page
            continue

        if re_appendix.match(sline):
            curr_ch = "Appendix"
            curr_base = 323
            bookmarks.append({"level": 1, "title": "Appendix: Forecast to the Year 2060", "page": 323})
            prev_page = 323
            continue

        m_b = re_bullet.match(line)
        if m_b:
            indent = len(m_b.group(1))
            level = 2 + (indent // 2)
            raw_text = m_b.group(2).strip()

            # Fix Chapter 7 typos where "Page 1-X" was OCRed instead of "Page 7-X"
            if curr_ch == 7:
                raw_text = re.sub(r"\(Page 1[-^]([0-9]+)\)", r"(7-\1)", raw_text)
                raw_text = re.sub(r"\(Page 1\^\)", r"(7-4)", raw_text)

            m_pref = re_page_ref.search(raw_text)
            page_num = prev_page
            if m_pref:
                ch_num = int(m_pref.group(1))
                p_sub = int(m_pref.group(2))
                base = DEFAULT_CHAPTER_BASES.get(ch_num, curr_base)
                page_num = base + p_sub - 1
                title = raw_text[:m_pref.start()].strip()
            else:
                m_snum = re_single_num.search(raw_text)
                if m_snum:
                    raw_num = m_snum.group(1)
                    p_sub = int(raw_num)
                    page_num = curr_base + p_sub - 1
                    title = raw_text[:m_snum.start()].strip()
                else:
                    title = raw_text.strip()
                    page_num = prev_page

            # Ensure page numbering does not drastically backslide within a chapter
            if page_num < prev_page and curr_ch != "Appendix":
                if prev_page - page_num > 5:
                    page_num = prev_page

            page_num = max(1, min(335, page_num))
            title = title.rstrip("-: ")
            if title:
                bookmarks.append({"level": level, "title": title, "page": page_num})
                prev_page = page_num

    return bookmarks


def get_default_chapter_bookmarks() -> List[Dict[str, Any]]:
    """Fallback top-level bookmarks if no markdown TOC is provided."""
    return [
        {"level": 1, "title": "Cover", "page": 1},
        {"level": 1, "title": "Title Page", "page": 2},
        {"level": 1, "title": "Acknowledgments", "page": 3},
        {"level": 1, "title": "Table of Contents", "page": 5},
        {"level": 1, "title": "Dedication", "page": 14},
        {"level": 1, "title": "Chapter 1: Elementary Discussions", "page": 16},
        {"level": 1, "title": "Chapter 2: General Concepts", "page": 28},
        {"level": 1, "title": "Chapter 3: Preliminary Analysis", "page": 42},
        {"level": 1, "title": "Chapter 4: Intermediary Observations", "page": 113},
        {"level": 1, "title": "Chapter 5: Central Considerations", "page": 121},
        {"level": 1, "title": "Chapter 6: Post-Constructive Rules of Logic", "page": 167},
        {"level": 1, "title": "Chapter 7: Conclusions", "page": 172},
        {"level": 1, "title": "Chapter 8: Construction of Complex Polywaves, Multiwaves etc.", "page": 188},
        {"level": 1, "title": "Chapter 9: Basic Neely Extensions", "page": 221},
        {"level": 1, "title": "Chapter 10: Advanced Logic Rules", "page": 232},
        {"level": 1, "title": "Chapter 11: Advanced Progress Label Application", "page": 245},
        {"level": 1, "title": "Chapter 12: Advanced Neely Extensions", "page": 276},
        {"level": 1, "title": "Appendix: Forecast to the Year 2060", "page": 323},
    ]


def split_spreads_and_inject_bookmarks(
    input_pdf_path: Path,
    output_pdf_path: Path,
    bookmarks: List[Dict[str, Any]]
):
    print(f"📖 Opening input spread PDF: {input_pdf_path}")
    start_time = time.time()

    reader = pypdf.PdfReader(str(input_pdf_path))
    writer = pypdf.PdfWriter()

    total_spreads = len(reader.pages)
    print(f"  Input spread pages: {total_spreads}")

    single_count = 0
    split_count = 0

    for idx, page in enumerate(reader.pages):
        # Rotate content upright if needed
        if page.rotation != 0:
            page.transfer_rotation_to_content()

        w = float(page.mediabox.width)
        h = float(page.mediabox.height)

        # Cover (first page) or already portrait
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

        if (idx + 1) % 40 == 0 or idx + 1 == total_spreads:
            print(f"  Processed {idx + 1}/{total_spreads} source spreads...")

    total_single_pages = len(writer.pages)
    print(f"  Generated {total_single_pages} single portrait pages ({single_count} single + {split_count} spreads split).")

    print(f"🔖 Injecting {len(bookmarks)} hierarchical bookmarks...")
    # Parent stack to track outline hierarchy
    parent_stack: Dict[int, Any] = {0: None}

    for b in bookmarks:
        level = b["level"]
        title = b["title"]
        page_num = b["page"]
        page_idx = max(0, min(total_single_pages - 1, page_num - 1))

        # Look for the nearest active parent at level - 1, level - 2, etc.
        parent = None
        for l in range(level - 1, 0, -1):
            if l in parent_stack and parent_stack[l] is not None:
                parent = parent_stack[l]
                break

        outline_item = writer.add_outline_item(title, page_idx, parent=parent)
        parent_stack[level] = outline_item

        # Clear any deeper levels
        for l in list(parent_stack.keys()):
            if l > level:
                del parent_stack[l]

    print(f"💾 Writing output to: {output_pdf_path} ...")
    output_pdf_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_pdf_path, "wb") as f:
        writer.write(f)

    elapsed = time.time() - start_time
    file_size_mb = output_pdf_path.stat().st_size / (1024 * 1024)
    print(f"✅ Successfully created '{output_pdf_path}' ({file_size_mb:.2f} MB, {total_single_pages} pages) in {elapsed:.2f}s!")


def main():
    parser = argparse.ArgumentParser(
        description="Split facing spread PDF into single portrait pages and inject full hierarchical bookmarks."
    )
    parser.add_argument(
        "--input",
        type=Path,
        default=Path("MasteEllioWav.pdf"),
        help="Input PDF with double-page spreads (default: MasteEllioWav.pdf)"
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("MasteEllioWav_single.pdf"),
        help="Output single-page PDF with bookmarks (default: MasteEllioWav_single.pdf)"
    )
    parser.add_argument(
        "--toc-md",
        type=Path,
        default=Path("MasteEllioWav_Complete.md"),
        help="Markdown file with formatted Table of Contents (default: MasteEllioWav_Complete.md)"
    )
    args = parser.parse_args()

    if not args.input.exists():
        print(f"Error: input file '{args.input}' not found.", file=sys.stderr)
        sys.exit(1)

    bookmarks = parse_toc_from_markdown(args.toc_md)
    split_spreads_and_inject_bookmarks(args.input, args.output, bookmarks)


if __name__ == "__main__":
    main()
