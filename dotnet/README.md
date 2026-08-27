# Firecrawl.PdfInspector

.NET bindings for the native
[pdf-inspector](https://github.com/firecrawl/pdf-inspector) Rust library.

```bash
dotnet add package Firecrawl.PdfInspector
```

```csharp
using Firecrawl.PdfInspector;

var pdf = File.ReadAllBytes("document.pdf");
var result = PdfInspector.ProcessPdf(pdf);

Console.WriteLine(result.PdfType);
Console.WriteLine(result.Markdown);
```

The package also exposes `DetectPdf`, `ClassifyPdf`, `ExtractText`, `Version`,
and `ProcessPdfWithOcr`, with stream and async convenience overloads.

Native assets are included for Windows x64, Linux x64/ARM64 (glibc), and macOS
ARM64. OCR follows the Node/Python package model: PDFium, ONNX Runtime, and OCR
models are external and are only needed when OCR is routed.

Release packages are assembled by `.github/workflows/publish-nuget.yml`, which
builds each native library on a matching runner and combines all four RID
assets into one `.nupkg`. A normal local `dotnet pack` fails when any supported
RID is missing, preventing an incomplete package from being published. For a
deliberately platform-only diagnostic package, pass
`-p:AllowIncompleteNativeAssets=true`.

## Test from source

```bash
dotnet test Firecrawl.PdfInspector.slnx -c Release
```

The test project builds the host-native Rust library and copies it into the
test output automatically. On Windows it also locates the installed MSVC and
Windows SDK tools, so a Developer PowerShell is not required. A Rust 1.98
toolchain and the platform C/C++ build tools are still required.
