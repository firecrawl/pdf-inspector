"""Type stubs for pdf_inspector."""

from typing import Literal, Optional

class PdfResult:
    """Result of processing a PDF file."""
    pdf_type: str
    """'text_based', 'scanned', 'image_based', or 'mixed'."""
    markdown: Optional[str]
    page_count: int
    processing_time_ms: int
    pages_needing_ocr: list[int]
    """1-indexed page numbers that need OCR."""
    ocr_reasons_by_page: list["PageOcrReasons"]
    """Machine-readable OCR reasons by 1-indexed page."""
    title: Optional[str]
    confidence: float
    is_complex_layout: bool
    pages_with_tables: list[int]
    pages_with_columns: list[int]
    has_encoding_issues: bool

class PageOcrReasons:
    """OCR reasons for a single 1-indexed page."""
    page: int
    """1-indexed page number."""
    reasons: list[str]
    """Machine-readable OCR reason identifiers."""

class OcrModelIdentity:
    """Exact OCR model identity retained in page provenance."""
    name: str
    revision: str

class OcrTimings:
    """Per-page OCR processing timings."""
    render_ms: int
    ocr_ms: int
    assembly_ms: int

class OcrPageProvenance:
    """Source, model, confidence, and fallback metadata for one page."""
    page_number: int
    """1-indexed page number."""
    source: Literal["native", "ocr", "fused"]
    """'native', 'ocr', or 'fused'."""
    ocr_model: Optional[OcrModelIdentity]
    render_dpi: Optional[float]
    ocr_confidence: Optional[float]
    timings: OcrTimings
    warnings: list[str]
    hosted_recommended: bool

class OcrPageResult:
    """Final Markdown and provenance for one page."""
    page_number: int
    """1-indexed page number."""
    markdown: str
    provenance: OcrPageProvenance

class OcrPdfResult:
    """Complete native/OCR Markdown output."""
    markdown: str
    pages: list[OcrPageResult]
    page_count: int
    pages_recommended_for_ocr: list[int]
    pages_routed_to_ocr: list[int]
    pages_recommending_hosted: list[int]
    ocr_reasons_by_page: list[PageOcrReasons]
    pages_with_tables: list[int]
    pages_with_columns: list[int]
    is_complex: bool
    processing_time_ms: int
    render_time_ms: int
    ocr_time_ms: int

class PdfClassification:
    """Lightweight PDF classification result."""
    pdf_type: str
    """'text_based', 'scanned', 'image_based', or 'mixed'."""
    page_count: int
    pages_needing_ocr: list[int]
    """0-indexed page numbers that need OCR."""
    confidence: float

