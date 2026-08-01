# pdf-inspector

Fast PDF text extraction to structured Markdown. CLI binary: `pdf2md`. Detection binary: `detect-pdf`.

## Build & Test

```bash
cargo fmt                                    # format
cargo clippy -- -D warnings                  # lint (enforced, zero warnings)
cargo test                                   # unit + integration tests (267+ unit, 73+ integration)
cargo build --release                        # release binary for benchmarks
```

All three must pass before committing.

## Binaries

- `pdf2md` — extract PDF → Markdown. Supports `--json` for structured output.
- `detect-pdf` — classify PDF type (TextBased/Scanned/Mixed/ImageBased). Supports `--analyze --json`.

## Architecture

```
src/
  lib.rs                        – public API, process_pdf_with_options, encoding issue detection
  detector.rs                   – PDF type classification, tiled-scan detection, page sampling
  types.rs                      – TextItem, TextLine, PdfRect, PdfLine
  tounicode.rs                  – CMap/ToUnicode parsing, CID decoding
  text_utils.rs                 – CJK/RTL handling, Otsu threshold, ligature expansion, NFKC
  extractor/
    mod.rs                      – top-level extraction orchestrator
    content_stream.rs           – PDF operator state machine (Tj/TJ/Td/Tm/q/Q)
    fonts.rs                    – font width/encoding, CMapDecisionCache, TrueType cmap fallback
    layout.rs                   – column detection (histogram), newspaper/tabular classification,
                                  spanning-line pre-masking, sidebar detection
  tables/
    detect_rects.rs             – rect-based table detection (union-find clustering)
    detect_heuristic.rs         – heuristic table detection (gap-histogram, body-font tables)
    detect_lines.rs             – line-based table detection (H/V line grids)
    grid.rs                     – column/row boundaries, cell assignment
    format.rs                   – table→Markdown formatting, continuation row merging
  markdown/
    convert.rs                  – core line→Markdown loop, struct-tree role support
    analysis.rs                 – font stats, heading tiers, paragraph thresholds
    classify.rs                 – line classification (header, list, code, caption)
    preprocess.rs               – drop cap merging, heading line merging
    postprocess.rs              – dot leaders, hyphenation, page numbers, URL formatting
```

## Key design decisions

- **Primary audience is AI agents.** Output optimized for token efficiency and semantic quality, not visual formatting. No cosmetic padding.
- **Three table detection strategies** run in priority order: rect-based → line-based → heuristic. First valid result wins.
- **Column detection** uses horizontal projection histograms with valley detection. Multi-item spanning lines (titles, headers) are pre-masked using column-aware thresholds before column assignment.
- **Newspaper vs tabular** classification determines reading order: newspaper reads columns sequentially, tabular Y-interleaves them.
- **Tiled-scan detection** catches scanned PDFs with JBIG2/strip images where no single tile exceeds the template threshold but aggregate area does (≥2M pixels).
- **Garbage text upgrade** reclassifies Mixed PDFs as Scanned when extracted text is <50% alphanumeric.
- **Tagged PDF support** uses structure tree roles (H1-H6, P, L, Code, BlockQuote) when available, falling back to font-size heuristics.

## Testing

- **Unit tests**: inline `#[cfg(test)] mod tests` in each module with synthetic data.
- **Integration tests**: `tests/integration_tests.rs` with fixture PDFs in `tests/fixtures/`.
- **Regression suite**: sibling repo `pdf-evals` with 179+ snapshot PDFs. Run `cargo build --release` then `bench.py test` in that repo before committing.

## Debugging

```bash
RUST_LOG=pdf_inspector::extractor::layout=debug cargo run --bin pdf2md -- file.pdf
RUST_LOG=pdf_inspector::tables=debug cargo run --bin pdf2md -- file.pdf
RUST_LOG=pdf_inspector::detector=debug cargo run --release --bin detect-pdf -- file.pdf
```

## Conventions

- Clippy: use `is_some_and(...)` not `map_or(false, ...)`
- lopdf quirk: `ParseError` is private — match by string for `InvalidFileHeader`
- Column limit for tables: 25 (wide statistical tables)
- `propagate_merged_cells` skipped for >10 columns (spanning rects = background fills)
