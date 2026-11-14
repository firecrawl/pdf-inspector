#!/usr/bin/env python3
"""
pdfFigureExtractor - Semantic Figure & Chart Extractor for Technical PDFs.

Instead of dumping unsemantic, fragmented XObject image slices (which lose axes,
labels, and context), this tool semantically locates figures by caption (e.g. "Figure 2-1",
"Figure 3-21a"), computes the exact visual bounding box containing the diagram, axes,
and annotations, renders it at high DPI, auto-trims whitespace, and saves it.

Can be used in two modes:
1. Single Figure mode:
   ./venv/bin/python scripts/pdf_figure_extractor.py --pdf MasteEllioWav_OCR.pdf --figure "Figure 2-1" --output figures/Figure_2-1.png

2. Markdown Backfill mode:
   Scan a Markdown file for figure placeholders (e.g. `{{FIGURE: Figure 2-1}}` or `![Figure 2-1](...)`),
   extract all referenced figures, and replace placeholders with the final image links.
   ./venv/bin/python scripts/pdf_figure_extractor.py --pdf MasteEllioWav_OCR.pdf --markdown input.md --output output.md --figure-dir figures/
"""

import argparse
import re
import sys
from pathlib import Path
from typing import Optional, Tuple

import pypdfium2 as pdfium
from PIL import Image, ImageChops


def auto_trim_whitespace(img: Image.Image, padding: int = 12) -> Image.Image:
    """Trim excess white borders around the diagram and add a clean padding."""
    # Convert to grayscale for thresholding
    gray = img.convert("L")
    # Invert so black content is foreground
    bg = Image.new("L", gray.size, 255)
    diff = ImageChops.difference(gray, bg)
    # Threshold slight scan background noise (pixel values > 20)
    diff = diff.point(lambda p: 255 if p > 20 else 0)
    bbox = diff.getbbox()
    if bbox:
        w, h = img.size
        x0 = max(0, bbox[0] - padding)
        y0 = max(0, bbox[1] - padding)
        x1 = min(w, bbox[2] + padding)
        y1 = min(h, bbox[3] + padding)
        return img.crop((x0, y0, x1, y1))
    return img


