"""Basic usage examples for pdf-inspector Python library."""

import sys
import pdf_inspector


def main():
    if len(sys.argv) < 2:
        print("Usage: python basic_usage.py <path-to-pdf>")
        sys.exit(1)

    path = sys.argv[1]

    # 1. Full processing: detect + extract + markdown
    print("=" * 60)
    print("Full processing")
    print("=" * 60)
    result = pdf_inspector.process_pdf(path)
    print(f"Type:       {result.pdf_type}")
    print(f"Pages:      {result.page_count}")
    print(f"Confidence: {result.confidence:.0%}")
    print(f"Time:       {result.processing_time_ms}ms")
    print(f"Title:      {result.title}")
    print(f"Complex:    {result.is_complex_layout}")
    print(f"Tables on:  {result.pages_with_tables}")
    print(f"Columns on: {result.pages_with_columns}")
    print(f"Encoding:   {'issues detected' if result.has_encoding_issues else 'ok'}")
    print(f"OCR needed: {result.pages_needing_ocr or 'none'}")
    if result.markdown:
        print(f"\n--- Markdown ({len(result.markdown)} chars) ---")
        print(result.markdown[:500])
        if len(result.markdown) > 500:
            print(f"\n... ({len(result.markdown) - 500} more chars)")

    # 2. Fast detection only
    print("\n" + "=" * 60)
    print("Detection only")
    print("=" * 60)
    info = pdf_inspector.detect_pdf(path)
    print(f"Type:       {info.pdf_type}")
    print(f"Confidence: {info.confidence:.0%}")
    print(f"Time:       {info.processing_time_ms}ms")

    # 3. From bytes
    print("\n" + "=" * 60)
    print("From bytes")
    print("=" * 60)
    with open(path, "rb") as f:
        data = f.read()
    result = pdf_inspector.process_pdf_bytes(data)
    print(f"Type: {result.pdf_type}, Pages: {result.page_count}")

    # 4. Plain text
    print("\n" + "=" * 60)
    print("Plain text extraction")
    print("=" * 60)
    text = pdf_inspector.extract_text(path)
    print(text[:300])

    # 5. Positioned items
    print("\n" + "=" * 60)
    print("Positioned text items (first 10)")
    print("=" * 60)
    items = pdf_inspector.extract_text_with_positions(path, pages=[1])
    for item in items[:10]:
        bold = " [B]" if item.is_bold else ""
        italic = " [I]" if item.is_italic else ""
        print(
            f"  p{item.page} ({item.x:6.1f}, {item.y:6.1f}) "
            f"size={item.font_size:5.1f}{bold}{italic} "
            f"'{item.text}'"
        )

    # 6. Lightweight classification
    print("\n" + "=" * 60)
    print("Lightweight classification")
    print("=" * 60)
    cls = pdf_inspector.classify_pdf(path)
    print(f"Type:       {cls.pdf_type}")
    print(f"Pages:      {cls.page_count}")
    print(f"Confidence: {cls.confidence:.0%}")
    print(f"OCR pages:  {cls.pages_needing_ocr or 'none'} (0-indexed)")

    # 7. Region-based text extraction
    print("\n" + "=" * 60)
    print("Region-based text extraction (page 0, top region)")
    print("=" * 60)
    regions = pdf_inspector.extract_text_in_regions(
        path, [(0, [[0.0, 0.0, 600.0, 200.0]])]
    )
    for page_result in regions:
        for i, region in enumerate(page_result.regions):
            print(f"  Region {i}: needs_ocr={region.needs_ocr}")
            print(f"  Text: {region.text[:200]}")


if __name__ == "__main__":
    main()
