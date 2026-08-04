package pdfinspector

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
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

func TestClassify_TextBasedDocument(t *testing.T) {
	data := fixture(t, "firecrawl_docs_tagged.pdf")

	result, err := Classify(data)
	if err != nil {
		t.Fatalf("Classify: %v", err)
	}
	if result.PdfType != TextBased {
		t.Errorf("PdfType = %q, want %q", result.PdfType, TextBased)
	}
	if result.PageCount == 0 {
		t.Error("PageCount = 0, want > 0")
	}
	if result.Confidence <= 0 {
		t.Errorf("Confidence = %v, want > 0", result.Confidence)
	}
}

func TestExtractText_TextBasedDocument(t *testing.T) {
	data := fixture(t, "firecrawl_docs_tagged.pdf")

	text, err := ExtractText(data)
	if err != nil {
		t.Fatalf("ExtractText: %v", err)
	}
	if strings.TrimSpace(text) == "" {
		t.Error("ExtractText returned empty text for a text-based PDF")
	}
}

func TestClassify_EncryptedDocument_ReturnsError(t *testing.T) {
	data := fixture(t, "encrypted-secret123.pdf")

	_, err := Classify(data)
	if err == nil {
		t.Fatal("Classify on an encrypted PDF: want error, got nil")
	}
}

func TestClassify_InvalidInput_ReturnsError(t *testing.T) {
	_, err := Classify([]byte("not a pdf"))
	if err == nil {
		t.Fatal("Classify on non-PDF bytes: want error, got nil")
	}
}

func TestClassify_EmptyInput_ReturnsError(t *testing.T) {
	_, err := Classify(nil)
	if err == nil {
		t.Fatal("Classify on empty input: want error, got nil")
	}
}

func TestExtractText_InvalidInput_ReturnsError(t *testing.T) {
	_, err := ExtractText([]byte("not a pdf"))
	if err == nil {
		t.Fatal("ExtractText on non-PDF bytes: want error, got nil")
	}
}