class TextItem:
    """A positioned text item extracted from a PDF.

    ``x``/``y`` are PDF points relative to the page's visible page box
    (``CropBox ∩ MediaBox``, else the MediaBox; a CropBox that does not overlap
    the MediaBox is ignored, and a page without a MediaBox is measured against
    US Letter), origin at the box's lower-left corner with ``y`` growing upward.
    :func:`extract_text_in_regions` reads its regions relative to the same box
    but from its top-left corner with ``y`` growing downward; flip with the box
    height. Pages whose text is drawn rotated by 90° are normalized into a
    synthetic landscape frame before the shift, and ``/Rotate`` is not applied.
    """
    text: str
    x: float
    """Left edge, in PDF points from the visible page box's left edge."""
    y: float
    """Baseline for text (rect bottom edge for image, link and form-field
    items), in PDF points from the visible page box's bottom edge."""
    width: float
    height: float
    """Axis-aligned box in PDF points (y-up): for horizontal text `y` is the
    baseline and `height` the em size; a rotated run is tall and thin."""
    rotation: float
    """Rotation of the run's baseline in degrees counter-clockwise from the
    page's x axis, in [0, 360): 0 for ordinary horizontal text, 90 for text
    reading bottom-to-top (a rotated margin stamp), 270 for top-to-bottom,
    180 for upside-down."""
    advance_known: bool
    """Whether the run's advance came from font metrics. False when the font
    carries no width information (or an ActualText span's advance could not be
    recovered): the box's extent along the baseline is then an estimate of half
    an em per painted glyph (an ActualText span counts the glyphs it covers, not
    its replacement text), not a measurement."""
    font: str
    font_tag: str
    font_size: float
    page: int
    is_bold: bool
    """Bold from the font name, the FontDescriptor's ForceBold flag, the
    embedded program's bold selection, or text filled and stroked to look
    heavier; with ``bold_from_weight`` also from the weight class.
    ``bold_source`` says which."""
    is_italic: bool
    font_weight: Optional[int]
    """The font's weight class on the 100..900 scale shared by CSS
    ``font-weight`` and the OS/2 ``usWeightClass`` field (400 regular, 700
    bold): the embedded font program's OS/2 table, else the FontDescriptor's
    ``/FontWeight``, else a weight word in the font name ("Light", "Medium",
    "-Md", "Black", "W6"). ``None`` when none of them says, and for image, link
    and form-field items. Independent of ``is_bold``, which is unchanged: a
    medium face reports ``500`` with ``is_bold`` ``False``."""
    bold_source: Optional[str]
    """Where ``is_bold`` came from — ``"font_name"``, ``"font_flags"``,
    ``"weight_class"`` (with ``bold_from_weight``) or ``"painted"``, the first
    of them in that order when more than one says bold — so a verdict can be
    weighed against ``font_weight``: a face whose name says Bold over a weight
    class of 400 reports ``"font_name"``. ``None`` when ``is_bold`` is
    ``False``, and for image, link and form-field items."""
    fixed_pitch: Optional[bool]
    """Whether the font is fixed-pitch (monospaced): ``True`` when the
    FontDescriptor's FixedPitch flag or the embedded program's ``post`` table
    says so, else measured from the font's width table — ``True`` when a dozen
    or more of its glyphs share one advance, ``False`` when two differ.
    ``None`` when the font declares nothing and carries too few glyphs to
    measure, and for image, link and form-field items. Many producers write
    ``/Flags 4`` whatever the face, so the flag is only ever read as a yes."""
    is_underline: bool
    is_strikeout: bool
    baseline_shift: float
    """Signed baseline offset (points) of a super/subscript glyph run from the
    body baseline it is attached to; 0.0 for normal text. Positive = raised
    (superscript: footnote/affiliation markers, exponents), negative = lowered
    (subscript). Digit-only markers beside a word are already fused into it as
    Unicode super/subscript characters ("word²") and carry 0.0."""
    item_type: str
    mcid: Optional[int]
    """Marked Content ID from the content stream's BDC/BMC operator, None when
    the text is not part of marked content. Join with the (page, mcid) pairs
    from extract_structure_elements to attach structure-tree roles in tagged
    PDFs."""

class PageRotation:
    """The coordinate frame of a page whose text was predominantly rotated."""
    page: int
    """1-indexed page number, matching TextItem.page."""
    rotation: Literal["ccw", "cw"]
    """'ccw' when the page's runs read bottom-to-top and the frame was turned so
    they read left-to-right, 'cw' for runs reading top-to-bottom."""

class PositionedText:
    """Positioned text plus the frame of every page whose text was turned."""
    items: list[TextItem]
    page_rotations: list[PageRotation]
    """One entry per re-based page; pages absent here are upright and their
    items are in plain page coordinates."""

def extract_text_with_positions_and_rotations(
    path: str, bold_from_weight: bool = False, bold_weight_threshold: int = 600
) -> PositionedText:
    """Extract positioned text plus the coordinate frame of every page whose
    text was predominantly rotated (items on such pages are in the turned
    frame). ``bold_from_weight`` and ``bold_weight_threshold`` are the options
    of :func:`extract_text_with_positions`."""
    ...

def extract_text_with_positions_and_rotations_bytes(
    data: bytes, bold_from_weight: bool = False, bold_weight_threshold: int = 600
) -> PositionedText:
    """Bytes variant of extract_text_with_positions_and_rotations."""
    ...

class StructureElement:
    """One structure-tree element reference from a tagged PDF."""
    page: int
    """1-indexed page number (matches TextItem.page)."""
    mcid: int
    """Marked Content ID from the page's content stream (matches TextItem.mcid)."""
    role: str
    """Standard structure type name ("H1".."H6", "P", "Table", "TD", ...)."""

class RegionText:
    """Extracted text for a single region."""
    text: str
    needs_ocr: bool
    """True when the text should not be trusted."""
    ocr_reason: Optional[str]
    """Machine-readable OCR reason when the cause is known."""

