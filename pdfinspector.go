package pdfinspector

import (
	"context"
	crypto_rand "crypto/rand"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"sync"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/imports/wasi_snapshot_preview1"
)

//go:generate cargo build --target wasm32-wasip1 --release

//go:embed target/wasm32-wasip1/release/pdf_inspector.wasm
var wasmBytes []byte

// ProcessOptions represents parameters for PDF processing.
type ProcessOptions struct {
	Pages              []uint32 `json:"pages,omitempty"`
	Password           string   `json:"password,omitempty"`
	Profile            string   `json:"profile,omitempty"` // "fidelity" | "compact"
	IncludePageMarkers bool     `json:"includePageMarkers,omitempty"`
	IncludeImages      bool     `json:"includeImages,omitempty"`
}

// PageOcrReasons describes reasons a page needs OCR.
type PageOcrReasons struct {
	Page    uint32   `json:"page"`
	Reasons []string `json:"reasons"`
}

// LayoutComplexity describes structural layout flags of the PDF.
type LayoutComplexity struct {
	IsComplex        bool     `json:"isComplex"`
	PagesWithTables  []uint32 `json:"pagesWithTables"`
	PagesWithColumns []uint32 `json:"pagesWithColumns"`
}

// PdfProcessResult is the high-level result of PDF inspection and extraction.
type PdfProcessResult struct {
	PdfType           string           `json:"pdfType"`
	Markdown          *string          `json:"markdown,omitempty"`
	PageCount         uint32           `json:"pageCount"`
	ProcessingTimeMs  uint64           `json:"processingTimeMs"`
	PagesNeedingOcr   []uint32         `json:"pagesNeedingOcr"`
	OcrReasonsByPage  []PageOcrReasons `json:"ocrReasonsByPage"`
	Title             *string          `json:"title,omitempty"`
	Confidence        float32          `json:"confidence"`
	Layout            LayoutComplexity `json:"layout"`
	HasEncodingIssues bool             `json:"hasEncodingIssues"`
	Error             string           `json:"error,omitempty"`
}

// PdfClassification is a fast metadata-only classification result.
type PdfClassification struct {
	PdfType         string   `json:"pdfType"`
	PageCount       uint32   `json:"pageCount"`
	PagesNeedingOcr []uint32 `json:"pagesNeedingOcr"`
	Confidence      float32  `json:"confidence"`
	Error           string   `json:"error,omitempty"`
}

// PageMarkdown holds extracted markdown for a single page.
type PageMarkdown struct {
	Page      uint32  `json:"page"`
	Markdown  string  `json:"markdown"`
	NeedsOcr  bool    `json:"needsOcr"`
	OcrReason *string `json:"ocrReason,omitempty"`
}

// PagesExtractionResult holds per-page markdown and document metadata.
type PagesExtractionResult struct {
	Pages            []PageMarkdown   `json:"pages"`
	PagesWithTables  []uint32         `json:"pagesWithTables"`
	PagesWithColumns []uint32         `json:"pagesWithColumns"`
	PagesNeedingOcr  []uint32         `json:"pagesNeedingOcr"`
	OcrReasonsByPage []PageOcrReasons `json:"ocrReasonsByPage"`
	IsComplex        bool             `json:"isComplex"`
	Error            string           `json:"error,omitempty"`
}

var (
	compiledModule wazero.CompiledModule
	wazeroRuntime  wazero.Runtime
	runtimeMu      sync.RWMutex
)

func initRuntime(ctx context.Context) (wazero.Runtime, wazero.CompiledModule, error) {
	runtimeMu.RLock()
	if wazeroRuntime != nil && compiledModule != nil {
		r, c := wazeroRuntime, compiledModule
		runtimeMu.RUnlock()
		return r, c, nil
	}
	runtimeMu.RUnlock()

	runtimeMu.Lock()
	defer runtimeMu.Unlock()

	if wazeroRuntime != nil && compiledModule != nil {
		return wazeroRuntime, compiledModule, nil
	}

	initCtx := context.Background()
	r := wazero.NewRuntimeWithConfig(initCtx, wazero.NewRuntimeConfigInterpreter())
	if _, err := wasi_snapshot_preview1.Instantiate(initCtx, r); err != nil {
		r.Close(initCtx)
		return nil, nil, fmt.Errorf("failed to instantiate WASI snapshot preview1: %w", err)
	}

	compiled, err := r.CompileModule(initCtx, wasmBytes)
	if err != nil {
		r.Close(initCtx)
		return nil, nil, fmt.Errorf("failed to compile pdf-inspector WASM module: %w", err)
	}

	wazeroRuntime = r
	compiledModule = compiled
	return wazeroRuntime, compiledModule, nil
}

func newModuleConfig() wazero.ModuleConfig {
	return wazero.NewModuleConfig().
		WithStdout(io.Discard).
		WithStderr(io.Discard).
		WithSysNanosleep().
		WithSysNanotime().
		WithSysWalltime().
		WithRandSource(crypto_rand.Reader)
}

