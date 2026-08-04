# Benchmarking against OpenDataLoader

The paired harness runs two `pdf2md` binaries through the same local
OpenDataLoader corpus, evaluates both outputs, and reports aggregate and
per-document deltas. This avoids comparing results produced from different
corpus revisions or evaluator versions.

Build a candidate and provide a released or worktree build as the baseline:

```bash
cargo build --release
python3 scripts/bench_opendataloader.py \
  --bench-dir ../opendataloader-bench \
  --baseline ../pdf-inspector-main/target/release/pdf2md \
  --candidate target/release/pdf2md \
  --max-document-regression 0.02 \
  --json-output /tmp/pdf-inspector-benchmark.json
```

Pass `--reference-evaluation path/to/evaluation.json` to report the candidate
delta against another evaluation, and add `--require-reference-lead` to make a
negative reference delta fail the run. By default, the candidate must not
regress the baseline overall score or introduce missing predictions. Use
`--min-overall-delta` to require a specific aggregate gain.

The OpenDataLoader repository is external and keeps its normal
`prediction/pdf-inspector` output. Paired evaluation copies each run into a
temporary directory before evaluating it, so the baseline and candidate cannot
overwrite one another.

## Published comparison protocol

The public benchmark table was refreshed on July 31, 2026, on an Apple M4 Pro
using pdf-inspector 0.2.6, LiteParse 2.10.1, OpenDataLoader 2.2.1,
PyMuPDF4LLM 0.2.0, and MarkItDown 0.1.5. Every engine processed the same 200
PDFs sequentially in a single process with OCR disabled. Reported speed is the
median of five alternating or rotating complete corpus runs after an excluded
warm-up run; quality scores come from the benchmark evaluator over all 200
outputs. Raw timings, predictions, evaluations, and charts are available in the
[results branch](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results).

## Optional renderer compatibility and timing

Selected-page rendering has a separate ignored test over the external
[`py-pdf/sample-files`](https://github.com/py-pdf/sample-files) corpus. The
files are CC-BY-SA-4.0 and are not vendored into this MIT repository. The test
pins commit `89039b6078fd0c9f98bf3d6fcb5583fac6b0ecaf` and verifies every selected
file's SHA-256 digest before rendering it.

```bash
git clone https://github.com/py-pdf/sample-files.git ../sample-files
git -C ../sample-files checkout 89039b6078fd0c9f98bf3d6fcb5583fac6b0ecaf

PDF_INSPECTOR_SAMPLE_FILES=../sample-files \
  cargo test --release --features render --test render_corpus_tests \
  renders_pinned -- --ignored --nocapture
```

The cases cover structured text, image-only pages, CMYK images, page
crop/rotation/scaling, repeated image references, and caller-ordered page
selection. The printed per-file durations are a smoke-test aid, not directly
comparable benchmark results. For reportable measurements, record the corpus
revision, compiler, target, CPU, and peak memory; discard one warm-up and report
the median of at least five release-profile runs at each tested DPI.

### Reference rasterization measurements

The following reference run used the checksum-verified
`018-base64-image/base64image.pdf` input from the pinned corpus revision above.
`pdf-inspector` classifies its only page as needing OCR. Measurements were made
on macOS 26.6, an Apple M3 Pro (12 cores, 18 GB), with Rust 1.95.0. Each row is
20 release-profile process runs after one discarded warm-up. Render time covers
PDF cloning, parsing, and rasterization; peak RSS is for the complete test
process. Nearest-rank p95 is reported.

| DPI | Output | RGBA8 bytes | Median | p95 | Min–max | Sample SD | Median peak RSS | p95 peak RSS |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 150 | 1,240 x 1,753 | 8,694,880 | 7.286 ms | 7.821 ms | 6.876–9.268 ms | 0.503 ms | 30,408,704 B | 31,391,744 B |
| 200 | 1,653 x 2,338 | 15,458,856 | 8.554 ms | 9.059 ms | 8.359–9.528 ms | 0.280 ms | 36,454,400 B | 37,404,672 B |
| 300 | 2,480 x 3,507 | 34,789,440 | 11.985 ms | 12.723 ms | 11.817–12.942 ms | 0.370 ms | 59,817,984 B | 60,768,256 B |

These figures measure rasterization only, not OCR inference. They should be
treated as one-machine reference data, not a cross-platform performance claim.
The returned RGBA8 buffer dominates the DPI-dependent memory increase.

## Optional backend evidence probe

The evidence probe compares positioned `pdf2md` items with MuPDF structured
text on the same pages. It is intended to find deterministic extraction or
layout evidence that could justify a future native implementation; it does not
merge MuPDF output into Markdown, invoke OCR, or add a runtime dependency.

Install MuPDF's `mutool`, build `pdf2md`, then run:

```bash
python3 scripts/probe_backend_evidence.py document.pdf \
  --pdf2md target/release/pdf2md \
  --json-output /tmp/backend-evidence.json
```

The report flags pages when MuPDF exposes a material net token gain, repeated
alignment anchors absent from local evidence, or additional image blocks. The
JSON includes bounded token samples and page-level counts so promising cases
can be inspected without treating backend disagreement as automatically
correct. Thresholds are configurable with `--min-token-gain`,
`--min-alternate-only-ratio`, and `--min-anchor-gain`.