class PageRegionTexts:
    """Extracted text for one page's regions."""
    page: int
    """0-indexed page number."""
    regions: list[RegionText]

class PageMarkdown:
    """Per-page markdown extraction result."""
    page: int
    """0-indexed page number."""
    markdown: str
    """Formatted markdown for this page (empty string when needs_ocr is True)."""
    needs_ocr: bool
    """True when text on this page is unreliable and OCR should be used instead."""
    ocr_reason: Optional[str]
    """Machine-readable OCR reason when the cause is known."""

class PagesExtractionResult:
    """Per-page markdown output with document-wide layout classification."""
    pages: list[PageMarkdown]
    """Per-page markdown results, in the order requested."""
    pages_with_tables: list[int]
    """1-indexed pages where tables were detected."""
    pages_with_columns: list[int]
    """1-indexed pages where multi-column layout was detected."""
    pages_needing_ocr: list[int]
    """1-indexed pages that need OCR."""
    ocr_reasons_by_page: list[PageOcrReasons]
    """Machine-readable OCR reasons by 1-indexed page."""
    is_complex: bool
    """True if any page has tables or multi-column layout."""

def process_pdf(path: str, pages: Optional[list[int]] = None) -> PdfResult:
    """Process a PDF: detect type, extract text, convert to Markdown."""
    ...

def process_pdf_bytes(data: bytes, pages: Optional[list[int]] = None) -> PdfResult:
    """Process a PDF from bytes in memory."""
    ...

def process_pdf_with_ocr(
    path: str,
    *,
    mode: Literal["off", "auto", "force"] = "auto",
    page_numbers: Optional[list[int]] = None,
    password: Optional[str] = None,
    dpi: float = 150.0,
    minimum_confidence: float = 0.0,
    hosted_recommendation_confidence: float = 0.5,
    model_directory: Optional[str] = None,
    offline: bool = False,
) -> OcrPdfResult:
    """Process a PDF through native extraction and selective OCR.

    Page numbers are 1-indexed. OCR runs without holding the Python GIL.
    """
    ...

def process_pdf_with_ocr_bytes(
    data: bytes,
    *,
    mode: Literal["off", "auto", "force"] = "auto",
    page_numbers: Optional[list[int]] = None,
    password: Optional[str] = None,
    dpi: float = 150.0,
    minimum_confidence: float = 0.0,
    hosted_recommendation_confidence: float = 0.5,
    model_directory: Optional[str] = None,
    offline: bool = False,
) -> OcrPdfResult:
    """Process PDF bytes through native extraction and selective OCR."""
    ...

def detect_pdf(path: str) -> PdfResult:
    """Fast detection only — no text extraction."""
    ...

def detect_pdf_bytes(data: bytes) -> PdfResult:
    """Fast detection from bytes."""
    ...

def classify_pdf(path: str) -> PdfClassification:
    """Lightweight classification — type, page count, and OCR pages (0-indexed)."""
    ...

def classify_pdf_bytes(data: bytes) -> PdfClassification:
    """Lightweight classification from bytes."""
    ...

def extract_text(path: str) -> str:
    """Extract plain text from a PDF."""
    ...

def extract_text_bytes(data: bytes) -> str:
    """Extract plain text from PDF bytes."""
    ...

def extract_text_with_positions(
    path: str,
    pages: Optional[list[int]] = None,
    bold_from_weight: bool = False,
    bold_weight_threshold: int = 600,
) -> list[TextItem]:
    """Extract text with position information.

    ``x``/``y`` are PDF points relative to the page's visible page box
    (``CropBox ∩ MediaBox``, else the MediaBox), origin at its lower-left
    corner with ``y`` up. :func:`extract_text_in_regions` reads regions
    relative to the same box from its top-left corner, so flip with the box
    height ``h``: for text items ``y`` is the baseline and
    ``[x, h - y - height, x + width, h - y]`` covers the glyph band above it
    (descenders fall below); for image, link and form-field items ``y`` is the
    rect bottom and that box is exact. Pages whose text is drawn rotated by
    90° are normalized into a synthetic landscape frame, where this does not
    apply.

    Args:
        path: Path to the PDF file.
        pages: Optional list of 1-indexed pages (matching ``TextItem.page``).
            When ``None`` (default), the whole document is returned.
        bold_from_weight: Also read bold from the font's weight class:
            ``TextItem.is_bold`` is then ``True`` as well when
            ``TextItem.font_weight`` is ``bold_weight_threshold`` or more
            (``bold_source`` ``"weight_class"``), and adjacent runs are merged
            by that verdict: a run the weight makes bold stays apart from its
            plain neighbours, so a heavier run inside a lighter paragraph
            keeps its own item, while runs whose weights differ but agree on
            bold merge as usual. ``False`` by default, where ``is_bold`` and
            item merging are unchanged; ``font_weight`` is reported either
            way.
        bold_weight_threshold: The weight class from which
            ``bold_from_weight`` reads bold, on the 100..900 scale; 600
            (SemiBold) by default. Read only when ``bold_from_weight`` is
            ``True``; a value outside 100..900 raises ``ValueError``.
    """
    ...

