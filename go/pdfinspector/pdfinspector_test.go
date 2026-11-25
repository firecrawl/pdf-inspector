package pdfinspector

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"unicode"
)

func fixture(t *testing.T, name string) []byte {
	t.Helper()
	// Fixtures live at <repo root>/tests/fixtures; this file is at
	// <repo root>/go/pdfinspector.
	path := filepath.Join("..", "..", "tests", "fixtures", name)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read fixture %s: %v", name, err)
	}
	return data
}

func textFixture(t *testing.T) []byte {
	return fixture(t, "thermo-freon12.pdf") // 3-page, TextBased
}

func taggedFixture(t *testing.T) []byte {
	return fixture(t, "firecrawl_docs_tagged.pdf")
}

// --- Classify ---

func TestClassify_TextBasedDocument(t *testing.T) {
	result, err := Classify(textFixture(t))
	if err != nil {
		t.Fatalf("Classify: %v", err)
	}
	if result.PdfType != TextBased {
		t.Errorf("PdfType = %q, want %q", result.PdfType, TextBased)
	}
	if result.PageCount != 3 {
		t.Errorf("PageCount = %d, want 3", result.PageCount)
	}
	if result.Confidence <= 0 {
		t.Errorf("Confidence = %v, want > 0", result.Confidence)
	}
}

func TestClassify_EncryptedDocument_ReturnsError(t *testing.T) {
	data := fixture(t, "encrypted-secret123.pdf")
	if _, err := Classify(data); err == nil {
		t.Fatal("Classify on an encrypted PDF: want error, got nil")
	}
}

func TestClassify_InvalidInput_ReturnsError(t *testing.T) {
	if _, err := Classify([]byte("not a pdf")); err == nil {
		t.Fatal("Classify on non-PDF bytes: want error, got nil")
	}
}

func TestClassify_EmptyInput_ReturnsError(t *testing.T) {
	if _, err := Classify(nil); err == nil {
		t.Fatal("Classify on empty input: want error, got nil")
	}
}

// --- ExtractText ---

func TestExtractText_TextBasedDocument(t *testing.T) {
	text, err := ExtractText(textFixture(t))
	if err != nil {
		t.Fatalf("ExtractText: %v", err)
	}
	if strings.TrimSpace(text) == "" {
		t.Error("ExtractText returned empty text for a text-based PDF")
	}
}

func TestExtractText_InvalidInput_ReturnsError(t *testing.T) {
	if _, err := ExtractText([]byte("not a pdf")); err == nil {
		t.Fatal("ExtractText on non-PDF bytes: want error, got nil")
	}
}

// --- ProcessPdf ---

func TestProcessPdf_FullExtraction(t *testing.T) {
	result, err := ProcessPdf(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ProcessPdf: %v", err)
	}
	if result.PdfType != TextBased {
		t.Errorf("PdfType = %q, want %q", result.PdfType, TextBased)
	}
	if result.PageCount != 3 {
		t.Errorf("PageCount = %d, want 3", result.PageCount)
	}
	if result.Markdown == nil || strings.TrimSpace(*result.Markdown) == "" {
		t.Error("Markdown is nil or empty, want non-empty markdown")
	}
}

func TestProcessPdf_WithPages(t *testing.T) {
	result, err := ProcessPdf(textFixture(t), []uint32{1})
	if err != nil {
		t.Fatalf("ProcessPdf: %v", err)
	}
	if result.Markdown == nil || strings.TrimSpace(*result.Markdown) == "" {
		t.Error("Markdown is nil or empty when restricted to page 1")
	}
}

