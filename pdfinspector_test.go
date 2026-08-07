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
	t.Logf("Extracted text snippet: %s", text[:min(100, len(text))])
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

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
