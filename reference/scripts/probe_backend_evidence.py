#!/usr/bin/env python3
"""Compare pdf-inspector evidence with optional MuPDF structured text.

This is an experiment and diagnostic tool, not an extraction fallback. It runs
MuPDF's deterministic ``stext.json`` backend without OCR and highlights pages
where that backend exposes materially different text or layout evidence.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
from typing import Any, Iterable


TOKEN_PATTERN = re.compile(r"[^\W_]+(?:[\u2019'][^\W_]+)*", re.UNICODE)


def _tokens(texts: Iterable[str]) -> Counter[str]:
    tokens: Counter[str] = Counter()
    for text in texts:
        for token in TOKEN_PATTERN.findall(text.casefold()):
            # Lone letters are frequently bullets, chart labels, or fragmented
            # glyphs. Digits remain useful even when they are one character.
            if len(token) > 1 or token.isdigit():
                tokens[token] += 1
    return tokens


def _repeated_x_anchors(xs: Iterable[float], *, tolerance: float = 4.0) -> int:
    buckets = Counter(round(float(x) / tolerance) for x in xs)
    return sum(count >= 3 for count in buckets.values())


def local_pages(payload: dict[str, Any]) -> dict[int, dict[str, Any]]:
    """Summarize positioned ``pdf2md --items-json`` evidence by page."""
    pages: dict[int, dict[str, Any]] = {}
    for item in payload.get("items", []):
        page_number = int(item["page"])
        page = pages.setdefault(
            page_number,
            {"texts": [], "xs": [], "text_items": 0, "image_items": 0},
        )
        if item.get("item_type") == "image":
            page["image_items"] += 1
            continue
        text = str(item.get("text", ""))
        if text.strip():
            page["texts"].append(text)
            page["xs"].append(float(item.get("x", 0.0)))
            page["text_items"] += 1
    return pages


def alternate_pages(
    payload: dict[str, Any] | list[dict[str, Any]],
) -> dict[int, dict[str, Any]]:
    """Summarize MuPDF ``stext.json`` evidence by page."""
    pages: dict[int, dict[str, Any]] = {}
    raw_pages = payload if isinstance(payload, list) else payload.get("pages", [])
    for index, raw_page in enumerate(raw_pages, start=1):
        page_number = int(raw_page.get("number", index))
        page = {
            "texts": [],
            "xs": [],
            "text_blocks": 0,
            "text_lines": 0,
            "image_blocks": 0,
        }
        for block in raw_page.get("blocks", []):
            if block.get("type") == "image":
                page["image_blocks"] += 1
                continue
            if block.get("type") != "text":
                continue
            page["text_blocks"] += 1
            for line in block.get("lines", []):
                text = str(line.get("text", ""))
                if text.strip():
                    page["texts"].append(text)
                    bbox = line.get("bbox", {})
                    page["xs"].append(float(bbox.get("x", line.get("x", 0.0))))
                    page["text_lines"] += 1
        pages[page_number] = page
    return pages


def compare_page(
    local: dict[str, Any],
    alternate: dict[str, Any],
    *,
    min_token_gain: int,
    min_alternate_only_ratio: float,
    min_anchor_gain: int,
) -> dict[str, Any]:
    """Compare semantic and coarse layout evidence for one page."""
    local_tokens = _tokens(local.get("texts", []))
    alternate_tokens = _tokens(alternate.get("texts", []))
    shared = local_tokens & alternate_tokens
    alternate_only = alternate_tokens - local_tokens
    local_only = local_tokens - alternate_tokens
    local_total = sum(local_tokens.values())
    alternate_total = sum(alternate_tokens.values())
    shared_total = sum(shared.values())
    alternate_only_total = sum(alternate_only.values())
    local_only_total = sum(local_only.values())
    net_token_gain = alternate_total - local_total
    alternate_only_ratio = alternate_only_total / max(alternate_total, 1)

    local_anchors = _repeated_x_anchors(local.get("xs", []))
    alternate_anchors = _repeated_x_anchors(alternate.get("xs", []))
    anchor_gain = alternate_anchors - local_anchors
    image_gain = int(alternate.get("image_blocks", 0)) - int(
        local.get("image_items", 0)
    )

    reasons: list[str] = []
    if local_total == 0 and alternate_total >= max(5, min_token_gain // 2):
        reasons.append("local_text_empty")
    elif (
        net_token_gain >= min_token_gain
        and alternate_only_ratio >= min_alternate_only_ratio
    ):
        reasons.append("alternate_has_more_text")
    if anchor_gain >= min_anchor_gain:
        reasons.append("alternate_has_more_alignment_anchors")
    if image_gain > 0:
        reasons.append("alternate_has_more_image_blocks")

    if reasons:
        classification = "investigate_alternate_evidence"
    elif local_total - alternate_total >= min_token_gain:
        classification = "local_has_more_text"
    elif alternate_only_total + local_only_total:
        classification = "different_segmentation_or_decoding"
    else:
        classification = "equivalent_text_evidence"

    return {
        "classification": classification,
        "reasons": reasons,
        "tokens": {
            "local": local_total,
            "alternate": alternate_total,
            "shared": shared_total,
            "net_alternate_gain": net_token_gain,
            "alternate_only": alternate_only_total,
            "local_only": local_only_total,
            "alternate_only_ratio": alternate_only_ratio,
            "alternate_only_sample": sorted(alternate_only)[:12],
            "local_only_sample": sorted(local_only)[:12],
        },
        "layout": {
            "local_text_items": int(local.get("text_items", 0)),
            "local_image_items": int(local.get("image_items", 0)),
            "local_repeated_x_anchors": local_anchors,
            "alternate_text_blocks": int(alternate.get("text_blocks", 0)),
            "alternate_text_lines": int(alternate.get("text_lines", 0)),
            "alternate_image_blocks": int(alternate.get("image_blocks", 0)),
            "alternate_repeated_x_anchors": alternate_anchors,
        },
    }


def compare_documents(
    local_payload: dict[str, Any],
    alternate_payload: dict[str, Any] | list[dict[str, Any]],
    *,
    min_token_gain: int = 20,
    min_alternate_only_ratio: float = 0.15,
    min_anchor_gain: int = 2,
) -> dict[str, Any]:
    """Return a page-level evidence report for already extracted payloads."""
    local = local_pages(local_payload)
    alternate = alternate_pages(alternate_payload)
    page_numbers = sorted(local.keys() | alternate.keys())
    pages = []
    for page_number in page_numbers:
        result = compare_page(
            local.get(page_number, {}),
            alternate.get(page_number, {}),
            min_token_gain=min_token_gain,
            min_alternate_only_ratio=min_alternate_only_ratio,
            min_anchor_gain=min_anchor_gain,
        )
        result["page"] = page_number
        pages.append(result)

    flagged = [
        page
        for page in pages
        if page["classification"] == "investigate_alternate_evidence"
    ]
    return {
        "summary": {
            "pages": len(pages),
            "flagged_pages": len(flagged),
            "flagged_page_numbers": [page["page"] for page in flagged],
            "local_tokens": sum(page["tokens"]["local"] for page in pages),
            "alternate_tokens": sum(page["tokens"]["alternate"] for page in pages),
            "alternate_only_tokens": sum(
                page["tokens"]["alternate_only"] for page in pages
            ),
        },
        "pages": pages,
    }


def _json_command(command: list[str]) -> Any:
    try:
        completed = subprocess.run(
            command,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip() or "no diagnostic output"
        raise RuntimeError(f"command failed: {' '.join(command)}\n{detail}") from error
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"command did not return JSON: {' '.join(command)}: {error}"
        ) from error


def probe_pdf(
    pdf: Path,
    *,
    pdf2md: Path,
    mutool: Path,
    min_token_gain: int,
    min_alternate_only_ratio: float,
    min_anchor_gain: int,
) -> dict[str, Any]:
    local_payload = _json_command([str(pdf2md), str(pdf), "--items-json"])
    # `stext.json` is MuPDF's native structured text output. The OCR formats
    # are intentionally not used so this remains a deterministic no-model
    # comparison.
    alternate_payload = _json_command(
        [str(mutool), "draw", "-q", "-F", "stext.json", "-o", "-", str(pdf)]
    )
    report = compare_documents(
        local_payload,
        alternate_payload,
        min_token_gain=min_token_gain,
        min_alternate_only_ratio=min_alternate_only_ratio,
        min_anchor_gain=min_anchor_gain,
    )
    report["pdf"] = str(pdf)
    return report


def _arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdf", type=Path, nargs="+")
    parser.add_argument("--pdf2md", type=Path, default=Path("target/release/pdf2md"))
    parser.add_argument("--mutool", type=Path)
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--min-token-gain", type=int, default=20)
    parser.add_argument("--min-alternate-only-ratio", type=float, default=0.15)
    parser.add_argument("--min-anchor-gain", type=int, default=2)
    return parser.parse_args(argv)


def _print_report(result: dict[str, Any]) -> None:
    summary = result["summary"]
    print(f"\n{result['pdf']}")
    print(
        f"  {summary['flagged_pages']}/{summary['pages']} pages flagged; "
        f"tokens local={summary['local_tokens']} alternate={summary['alternate_tokens']} "
        f"alternate-only={summary['alternate_only_tokens']}"
    )
    for page in result["pages"]:
        if page["classification"] != "investigate_alternate_evidence":
            continue
        reasons = ", ".join(page["reasons"])
        tokens = page["tokens"]
        print(
            f"  page {page['page']}: {reasons}; "
            f"net tokens={tokens['net_alternate_gain']:+d}, "
            f"alternate-only={tokens['alternate_only']}"
        )


def main(argv: list[str] | None = None) -> int:
    args = _arguments(argv)
    pdf2md = args.pdf2md.absolute()
    mutool = args.mutool or (Path(found) if (found := shutil.which("mutool")) else None)
    if not pdf2md.is_file():
        print(f"error: pdf2md binary not found: {pdf2md}", file=sys.stderr)
        return 2
    if mutool is None or not mutool.is_file():
        print("error: mutool not found; install MuPDF or pass --mutool", file=sys.stderr)
        return 2
    if (
        args.min_token_gain < 0
        or args.min_anchor_gain < 0
        or not 0.0 <= args.min_alternate_only_ratio <= 1.0
    ):
        print("error: thresholds must be non-negative and ratio must be in [0, 1]", file=sys.stderr)
        return 2

    results = []
    for pdf in args.pdf:
        path = pdf.absolute()
        if not path.is_file():
            print(f"error: PDF not found: {path}", file=sys.stderr)
            return 2
        try:
            result = probe_pdf(
                path,
                pdf2md=pdf2md,
                mutool=mutool,
                min_token_gain=args.min_token_gain,
                min_alternate_only_ratio=args.min_alternate_only_ratio,
                min_anchor_gain=args.min_anchor_gain,
            )
        except RuntimeError as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
        results.append(result)
        _print_report(result)

    payload = {
        "schema_version": 1,
        "experiment": "optional_mupdf_stext_evidence",
        "ocr": False,
        "thresholds": {
            "min_token_gain": args.min_token_gain,
            "min_alternate_only_ratio": args.min_alternate_only_ratio,
            "min_anchor_gain": args.min_anchor_gain,
        },
        "documents": results,
    }
    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