// TestProcessPdf_PagesAre1Indexed cross-checks against ExtractPagesMarkdown
// (definitively 0-indexed — see TestExtractPagesMarkdown_AllPages) to prove
// ProcessPdf's "pages" parameter is 1-indexed, rather than just checking
// that *some* filtering happened (which self-consistency checks alone
// wouldn't catch an off-by-one on).
func TestProcessPdf_PagesAre1Indexed(t *testing.T) {
	data := textFixture(t) // thermo-freon12.pdf, 3 pages
	perPage, err := ExtractPagesMarkdown(data, nil)
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown: %v", err)
	}
	if len(perPage.Pages) < 3 {
		t.Fatalf("fixture has %d pages, want >= 3 for this test", len(perPage.Pages))
	}
	firstPageSignature := distinctiveSubstring(t, perPage.Pages[0].Markdown)
	thirdPageSignature := distinctiveSubstring(t, perPage.Pages[2].Markdown)

	full, err := ProcessPdf(data, nil)
	if err != nil {
		t.Fatalf("ProcessPdf(nil): %v", err)
	}
	viaPage1, err := ProcessPdf(data, []uint32{1})
	if err != nil {
		t.Fatalf("ProcessPdf(pages=[1]): %v", err)
	}
	viaPage3, err := ProcessPdf(data, []uint32{3})
	if err != nil {
		t.Fatalf("ProcessPdf(pages=[3]): %v", err)
	}

	fullMd, md1, md3 := markdownOrEmpty(full), markdownOrEmpty(viaPage1), markdownOrEmpty(viaPage3)

	// Positive: filtering to page 1/3 actually returns that specific
	// page's content (proves 1-indexing, not just "some" content).
	if !strings.Contains(md1, firstPageSignature) {
		t.Errorf("ProcessPdf(pages=[1]) markdown does not contain page-0's (0-indexed) distinctive text %q — pages may not be 1-indexed", firstPageSignature)
	}
	if !strings.Contains(md3, thirdPageSignature) {
		t.Errorf("ProcessPdf(pages=[3]) markdown does not contain page-2's (0-indexed) distinctive text %q — pages may not be 1-indexed", thirdPageSignature)
	}

	// Structural, fixture-content-agnostic: a single selected page must
	// produce meaningfully less markdown than the whole 3-page document,
	// proving the filter actually restricted output rather than the
	// parameter being silently ignored. Deliberately not "page1 doesn't
	// contain page3's word" — a real word repeating across pages (common
	// in technical documents reusing the same jargon) would make that
	// kind of check fail spuriously despite correct indexing.
	if len(md1) >= len(fullMd) {
		t.Errorf("ProcessPdf(pages=[1]) markdown length %d, want less than the full document's %d — the page filter may not be restricting output", len(md1), len(fullMd))
	}
	if len(md3) >= len(fullMd) {
		t.Errorf("ProcessPdf(pages=[3]) markdown length %d, want less than the full document's %d — the page filter may not be restricting output", len(md3), len(fullMd))
	}
}

// TestProcessPdf_EmptyPagesSliceSelectsNoPages guards against the
// `omitempty` bug class: an explicit non-nil empty []uint32{} must be
// distinguishable on the wire from nil (which selects every page), or the
// two collapse to the same "field omitted" JSON and a caller can never
// request "no pages".
func TestProcessPdf_EmptyPagesSliceSelectsNoPages(t *testing.T) {
	data := textFixture(t)
	all, err := ProcessPdf(data, nil)
	if err != nil {
		t.Fatalf("ProcessPdf(nil): %v", err)
	}
	none, err := ProcessPdf(data, []uint32{})
	if err != nil {
		t.Fatalf("ProcessPdf([]uint32{}): %v", err)
	}

	allLen, noneLen := len(markdownOrEmpty(all)), len(markdownOrEmpty(none))
	if noneLen >= allLen {
		t.Errorf("ProcessPdf with an explicit empty pages slice produced markdown of length %d, want much less than the all-pages length %d — nil vs. empty-slice may have collapsed to the same thing", noneLen, allLen)
	}
}

func markdownOrEmpty(r *PdfResult) string {
	if r.Markdown == nil {
		return ""
	}
	return *r.Markdown
}

// distinctiveSubstring picks the longest all-alphabetic word (>=8 letters)
// on the page to use as a fingerprint for "this content came from this
// specific page" in cross-checks between differently-indexed APIs. A
// single word survives markdown re-rendering differences (heading-tier
// classification depends on font-size statistics computed over whatever
// page subset is in scope, so e.g. "### Foo" vs "**Foo**" for the same
// text is expected and not a bug) in a way a multi-word phrase — which a
// once-off trial of this test caught failing on, deep inside a large
// numeric table subject to continuation-row-merging nuances — does not.
func distinctiveSubstring(t *testing.T, markdown string) string {
	t.Helper()
	// Prefer a long word (less likely to coincidentally appear on every
	// page of a technical document), but fall back to progressively
	// shorter minimums rather than failing outright — an unrelated
	// fixture or extraction change shouldn't be able to dead-end this
	// helper just because the longest available word happens to be short.
	for _, minLen := range []int{8, 6, 4} {
		if word := longestAlphabeticWord(markdown, minLen); word != "" {
			return word
		}
	}
	t.Fatalf("could not find any alphabetic word (>=4 letters) in page markdown: %q", markdown)
	return ""
}

