# Changelog

Notable changes to pdf-inspector. Every distribution (Rust crate, Python
package, Node package and platform packages, WebAssembly package) shares one
version. A separate release pull request bumps the manifests with
`scripts/version.py` and renames the `Unreleased` section below to that
version and date. Earlier releases are described in their
[GitHub releases](https://github.com/firecrawl/pdf-inspector/releases).

## [Unreleased]

### Added

- `TextItem::baseline_shift`: signed offset, in points, of a superscript or
  subscript glyph run from the baseline of the body text it is attached to
  (positive = raised, negative = lowered, `0` for normal text). Exposed as
  `baseline_shift` in the Python bindings and `pdf2md --items-json`, and as
  `baselineShift` in the Node bindings, so consumers can emit `<sup>`/`<sub>`
  themselves. `TextItem::line_y()` returns the body baseline a run belongs to
  and `TextItem::is_script()` tells flagged runs apart.
- `TextLine::text()` and `text_with_formatting()` wrap flagged runs in
  `<sup>…</sup>` / `<sub>…</sub>` (`Yibo Yan<sup>1,2,3</sup>`,
  `V<sub>f</sub>`, `10<sup>–15</sup>`), with word spacing decided by the
  measured gap at the run's edges. Table cells render runs the same way
  through one shared cell-text module, and items are assigned to cells by the
  body baseline they belong to, so `V<sub>f</sub>` and `$1,234<sup>1</sup>`
  survive inside tables too.
- `tests/fixtures/cropbox_offset_origin.pdf`, a page whose CropBox origin is
  not `(0, 0)`, with Rust, Node and Python tests pinning the shared coordinate
  frame described under Changed.
- `TextItem::rotation`: the run's baseline angle in degrees counter-clockwise,
  in `[0, 360)` (`0` horizontal, `90` reading bottom-to-top, `270`
  top-to-bottom, `180` upside-down), and `TextItem::advance_known`, which is
  `false` only when the box's extent along the baseline is an estimate
  because the font carries no width metrics. Both are exposed as `rotation` /
  `advance_known` in the Python bindings and `pdf2md --items-json`, and as
  `rotation` / `advanceKnown` in the Node bindings. Consumers that detected
  rotated runs through `width == 0` should key off `rotation` instead.
- `extract_text_with_positions_and_rotations_mem` (Node
  `extractTextWithPositionsAndRotations`, Python
  `extract_text_with_positions_and_rotations[_bytes]`) returns the items
  together with the frame of every page whose text was predominantly rotated
  and therefore turned (`PageRotation::Ccw` / `Cw`, now public).
  `collect_text_in_region_in_frame` takes that frame explicitly, and
  `RegionCoordSpace::Rotated90Cw` completes the region API for clockwise
  pages.

### Fixed

- Raised and lowered marker glyphs no longer form their own line. Line
  grouping — the Markdown pipeline and `extract_text_in_regions`
  (`extractTextInRegions`) alike — compares baselines through `line_y()`, so
  affiliation markers 4–7pt above an author line, footnote references after
  a sentence, and unit exponents (`kg/m³`) stay on the line they annotate.
  Previously the whole marker run of an author block came out as an orphan
  `,2,3,2,4,*` line above the names.
- Script detection is geometric: a run is a sub/superscript when it is
  0.4–0.75× the size of a tightly adjacent neighbor and sits at a real
  baseline offset from it. Multi-glyph runs (`1` `,` `2` `,` `3`, `2,*`,
  `1)`, `th`, `max`) are recognised as one run; markers that LEAD their word
  (`¹Hong Kong University`, `<sup>3,4</sup>Some Institute`) attach to the
  following word; markers after closing punctuation (`sentence.²`) and after
  digits (`$1,234<sup>1</sup>`) are no longer glued on as body text.
- Digit-only runs beside a word keep fusing as Unicode super/subscript
  characters (`H₂O`, `word²`, `See note¹²`); level small runs (small caps,
  same-baseline size changes) are no longer mistaken for subscripts.

- Text shown with a rotated text matrix (a vertical arXiv-style margin stamp,
  a rotated table header) reported `width == 0` and the font size as
  `height`; downstream code then substituted a character-count width, turning
  the stamp into a phantom horizontal line that `extract_text_in_regions`
  assigned to the body paragraph it crossed. Every run now gets the
  axis-aligned box of its glyph run — a vertical run is tall and thin — from
  one geometry helper shared by the page and Form XObject parsers, and upright
  text keeps its historical box exactly.
- Pages whose text reads top-to-bottom (clockwise) are turned against their
  own direction instead of the fixed counter-clockwise turn that mirrored
  word and line order; link and AcroForm widget boxes, page-box clipping, and
  region matching follow the turned frame. Only runs within about 20° of an
  axis vote on the turn, so a page of diagonal text keeps its frame, and so
  does a page whose vertical runs split evenly between the two directions.
- A run whose font carries no width metrics gets a half-em-per-painted-glyph
  estimate laid along its baseline (character and word spacing included),
  and the text cursor moves by the same estimate, so the runs that follow it
  no longer pile up on one origin.
- A reflected text matrix has no rotation — its reading direction and its
  glyphs' orientation differ by a half turn — so such a run reports how its glyphs
  stand: `0` for the mirrored-x matrix some producers paint right-to-left
  text with, which then merges, groups into lines, and carries
  decorations like the upright run it looks like. A negative `Tf` size turns
  a run around and reads as `180`; upside-down runs group into lines by the
  baseline they hang from (`TextItem::baseline_y()`).
- ActualText runs shown under a scaled text matrix reported widths multiplied
  by the scale twice.
- Form XObjects inherit the invoking stream's text rise and rendering mode
  (text state is graphics state), so `3 Tr` hidden text inside a form stays
  hidden on the visible pass and a form drawn under a raised baseline keeps
  it.
- Upside-down (180°) runs are never merged or split in reverse, their lines
  sort in reading order, and their underlines and strikeouts are recognised
  from their own baseline; RTL lines drawn with mirrored-x matrices keep the
  classic right-to-left order.

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
- The region APIs interpret their inputs relative to the same box:
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
- Rust: `TextItem` gained the required public field `baseline_shift`, so code
  that builds a `TextItem` with a struct literal must add it (`0.0` for normal
  text). This follows the precedent of `font_tag` in 1.16.0; the Python and
  Node bindings are unaffected.
- Snapshot fixtures `thermo-freon12` and `shannon-entropy-p1-2` updated for
  the corrected script handling (`Freon<sup>®</sup>`, `V<sub>f</sub>`,
  `2<sup>N</sup>`, `¹Nyquist`, `log<sub>b</sub> a`).
- Rust: `TextItem` gained the required public fields `rotation` and
  `advance_known`, so struct literals must add them (`0.0` and `true` for
  ordinary upright text). Items that do not come from a text matrix (images,
  links, form fields, OCR) report `0.0` / `true`.
- Rust: the legacy `collect_text_in_region` still infers only the
  counter-clockwise frame from a page's item coordinates; callers with
  clockwise pages should pass the frame reported by
  `extract_text_with_positions_and_rotations_mem` to
  `collect_text_in_region_in_frame`, or use `extract_text_in_regions_mem`,
  which handles both turns itself.