func invokeWasm(ctx context.Context, fnName string, pdfBytes []byte, extraBytes []byte) ([]byte, error) {
	r, compiled, err := initRuntime(ctx)
	if err != nil {
		return nil, err
	}

	mod, err := r.InstantiateModule(ctx, compiled, newModuleConfig())
	if err != nil {
		return nil, fmt.Errorf("failed to instantiate WASM module instance: %w", err)
	}
	defer mod.Close(ctx)

	allocFn := mod.ExportedFunction("alloc")
	deallocFn := mod.ExportedFunction("dealloc")
	targetFn := mod.ExportedFunction(fnName)

	if allocFn == nil || targetFn == nil {
		return nil, fmt.Errorf("WASM function %s or alloc not found", fnName)
	}

	// Allocate & write PDF bytes
	res, err := allocFn.Call(ctx, uint64(len(pdfBytes)))
	if err != nil {
		return nil, fmt.Errorf("alloc PDF buffer failed: %w", err)
	}
	pdfPtr := uint32(res[0])
	if !mod.Memory().Write(pdfPtr, pdfBytes) {
		return nil, errors.New("failed to write PDF buffer into WASM memory")
	}
	defer func() {
		if deallocFn != nil && pdfPtr != 0 {
			_, _ = deallocFn.Call(ctx, uint64(pdfPtr), uint64(len(pdfBytes)))
		}
	}()

	// Allocate & write extra bytes (options / password / pages)
	var extraPtr uint32
	if len(extraBytes) > 0 {
		resExtra, err := allocFn.Call(ctx, uint64(len(extraBytes)))
		if err != nil {
			return nil, fmt.Errorf("alloc extra buffer failed: %w", err)
		}
		extraPtr = uint32(resExtra[0])
		if !mod.Memory().Write(extraPtr, extraBytes) {
			return nil, errors.New("failed to write extra buffer into WASM memory")
		}
		defer func() {
			if deallocFn != nil && extraPtr != 0 {
				_, _ = deallocFn.Call(ctx, uint64(extraPtr), uint64(len(extraBytes)))
			}
		}()
	}

	// Call target FFI function
	var callResults []uint64
	if fnName == "ffi_process_pdf" || fnName == "ffi_detect_pdf" || fnName == "ffi_extract_pages_markdown" {
		callResults, err = targetFn.Call(ctx, uint64(pdfPtr), uint64(len(pdfBytes)), uint64(extraPtr), uint64(len(extraBytes)))
	} else {
		callResults, err = targetFn.Call(ctx, uint64(pdfPtr), uint64(len(pdfBytes)))
	}
	if err != nil {
		return nil, fmt.Errorf("WASM function %s call error: %w", fnName, err)
	}

	packed := callResults[0]
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed)

	if outPtr == 0 || outLen == 0 {
		return nil, errors.New("WASM returned empty or null output pointer")
	}

	memSize := mod.Memory().Size()
	if outPtr+outLen > memSize {
		return nil, fmt.Errorf("WASM output out of bounds: outPtr=%d outLen=%d memSize=%d", outPtr, outLen, memSize)
	}

	outBytes, ok := mod.Memory().Read(outPtr, outLen)
	if !ok {
		return nil, fmt.Errorf("failed to read WASM memory output at %d len %d", outPtr, outLen)
	}

	resultCopy := make([]byte, outLen)
	copy(resultCopy, outBytes)

	if deallocFn != nil {
		_, _ = deallocFn.Call(ctx, uint64(outPtr), uint64(outLen))
	}

	return resultCopy, nil
}

// ProcessPdf inspects a PDF buffer and extracts Markdown with full layout analysis.
func ProcessPdf(pdfBytes []byte, opts *ProcessOptions) (*PdfProcessResult, error) {
	return ProcessPdfWithContext(context.Background(), pdfBytes, opts)
}

// ProcessPdfWithContext inspects a PDF buffer with a given context.
func ProcessPdfWithContext(ctx context.Context, pdfBytes []byte, opts *ProcessOptions) (*PdfProcessResult, error) {
	var optsBytes []byte
	if opts != nil {
		var err error
		optsBytes, err = json.Marshal(opts)
		if err != nil {
			return nil, fmt.Errorf("failed to marshal ProcessOptions: %w", err)
		}
	}

	out, err := invokeWasm(ctx, "ffi_process_pdf", pdfBytes, optsBytes)
	if err != nil {
		return nil, err
	}

	var res PdfProcessResult
	if err := json.Unmarshal(out, &res); err != nil {
		return nil, fmt.Errorf("failed to unmarshal PdfProcessResult: %w (output: %s)", err, string(out))
	}

	if res.Error != "" {
		return nil, errors.New(res.Error)
	}

	return &res, nil
}

// DetectPdf performs fast metadata-only detection on a PDF buffer without full text extraction.
func DetectPdf(pdfBytes []byte, password string) (*PdfProcessResult, error) {
	return DetectPdfWithContext(context.Background(), pdfBytes, password)
}