func longestAlphabeticWord(markdown string, minLen int) string {
	best := ""
	for _, field := range strings.Fields(markdown) {
		word := strings.Trim(field, "*#|_-.,:;()[]{}")
		if len(word) < minLen {
			continue
		}
		alphabetic := true
		for _, r := range word {
			if !unicode.IsLetter(r) {
				alphabetic = false
				break
			}
		}
		if alphabetic && len(word) > len(best) {
			best = word
		}
	}
	return best
}

func TestProcessPdf_InvalidInput_ReturnsError(t *testing.T) {
	if _, err := ProcessPdf([]byte("not a pdf"), nil); err == nil {
		t.Fatal("ProcessPdf on non-PDF bytes: want error, got nil")
	}
}

// --- DetectPdf ---

func TestDetectPdf_NoMarkdown(t *testing.T) {
	result, err := DetectPdf(textFixture(t))
	if err != nil {
		t.Fatalf("DetectPdf: %v", err)
	}
	if result.PdfType != TextBased {
		t.Errorf("PdfType = %q, want %q", result.PdfType, TextBased)
	}
	if result.PageCount != 3 {
		t.Errorf("PageCount = %d, want 3", result.PageCount)
	}
	if result.Markdown != nil {
		t.Errorf("Markdown = %v, want nil for detect-only", *result.Markdown)
	}
}

// --- ExtractPagesMarkdown ---

func TestExtractPagesMarkdown_AllPages(t *testing.T) {
	result, err := ExtractPagesMarkdown(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown: %v", err)
	}
	if len(result.Pages) != 3 {
		t.Fatalf("len(Pages) = %d, want 3", len(result.Pages))
	}
	for i, page := range result.Pages {
		if page.Page != uint32(i) {
			t.Errorf("Pages[%d].Page = %d, want %d", i, page.Page, i)
		}
	}
}

func TestExtractPagesMarkdown_PreservesCallerOrder(t *testing.T) {
	result, err := ExtractPagesMarkdown(textFixture(t), []uint32{2, 0})
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown: %v", err)
	}
	if len(result.Pages) != 2 {
		t.Fatalf("len(Pages) = %d, want 2", len(result.Pages))
	}
	if result.Pages[0].Page != 2 || result.Pages[1].Page != 0 {
		t.Errorf("Pages = [%d, %d], want [2, 0]", result.Pages[0].Page, result.Pages[1].Page)
	}
}

// --- ExtractTextWithPositions ---

func TestExtractTextWithPositions_AllPages(t *testing.T) {
	items, err := ExtractTextWithPositions(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractTextWithPositions: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("ExtractTextWithPositions returned no items")
	}
	if items[0].ItemType == "" {
		t.Error("first item has empty ItemType")
	}
}

// TestExtractTextWithPositions_PagesAre1Indexed proves the "pages" filter
// shares TextItem.Page's indexing space, rather than just checking the
// self-consistent-but-not-necessarily-correct fact that returned items
// happen to report the same page number passed in (which a filter that
// silently no-ops, or one with a consistent off-by-one, could both satisfy
// too — see TestExtractTextWithPositions_PageFilter for that weaker check).
func TestExtractTextWithPositions_PagesAre1Indexed(t *testing.T) {
	data := textFixture(t)
	all, err := ExtractTextWithPositions(data, nil)
	if err != nil {
		t.Fatalf("ExtractTextWithPositions(nil): %v", err)
	}
	var wantPage1 int
	for _, item := range all {
		if item.Page == 1 {
			wantPage1++
		}
	}
	if wantPage1 == 0 {
		t.Fatal("no unfiltered items report Page == 1; fixture assumption invalid")
	}

	filtered, err := ExtractTextWithPositions(data, []uint32{1})
	if err != nil {
		t.Fatalf("ExtractTextWithPositions(pages=[1]): %v", err)
	}
	if len(filtered) != wantPage1 {
		t.Errorf("ExtractTextWithPositions(pages=[1]) returned %d items, want %d (the count of unfiltered items with Page == 1) — the filter's indexing may not match TextItem.Page's", len(filtered), wantPage1)
	}
}

