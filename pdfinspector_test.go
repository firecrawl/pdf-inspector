package pdfinspector_test

import (
	"os"
	"strings"
	"testing"

	pdfinspector "github.com/firecrawl/pdf-inspector"
)

func TestVersion(t *testing.T) {
	ver, err := pdfinspector.Version()
	if err != nil {
		t.Fatalf("Version failed: %v", err)
	}
	if ver == "" {
		t.Fatal("Version returned empty string")
	}
	t.Logf("pdf-inspector WASM version: %s", ver)
}

func TestProcessPdf(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	result, err := pdfinspector.ProcessPdf(pdfData, nil)
	if err != nil {
		t.Fatalf("ProcessPdf failed: %v", err)
	}

	if result.PdfType != "TextBased" {
		t.Errorf("Expected PdfType TextBased, got %s", result.PdfType)
	}
	if result.PageCount == 0 {
		t.Errorf("Expected PageCount > 0")
	}
	if result.Markdown == nil || len(*result.Markdown) == 0 {
		t.Errorf("Expected non-empty Markdown output")
	}

	t.Logf("PdfType: %s, PageCount: %d, Markdown len: %d", result.PdfType, result.PageCount, len(*result.Markdown))
}

func TestDetectPdf(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	result, err := pdfinspector.DetectPdf(pdfData, "")
	if err != nil {
		t.Fatalf("DetectPdf failed: %v", err)
	}

	if result.PdfType != "TextBased" {
		t.Errorf("Expected PdfType TextBased, got %s", result.PdfType)
	}
	if result.Markdown != nil {
		t.Errorf("Expected nil Markdown in detect-only mode")
	}
}

func TestClassifyPdf(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	classification, err := pdfinspector.ClassifyPdf(pdfData)
	if err != nil {
		t.Fatalf("ClassifyPdf failed: %v", err)
	}

	if classification.PdfType != "TextBased" {
		t.Errorf("Expected PdfType TextBased, got %s", classification.PdfType)
	}
	if classification.PageCount == 0 {
		t.Errorf("Expected PageCount > 0")
	}
}

func TestExtractText(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	text, err := pdfinspector.ExtractText(pdfData)
	if err != nil {
		t.Fatalf("ExtractText failed: %v", err)
	}

	if len(strings.TrimSpace(text)) == 0 {
		t.Fatalf("ExtractText returned empty string")
	}
	runes := []rune(text)
	if len(runes) > 100 {
		runes = runes[:100]
	}
	t.Logf("Extracted text snippet: %s", string(runes))
}

func TestEncryptedPdfWithPassword(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/encrypted-secret123.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	// Should fail without password
	_, err = pdfinspector.ProcessPdf(pdfData, nil)
	if err == nil {
		t.Fatal("Expected error for encrypted PDF without password, but got none")
	}

	// Should succeed with password
	opts := &pdfinspector.ProcessOptions{
		Password: "secret123",
	}
	res, err := pdfinspector.ProcessPdf(pdfData, opts)
	if err != nil {
		t.Fatalf("ProcessPdf with password failed: %v", err)
	}
	if res.Markdown == nil || len(*res.Markdown) == 0 {
		t.Fatal("Expected markdown from encrypted PDF with password")
	}
}

func TestExtractPagesMarkdown(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	res, err := pdfinspector.ExtractPagesMarkdown(pdfData, nil)
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown failed: %v", err)
	}
	if len(res.Pages) == 0 {
		t.Fatal("Expected pages in result")
	}
	t.Logf("Extracted %d pages markdown", len(res.Pages))
}

func TestValidationAndMetrics(t *testing.T) {
	pdfData, err := os.ReadFile("tests/fixtures/thermo-freon12.pdf")
	if err != nil {
		t.Fatalf("Failed to read fixture: %v", err)
	}

	// 1. ProcessingTimeMs check (logged rather than strictly > 0 to avoid test flakiness on fast sub-ms runs)
	res, err := pdfinspector.ProcessPdf(pdfData, nil)
	if err != nil {
		t.Fatalf("ProcessPdf failed: %v", err)
	}
	t.Logf("ProcessPdf ProcessingTimeMs: %d", res.ProcessingTimeMs)

	detRes, err := pdfinspector.DetectPdf(pdfData, "")
	if err != nil {
		t.Fatalf("DetectPdf failed: %v", err)
	}
	t.Logf("DetectPdf ProcessingTimeMs: %d", detRes.ProcessingTimeMs)

	// 2. Zero-page validation in both ProcessPdf and ExtractPagesMarkdown
	_, err = pdfinspector.ProcessPdf(pdfData, &pdfinspector.ProcessOptions{Pages: []uint32{0}})
	if err == nil || !strings.Contains(err.Error(), "1-indexed") {
		t.Errorf("Expected error for page index 0 in ProcessPdf, got: %v", err)
	}

	_, err = pdfinspector.ExtractPagesMarkdown(pdfData, []uint32{0})
	if err == nil || !strings.Contains(err.Error(), "1-indexed") {
		t.Errorf("Expected error for page index 0 in ExtractPagesMarkdown, got: %v", err)
	}

	// 3. 1-indexed page selection consistency (page 1 = first page)
	page1Res, err := pdfinspector.ExtractPagesMarkdown(pdfData, []uint32{1})
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown for page 1 failed: %v", err)
	}
	if len(page1Res.Pages) != 1 || page1Res.Pages[0].Page != 1 {
		t.Errorf("Expected 1-indexed page 1 result, got: %+v", page1Res.Pages)
	}

	// 4. Invalid profile validation
	_, err = pdfinspector.ProcessPdf(pdfData, &pdfinspector.ProcessOptions{Profile: "invalid_profile"})
	if err == nil || !strings.Contains(err.Error(), "Invalid markdown profile") {
		t.Errorf("Expected error for invalid profile, got: %v", err)
	}

	// 5. Empty slice/nil page selection consistency across APIs
	pagesRes, err := pdfinspector.ExtractPagesMarkdown(pdfData, []uint32{})
	if err != nil {
		t.Fatalf("ExtractPagesMarkdown with empty slice failed: %v", err)
	}
	if len(pagesRes.Pages) == 0 {
		t.Errorf("Expected empty pages slice to process all pages, got 0 pages")
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
