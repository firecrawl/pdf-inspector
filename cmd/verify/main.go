package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	pdfinspector "github.com/firecrawl/pdf-inspector"
)

func main() {
	dir := filepath.Join(os.Getenv("USERPROFILE"), "Documents", "HardikDocuments")
	files, err := os.ReadDir(dir)
	if err != nil {
		fmt.Printf("Error reading directory %s: %v\n", dir, err)
		os.Exit(1)
	}

	fmt.Println("================================================================================")
	fmt.Println("   PDF INSPECTOR REAL-WORLD VERIFICATION SUITE")
	fmt.Println("================================================================================")
	
	ver, err := pdfinspector.Version()
	if err != nil {
		fmt.Printf("Failed to get version: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("pdf-inspector WASM module version: %s\n\n", ver)

	count := 0
	for _, entry := range files {
		if entry.IsDir() || !strings.HasSuffix(strings.ToLower(entry.Name()), ".pdf") {
			continue
		}
		count++
		pdfPath := filepath.Join(dir, entry.Name())
		fmt.Printf("[%d] Processing file: %s ...\n", count, entry.Name())

		pdfBytes, err := os.ReadFile(pdfPath)
		if err != nil {
			fmt.Printf("    FAILED TO READ: %v\n", err)
			continue
		}

		// 1. DetectPdf
		startDetect := time.Now()
		detRes, err := pdfinspector.DetectPdf(pdfBytes, "")
		elapsedDetect := time.Since(startDetect)
		if err != nil {
			fmt.Printf("    - DetectPdf:   ERROR: %v (%v)\n", err, elapsedDetect)
		} else {
			fmt.Printf("    - DetectPdf:   Type=%s, Pages=%d, Confidence=%.2f (%v)\n", detRes.PdfType, detRes.PageCount, detRes.Confidence, elapsedDetect)
		}

		// 2. ClassifyPdf
		startClass := time.Now()
		classRes, err := pdfinspector.ClassifyPdf(pdfBytes)
		elapsedClass := time.Since(startClass)
		if err != nil {
			fmt.Printf("    - ClassifyPdf: ERROR: %v (%v)\n", err, elapsedClass)
		} else {
			fmt.Printf("    - ClassifyPdf: Type=%s, Pages=%d (%v)\n", classRes.PdfType, classRes.PageCount, elapsedClass)
		}

		// 3. ProcessPdf
		start := time.Now()
		res, err := pdfinspector.ProcessPdf(pdfBytes, nil)
		elapsed := time.Since(start)

		if err != nil {
			fmt.Printf("    - ProcessPdf:  ERROR: %v (%v)\n", err, elapsed)
		} else {
			fmt.Printf("    - ProcessPdf:  SUCCESS (%v)\n", elapsed)
			fmt.Printf("        Type:       %s\n", res.PdfType)
			fmt.Printf("        PageCount:  %d\n", res.PageCount)
			if res.Title != nil {
				fmt.Printf("        Title:      %s\n", *res.Title)
			}
			if res.Markdown != nil && len(*res.Markdown) > 0 {
				snippet := strings.ReplaceAll(*res.Markdown, "\n", " ")
				if len(snippet) > 120 {
					snippet = snippet[:120] + "..."
				}
				fmt.Printf("        Markdown:   %s\n", snippet)
			}
		}
		fmt.Println("--------------------------------------------------------------------------------")
	}

	fmt.Printf("\nDone! Verified %d real PDF files.\n", count)
}
