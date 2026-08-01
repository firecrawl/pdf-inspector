# pdf-inspector (.NET 10 / C#)

Fast PDF text extraction to structured Markdown. A port of the original Rust crate,
which is preserved verbatim under `reference/` and remains the behavioural source of truth.

## Build & Test

```bash
dotnet build                                  # build all projects
dotnet test                                   # unit + integration tests
dotnet build -c Release                       # release build for benchmarks
```

Warnings are errors (`Directory.Build.props`), so a clean build is required before committing.

## Layout

```
reference/                     – the original Rust crate, unmodified. Behavioural source of truth.
src/PdfInspector/              – the library
src/PdfInspector.Cli/          – pdf2md and detect-pdf entry points
tests/PdfInspector.Tests/      – xUnit unit + integration tests
```

## Library structure

```
src/PdfInspector/
  Pdf/                         – PDF core, replacing the Rust build's `lopdf` dependency
    PdfObject.cs               – object model (Null/Bool/Integer/Real/String/Name/Array/Dict/Stream/Reference)
    PdfLexer.cs                – byte-level tokeniser shared by the file and content-stream parsers
    PdfParser.cs               – object parser; deliberately permissive about malformed files
    PdfDocument.cs             – xref tables + xref streams, object streams, page tree, recovery scan
    StreamFilters.cs           – Flate/LZW/ASCIIHex/ASCII85/RunLength + PNG/TIFF predictors
    PdfDecryptor.cs            – standard security handler, revisions 2–6 (RC4, AESV2, AESV3)
    ContentStream.cs           – operator decoding, including inline-image skipping
  Types/                       – TextItem, TextLine, PdfRect, PdfLine, LayoutComplexity
  Text/                        – CJK/RTL handling, Otsu threshold, ligatures, NFKC, glyph-name tables
  ToUnicode/                   – CMap/ToUnicode parsing, CID decoding, bundled Adobe CMaps
  Extractor/                   – content-stream state machine, fonts, layout, XObjects, links, reading order
  Tables/                      – rect / line / heuristic / struct table detection, grid building, formatting
  Markdown/                    – line→Markdown conversion, heading tiers, classification, pre/post-processing
  Detector/                    – PDF type classification, tiled-scan detection, page sampling
```

## Key design decisions

These carry over from the Rust original; see `reference/CLAUDE.md` for the long-form rationale.

- **Primary audience is AI agents.** Output optimized for token efficiency and semantic quality, not visual formatting. No cosmetic padding.
- **Three table detection strategies** run in priority order: rect-based → line-based → heuristic. First valid result wins.
- **Column detection** uses horizontal projection histograms with valley detection. Multi-item spanning lines (titles, headers) are pre-masked using column-aware thresholds before column assignment.
- **Newspaper vs tabular** classification determines reading order: newspaper reads columns sequentially, tabular Y-interleaves them.
- **Tiled-scan detection** catches scanned PDFs with JBIG2/strip images where no single tile exceeds the template threshold but aggregate area does (≥2M pixels).
- **Garbage text upgrade** reclassifies Mixed PDFs as Scanned when extracted text is <50% alphanumeric.
- **Tagged PDF support** uses structure tree roles (H1-H6, P, L, Code, BlockQuote) when available, falling back to font-size heuristics.

## Porting conventions

- The Rust module a file was ported from is named in a header comment; keep that link intact.
- Rust `Option<T>` becomes a nullable reference or `T?`; `Result<T, E>` becomes an exception
  or a `bool TryX(out T)` pair, whichever reads better at the call site.
- `f32` is preserved as `float` throughout. The layout and table heuristics compare
  against tuned thresholds, so widening to `double` changes output.
- Iterator chains become LINQ only where it stays readable; hot loops in the extractor
  and table detectors stay imperative.
- `rayon` parallelism maps to `Parallel.For`/PLINQ, and only where the Rust used it.

## Validation

The Rust binaries under `reference/target/release/` are the golden reference:

```bash
cargo build --release --bins                          # in reference/
./reference/target/release/pdf2md FILE.pdf            # golden Markdown
dotnet run --project src/PdfInspector.Cli -- pdf2md FILE.pdf
```

`tests/PdfInspector.Tests` includes a differential suite that runs both over
`reference/tests/fixtures` and compares. Snapshots in `reference/tests/snapshots`
are checked directly.

## Debugging

Set `PDFINSPECTOR_LOG` to a comma-separated list of module names for trace output:

```bash
PDFINSPECTOR_LOG=layout dotnet run --project src/PdfInspector.Cli -- pdf2md file.pdf
PDFINSPECTOR_LOG=tables,detector dotnet run --project src/PdfInspector.Cli -- pdf2md file.pdf
```
