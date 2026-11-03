#!/usr/bin/env python3
"""
Batch process and extract clean Markdown from a PDF using a local LLM.

Core Principles:
1. Does NOT use pdf2md output as baseline: reads raw text layers directly from the PDF.
2. The local LLM outputs fluent, natural, publication-grade Markdown directly.
3. Sentence continuity: Never breaks sentences in half for images.
4. Leaves clean figure placeholders: `{{FIGURE: Figure X-Y}}` at natural paragraph boundaries.
5. Optional automatic backfill: Calls PdfFigureExtractor to extract real, complete, high-res
   diagrams from the PDF and backfill them into the Markdown.

Usage:
    ./venv/bin/python scripts/llm_batch_refine.py [options]

Examples:
    # Test on pages 1-10:
    ./venv/bin/python scripts/llm_batch_refine.py --pages 1-10 --output MasteEllioWav_text.md

    # Extract text and automatically backfill figures:
    ./venv/bin/python scripts/llm_batch_refine.py --pages 1-10 --output MasteEllioWav_final.md --backfill-figures
"""

import argparse
import base64
import json
import re
import sys
import time
import urllib.error
import urllib.request
from io import BytesIO
from pathlib import Path
from typing import Dict, List, Optional

import pypdfium2 as pdfium


DEFAULT_API_URL = "http://127.0.0.1:8888/v1"
DEFAULT_PDF = "MasteEllioWav_single.pdf"
DEFAULT_SOURCE_PDF = "MasteEllioWav_OCR.pdf"
DEFAULT_OUTPUT_MD = "MasteEllioWav_text.md"
DEFAULT_CACHE_DIR = ".cache_llm_text"

CHAPTER_TITLES = [
    "Elementary Discussions",
    "General Concepts",
    "Preliminary Analysis",
    "Intermediary Observations",
    "Central Considerations",
    "Post-Constructive Rules of Logic",
    "Conclusions",
    "Construction of Complex Polywaves",
    "Basic Neely Extensions",
    "Advanced Logic Rules",
    "Advanced Progress Label Application",
    "Advanced Neely Extensions",
    "Table of Contents",
]