func TestExtractTextWithPositions_PageFilter(t *testing.T) {
	items, err := ExtractTextWithPositions(textFixture(t), []uint32{1})
	if err != nil {
		t.Fatalf("ExtractTextWithPositions: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("ExtractTextWithPositions returned no items for page 1")
	}
	for _, item := range items {
		if item.Page != 1 {
			t.Errorf("item.Page = %d, want 1", item.Page)
		}
	}
}

func TestExtractTextWithPositions_Mcid(t *testing.T) {
	items, err := ExtractTextWithPositions(taggedFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractTextWithPositions: %v", err)
	}
	found := false
	for _, item := range items {
		if item.Mcid != nil {
			found = true
			break
		}
	}
	if !found {
		t.Error("tagged PDF text items should carry Marked Content IDs")
	}
}

// --- ExtractStructureElements ---

func TestExtractStructureElements_TaggedDocument(t *testing.T) {
	elements, err := ExtractStructureElements(taggedFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractStructureElements: %v", err)
	}
	if len(elements) == 0 {
		t.Fatal("ExtractStructureElements returned no elements for a tagged PDF")
	}
	sawH1 := false
	for _, e := range elements {
		if e.Role == "" {
			t.Error("element has empty Role")
		}
		if e.Role == "H1" {
			sawH1 = true
		}
	}
	if !sawH1 {
		t.Error("tagged fixture should surface H1 heading roles")
	}
}

func TestExtractStructureElements_PageFilterIs1Indexed(t *testing.T) {
	elements, err := ExtractStructureElements(taggedFixture(t), []uint32{1})
	if err != nil {
		t.Fatalf("ExtractStructureElements: %v", err)
	}
	if len(elements) == 0 {
		t.Fatal("ExtractStructureElements returned no elements for page 1")
	}
	for _, e := range elements {
		if e.Page != 1 {
			t.Errorf("element.Page = %d, want 1", e.Page)
		}
	}
}

func TestExtractStructureElements_UntaggedDocument_ReturnsEmpty(t *testing.T) {
	elements, err := ExtractStructureElements(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractStructureElements: %v", err)
	}
	if len(elements) != 0 {
		t.Errorf("len(elements) = %d, want 0 for an untagged PDF", len(elements))
	}
}

// --- ExtractTextInRegions / ExtractTablesInRegions ---

func TestExtractTextInRegions(t *testing.T) {
	results, err := ExtractTextInRegions(textFixture(t), []PageRegions{
		{Page: 0, Regions: [][4]float32{{0, 0, 600, 100}}},
	})
	if err != nil {
		t.Fatalf("ExtractTextInRegions: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
	if results[0].Page != 0 {
		t.Errorf("results[0].Page = %d, want 0", results[0].Page)
	}
	if len(results[0].Regions) != 1 {
		t.Fatalf("len(results[0].Regions) = %d, want 1", len(results[0].Regions))
	}
}

func TestExtractTablesInRegions(t *testing.T) {
	results, err := ExtractTablesInRegions(textFixture(t), []PageRegions{
		{Page: 0, Regions: [][4]float32{{0, 0, 600, 800}}},
	})
	if err != nil {
		t.Fatalf("ExtractTablesInRegions: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
	// No assertion on NeedsOCR/table detection here: the fixture may or may
	// not contain a detectable table. The call succeeding with the right
	// shape is the contract under test.
	if len(results[0].Regions) != 1 {
		t.Fatalf("len(results[0].Regions) = %d, want 1", len(results[0].Regions))
	}
}

// --- DetectVectorGridInRegion ---

func TestDetectVectorGridInRegion_NoGridReturnsNil(t *testing.T) {
	// A region with no ruled lines/rects should report "not found" rather
	// than erroring.
	grid, err := DetectVectorGridInRegion(textFixture(t), 0, [4]float32{0, 0, 10, 10}, 72)
	if err != nil {
		t.Fatalf("DetectVectorGridInRegion: %v", err)
	}
	if grid != nil {
		t.Errorf("grid = %+v, want nil for an empty region", grid)
	}
}

// --- ExtractTablesWithStructure family ---

func TestExtractTablesWithStructure_EmptyInputs(t *testing.T) {
	results, err := ExtractTablesWithStructure(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ExtractTablesWithStructure: %v", err)
	}
	if len(results) != 0 {
		t.Errorf("len(results) = %d, want 0 for no inputs", len(results))
	}
}

func TestExtractTablesWithStructure_SimpleGrid(t *testing.T) {
	input := TsrTableInput{
		Page:            0,
		CropPdfPtBbox:   [4]float32{0, 0, 200, 100},
		RenderDpi:       72,
		StructureTokens: []string{"<table>", "<tr>", "<td></td>", "<td></td>", "</tr>", "</table>"},
		CellBboxes:      [][]float32{{0, 0, 100, 50}, {100, 0, 200, 50}},
	}
	results, err := ExtractTablesWithStructure(textFixture(t), []TsrTableInput{input})
	if err != nil {
		t.Fatalf("ExtractTablesWithStructure: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
}

func TestExtractTablesWithStructureCells_SimpleGrid(t *testing.T) {
	input := TsrTableInput{
		Page:            0,
		CropPdfPtBbox:   [4]float32{0, 0, 200, 100},
		RenderDpi:       72,
		StructureTokens: []string{"<table>", "<tr>", "<td></td>", "<td></td>", "</tr>", "</table>"},
		CellBboxes:      [][]float32{{0, 0, 100, 50}, {100, 0, 200, 50}},
	}
	results, err := ExtractTablesWithStructureCells(textFixture(t), []TsrTableInput{input})
	if err != nil {
		t.Fatalf("ExtractTablesWithStructureCells: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
	if len(results[0]) != 2 {
		t.Fatalf("len(results[0]) = %d, want 2 cells", len(results[0]))
	}
}

func TestExtractTablesWithStructureAuto_SimpleGrid(t *testing.T) {
	input := TsrTableInput{
		Page:            0,
		CropPdfPtBbox:   [4]float32{0, 0, 200, 100},
		RenderDpi:       72,
		StructureTokens: []string{"<table>", "<tr>", "<td></td>", "<td></td>", "</tr>", "</table>"},
		CellBboxes:      [][]float32{{0, 0, 100, 50}, {100, 0, 200, 50}},
	}
	results, err := ExtractTablesWithStructureAuto(textFixture(t), []TsrTableInput{input})
	if err != nil {
		t.Fatalf("ExtractTablesWithStructureAuto: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
}

// --- ProcessPdfWithOcr ---
//
// Mode "off" exercises the complete result/provenance contract without
// loading external PDFium, ONNX Runtime, or model artifacts — mirroring
// napi/test.mjs's approach to testing this surface in CI.

func TestProcessPdfWithOcr_ModeOff(t *testing.T) {
	result, err := ProcessPdfWithOcr(textFixture(t), &OcrOptions{Mode: OcrOff})
	if err != nil {
		t.Fatalf("ProcessPdfWithOcr: %v", err)
	}
	if result.PageCount != 3 {
		t.Errorf("PageCount = %d, want 3", result.PageCount)
	}
	if len(result.Pages) != 3 {
		t.Fatalf("len(Pages) = %d, want 3", len(result.Pages))
	}
	if len(result.PagesRoutedToOCR) != 0 {
		t.Errorf("PagesRoutedToOCR = %v, want empty with mode off", result.PagesRoutedToOCR)
	}
	for _, page := range result.Pages {
		if page.Provenance.Source != SourceNative {
			t.Errorf("page %d Source = %q, want %q with mode off", page.PageNumber, page.Provenance.Source, SourceNative)
		}
		if page.Provenance.OcrModel != nil {
			t.Errorf("page %d OcrModel = %+v, want nil with mode off", page.PageNumber, page.Provenance.OcrModel)
		}
	}
	if strings.TrimSpace(result.Markdown) == "" {
		t.Error("Markdown is empty")
	}
}

func TestProcessPdfWithOcr_DefaultOptionsIsAuto(t *testing.T) {
	// Auto must preserve the lightweight native path for clean text PDFs:
	// no page should be routed to OCR and no render/OCR time should be spent.
	result, err := ProcessPdfWithOcr(textFixture(t), nil)
	if err != nil {
		t.Fatalf("ProcessPdfWithOcr: %v", err)
	}
	if len(result.PagesRoutedToOCR) != 0 {
		t.Errorf("PagesRoutedToOCR = %v, want empty for a clean text PDF", result.PagesRoutedToOCR)
	}
	if result.RenderTimeMs != 0 || result.OcrTimeMs != 0 {
		t.Errorf("RenderTimeMs=%d OcrTimeMs=%d, want both 0 when nothing was routed to OCR", result.RenderTimeMs, result.OcrTimeMs)
	}
}

func TestProcessPdfWithOcr_PageNumbersIs1Indexed(t *testing.T) {
	result, err := ProcessPdfWithOcr(textFixture(t), &OcrOptions{
		Mode:        OcrOff,
		PageNumbers: []uint32{2},
	})
	if err != nil {
		t.Fatalf("ProcessPdfWithOcr: %v", err)
	}
	if len(result.Pages) != 1 || result.Pages[0].PageNumber != 2 {
		t.Errorf("Pages = %+v, want exactly page 2", result.Pages)
	}
}

func TestProcessPdfWithOcr_InvalidPageNumber_ReturnsError(t *testing.T) {
	_, err := ProcessPdfWithOcr(textFixture(t), &OcrOptions{
		Mode:        OcrOff,
		PageNumbers: []uint32{0}, // 1-indexed: 0 is out of range
	})
	if err == nil {
		t.Fatal("ProcessPdfWithOcr with page 0: want error, got nil")
	}
}

func TestProcessPdfWithOcr_InvalidMode_ReturnsError(t *testing.T) {
	_, err := ProcessPdfWithOcr(textFixture(t), &OcrOptions{Mode: "bogus"})
	if err == nil {
		t.Fatal("ProcessPdfWithOcr with an invalid mode: want error, got nil")
	}
}

// --- Password-protected PDFs ---
//
// ProcessPdfWithOcr is currently the only function in this package whose
// options accept a password (matching napi/Python: the plain ProcessPdf
// entry point does not expose one either). Mode "off" keeps this a native
// decrypt-and-extract test with no OCR runtime dependency.

func TestProcessPdfWithOcr_EncryptedDocument_WrongPassword_ReturnsError(t *testing.T) {
	data := fixture(t, "encrypted-secret123.pdf")
	_, err := ProcessPdfWithOcr(data, &OcrOptions{Mode: OcrOff})
	if err == nil {
		t.Fatal("ProcessPdfWithOcr on an encrypted PDF with no password: want error, got nil")
	}
}

func TestProcessPdfWithOcr_EncryptedDocument_CorrectPassword_Decrypts(t *testing.T) {
	data := fixture(t, "encrypted-secret123.pdf")
	result, err := ProcessPdfWithOcr(data, &OcrOptions{Mode: OcrOff, Password: "secret123"})
	if err != nil {
		t.Fatalf("ProcessPdfWithOcr with correct password: %v", err)
	}
	if strings.TrimSpace(result.Markdown) == "" {
		t.Error("Markdown is empty after decrypting with the correct password")
	}
}

// TestProcessPdfWithOcr_AutoMode_ScannedFixture exercises the real OCR
// runtime path (PDFium rendering + ONNX Runtime inference), unlike every
// other ProcessPdfWithOcr test in this file, which uses Mode: OcrOff (or
// Auto on a clean text PDF that routes zero pages) specifically to avoid
// needing PDFium/ONNX Runtime present. Skips itself unless both are
// provisioned (PDFIUM_LIB_PATH / ORT_DYLIB_PATH set) — a plain local
// `go test` will skip it, but `.github/workflows/ci.yml`'s `go` job
// provisions both and runs it for real on both its Ubuntu and macOS
// runners (with a dedicated cache-warming step first, since nothing else
// pre-warms PDF_INSPECTOR_MODEL_CACHE in that job the way ocr-runtime's
// CLI-based step does). That makes Go's real OCR coverage broader than
// the rest of the project's: ocr-runtime is Ubuntu-only and is still the
// only place the Rust CLI's, Node's, and Python's own OCR runtime paths
// get tested at all.
func TestProcessPdfWithOcr_AutoMode_ScannedFixture(t *testing.T) {
	if os.Getenv("PDFIUM_LIB_PATH") == "" || os.Getenv("ORT_DYLIB_PATH") == "" {
		t.Skip("PDFIUM_LIB_PATH/ORT_DYLIB_PATH not set; see ci.yml's ocr-runtime job")
	}

	// Offline is deliberately not set: unlike ocr-runtime's Node/Python
	// steps, nothing pre-warms PDF_INSPECTOR_MODEL_CACHE for the `go` job
	// specifically, so this needs to tolerate a cold cache by downloading
	// the model over the network on first use. A warm cache (e.g. when
	// this same test runs inside ocr-runtime, which does pre-warm it via
	// the CLI) still short-circuits the download, so this works either way.
	data := fixture(t, "scan_with_native_header_text.pdf")
	result, err := ProcessPdfWithOcr(data, &OcrOptions{Mode: OcrAuto})
	if err != nil {
		t.Fatalf("ProcessPdfWithOcr: %v", err)
	}
	if got := result.PagesRoutedToOCR; len(got) != 1 || got[0] != 1 {
		t.Errorf("PagesRoutedToOCR = %v, want [1]", got)
	}
	if len(result.PagesRecommendingHosted) != 0 {
		t.Errorf("PagesRecommendingHosted = %v, want empty", result.PagesRecommendingHosted)
	}
	if len(result.Pages) == 0 {
		t.Fatal("Pages is empty")
	}
	source := result.Pages[0].Provenance.Source
	if source != SourceOCR && source != SourceFused {
		t.Errorf("Pages[0].Provenance.Source = %q, want %q or %q", source, SourceOCR, SourceFused)
	}
	if strings.TrimSpace(result.Markdown) == "" {
		t.Error("Markdown is empty")
	}
}

// --- Concurrency ---
//
// cgo calls release the calling goroutine's OS thread for the duration of
// the call, so concurrent callers can genuinely run the underlying Rust
// library in parallel. This guards against shared mutable state (e.g. a
// global cache) in the Rust side introducing data races.

func TestConcurrentCalls(t *testing.T) {
	data := textFixture(t)
	tagged := taggedFixture(t)

	const goroutines = 8
	var wg sync.WaitGroup
	errs := make(chan error, goroutines*3)

	for i := 0; i < goroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if _, err := Classify(data); err != nil {
				errs <- fmt.Errorf("Classify: %w", err)
			}
			if _, err := ProcessPdf(data, nil); err != nil {
				errs <- fmt.Errorf("ProcessPdf: %w", err)
			}
			if _, err := ExtractStructureElements(tagged, nil); err != nil {
				errs <- fmt.Errorf("ExtractStructureElements: %w", err)
			}
		}()
	}
	wg.Wait()
	close(errs)

	for err := range errs {
		t.Error(err)
	}
}

// --- Corpus smoke test ---
//
// Runs Classify and ExtractText against every fixture PDF in the repo
// (encrypted, malformed-edge-case, scanned, tagged, table-heavy, etc.) to
// catch panics or crashes the per-function unit tests above — which each
// exercise only one or two curated fixtures — would not. A well-formed
// error is an acceptable outcome for any given fixture; a panic or hang is
// not.

func TestCorpus_ClassifyAndExtractTextDoNotPanic(t *testing.T) {
	dir := filepath.Join("..", "..", "tests", "fixtures")
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("read fixtures dir: %v", err)
	}

	tested := 0
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".pdf") {
			continue
		}
		name := entry.Name()
		tested++
		t.Run(name, func(t *testing.T) {
			data, err := os.ReadFile(filepath.Join(dir, name))
			if err != nil {
				t.Fatalf("read %s: %v", name, err)
			}
			// Errors are fine (encrypted/malformed fixtures are expected to
			// fail); a panic crossing the cgo boundary is not, and would
			// abort the whole test binary rather than fail gracefully here.
			_, _ = Classify(data)
			_, _ = ExtractText(data)
		})
	}
	if tested == 0 {
		t.Fatal("no .pdf fixtures found — fixture directory path may be wrong")
	}
}