def extract_text_with_positions_bytes(
    data: bytes,
    pages: Optional[list[int]] = None,
    bold_from_weight: bool = False,
    bold_weight_threshold: int = 600,
) -> list[TextItem]:
    """Extract text with position information from bytes.

    See :func:`extract_text_with_positions` for the coordinate frame.
    """
    ...

def extract_structure_elements(path: str, pages: Optional[list[int]] = None) -> list[StructureElement]:
    """Extract structure-tree element references from a tagged PDF file.

    Returns one entry per marked-content reference, resolved to its 1-indexed
    page, MCID, and structure type name ("H1".."H6", "P", "Table", ...), sorted
    by (page, mcid). Returns an empty list when the PDF is not tagged.

    Args:
        path: Path to the PDF file.
        pages: Optional list of 1-indexed pages (matching ``TextItem.page``).
            When ``None`` (default), the whole document is returned.
    """
    ...

def extract_structure_elements_bytes(data: bytes, pages: Optional[list[int]] = None) -> list[StructureElement]:
    """Extract structure-tree element references from tagged PDF bytes.

    See :func:`extract_structure_elements` for details.
    """
    ...

def extract_text_in_regions(
    path: str,
    page_regions: list[tuple[int, list[list[float]]]],
    bold_from_weight: bool = False,
    bold_weight_threshold: int = 600,
) -> list[PageRegionTexts]:
    """Extract text within bounding-box regions from a PDF file.

    Args:
        path: Path to the PDF file.
        page_regions: List of (page_0indexed, [[x1, y1, x2, y2], ...]) tuples.
            Coordinates are PDF points with top-left origin, relative to the
            visible page box (``CropBox ∩ MediaBox``, else the MediaBox) — the
            same box :func:`extract_text_with_positions` reports items in,
            flipped to a top-left origin (``y_top = box_height - y``).
        bold_from_weight: The option of :func:`extract_text_with_positions`:
            read bold from the font's weight class too, so a run the weight
            makes bold is its own item while a region's lines are assembled.
            ``False`` by default.
        bold_weight_threshold: The weight class ``bold_from_weight`` reads
            bold from, 100..900; 600 by default.
    """
    ...

def extract_text_in_regions_bytes(
    data: bytes,
    page_regions: list[tuple[int, list[list[float]]]],
    bold_from_weight: bool = False,
    bold_weight_threshold: int = 600,
) -> list[PageRegionTexts]:
    """Extract text within bounding-box regions from PDF bytes.

    Args:
        data: PDF file contents as bytes.
        page_regions: List of (page_0indexed, [[x1, y1, x2, y2], ...]) tuples.
            Coordinates: see :func:`extract_text_in_regions`.
    """
    ...

def extract_pages_markdown(
    path: str,
    pages: Optional[list[int]] = None,
) -> PagesExtractionResult:
    """Extract formatted markdown for pages of a PDF, with layout classification.

    Args:
        path: Path to the PDF file.
        pages: Optional list of 0-indexed pages. When ``None`` (default), every
            page is returned in document order. Otherwise, output matches the
            caller-supplied order.

    Returns:
        PagesExtractionResult with per-page markdown and document-wide layout
        classification (tables, columns, OCR needs).
    """
    ...

def extract_pages_markdown_bytes(
    data: bytes,
    pages: Optional[list[int]] = None,
) -> PagesExtractionResult:
    """Extract formatted markdown for pages of a PDF from bytes.

    See :func:`extract_pages_markdown` for details.
    """
    ...