// DetectPdfWithContext performs fast metadata-only detection with context.
func DetectPdfWithContext(ctx context.Context, pdfBytes []byte, password string) (*PdfProcessResult, error) {
	out, err := invokeWasm(ctx, "ffi_detect_pdf", pdfBytes, []byte(password))
	if err != nil {
		return nil, err
	}

	var res PdfProcessResult
	if err := json.Unmarshal(out, &res); err != nil {
		return nil, fmt.Errorf("failed to unmarshal PdfProcessResult: %w", err)
	}

	if res.Error != "" {
		return nil, errors.New(res.Error)
	}

	return &res, nil
}

// ClassifyPdf classifies PDF bytes into TextBased, Scanned, ImageBased, or Mixed.
func ClassifyPdf(pdfBytes []byte) (*PdfClassification, error) {
	return ClassifyPdfWithContext(context.Background(), pdfBytes)
}

// ClassifyPdfWithContext classifies PDF bytes with context.
func ClassifyPdfWithContext(ctx context.Context, pdfBytes []byte) (*PdfClassification, error) {
	out, err := invokeWasm(ctx, "ffi_classify_pdf", pdfBytes, nil)
	if err != nil {
		return nil, err
	}

	var res PdfClassification
	if err := json.Unmarshal(out, &res); err != nil {
		return nil, fmt.Errorf("failed to unmarshal PdfClassification: %w", err)
	}

	if res.Error != "" {
		return nil, errors.New(res.Error)
	}

	return &res, nil
}

// ExtractText extracts raw plain text from PDF bytes without Markdown conversion.
func ExtractText(pdfBytes []byte) (string, error) {
	return ExtractTextWithContext(context.Background(), pdfBytes)
}

// ExtractTextWithContext extracts raw plain text from PDF bytes with context.
func ExtractTextWithContext(ctx context.Context, pdfBytes []byte) (string, error) {
	out, err := invokeWasm(ctx, "ffi_extract_text", pdfBytes, nil)
	if err != nil {
		return "", err
	}

	var wrapper struct {
		Text  string `json:"text"`
		Error string `json:"error"`
	}
	if err := json.Unmarshal(out, &wrapper); err != nil {
		return "", fmt.Errorf("failed to unmarshal extract text response: %w", err)
	}

	if wrapper.Error != "" {
		return "", errors.New(wrapper.Error)
	}

	return wrapper.Text, nil
}

// ExtractPagesMarkdown extracts formatted markdown per page.
func ExtractPagesMarkdown(pdfBytes []byte, pages []uint32) (*PagesExtractionResult, error) {
	return ExtractPagesMarkdownWithContext(context.Background(), pdfBytes, pages)
}

// ExtractPagesMarkdownWithContext extracts formatted markdown per page with context.
func ExtractPagesMarkdownWithContext(ctx context.Context, pdfBytes []byte, pages []uint32) (*PagesExtractionResult, error) {
	var pagesBytes []byte
	if pages != nil {
		var err error
		pagesBytes, err = json.Marshal(pages)
		if err != nil {
			return nil, fmt.Errorf("failed to marshal pages: %w", err)
		}
	}

	out, err := invokeWasm(ctx, "ffi_extract_pages_markdown", pdfBytes, pagesBytes)
	if err != nil {
		return nil, err
	}

	var res PagesExtractionResult
	if err := json.Unmarshal(out, &res); err != nil {
		return nil, fmt.Errorf("failed to unmarshal PagesExtractionResult: %w", err)
	}

	if res.Error != "" {
		return nil, errors.New(res.Error)
	}

	return &res, nil
}

// Version returns the version of the pdf-inspector library.
func Version() (string, error) {
	return VersionWithContext(context.Background())
}

// VersionWithContext returns the version of the pdf-inspector library with context.
func VersionWithContext(ctx context.Context) (string, error) {
	r, compiled, err := initRuntime(ctx)
	if err != nil {
		return "", err
	}
	mod, err := r.InstantiateModule(ctx, compiled, newModuleConfig())
	if err != nil {
		return "", err
	}
	defer mod.Close(ctx)

	targetFn := mod.ExportedFunction("ffi_version")
	if targetFn == nil {
		return "", errors.New("ffi_version not found in WASM module")
	}

	res, err := targetFn.Call(ctx)
	if err != nil {
		return "", err
	}
	packed := res[0]
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed)

	outBytes, ok := mod.Memory().Read(outPtr, outLen)
	if !ok {
		return "", errors.New("failed to read WASM memory for version")
	}

	var wrapper struct {
		Version string `json:"version"`
		Error   string `json:"error"`
	}
	if err := json.Unmarshal(outBytes, &wrapper); err != nil {
		return "", err
	}

	if wrapper.Error != "" {
		return "", errors.New(wrapper.Error)
	}

	return wrapper.Version, nil
}