SYSTEM_PROMPT = """You are an elite technical book editor and typesetter transcribing the definitive edition of Glenn Neely's financial classic: 'Mastering Elliott Wave'.

Your objective is to produce publication-grade, pristine Markdown from raw OCR text with perfect technical fidelity, flawless typography, and structural elegance.

### 1. Document Architecture & Heading Hierarchy
Use clear, semantic ATX headings:
- Book & Chapter Titles (#):
  - '# Chapter 1: Elementary Discussions' (Pages 16-27)
  - '# Chapter 2: General Concepts' (Pages 28-42)
  - '# Chapter 3: Preliminary Analysis' (Pages 43-112)
  - '# Chapter 4: Intermediary Observations'
  - '# Chapter 5: Central Considerations'
  - '# Chapter 6: Post-Constructive Rules of Logic'
  - '# Chapter 7: Conclusions'
  - '# Chapter 8: Construction of Complex Polywaves'
  - '# Chapter 9: Basic Neely Extensions'
  - '# Chapter 10: Advanced Logic Rules'
  - '# Chapter 11: Advanced Progress Label Application'
  - '# Chapter 12: Advanced Neely Extensions'
  * Note: The start of each chapter often has giant decorative letters that OCR garbles (e.g. 'fe E/emen ss/ons'). Replace these artifacts with the clean chapter header: '# Chapter N: [Title]'.
- Major Sections (##): E.g. '## What is the Elliott Wave Theory?', '## What is a "Wave"?', '## How Do You Categorize Waves?'.
- Subsections (###): E.g. '### Finances', '### Psychology', '### Classes', '### Degree', '### Structure Labels', '### Progress Labels'.
- Sub-subsections (####): For deeper nested items (e.g. '#### Closing Price Data', '#### Bar Charts').

### 2. Chapter Outline & Table of Contents Pages
- Each chapter begins with a Chapter Outline page (e.g. Page 16 for Chapter 1, Page 28 for Chapter 2) listing sections and page numbers.
- In raw OCR, these columns are often split into a list of titles followed by a clump of page numbers at the bottom.
- Format these pages as a clean, elegant Markdown outline:
  `## Chapter Outline`
  `- Section Title (Page-Number)`
  `- Sub-section Title (Page-Number)`
- Never output an isolated list of detached page numbers. Align each title with its page number.
- Correct obvious OCR corruptions in outline titles (e.g. 'Peri°diCPhemme\' ton' -> 'Natural, Non-Periodic Phenomenon', '4 Un^ue Analytical Tool' -> 'A Unique Analytical Tool', 'MSWPSSKVVFzaTheNeet%Eztensions' -> 'New Discoveries, The Neely Extensions').

### 3. Figure Rules (STRICT — ZERO HALLUCINATIONS)
- NEVER hallucinate or invent figure placeholders!
- IMPORTANT: Chapter 1 contains NO figures whatsoever. NEVER insert any '{{FIGURE: ...}}' placeholder in Chapter 1.
- For all other chapters (starting Chapter 2): ONLY insert a placeholder '{{FIGURE: Figure X-Y}}' when:
  1. The text explicitly refers to a figure by name (e.g. '(see Figure 2-1)', 'Figure 2-3b', etc.), OR
  2. A dedicated figure caption appears in the text.
- Placement: Place '{{FIGURE: Figure X-Y}}' on its own blank line between natural paragraphs.
- Sentence continuity: NEVER place a figure placeholder in the middle of an incomplete sentence! Keep the sentence whole, and put the placeholder before or after the paragraph.
- Never transcribe fragmented coordinate labels from inside charts (e.g. isolated 'Time Time', 'Price', floating numbers).

### 4. Technical Terminology & Notation Accuracy
Maintain exact Elliott Wave technical notation:
- Structure Labels: Always use ':3' and ':5' (colon followed by digit).
- Wave identifiers: 'm0', 'm1', 'm2', 'm3', 'm(-1)', 'mg0', 's1', 's2', 'c1', 'c2' (fix OCR typos like 'ml' -> 'm1', 'mO' -> 'm0', 'mgO' -> 'mg0').
- Core terms: 'Monowave', 'Polywave', 'Multiwave', 'Macrowave', 'Impulsion', 'Zigzag', 'Flat', 'Triangle' (fix 'Tigzags' -> 'Zigzags').
- De-hyphenate words broken across line wraps (e.g. 'mate- rialize' -> 'materialize').

### 5. Noise & Artifact Stripping
- Decorative dividers: Thick black underline bars under headings often OCR as blocks or symbols ('■■■■', 'IIIIII', 'MMMMMTTT...'). Do NOT transcribe these.
- Running headers and footers: Omit all page headers and footers (e.g. 'Elementary Discussions 1-3', '2-2 General Concepts', 'Table of Contents - 3').

### 6. Output Formatting
- Output ONLY the pristine Markdown.
- Do NOT wrap your entire answer in ```markdown ... ``` code fences.
- Absolutely NO conversational filler, greetings, or post-hoc notes. Start directly with the content.
- Preserve page comment markers like '<!-- Page N -->' to preserve page boundaries."""


def get_available_model(api_base: str) -> str:
    url = f"{api_base}/models"
    try:
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=5) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            models = [m.get("id") for m in data.get("data", []) if m.get("id")]
            if models:
                for preferred in ["Huihui", "Qwen", "MiniCPM"]:
                    for m in models:
                        if preferred.lower() in m.lower():
                            return m
                return models[0]
    except Exception as e:
        print(f"Warning: Could not fetch models from {url}: {e}")
    return "Huihui-Ornith-1.5-35B-A3B-abliterated-oQ5e-fp16-mtp"


def is_noise_divider(line: str) -> bool:
    """Detect OCR scan artifacts of thick decorative bars under headings."""
    s = re.sub(r"\s+", "", line)
    if len(s) < 5:
        return False
    noise_chars = sum(1 for c in s if c in "■IMTil1|-_=~^/\\•*™S")
    return (noise_chars / len(s)) >= 0.8


