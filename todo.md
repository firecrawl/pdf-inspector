# Port to .NET 10 / C# — working plan

Source of truth for behaviour: `reference/` (the original Rust crate, kept verbatim).
Target: `src/PdfInspector` (library), `src/PdfInspector.Cli` (tools), `tests/PdfInspector.Tests`.

## Status legend
`[ ]` not started `[~]` in progress `[x]` done

## Phase 0 — scaffolding
- [x] Move original repo content to `reference/`
- [x] `Directory.Build.props` (net10.0, nullable, warnings-as-errors)
- [x] Solution + library/CLI/test projects
- [x] Root `CLAUDE.md`

## Phase 1 — PDF core (replaces the `lopdf` crate) — done

All 22 fixtures load; page counts match the Rust reference exactly.
- [x] Object model (`PdfObject`: Null/Bool/Integer/Real/String/Name/Array/Dictionary/Stream/Reference)
- [x] Lexer + object parser
- [x] Cross-reference tables and cross-reference streams
- [x] Object streams (`ObjStm`)
- [x] Stream filters: Flate, LZW, ASCIIHex, ASCII85, RunLength (+ predictors)
- [x] Encryption: RC4 40/128, AESV2, AESV3
- [x] Page tree with attribute inheritance
- [x] Content-stream operator decoder
- [x] Damaged-file recovery scan (lopdf tolerates broken xrefs)

## Phase 2 — foundational modules
- [x] `types.rs` → `Types/`
- [x] `process_mode.rs`
- [x] `text_utils.rs` (CJK/RTL, Otsu, ligatures, NFKC)
- [x] `glyph_names.rs` (generated table)
- [x] `adobe_korea1.rs` (generated table)
- [x] `tounicode.rs` (CMap parsing, CID decoding, bcmap loading)
- [x] `structure_tree.rs`
- [x] `text_quality.rs`

## Phase 3 — extractor
- [ ] `content_stream.rs`
- [ ] `fonts.rs`
- [ ] `xobjects.rs`
- [ ] `links.rs`
- [ ] `underline.rs`
- [ ] `layout.rs`
- [ ] `reading_order.rs`
- [ ] `mod.rs`

## Phase 4 — tables
- [ ] `grid.rs`
- [ ] `detect_rects.rs`
- [ ] `detect_lines.rs`
- [ ] `detect_heuristic.rs`
- [ ] `detect_struct.rs`
- [ ] `financial.rs`
- [ ] `structured.rs`
- [ ] `format.rs`
- [ ] `mod.rs`

## Phase 5 — markdown
- [ ] `analysis.rs`
- [ ] `classify.rs`
- [ ] `preprocess.rs`
- [ ] `heading.rs`
- [ ] `convert.rs`
- [ ] `postprocess.rs`
- [ ] `mod.rs`

## Phase 6 — detector + public API
- [ ] `detector.rs`
- [ ] `lib.rs` public surface

## Phase 7 — CLI
- [ ] `pdf2md`
- [ ] `detect-pdf`

## Phase 8 — validation
- [ ] Port unit tests
- [ ] Port `tests/integration_tests.rs`
- [ ] Differential check vs the release Rust binaries over `reference/tests/fixtures`
- [ ] Snapshot comparison against `reference/tests/snapshots`
