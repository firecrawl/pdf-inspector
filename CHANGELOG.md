# Changelog

Notable changes to pdf-inspector. All distributions (Rust crate, Python
package, Node and WebAssembly packages) share one version; see
[docs/publishing.md](docs/publishing.md). Earlier releases are described in the
[GitHub releases](https://github.com/firecrawl/pdf-inspector/releases).

## 1.18.0 — 2026-09-01

### Changed

- **Coordinate frame of positioned output — consumer action may be required.**
  `extract_text_with_positions*` (Rust), `extractTextWithPositions` (Node),
  `extract_text_with_positions[_bytes]` (Python) and `pdf2md --items-json` now
  report `x`/`y` relative to the page's visible page box — `CropBox ∩ MediaBox`,
  else the MediaBox — with the box's lower-left corner as the origin. Image,
  link and form-field items shift the same way. Previously the values were raw
  content-stream coordinates, so on pages whose CropBox (or MediaBox) origin is
  not `(0, 0)` every item was displaced from anything rendered from the CropBox,
  and consumers intersecting items with rendered regions silently selected the
  wrong text.
- The region APIs interpret their inputs in the same frame:
  `extract_text_in_regions*`, `extract_tables_in_regions*`,
  `detect_vector_grid_in_region*` and the TSR crop bboxes
  (`TsrTableInput.crop_pdf_pt_bbox`) are top-left-origin PDF points relative to
  the visible page box, and `StructuredCell.page_pt_bbox` is returned in it.
  These previously flipped `y` with the MediaBox height and ignored the box
  origin.
- Pages whose CropBox equals the MediaBox and whose MediaBox origin is `(0, 0)`
  — the vast majority — produce identical output. Consumers that compensated
  for the CropBox origin themselves must drop that adjustment. `/Rotate` is
  still not applied.

### Added

- `tests/fixtures/cropbox_offset_origin.pdf`, a page whose CropBox origin is
  not `(0, 0)`, with Rust, Node and Python tests pinning the shared frame.
