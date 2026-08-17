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
