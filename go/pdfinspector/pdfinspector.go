// Package pdfinspector provides Go bindings to pdf-inspector's PDF
// classification, text extraction, markdown conversion, and table
// structure recovery, via cgo against the compiled Rust library in go/
// (see go/src/lib.rs for the C ABI, and go/README.md for how to build it).
//
// The surface mirrors the napi (Node.js) and Python bindings' full
// document-processing API, including selective OCR: classify/detect/process
// a PDF, extract text (plain, positioned, per-page-markdown, or
// region-scoped), read a tagged PDF's structure tree, recover table
// structure from an externally supplied TSR model's output, and run native
// extraction with selective OCR via [ProcessPdfWithOcr]. PDFium and an ONNX
// Runtime backend are loaded dynamically at runtime (not linked at build
// time) and are only required on the host when OCR actually routes a page —
// see "OCR" in go/README.md for how to make them available.
//
// Every function takes the PDF as `[]byte` (no filesystem access inside
// the binding) and returns a Go error built from the Rust side's error
// message on failure; there is no panic path across the FFI boundary (the
// Rust side catches panics and reports them as errors instead).
//
// Run `go generate ./...` before building on a supported platform
// (darwin/arm64, darwin/amd64, linux/amd64, linux/arm64) to fetch a
// prebuilt native library instead of requiring a local Rust toolchain; it
// no-ops if one is already built. On other platforms, or if you'd rather
// build from source, run `cargo build --release` in go/ (or `make native`)
// directly — see go/README.md.
//
//go:generate go run ./internal/fetchnative
package pdfinspector