def is_running_header_or_footer(line: str) -> bool:
    """Detect running headers/footers like 'Elementary Discussions 1-3' or '2-2 General Concepts'."""
    ch_pat = "|".join(re.escape(t) for t in CHAPTER_TITLES)
    if re.match(rf"^\s*(?:{ch_pat})\s*[-–—]?\s*\d*(?:\s*[-–—]\s*\d+)?\s*$", line, re.IGNORECASE):
        return True
    if re.match(rf"^\s*\d*(?:\s*[-–—]\s*\d+)?\s*[-–—]?\s*(?:{ch_pat})\s*$", line, re.IGNORECASE):
        return True
    if re.match(rf"^\s*\d+[a-zA-Z]?\s+(?:{ch_pat})\s*$", line, re.IGNORECASE):
        return True
    return False


def pre_clean_page_text(text: str) -> str:
    """Filter out decorative divider noise and page footers before LLM prompt."""
    cleaned_lines = []
    for line in text.splitlines():
        line_str = line.strip()
        if not line_str:
            cleaned_lines.append("")
            continue
        if is_running_header_or_footer(line_str):
            continue
        if is_noise_divider(line_str):
            continue
        cleaned_lines.append(line)
    return "\n".join(cleaned_lines)


def clean_llm_output(text: str) -> str:
    """Strip any conversational chatter, horizontal rules, or trailing note disclaimers."""
    text = re.sub(
        r"^(?:(?:I\'ll|Here is|Sure|I will|Below is|Certainly)[^\n]*\n+)+",
        "",
        text,
        flags=re.IGNORECASE,
    )
    # Strip wrapping code fences (e.g. ```markdown ... ```)
    text = re.sub(r"^\s*```(?:markdown)?\s*\n", "", text, flags=re.IGNORECASE)
    text = re.sub(r"\n\s*```\s*$", "", text)

    text = re.sub(r"^\s*---+\s*\n", "", text)
    text = re.sub(
        r"\n+\s*(?:---+\s*\n+)?(?:_?Note:?|Disclaimer:|Here ends)[^\n]*.*$",
        "",
        text,
        flags=re.DOTALL | re.IGNORECASE,
    )

    # Post-filter: remove any hallucinated Figure 1-X (Chapter 1 has NO figures)
    text = re.sub(r"\{\{FIGURE:\s*Figure\s*1-\d+\w*\}\}\n*", "", text, flags=re.IGNORECASE)

    # Filter any residual noise lines that slipped through
    filtered_lines = []
    for line in text.splitlines():
        if is_running_header_or_footer(line.strip()) or is_noise_divider(line.strip()):
            continue
        filtered_lines.append(line)
    text = "\n".join(filtered_lines)

    return text.strip()


def extract_pages_from_pdf(pdf_path: Path) -> Dict[int, str]:
    """Directly extract raw page text from PDF in memory with pre-cleaning."""
    doc = pdfium.PdfDocument(str(pdf_path))
    pages = {}
    for i in range(len(doc)):
        page_num = i + 1
        raw_text = doc[i].get_textpage().get_text_bounded().strip()
        cleaned_text = pre_clean_page_text(raw_text)
        pages[page_num] = f"<!-- Page {page_num} -->\n\n{cleaned_text}"
    return pages


def parse_page_range(spec: str, max_page: int) -> List[int]:
    selected = set()
    for token in spec.split(","):
        token = token.strip()
        if not token:
            continue
        if "-" in token:
            start, end = token.split("-", 1)
            start_num = int(start) if start else 1
            end_num = int(end) if end else max_page
            for p in range(start_num, end_num + 1):
                if 1 <= p <= max_page:
                    selected.add(p)
        else:
            p = int(token)
            if 1 <= p <= max_page:
                selected.add(p)
    return sorted(selected)


def call_llm(
    api_base: str, model: str, prompt_text: str, timeout: int = 180
) -> str:
    url = f"{api_base}/chat/completions"
    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt_text},
        ],
        "temperature": 0.1,
        "max_tokens": 4096,
    }

    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )

    with urllib.request.urlopen(req, timeout=timeout) as resp:
        result = json.loads(resp.read().decode("utf-8"))
        raw = result["choices"][0]["message"]["content"]
        return clean_llm_output(raw)


