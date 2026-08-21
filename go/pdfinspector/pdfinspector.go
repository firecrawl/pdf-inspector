// Package pdfinspector provides Go bindings to pdf-inspector's PDF
// classification and text extraction, via cgo against the compiled Rust
// library in go/ (see go/src/lib.rs for the C ABI, and go/README.md for how
// to build it).
//
// Scope matches the current v1: [Classify] (fast, no text extraction) and
// [ExtractText] (plain text, no layout/markdown). These are the two
// operations an OCR-routing pipeline needs: classify to decide whether a
// PDF's text layer is trustworthy enough to skip OCR, then extract the text
// if so.
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

// Classify inspects a PDF's bytes and reports its type, page count, which
// 0-indexed pages need OCR, and a confidence score — without extracting any
// text. Typically 10-50ms even on large documents, since it samples content
// streams rather than fully parsing them.
func Classify(data []byte) (*Classification, error) {
	cData, cLen := cBytes(data)
	defer freeCBytes(cData)

	raw := C.pdfinspector_classify((*C.uchar)(cData), cLen)
	defer C.pdfinspector_free_string(raw)

	var env classifyEnvelope
	if err := json.Unmarshal([]byte(C.GoString(raw)), &env); err != nil {
		return nil, fmt.Errorf("pdfinspector: decode classify result: %w", err)
	}
	if !env.Ok {
		return nil, classifyError(env.Error)
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
	cData, cLen := cBytes(data)
	defer freeCBytes(cData)

	raw := C.pdfinspector_extract_text((*C.uchar)(cData), cLen)
	defer C.pdfinspector_free_string(raw)

	var env textEnvelope
	if err := json.Unmarshal([]byte(C.GoString(raw)), &env); err != nil {
		return "", fmt.Errorf("pdfinspector: decode extract_text result: %w", err)
	}
	if !env.Ok {
		return "", classifyError(env.Error)
	}
	if env.Text == nil {
		return "", errors.New("pdfinspector: extract_text reported ok with no text")
	}
	return *env.Text, nil
}

func classifyError(msg *string) error {
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