class PdfFigureExtractor:
    def __init__(self, pdf_path: Path, dpi: float = 250.0):
        self.pdf_path = pdf_path
        self.dpi = dpi
        self.scale = dpi / 72.0
        self.doc = pdfium.PdfDocument(str(pdf_path))
        self.page_count = len(self.doc)

    def locate_figure_caption(
        self, figure_name: str
    ) -> Optional[dict]:
        """Search PDF pages for a standalone figure caption (excluding inline references)."""
        target_norm = re.sub(r"[\s\-_]+", "", figure_name).lower()

        best = None
        for p_idx in range(self.page_count):
            page = self.doc[p_idx]
            tp = page.get_textpage()

            search = tp.search(figure_name)
            while True:
                res = search.get_next()
                if not res:
                    break
                c_idx, count = res
                boxes = [tp.get_charbox(c_idx + i) for i in range(count)]
                bx0 = min(b[0] for b in boxes)
                by0 = min(b[1] for b in boxes)
                bx1 = max(b[2] for b in boxes)
                by1 = max(b[3] for b in boxes)

                # Check preceding context
                start_before = max(0, c_idx - 15)
                context_before = "".join(
                    tp.get_text_range(start_before, c_idx - start_before)
                ).strip().lower()

                # An in-sentence reference usually has '(' or 'see' or 'like' right before it
                is_reference = (
                    "see" in context_before
                    or "like" in context_before
                    or "(" in context_before
                )

                score = 10 if not is_reference else 1
                candidate = {
                    "p_idx": p_idx,
                    "page": page,
                    "tp": tp,
                    "bx0": bx0,
                    "by0": by0,
                    "bx1": bx1,
                    "by1": by1,
                    "score": score,
                }
                if best is None or score > best["score"]:
                    best = candidate
                if score == 10:
                    return best

        return best

    def compute_figure_bounds(
        self, caption_info: dict
    ) -> Tuple[float, float, float, float]:
        """Compute the PDF coordinate box [x0, y0, x1, y1] for the entire diagram."""
        page = caption_info["page"]
        tp = caption_info["tp"]
        cap_bx0 = caption_info["bx0"]
        cap_by0 = caption_info["by0"]
        cap_by1 = caption_info["by1"]

        page_w = page.get_width()
        page_h = page.get_height()
        mid_x = page_w / 2.0

        is_left = cap_bx0 < mid_x
        col_x0 = 42.0 if is_left else mid_x + 10.0
        col_x1 = mid_x - 10.0 if is_left else page_w - 42.0

        # Scan text rects on the page below cap_by0 to find the next section / body text
        num_rects = tp.count_rects(0, tp.count_chars())
        bottom_y = 65.0  # default bottom page margin
        next_anchor_y = 0.0

        for i in range(num_rects):
            r = tp.get_rect(i)
            rx0, ry0, rx1, ry1 = r[0], r[1], r[2], r[3]
            rw = rx1 - rx0
            rh = ry1 - ry0

            in_col = (col_x0 - 15 <= rx0) and (rx1 <= col_x1 + 15)
            # Find the top of the closest text line below the diagram
            if in_col and ry1 < cap_by0 - 18:
                txt = tp.get_text_bounded(*r).strip()
                is_fig = bool(re.match(r"^Figure\s+\d+", txt, re.IGNORECASE))
                is_heading = (rh >= 8.0 and rw > 50.0) or (rx0 <= col_x0 + 50.0 and rw > 70.0)
                is_body_para = rw > 150.0
                if (is_fig or is_heading or is_body_para) and ry1 > next_anchor_y:
                    next_anchor_y = ry1

        if next_anchor_y > 0.0:
            bottom_y = next_anchor_y + 8.0  # leave 8 pt buffer above the body line

        top_y = min(page_h - 15.0, cap_by1 + 14.0)
        return (col_x0, bottom_y, col_x1, top_y)

    def extract_figure(
        self, figure_name: str, output_path: Path
    ) -> bool:
        """Locate, render, crop, and save a figure by its name."""
        cap_info = self.locate_figure_caption(figure_name)
        if not cap_info:
            print(f"Warning: Could not locate caption for '{figure_name}' in PDF.")
            return False

        page = cap_info["page"]
        p_idx = cap_info["p_idx"]
        page_h = page.get_height()

        x0, y0, x1, y1 = self.compute_figure_bounds(cap_info)

        # Render page
        bitmap = page.render(scale=self.scale)
        full_img = bitmap.to_pil()

        # PDF coords (y=0 bottom) to PIL pixel coords (y=0 top)
        crop_box = (
            int(x0 * self.scale),
            int((page_h - y1) * self.scale),
            int(x1 * self.scale),
            int((page_h - y0) * self.scale),
        )

        cropped = full_img.crop(crop_box)
        final_img = auto_trim_whitespace(cropped)

        output_path.parent.mkdir(parents=True, exist_ok=True)
        final_img.save(str(output_path), format="PNG")
        print(
            f"Extracted '{figure_name}' (PDF p.{p_idx+1}) -> {output_path} ({final_img.width}x{final_img.height} px)"
        )
        return True

    def backfill_markdown(
        self, markdown_in: Path, markdown_out: Path, figure_dir: Path
    ):
        """Find all figure placeholders in markdown, extract figures, and replace links."""
        content = markdown_in.read_text(encoding="utf-8")
        figure_dir.mkdir(parents=True, exist_ok=True)

        # Match patterns like:
        # {{FIGURE: Figure 2-1}}
        # <!-- FIGURE: Figure 2-1 -->
        # Standalone lines: Figure 2-1, ### Figure 2-1, **Figure 2-1**
        # ![Figure 2-1](placeholder)
        patterns = [
            r"\{\{FIGURE:\s*(Figure\s*[\d\w\-\.]+)\}\}",
            r"<!--\s*FIGURE:\s*(Figure\s*[\d\w\-\.]+)\s*-->",
            r"!\[([^\]]*Figure\s*[\d\w\-\.]+)\]\(figure:([^\)]+)\)",
            r"!\[(Figure\s*[\d\w\-\.]+)\]\((?:placeholder|figure_placeholder|[^\)]*)\)",
            r"(?m)^\s*#{1,6}\s*(Figure\s*[\d\w\-\.]+)\s*$",
            r"(?m)^\s*\*\*(Figure\s*[\d\w\-\.]+)\*\*\s*$",
            r"(?m)^\s*(Figure\s*[\d\w\-\.]+)\s*$",
        ]

        # Extract all figure references
        figures_to_extract = set()
        for pat in patterns:
            for match in re.finditer(pat, content, re.IGNORECASE):
                fig_label = match.group(1).strip()
                figures_to_extract.add(fig_label)

        print(f"Found {len(figures_to_extract)} unique figure placeholders in {markdown_in}:")
        for f in sorted(figures_to_extract):
            print(f"  - {f}")

        # Extract figures
        fig_map = {}
        for fig_name in sorted(figures_to_extract):
            # Normalize filename, e.g. "Figure 2-1" -> "Figure_2-1.png"
            clean_name = re.sub(r"\s+", "_", fig_name)
            out_img = figure_dir / f"{clean_name}.png"
            if not out_img.exists():
                ok = self.extract_figure(fig_name, out_img)
            else:
                ok = True
                print(f"Using cached figure: {out_img}")

            if ok:
                # Relative path from markdown to figure_dir
                fig_map[fig_name] = str(out_img)

        # Replace in content
        def get_img_link(fig_str: str) -> Optional[str]:
            norm_str = re.sub(r"\s+", " ", fig_str.strip().lower())
            for k in fig_map:
                if re.sub(r"\s+", " ", k.lower()) == norm_str:
                    return f"![{k}]({fig_map[k]})"
            return None

        updated = content
        # Replace {{FIGURE: ...}} and <!-- FIGURE: ... -->
        def replace_tag(match):
            name = match.group(1).strip()
            link = get_img_link(name)
            return link if link else match.group(0)

        updated = re.sub(r"\{\{FIGURE:\s*(Figure\s*[\d\w\-\.]+)\}\}", replace_tag, updated, flags=re.IGNORECASE)
        updated = re.sub(r"<!--\s*FIGURE:\s*(Figure\s*[\d\w\-\.]+)\s*-->", replace_tag, updated, flags=re.IGNORECASE)
        updated = re.sub(r"!\[(Figure\s*[\d\w\-\.]+)\]\([^\)]*\)", replace_tag, updated, flags=re.IGNORECASE)

        # Replace standalone lines
        def replace_standalone(match):
            name = match.group(1).strip()
            link = get_img_link(name)
            return link if link else match.group(0)

        updated = re.sub(r"(?m)^\s*#{1,6}\s*(Figure\s*[\d\w\-\.]+)\s*$", replace_standalone, updated, flags=re.IGNORECASE)
        updated = re.sub(r"(?m)^\s*\*\*(Figure\s*[\d\w\-\.]+)\*\*\s*$", replace_standalone, updated, flags=re.IGNORECASE)
        updated = re.sub(r"(?m)^\s*(Figure\s*[\d\w\-\.]+)\s*$", replace_standalone, updated, flags=re.IGNORECASE)

        markdown_out.write_text(updated, encoding="utf-8")
        print(f"\nSuccessfully backfilled markdown to: {markdown_out}")


