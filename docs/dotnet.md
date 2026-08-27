# .NET API

`Firecrawl.PdfInspector` provides `netstandard2.0` bindings to the native Rust
PDF parser. PDF bytes remain in-process and are not uploaded.

The managed API targets `netstandard2.0`, but automatic RID-native asset
selection requires a modern .NET runtime. The supported deployment baseline is
.NET Core 3.1 or .NET 5 and later; classic .NET Framework is not currently a
supported runtime.

## Install

```bash
dotnet add package Firecrawl.PdfInspector
```

The package includes native libraries for:

- Windows x64 (`win-x64`)
- Linux x64 glibc (`linux-x64`)
- Linux ARM64 glibc (`linux-arm64`)
- macOS ARM64 (`osx-arm64`)

## Extract Markdown

```csharp
using System;
using System.IO;
using Firecrawl.PdfInspector;

var pdf = File.ReadAllBytes("annual-report.pdf");
var result = PdfInspector.ProcessPdf(pdf, new ProcessOptions
{
    Pages = new uint[] { 1, 3, 5 },
    Profile = MarkdownProfile.Compact,
    IncludePageMarkers = true,
});

Console.WriteLine(result.PdfType);
Console.WriteLine(result.Markdown);
```

`ProcessPdf` also accepts a readable `Stream`. `Pages` is 1-indexed, matching
the WebAssembly API.

## Detection, classification, and plain text

```csharp
var detection = PdfInspector.DetectPdf(pdf);
var classification = PdfInspector.ClassifyPdf(pdf);
var text = PdfInspector.ExtractText(pdf);
var version = PdfInspector.Version();
```

`DetectPdf` returns the full processing result without Markdown.
`ClassifyPdf.PagesNeedingOcr` is 0-indexed for compatibility with the Node and
WebAssembly classification contract. Other page-number fields are 1-indexed.

Async convenience methods (`ProcessPdfAsync`, `DetectPdfAsync`,
`ClassifyPdfAsync`, and `ExtractTextAsync`) copy the input before scheduling
native work on the thread pool. Cancellation prevents work that has not
started, but cannot interrupt a native call already in progress.

## Selective OCR

```csharp
var result = await PdfInspector.ProcessPdfWithOcrAsync(pdf, new OcrOptions
{
    Mode = OcrMode.Auto,
    Offline = true,
});

Console.WriteLine(result.Markdown);
Console.WriteLine(string.Join(", ", result.PagesRoutedToOcr));
```

The NuGet package does not embed PDFium, ONNX Runtime, or OCR model files.
`OcrMode.Off` and clean `OcrMode.Auto` requests do not load those external
components. When OCR is routed, configure `PDFIUM_LIB_PATH` and
`ORT_DYLIB_PATH` (or the operating system library search path). Models are
downloaded into the shared cache when missing unless `Offline` is true; use
`ModelDirectory` for an explicit offline model set.

See [OCR runtime setup](ocr-runtime.md) for pinned runtime downloads and model
cache behavior.

## Errors and threading

Native failures throw `PdfInspectorException`. `NativeStatus` distinguishes
invalid arguments, PDF processing failures, serialization failures, and
contained Rust panics. The wrapper checks the native ABI version before its
first operation so a mismatched managed/native installation fails early.

Synchronous methods execute on the calling thread. Use the async convenience
methods or schedule synchronous calls on an application-owned worker when
processing large documents in latency-sensitive applications.
