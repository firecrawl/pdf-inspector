# pdf-inspector 1.17.0, patched

Base: the published `pdf-inspector 1.17.0` package (commit 32555d2),
with the package renamed to `pdf-inspector-gov` so that it never
replaces `pdf-inspector` for other crates in a dependency graph. Its
`lopdf` dependency points at the patched lopdf (`lopdf-gov`, branch
`gov-patched-0.42.0` of firecrawl/lopdf) by git revision, because
the changes below use that branch's lazy loading and streaming
content reader.

## What is patched

### Typed block output (src/blocks/ + pdf2blocks)

`process_pdf_blocks_mem_with_options` parses a PDF into typed content
blocks per page (heading with a document-calibrated level, paragraph,
list item, code, table as structured cells, caption, image) instead
of a markdown string. It makes two page-at-a-time passes: an evidence
pass keeps only small per-line statistics (font-size splits, line
gaps, running-line and folio repetition, struct-tag usage, the
document's own vocabulary for dehyphenation), then an emit pass
classifies each page against that calibration, so peak memory follows
the largest page rather than the document. Running headers, footers,
and folios are emitted flagged (`furniture: true`), never deleted.
Wrapped lines rejoin with the same document-as-dictionary
dehyphenation policy the markdown postprocess uses. The markdown
pipeline is untouched; the block path reuses extraction, line
grouping, the table detectors, and the classification predicates, and
carries its own per-page emit loop. `pdf2blocks` prints one block per
line for tooling. Supporting visibility changes: the per-page
extraction loop body became `extract_page_unit` (a pure move the
whole-document path now calls), and several markdown classification
helpers are `pub(crate)`.

### OCR text layers (src/extractor/mod.rs)

`extract_page_unit` reads a page's invisible (Tr 3) text when the
visible pass skipped some and found no visible text. Scanners and OCR
software write a scan's recognised text that way, behind the page
image. 1.17.0 retries with invisible text only for documents it
classifies `Mixed`, and its detector classifies an image page carrying
an OCR layer as `TextBased`, so such a document's markdown came back
empty. The page adopts the layer under the same rule as the region
API's OCR-layer fallback in `src/lib.rs`: at least
`OCR_LAYER_MIN_ALNUM` alphanumerics and not garbage. Both are
`pub(crate)` now for that. Output changes only in documents that have
such a page, where the recovered text also feeds document-wide
statistics such as heading levels and running-line detection.

### Performance

Performance changes that keep markdown output byte-identical on the
regression fixtures, with the upstream test suite, fmt, and clippy
passing.

- `src/tables/detect_rects.rs`: `has_chart_bar_signature` iterates
  distinct rect breadths instead of every anchor rect, and its
  orientation accessors inline. Removes the cubic worst case over
  uniform rect grids.
- `src/extractor/underline.rs`: the repeated-ruling predicates take
  sorted-index windows instead of rescanning every rule and item per
  rule, with scratch vectors reused across calls.
- `src/extractor/content_stream.rs`, `src/extractor/xobjects.rs`,
  `src/extractor/content_decode.rs`: content streams are read through
  lopdf's streaming `ContentOperations` reader instead of a
  materialized `Content`. The bounded operator pre-count gate is
  unchanged. A stream the whole-stream decoder would refuse surfaces
  as a poisoned reader; the page errors and the form contributes
  nothing, exactly as before (the form walk budget rolls back to its
  pre-walk state).
- `src/extractor/mod.rs`: page accumulator capacity hints carried
  across pages (`PageVecSizes`); values never affect output.
- `src/lib.rs`: `compute_layout_complexity_with_chart_regions` slices
  contiguous per-page runs (`single_page_runs`) instead of filtering
  and cloning the whole document's items, rects, and lines once per
  page.
- `src/text_quality.rs`: `is_garbage_text` classifies character runs
  in one forward pass instead of collecting a `Vec<char>`.
- `src/markdown/analysis.rs`: `looks_title_case` compares words to
  the minor-word list by character iterator instead of allocating two
  strings per word.
- `src/extractor/underline.rs`: `segmented_row_rules` decides every
  rule's row in one sweep in y order, keeping the row's members ordered
  by left edge with a running wide-gap count, instead of sorting each
  rule's row once per rule.
- `src/tables/detect_rects.rs`: the contained-sub-rect dedup tests only
  the window of containers sorted by bottom edge that can contain the
  rect; `try_build_grid` finds each rect's covered cells by binary
  search over the grid edges; image items are cloned out only when an
  image is present; the hint pass reuses the call's own clustering
  when no page-background rect was excluded.
- `src/markdown/mod.rs`, `src/lib.rs`, `src/extractor/layout.rs`:
  per-page work reads per-page slices (`single_page_runs`, a per-page
  map of moved items) instead of filtering the whole document's items,
  rects and lines once per page; struct-table rows are grouped by page
  once; layout analysis takes the folio removal mask instead of a
  folio-filtered copy of every item; non-table items and images are
  separated and filtered in place.
- `src/lib.rs` `process_document`: the lopdf `Document` is dropped
  once the structure tree is parsed.
- `src/extractor/fonts.rs`, `content_stream.rs`, `xobjects.rs`: each
  font's decode settings (`FontDecodeSettings`) are resolved when the
  font changes (`Tf`, `Q`, page start) instead of looked up per shown
  string; the cp1252 fallback check compares bytes case-insensitively
  instead of lowercasing a string; page content is copied for comment
  stripping only when it contains `%`; read-only stream decodes borrow
  (`Stream::decompressed_content_cow`).
- `src/structure_tree.rs`: objects resolve through lopdf's
  `Document::get_object_transient`, so struct elements parsed by the
  walk are released with their subtree instead of staying in the
  document; node vectors are shrunk to fit.
- `src/extractor/mod.rs`: extraction accumulators are shrunk to fit.
- `src/markdown/postprocess.rs`: punctuation cleanup peeks the next
  character instead of collecting a `Vec<char>`; the final trim works
  in place.

### Output changes by design

`src/tables/detect_rects.rs` `cluster_rects` computes the connected
components of the rect-overlap graph with an x-sweep over a segment
tree of y edges and union-find, O(n log n) and independent of page
layout, replacing the 64pt grid, its oversized-rect path and its
per-cell pair cap. `MAX_CLUSTER_RECTS` still stops a component
growing; groups are ordered by their smallest member. Output changes
only on pages where a cap applies, where the old result depended on
grid cell boundaries.

`src/markdown/furniture.rs` `strip_repeated_lines` used to keep the
first page's copy of every proven running header or footer. It now
removes that copy too when it sits in the running slot, within the
same 5% of the average page span that `has_consistent_y` allows, of
the mean Y of the copies on later pages. A same-text line elsewhere on
the first page, such as a title set in the body, is kept as before.
The kept copy was otherwise classified like body text and could
surface as a heading, and from there as a document's title. Output
changes only on documents of three or more pages with a proven running
line: that line's first copy leaves the markdown. Two upstream
assertions follow the new rule:
`test_strip_repeated_keeps_first_occurrence` and
`repetition_normalizes_unicode_page_numbers` now expect no copy.

## Packaging deltas

The tree is the published package contents (no tests, benches, or
examples). The manifest is the development manifest with the package
renamed, the `pdf2blocks` binary added, and `lopdf` resolved from the
patched lopdf branch by git revision.

## Upstreaming

Upstream is `github.com/firecrawl/pdf-inspector`. The intended end
state is to merge these changes upstream after its regression suite
passes, release, and switch consumers back to the published crate.
