#!/usr/bin/env python3
# Minimal OCR routing example for pdf-inspector.
from argparse import ArgumentParser
from pathlib import Path
import pdf_inspector

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PDF = ROOT / 'tests' / 'fixtures' / 'thermo-freon12.pdf'


def field(obj, *names, default=None):
    for name in names:
        if isinstance(obj, dict) and name in obj:
            return obj[name]
        if hasattr(obj, name):
            return getattr(obj, name)
    return default


def call_inspector(pdf_path, thresholds):
    # Newer bindings may accept configurable thresholds. Older bindings
    # fall back to the stable single-argument call.
    try:
        return pdf_inspector.classify_pdf(str(pdf_path), thresholds=thresholds)
    except (AttributeError, TypeError):
        pass
    try:
        return pdf_inspector.process_pdf(str(pdf_path), thresholds=thresholds)
    except (AttributeError, TypeError):
        pass
    try:
        return pdf_inspector.classify_pdf(str(pdf_path))
    except (AttributeError, TypeError):
        pass
    return pdf_inspector.process_pdf(str(pdf_path))


def main():
    parser = ArgumentParser(description='Show pdf-inspector confidence and OCR routing.')
    parser.add_argument('pdf', nargs='?', default=str(DEFAULT_PDF))
    parser.add_argument('--text-min', type=float, default=0.85)
    parser.add_argument('--ocr-max', type=float, default=0.35)
    parser.add_argument('--mixed-page-threshold', type=float, default=0.25)
    args = parser.parse_args()

    thresholds = {
        'text_based_min_confidence': args.text_min,
        'needs_ocr_max_confidence': args.ocr_max,
        'mixed_page_threshold': args.mixed_page_threshold,
    }
    result = call_inspector(Path(args.pdf), thresholds)

    pdf_type = field(result, 'pdf_type', 'pdfType', default='unknown')
    confidence = field(result, 'confidence', default=None)
    pages = field(result, 'pages_needing_ocr', 'pagesNeedingOcr', default=[])

    print(f'PDF: {args.pdf}')
    print(f'Document class: {pdf_type}')
    print(f'Confidence: {confidence}')
    print(f'Pages needing OCR: {pages}')
    print('Thresholds requested: '
          f'text_min={args.text_min}, '
          f'ocr_max={args.ocr_max}, '
          f'mixed_page_threshold={args.mixed_page_threshold}')


if __name__ == '__main__':
    main()