/*
#cgo CFLAGS: -I${SRCDIR}/../include
#cgo LDFLAGS: -L${SRCDIR}/../target/release -lpdf_inspector_go -Wl,-rpath,${SRCDIR}/../target/release
#include <pdf_inspector.h>
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"unsafe"
)

// PdfType classifies a PDF's text layer, mirroring pdf_inspector::PdfType.
type PdfType string

const (
	TextBased  PdfType = "TextBased"
	Scanned    PdfType = "Scanned"
	ImageBased PdfType = "ImageBased"
	Mixed      PdfType = "Mixed"
)

// PageOcrReasons carries machine-readable OCR reason identifiers for one
// page. Which indexing convention `Page` uses depends on the containing
// result — see each function's doc comment.
type PageOcrReasons struct {
	Page    uint32   `json:"page"`
	Reasons []string `json:"reasons"`
}

// Classification is the result of [Classify]: enough information to decide
// whether a PDF's text layer can be trusted, without extracting anything.
type Classification struct {
	PdfType   PdfType `json:"pdf_type"`
	PageCount uint32  `json:"page_count"`
	// PagesNeedingOCR is 0-indexed, matching classify_pdf_mem's convention.
	PagesNeedingOCR []uint32 `json:"pages_needing_ocr"`
	// Confidence is a score in [0.0, 1.0]; higher means more confident the
	// PdfType classification is correct.
	Confidence float32 `json:"confidence"`
}

// PdfResult is the result of [ProcessPdf] and [DetectPdf]: the full
// classification plus (for ProcessPdf) extracted Markdown.
type PdfResult struct {
	PdfType          PdfType `json:"pdf_type"`
	Markdown         *string `json:"markdown"`
	PageCount        uint32  `json:"page_count"`
	ProcessingTimeMs uint64  `json:"processing_time_ms"`
	// PagesNeedingOCR is 1-indexed here (unlike [Classification]'s
	// 0-indexed field) — matches the core crate's PdfProcessResult.
	PagesNeedingOCR   []uint32         `json:"pages_needing_ocr"`
	OcrReasonsByPage  []PageOcrReasons `json:"ocr_reasons_by_page"`
	Title             *string          `json:"title"`
	Confidence        float32          `json:"confidence"`
	IsComplexLayout   bool             `json:"is_complex_layout"`
	PagesWithTables   []uint32         `json:"pages_with_tables"`
	PagesWithColumns  []uint32         `json:"pages_with_columns"`
	HasEncodingIssues bool             `json:"has_encoding_issues"`
}

// TextItem is a positioned, styled piece of extracted content: text, an
// image placeholder, a hyperlink, or a form field.
type TextItem struct {
	Text        string  `json:"text"`
	X           float32 `json:"x"`
	Y           float32 `json:"y"`
	Width       float32 `json:"width"`
	Height      float32 `json:"height"`
	Font        string  `json:"font"`
	FontSize    float32 `json:"font_size"`
	Page        uint32  `json:"page"` // 1-indexed
	IsBold      bool    `json:"is_bold"`
	IsItalic    bool    `json:"is_italic"`
	IsUnderline bool    `json:"is_underline"`
	IsStrikeout bool    `json:"is_strikeout"`
	// ItemType is one of "text", "image", "link", or "form_field".
	ItemType string `json:"item_type"`
	// LinkURL is set when ItemType == "link", nil otherwise.
	LinkURL *string `json:"link_url"`
	// Mcid is the Marked Content ID linking this item to a tagged PDF's
	// structure tree, when present.
	Mcid *int64 `json:"mcid"`
}

// StructureElement is one marked-content reference from a tagged PDF's
// structure tree, resolved to its page, MCID, and structure type name.
type StructureElement struct {
	Page uint32 `json:"page"` // 1-indexed, matches TextItem.Page
	Mcid int64  `json:"mcid"`
	Role string `json:"role"` // e.g. "H1".."H6", "P", "Table", "TD"
}

// PageMarkdown is one page's result from [ExtractPagesMarkdown].
type PageMarkdown struct {
	Page     uint32 `json:"page"` // 0-indexed
	Markdown string `json:"markdown"`
	// NeedsOCR is true when text on this page is unreliable (GID-encoded
	// fonts, encoding issues, garbage text, or empty extraction).
	NeedsOCR  bool    `json:"needs_ocr"`
	OcrReason *string `json:"ocr_reason"`
}

// PagesExtractionResult is the result of [ExtractPagesMarkdown]: per-page
// markdown plus document-wide layout classification.
type PagesExtractionResult struct {
	Pages            []PageMarkdown   `json:"pages"`
	PagesWithTables  []uint32         `json:"pages_with_tables"`  // 1-indexed
	PagesWithColumns []uint32         `json:"pages_with_columns"` // 1-indexed
	PagesNeedingOCR  []uint32         `json:"pages_needing_ocr"`  // 1-indexed
	OcrReasonsByPage []PageOcrReasons `json:"ocr_reasons_by_page"`
	IsComplex        bool             `json:"is_complex"`
}

// RegionText is the result of extracting one bounding-box region.
type RegionText struct {
	Text string `json:"text"`
	// NeedsOCR is true when the text should not be trusted (empty,
	// GID-encoded fonts, garbage, encoding issues).
	NeedsOCR  bool    `json:"needs_ocr"`
	OcrReason *string `json:"ocr_reason"`
}

// PageRegionTexts is one page's region results, parallel to the
// [PageRegions] entry that produced it.
type PageRegionTexts struct {
	Page    uint32       `json:"page"` // 0-indexed
	Regions []RegionText `json:"regions"`
}

// PageRegions is one page's bounding-box regions to extract, for
// [ExtractTextInRegions] and [ExtractTablesInRegions]. Coordinates are PDF
// points with top-left origin.
type PageRegions struct {
	Page    uint32       `json:"page"` // 0-indexed
	Regions [][4]float32 `json:"regions"`
}

// VectorGridDetection is the result of [DetectVectorGridInRegion]: a
// TSR-compatible structure recovered from ruled lines or rectangles,
// without any external model.
type VectorGridDetection struct {
	StructureTokens []string    `json:"structure_tokens"`
	CellBboxes      [][]float32 `json:"cell_bboxes"`
}

// TsrTableInput pairs one externally-recovered table structure (e.g. from
// an SLANet/TSR model run on a rendered crop) with the page region it came
// from, for [ExtractTablesWithStructure] and its siblings. pdf-inspector
// lays out the cells and pulls text from the native PDF — no OCR involved.
type TsrTableInput struct {
	Page uint32 `json:"page"` // 0-indexed
	// CropPdfPtBbox is the crop's bbox on the page, PDF points, top-left origin.
	CropPdfPtBbox [4]float32 `json:"crop_pdf_pt_bbox"`
	// RenderDpi is the DPI the crop image was rendered at (e.g. 200.0).
	RenderDpi float32 `json:"render_dpi"`
	// StructureTokens are the raw structure tokens emitted by the TSR
	// model, in document order.
	StructureTokens []string `json:"structure_tokens"`
	// CellBboxes has one bbox per cell (document order), each either a
	// 4-element [x1,y1,x2,y2] or 8-element 4-corner polygon, in crop
	// image-pixel space.
	CellBboxes [][]float32 `json:"cell_bboxes"`
}

// StructuredCell is one resolved cell from [ExtractTablesWithStructureCells].
type StructuredCell struct {
	Row      int    `json:"row"` // 0-indexed grid row
	Col      int    `json:"col"` // 0-indexed grid column
	Rowspan  int    `json:"rowspan"`
	Colspan  int    `json:"colspan"`
	IsHeader bool   `json:"is_header"`
	Text     string `json:"text"`
	// PagePtBbox is [x1,y1,x2,y2] in page PDF-points, top-left origin.
	PagePtBbox [4]float32 `json:"page_pt_bbox"`
}

// OcrMode controls whether/when [ProcessPdfWithOcr] renders and runs OCR on
// a page. The zero value ("") is treated as "auto" by the Rust side.
type OcrMode string

const (
	// OcrOff never rasterizes or runs OCR; only native PDF text is used.
	// Exercises the full ProcessPdfWithOcr result/provenance contract
	// without requiring PDFium or an ONNX Runtime library to be present.
	OcrOff OcrMode = "off"
	// OcrAuto (the default) renders and runs OCR only on pages pdf-inspector's
	// native pass flags as needing it.
	OcrAuto OcrMode = "auto"
	// OcrForce renders and runs OCR on every selected page, including pages
	// with usable native text.
	OcrForce OcrMode = "force"
)

// PageContentSource identifies which pass produced a page's final content.
type PageContentSource string

const (
	SourceNative PageContentSource = "native"
	SourceOCR    PageContentSource = "ocr"
	SourceFused  PageContentSource = "fused"
)

// OcrOptions configures [ProcessPdfWithOcr]. The zero value runs with mode
// "auto" and every other core default (150 DPI, minimum confidence 0.0,
// hosted-recommendation threshold 0.5, online model downloads).
type OcrOptions struct {
	// Mode defaults to [OcrAuto] when empty.
	Mode OcrMode `json:"mode,omitempty"`
	// PageNumbers is 1-indexed; nil processes every page. No `omitempty`:
	// see pagesParams.Pages's comment — an explicit non-nil empty slice
	// must stay distinguishable from nil on the wire (`[]` vs `null`),
	// which `omitempty` would collapse.
	PageNumbers []uint32 `json:"page_numbers"`
	// Password decrypts an encrypted PDF, same as elsewhere in this package.
	Password string `json:"password,omitempty"`
	// Dpi is the page rasterization resolution used when a page is routed
	// to OCR. Nil means "use the core default" (150 DPI).
	Dpi *float32 `json:"dpi,omitempty"`
	// MinimumConfidence drops OCR spans below this inclusive 0-1 threshold.
	// Nil means "use the core default" (0.0). A pointer, not a bare
	// float32, so an explicit 0.0 is distinguishable from "unset" — the
	// core default for this field happens to be 0.0, but
	// HostedRecommendationConfidence's is not, and both should behave the
	// same way for the same reason.
	MinimumConfidence *float32 `json:"minimum_confidence,omitempty"`
	// HostedRecommendationConfidence recommends Firecrawl's hosted pipeline
	// for pages whose OCR confidence falls below this inclusive 0-1
	// threshold despite native extraction flagging them as needing OCR.
	// Nil means "use the core default" (0.5).
	HostedRecommendationConfidence *float32 `json:"hosted_recommendation_confidence,omitempty"`
	// ModelDirectory points at an offline OCR model set.
	ModelDirectory string `json:"model_directory,omitempty"`
	// Offline disables model downloads, requiring ModelDirectory or a warm
	// model cache.
	Offline bool `json:"offline,omitempty"`
}

// OcrModelIdentity is the exact OCR model identity retained in page
// provenance.
type OcrModelIdentity struct {
	Name     string `json:"name"`
	Revision string `json:"revision"`
}

// OcrTimings carries per-page OCR processing timings, in milliseconds.
type OcrTimings struct {
	RenderMs   uint64 `json:"render_ms"`
	OcrMs      uint64 `json:"ocr_ms"`
	AssemblyMs uint64 `json:"assembly_ms"`
}

// OcrPageProvenance carries source, model, confidence, and fallback
// metadata for one page of an [OcrPdfResult].
type OcrPageProvenance struct {
	PageNumber uint32            `json:"page_number"` // 1-indexed
	Source     PageContentSource `json:"source"`
	OcrModel   *OcrModelIdentity `json:"ocr_model"`
	RenderDpi  *float32          `json:"render_dpi"`
	// OcrConfidence is nil unless Source is [SourceOCR] or [SourceFused].
	OcrConfidence *float32   `json:"ocr_confidence"`
	Timings       OcrTimings `json:"timings"`
	Warnings      []string   `json:"warnings"`
	// HostedRecommended is true when this lightweight local path detected a
	// case better suited to Firecrawl's hosted document pipeline.
	HostedRecommended bool `json:"hosted_recommended"`
}

// OcrPageResult is the final Markdown and provenance for one page.
type OcrPageResult struct {
	PageNumber uint32            `json:"page_number"` // 1-indexed
	Markdown   string            `json:"markdown"`
	Provenance OcrPageProvenance `json:"provenance"`
}

// OcrPdfResult is the result of [ProcessPdfWithOcr]: complete native/OCR
// Markdown output plus per-page provenance and routing metadata.
type OcrPdfResult struct {
	Markdown                string           `json:"markdown"`
	Pages                   []OcrPageResult  `json:"pages"`
	PageCount               uint32           `json:"page_count"`
	PagesRecommendedForOCR  []uint32         `json:"pages_recommended_for_ocr"` // 1-indexed
	PagesRoutedToOCR        []uint32         `json:"pages_routed_to_ocr"`       // 1-indexed
	PagesRecommendingHosted []uint32         `json:"pages_recommending_hosted"` // 1-indexed
	OcrReasonsByPage        []PageOcrReasons `json:"ocr_reasons_by_page"`
	PagesWithTables         []uint32         `json:"pages_with_tables"`  // 1-indexed
	PagesWithColumns        []uint32         `json:"pages_with_columns"` // 1-indexed
	IsComplex               bool             `json:"is_complex"`
	ProcessingTimeMs        uint64           `json:"processing_time_ms"`
	// RenderTimeMs and OcrTimeMs are both zero when no page was routed to OCR
	// (e.g. Mode is [OcrOff], or Auto/Force routed nothing).
	RenderTimeMs uint64 `json:"render_time_ms"`
	OcrTimeMs    uint64 `json:"ocr_time_ms"`
}

// TableExtractionResult is one result from [ExtractTablesWithStructureAuto].
type TableExtractionResult struct {
	Markdown string `json:"markdown"`
	// FallbackReason is nil when the TSR-hybrid path produced the markdown
	// directly, or a short diagnostic label (e.g.
	// "multi_row_in_cell_expanded", "phantom_empty_row") when a detected
	// TSR pathology triggered in-place cell expansion or the heuristic
	// fallback extractor.
	FallbackReason *string `json:"fallback_reason"`
}

// ---------------------------------------------------------------------------
// Envelopes (internal decode targets — see go/src/results.rs)
// ---------------------------------------------------------------------------

type classifyEnvelope struct {
	Ok     bool            `json:"ok"`
	Result *Classification `json:"result"`
	Error  *string         `json:"error"`
}

type textEnvelope struct {
	Ok    bool    `json:"ok"`
	Text  *string `json:"text"`
	Error *string `json:"error"`
}

type pdfResultEnvelope struct {
	Ok     bool       `json:"ok"`
	Result *PdfResult `json:"result"`
	Error  *string    `json:"error"`
}

type textItemsEnvelope struct {
	Ok    bool       `json:"ok"`
	Items []TextItem `json:"items"`
	Error *string    `json:"error"`
}

type structureElementsEnvelope struct {
	Ok       bool               `json:"ok"`
	Elements []StructureElement `json:"elements"`
	Error    *string            `json:"error"`
}

type pagesExtractionEnvelope struct {
	Ok     bool                   `json:"ok"`
	Result *PagesExtractionResult `json:"result"`
	Error  *string                `json:"error"`
}

type pageRegionTextsEnvelope struct {
	Ok      bool              `json:"ok"`
	Results []PageRegionTexts `json:"results"`
	Error   *string           `json:"error"`
}

type vectorGridEnvelope struct {
	Ok     bool                 `json:"ok"`
	Found  bool                 `json:"found"`
	Result *VectorGridDetection `json:"result"`
	Error  *string              `json:"error"`
}

type markdownStringsEnvelope struct {
	Ok      bool     `json:"ok"`
	Results []string `json:"results"`
	Error   *string  `json:"error"`
}

type structuredCellsEnvelope struct {
	Ok      bool               `json:"ok"`
	Results [][]StructuredCell `json:"results"`
	Error   *string            `json:"error"`
}

type tableExtractionEnvelope struct {
	Ok      bool                    `json:"ok"`
	Results []TableExtractionResult `json:"results"`
	Error   *string                 `json:"error"`
}

type ocrPdfEnvelope struct {
	Ok     bool          `json:"ok"`
	Result *OcrPdfResult `json:"result"`
	Error  *string       `json:"error"`
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

// Classify inspects a PDF's bytes and reports its type, page count, which
// 0-indexed pages need OCR, and a confidence score — without extracting any
// text. Typically 10-50ms even on large documents, since it samples content
// streams rather than fully parsing them.
func Classify(data []byte) (*Classification, error) {
	var env classifyEnvelope
	if err := callNoParams(data, &env, func(d *C.uchar, l C.size_t) *C.char {
		return C.pdfinspector_classify(d, l)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if env.Result == nil {
		return nil, errors.New("pdfinspector: classify reported ok with no result")
	}
	return env.Result, nil
}

// ExtractText extracts a PDF's plain text (no layout, formatting, or
// markdown). Callers that need to know first whether the extracted text is
// trustworthy should call [Classify] and check its Confidence/PdfType.
func ExtractText(data []byte) (string, error) {
	var env textEnvelope
	if err := callNoParams(data, &env, func(d *C.uchar, l C.size_t) *C.char {
		return C.pdfinspector_extract_text(d, l)
	}); err != nil {
		return "", err
	}
	if !env.Ok {
		return "", wrapError(env.Error)
	}
	if env.Text == nil {
		return "", errors.New("pdfinspector: extract_text reported ok with no text")
	}
	return *env.Text, nil
}

// ProcessPdf runs full extraction: detect type, extract text, and convert
// to Markdown. Pass nil for pages to process every page; otherwise pages
// are **1-indexed** (matching Python's `process_pdf(path, pages=[1, 3, 5])`
// and napi's `processPdf` — this is the one function besides
// [ExtractTextWithPositions] and [ExtractStructureElements] where this
// package departs from its usual 0-indexed convention, because it forwards
// directly to the core crate's 1-indexed `PdfOptions::pages`).
func ProcessPdf(data []byte, pages []uint32) (*PdfResult, error) {
	var env pdfResultEnvelope
	if err := call(data, pagesParams{Pages: pages}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_process_pdf(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if env.Result == nil {
		return nil, errors.New("pdfinspector: process_pdf reported ok with no result")
	}
	return env.Result, nil
}

// DetectPdf runs fast detection only — no text extraction or Markdown
// conversion. The result has the same shape as [ProcessPdf] with Markdown
// always nil.
func DetectPdf(data []byte) (*PdfResult, error) {
	var env pdfResultEnvelope
	if err := callNoParams(data, &env, func(d *C.uchar, l C.size_t) *C.char {
		return C.pdfinspector_detect_pdf(d, l)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if env.Result == nil {
		return nil, errors.New("pdfinspector: detect_pdf reported ok with no result")
	}
	return env.Result, nil
}

// ProcessPdfWithOcr processes a PDF through native extraction with
// selective OCR. Native extraction always runs first; [OcrAuto] renders and
// runs OCR only on pages the native pass flags, [OcrForce] runs it on every
// selected page, and [OcrOff] never touches the renderer or OCR engine at
// all — useful for exercising this function's full result/provenance
// contract without PDFium or an ONNX Runtime library present.
//
// Pass nil for options to run with every default ([OcrAuto], 150 DPI,
// online model downloads). [OcrAuto]/[OcrForce] need PDFium and an ONNX
// Runtime library available on the host at runtime — see go/README.md's
// OCR section for how to make those available.
func ProcessPdfWithOcr(data []byte, options *OcrOptions) (*OcrPdfResult, error) {
	if options == nil {
		options = &OcrOptions{}
	}
	var env ocrPdfEnvelope
	if err := call(data, options, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_process_pdf_with_ocr(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if env.Result == nil {
		return nil, errors.New("pdfinspector: process_pdf_with_ocr reported ok with no result")
	}
	return env.Result, nil
}

// ExtractPagesMarkdown extracts per-page Markdown with layout
// classification metadata (tables, columns, OCR needs) from a single
// parse. Font statistics are computed from the full document so header
// detection is consistent across pages. Pass nil for pages to return every
// page in document order; otherwise pages are 0-indexed and results are
// returned in the given order.
func ExtractPagesMarkdown(data []byte, pages []uint32) (*PagesExtractionResult, error) {
	var env pagesExtractionEnvelope
	if err := call(data, pagesParams{Pages: pages}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_pages_markdown(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if env.Result == nil {
		return nil, errors.New("pdfinspector: extract_pages_markdown reported ok with no result")
	}
	return env.Result, nil
}

// ExtractTextWithPositions extracts text with position and style
// information (font, bold/italic/underline/strikeout, bounding box). Pass
// nil for pages to return every page; otherwise pages are **1-indexed**,
// matching the [TextItem.Page] field the results carry (confirmed against
// napi's own tested behavior: `extractTextWithPositions(buf, [1])` returns
// items with `page === 1`).
func ExtractTextWithPositions(data []byte, pages []uint32) ([]TextItem, error) {
	var env textItemsEnvelope
	if err := call(data, pagesParams{Pages: pages}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_text_with_positions(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Items, nil
}

// ExtractStructureElements reads a tagged PDF's structure tree and returns
// one entry per marked-content reference (page, MCID, structure role).
// Returns an empty slice for untagged PDFs. Pass nil for pages to return
// every page; otherwise pages are 1-indexed, matching [TextItem.Page].
func ExtractStructureElements(data []byte, pages []uint32) ([]StructureElement, error) {
	var env structureElementsEnvelope
	if err := call(data, pagesParams{Pages: pages}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_structure_elements(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Elements, nil
}

// ExtractTextInRegions extracts text within bounding-box regions.
//
// For hybrid OCR pipelines: a layout model detects regions in rendered
// page images, and this extracts the PDF text within those regions,
// skipping OCR for text-based pages. Each result's NeedsOCR is set when
// the extracted text is unreliable (empty, GID-encoded fonts, garbage,
// encoding issues).
func ExtractTextInRegions(data []byte, pageRegions []PageRegions) ([]PageRegionTexts, error) {
	var env pageRegionTextsEnvelope
	if err := call(data, pageRegionsParams{PageRegions: pageRegions}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_text_in_regions(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Results, nil
}

// ExtractTablesInRegions extracts markdown tables within bounding-box
// regions. Like [ExtractTextInRegions] but runs table detection on items
// within each region: when a table is detected, Text is a markdown
// pipe-table and NeedsOCR is false; otherwise Text is empty and NeedsOCR is
// true so the caller can fall back to OCR.
func ExtractTablesInRegions(data []byte, pageRegions []PageRegions) ([]PageRegionTexts, error) {
	var env pageRegionTextsEnvelope
	if err := call(data, pageRegionsParams{PageRegions: pageRegions}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_tables_in_regions(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Results, nil
}

// DetectVectorGridInRegion detects a vector ruled-line / rectangle grid
// inside one page region, for callers building their own TSR-hybrid
// pipeline. pageIdx is 0-indexed; regionPdfPtBbox is [x1,y1,x2,y2] in PDF
// points with top-left origin; renderDpi is the DPI of the crop image that
// will consume the returned cell bboxes.
//
// Returns (nil, nil) when the region does not contain a valid vector grid.
func DetectVectorGridInRegion(data []byte, pageIdx uint32, regionPdfPtBbox [4]float32, renderDpi float32) (*VectorGridDetection, error) {
	params := vectorGridParams{
		PageIdx:         pageIdx,
		RegionPdfPtBbox: regionPdfPtBbox,
		RenderDpi:       renderDpi,
	}
	var env vectorGridEnvelope
	if err := call(data, params, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_detect_vector_grid_in_region(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	if !env.Found {
		return nil, nil
	}
	return env.Result, nil
}

// ExtractTablesWithStructure extracts markdown tables using
// externally-supplied structure recovery — typically a table-structure
// recognition model's output run on rendered page crops. For each input,
// this pairs structure tokens with cell bboxes (rowspan/colspan aware),
// converts each cell bbox from crop image-pixels into page PDF points,
// pulls the cell's text from the native PDF, and emits a markdown
// pipe-table. Returns one markdown string per input, in input order.
func ExtractTablesWithStructure(data []byte, inputs []TsrTableInput) ([]string, error) {
	var env markdownStringsEnvelope
	if err := call(data, tsrInputsParams{Inputs: inputs}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_tables_with_structure(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Results, nil
}

// ExtractTablesWithStructureCells is the lower-level sibling of
// [ExtractTablesWithStructure]: instead of rendering markdown, it returns
// the resolved cells so callers can drive their own rendering, debug
// overlays, or per-cell post-processing. Returns one []StructuredCell per
// input, in input order.
func ExtractTablesWithStructureCells(data []byte, inputs []TsrTableInput) ([][]StructuredCell, error) {
	var env structuredCellsEnvelope
	if err := call(data, tsrInputsParams{Inputs: inputs}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_tables_with_structure_cells(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Results, nil
}

// ExtractTablesWithStructureAuto is the auto-fallback variant of
// [ExtractTablesWithStructure]: it runs the TSR-hybrid path, checks the
// resulting cells for known TSR detection pathologies (phantom rows,
// multi-row content merged into a single cell), expands multi-row cells
// in-place when possible, and otherwise falls back to heuristic table
// extraction for inputs where the TSR path looks compromised. On clean
// inputs this returns identical markdown to [ExtractTablesWithStructure];
// on flagged inputs, FallbackReason identifies the recovery path used.
func ExtractTablesWithStructureAuto(data []byte, inputs []TsrTableInput) ([]TableExtractionResult, error) {
	var env tableExtractionEnvelope
	if err := call(data, tsrInputsParams{Inputs: inputs}, &env, func(d *C.uchar, l C.size_t, p *C.char) *C.char {
		return C.pdfinspector_extract_tables_with_structure_auto(d, l, p)
	}); err != nil {
		return nil, err
	}
	if !env.Ok {
		return nil, wrapError(env.Error)
	}
	return env.Results, nil
}

// ---------------------------------------------------------------------------
// Request param shapes (internal — see go/src/params.rs for the Rust side)
// ---------------------------------------------------------------------------

type pagesParams struct {
	// No `omitempty`: the Rust side's field is `Option<Vec<u32>>`, so a Go
	// nil slice marshals to JSON `null` (-> `None`, "every page") while an
	// explicit non-nil empty slice marshals to `[]` (-> `Some(vec![])`,
	// "no pages"). `omitempty` would collapse both to "field omitted",
	// making an intentional empty selection indistinguishable from nil.
	Pages []uint32 `json:"pages"`
}

type pageRegionsParams struct {
	PageRegions []PageRegions `json:"page_regions,omitempty"`
}

type vectorGridParams struct {
	PageIdx         uint32     `json:"page_idx"`
	RegionPdfPtBbox [4]float32 `json:"region_pdf_pt_bbox"`
	RenderDpi       float32    `json:"render_dpi"`
}

type tsrInputsParams struct {
	Inputs []TsrTableInput `json:"inputs,omitempty"`
}

// ---------------------------------------------------------------------------
// cgo call plumbing
// ---------------------------------------------------------------------------

func wrapError(msg *string) error {
	if msg == nil {
		return errors.New("pdfinspector: unknown error")
	}
	return errors.New("pdfinspector: " + *msg)
}

// cBytes copies data into C-owned memory and returns a pointer usable from
// cgo plus its length. Copying (rather than using unsafe.Pointer directly
// into the Go slice's backing array) avoids any question of whether the Go
// runtime could move or collect the slice while the Rust side is running —
// cgo forbids passing Go-managed pointers to C across a call that might
// retain them, and this keeps the rule simple: C never sees Go memory.
func cBytes(data []byte) (unsafe.Pointer, C.size_t) {
	if len(data) == 0 {
		return nil, 0
	}
	return C.CBytes(data), C.size_t(len(data))
}

func freeCBytes(p unsafe.Pointer) {
	if p != nil {
		C.free(p)
	}
}

// callNoParams invokes one of the two-argument (data, len) ABI functions
// and decodes its JSON envelope into out. `invoke` is a closure wrapping
// the specific C.pdfinspector_* call — cgo function references cannot be
// passed around as ordinary Go func values, only called directly, so each
// exported function below supplies its own one-line closure.
func callNoParams(data []byte, out any, invoke func(*C.uchar, C.size_t) *C.char) error {
	cData, cLen := cBytes(data)
	defer freeCBytes(cData)

	raw := invoke((*C.uchar)(cData), cLen)
	defer C.pdfinspector_free_string(raw)

	if err := json.Unmarshal([]byte(C.GoString(raw)), out); err != nil {
		return fmt.Errorf("pdfinspector: decode result: %w", err)
	}
	return nil
}

// call invokes one of the three-argument (data, len, params_json) ABI
// functions and decodes its JSON envelope into out. params is marshaled to
// JSON on the Go side — see go/src/params.rs for what each function
// expects to find there. See callNoParams for why `invoke` is a closure.
func call(data []byte, params any, out any, invoke func(*C.uchar, C.size_t, *C.char) *C.char) error {
	cData, cLen := cBytes(data)
	defer freeCBytes(cData)

	paramsJSON, err := json.Marshal(params)
	if err != nil {
		return fmt.Errorf("pdfinspector: encode params: %w", err)
	}
	cParams := C.CString(string(paramsJSON))
	defer C.free(unsafe.Pointer(cParams))

	raw := invoke((*C.uchar)(cData), cLen, cParams)
	defer C.pdfinspector_free_string(raw)

	if err := json.Unmarshal([]byte(C.GoString(raw)), out); err != nil {
		return fmt.Errorf("pdfinspector: decode result: %w", err)
	}
	return nil
}