def call_vlm(
    api_base: str,
    model: str,
    page_img_b64: str,
    page_num: int,
    timeout: int = 180,
) -> str:
    url = f"{api_base}/chat/completions"
    user_instruction = f"""Transcribe this book page (Page {page_num}) of 'Mastering Elliott Wave' into pristine, publication-grade Markdown.
Follow all formatting, heading hierarchy, outline, and figure placeholder rules strictly.
Output ONLY the transcribed Markdown."""

    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": user_instruction},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": f"data:image/jpeg;base64,{page_img_b64}"
                        },
                    },
                ],
            },
        ],
        "temperature": 0.1,
        "max_tokens": 4096,
    }

    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )

    with urllib.request.urlopen(req, timeout=timeout) as resp:
        result = json.loads(resp.read().decode("utf-8"))
        raw = result["choices"][0]["message"]["content"]
        return clean_llm_output(raw)


def main():
    parser = argparse.ArgumentParser(
        description="Direct PDF text extraction & Markdown generation via local LLM / VLM"
    )
    parser.add_argument(
        "--mode",
        choices=["vlm", "text"],
        default="vlm",
        help="Extraction mode: 'vlm' (default, single-page visual reading) or 'text' (bounded OCR text)",
    )
    parser.add_argument(
        "--pdf",
        type=Path,
        default=Path(DEFAULT_PDF),
        help=f"Input PDF file to read from (default: {DEFAULT_PDF})",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(DEFAULT_OUTPUT_MD),
        help=f"Output refined markdown file (default: {DEFAULT_OUTPUT_MD})",
    )
    parser.add_argument(
        "--api-base",
        type=str,
        default=DEFAULT_API_URL,
        help="OpenAI-compatible API base URL",
    )
    parser.add_argument(
        "--model",
        type=str,
        default=None,
        help="Model ID (auto-detected if None)",
    )
    parser.add_argument(
        "--batch-size",
        type=int,
        default=3,
        help="Number of pages per batch request (text mode only, default: 3)",
    )
    parser.add_argument(
        "--dpi",
        type=float,
        default=150.0,
        help="Rendering resolution DPI for VLM mode (default: 150.0)",
    )
    parser.add_argument(
        "--pages",
        type=str,
        default=None,
        help="Page range to process (e.g. 1-10, 28-35)",
    )
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=Path(DEFAULT_CACHE_DIR),
        help=f"Cache directory for checkpoints (default: {DEFAULT_CACHE_DIR})",
    )
    parser.add_argument(
        "--backfill-figures",
        action="store_true",
        help="Automatically invoke pdfFigureExtractor after text extraction to backfill figures",
    )
    parser.add_argument(
        "--figure-pdf",
        type=Path,
        default=Path(DEFAULT_SOURCE_PDF),
        help=f"PDF to crop figures from (default: {DEFAULT_SOURCE_PDF})",
    )
    parser.add_argument(
        "--figure-dir",
        type=Path,
        default=Path("figures"),
        help="Directory to save extracted figure images (default: figures/)",
    )
    args = parser.parse_args()

    # Determine input PDF
    if not args.pdf.exists():
        if Path(DEFAULT_SOURCE_PDF).exists():
            args.pdf = Path(DEFAULT_SOURCE_PDF)
        else:
            print(f"Error: Input PDF '{args.pdf}' not found.", file=sys.stderr)
            sys.exit(1)

    model = args.model or get_available_model(args.api_base)
    print(f"Reading directly from PDF: {args.pdf}")
    print(f"Mode:                      {args.mode.upper()}")
    print(f"API endpoint:              {args.api_base}")
    print(f"Model:                     {model}")

    doc = pdfium.PdfDocument(str(args.pdf))
    total_doc_pages = len(doc)
    print(f"Loaded {total_doc_pages} pages from {args.pdf}")

    if args.pages:
        target_pages = parse_page_range(args.pages, total_doc_pages)
    else:
        target_pages = list(range(1, total_doc_pages + 1))

    print(f"Target pages to process:   {len(target_pages)} pages")
    args.cache_dir.mkdir(parents=True, exist_ok=True)

    refined_results = {}
    start_total_time = time.time()

    if args.mode == "vlm":
        print(f"\nBeginning VLM single-page visual transcription ({len(target_pages)} pages, 1 page/req)...")
        render_scale = args.dpi / 72.0

        for idx, page_num in enumerate(target_pages):
            cache_file = args.cache_dir / f"page_{page_num:03d}.md"
            print(
                f"[{idx+1}/{len(target_pages)}] Processing Page {page_num} ... ",
                end="",
                flush=True,
            )

            if cache_file.exists():
                print("(cached)")
                refined_results[page_num] = cache_file.read_text(encoding="utf-8")
                continue

            # Render page to JPEG in memory
            p_start = time.time()
            try:
                page_obj = doc[page_num - 1]
                pil_img = page_obj.render(scale=render_scale).to_pil()
                buf = BytesIO()
                pil_img.save(buf, format="JPEG", quality=85)
                b64 = base64.b64encode(buf.getvalue()).decode("utf-8")

                cleaned = call_vlm(args.api_base, model, b64, page_num)
                if re.match(rf"^\s*<!--\s*Page\s*{page_num}\s*-->", cleaned, re.IGNORECASE):
                    formatted = cleaned
                else:
                    formatted = f"<!-- Page {page_num} -->\n\n{cleaned}"
                cache_file.write_text(formatted, encoding="utf-8")
                refined_results[page_num] = formatted
                elapsed = time.time() - p_start
                print(f"done ({elapsed:.1f}s)")
            except Exception as e:
                print(f"FAILED: {e}")
                print("Stopping execution. Resume later after checking LLM server.")
                break

        ordered_markdown = [
            refined_results[p] for p in target_pages if p in refined_results
        ]

    else:
        # Text mode
        pages_dict = extract_pages_from_pdf(args.pdf)
        batches = []
        for i in range(0, len(target_pages), args.batch_size):
            batches.append(target_pages[i : i + args.batch_size])

        total_batches = len(batches)
        print(f"\nBeginning LLM text extraction ({total_batches} batches total)...")

        for b_idx, batch in enumerate(batches):
            batch_key = f"pages_{batch[0]}_to_{batch[-1]}"
            cache_file = args.cache_dir / f"{batch_key}.md"

            print(
                f"[{b_idx+1}/{total_batches}] Processing pages {batch} ... ",
                end="",
                flush=True,
            )

            if cache_file.exists():
                print("(cached)")
                refined_results[batch_key] = cache_file.read_text(encoding="utf-8")
                continue

            batch_content = "\n\n".join(
                pages_dict[p] for p in batch if p in pages_dict
            )

            b_start = time.time()
            try:
                cleaned = call_llm(args.api_base, model, batch_content)
                cache_file.write_text(cleaned, encoding="utf-8")
                refined_results[batch_key] = cleaned
                elapsed = time.time() - b_start
                print(f"done ({elapsed:.1f}s)")
            except Exception as e:
                print(f"FAILED: {e}")
                print("Stopping execution. Resume later after checking LLM server.")
                break

        ordered_markdown = []
        for batch in batches:
            batch_key = f"pages_{batch[0]}_to_{batch[-1]}"
            if batch_key in refined_results:
                ordered_markdown.append(refined_results[batch_key])

    # Assemble output
    if ordered_markdown:
        args.output.write_text("\n\n".join(ordered_markdown), encoding="utf-8")
        total_time = time.time() - start_total_time
        print(f"\nSuccessfully wrote Markdown to: {args.output}")
        print(
            f"Processed {len(refined_results)} units in {total_time:.1f}s"
        )

        # Backfill figures if requested
        if args.backfill_figures:
            print("\n--- Running pdfFigureExtractor Backfill ---")
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            from pdf_figure_extractor import PdfFigureExtractor

            extractor = PdfFigureExtractor(args.figure_pdf)
            extractor.backfill_markdown(args.output, args.output, args.figure_dir)


if __name__ == "__main__":
    main()