def main():
    parser = argparse.ArgumentParser(
        description="Extract complete semantic figures/charts from PDF and backfill Markdown"
    )
    parser.add_argument(
        "--pdf",
        type=Path,
        default=Path("MasteEllioWav_OCR.pdf"),
        help="Input PDF file (e.g. MasteEllioWav_OCR.pdf)",
    )
    parser.add_argument(
        "--figure",
        type=str,
        default=None,
        help="Single figure name to extract (e.g. 'Figure 2-1')",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Output image path for single figure extraction",
    )
    parser.add_argument(
        "--markdown",
        type=Path,
        default=None,
        help="Input markdown file containing {{FIGURE: ...}} placeholders",
    )
    parser.add_argument(
        "--markdown-out",
        type=Path,
        default=None,
        help="Output markdown file with backfilled real image paths",
    )
    parser.add_argument(
        "--figure-dir",
        type=Path,
        default=Path("figures"),
        help="Directory to save extracted figure images (default: figures/)",
    )
    parser.add_argument(
        "--dpi",
        type=float,
        default=250.0,
        help="Rendering resolution DPI (default: 250.0)",
    )
    args = parser.parse_args()

    if not args.pdf.exists():
        print(f"Error: PDF '{args.pdf}' not found.", file=sys.stderr)
        sys.exit(1)

    extractor = PdfFigureExtractor(args.pdf, dpi=args.dpi)

    if args.figure:
        out = args.output or (args.figure_dir / f"{re.sub(r'\s+', '_', args.figure)}.png")
        extractor.extract_figure(args.figure, out)
    elif args.markdown:
        md_out = args.markdown_out or args.markdown
        extractor.backfill_markdown(args.markdown, md_out, args.figure_dir)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
