# pdf-inspector — Performance Optimization Report

CURRENT SCORE
=============

Original baseline SHA:  f4aab3b36f7fa1752c65ea45fa64be1274f671cf
Current best SHA:       f4aab3b36f7fa1752c65ea45fa64be1274f671cf

Official corpus (OpenDataLoader 200 PDFs, in-process Rust, median of 7 runs)
---------------------------------------------------------------------------
Baseline median: 427.2 ms
Current median:  427.2 ms
Overall speedup: 1.00x

Quality (official paired evaluator)
-----------------------------------
Baseline overall: 0.875690
Current overall:  0.875690
Reading order (NID):  Baseline 0.915409 / Current 0.915409
Tables (TEDS):        Baseline 0.814117 / Current 0.814117
Headings (MHS):       Baseline 0.787774 / Current 0.787774

Tail
----
Baseline P95: 3.86 ms
Current P95:  3.86 ms
Worst pathological speedup: n/a

Memory
------
Baseline peak RSS: 44.1 MB
Current peak RSS:  44.1 MB
Difference:        —

---

# Environment

| Item          | Value |
| ------------- | ----- |
| Git SHA       | f4aab3b36f7fa1752c65ea45fa64be1274f671cf |
| Rust          | 1.92.0 (ded5c06cf 2025-12-08) |
| OS            | macOS 26.3 (Darwin 25.3.0, arm64) |
| CPU           | Apple M4 Pro (14 cores) |
| RAM           | 24 GB |
| Compiler flags| default `release` (no LTO, codegen-units=16) |

# Baseline scorecard

| Metric                  | Baseline |
| ----------------------- | -------: |
| Git SHA                 | f4aab3b |
| 200-document total time | 427.2 ms |
| Documents/sec           | 468.2 |
| Pages/sec               | 468.2 |
| Median document latency | 1.91 ms |
| P95 document latency    | 3.86 ms |
| P99 document latency    | 5.42 ms |
| Slowest document        | 13.68 ms (01030000000141) |
| Peak RSS                | 44.1 MB |
| Overall quality         | 0.875690 |
| Reading-order NID       | 0.915409 |
| Table TEDS              | 0.814117 |
| Heading MHS             | 0.787774 |
| Failed documents        | 0 |

Raw timing JSON: `bench/baseline-timing.json` (see `bench/` dir).
Raw quality JSON: `bench/bench-baseline.json`.

# Profiling (baseline)

`sample` (macOS, 10ms interval) over a 400-run loop of the in-process harness.
Top CPU consumers (excluding rayon idle sleeps):

1. `miniz_oxide::inflate` (FlateDecode)         ~15.8%  — content stream decompression
2. `Content::decode` / `nom` parser             ~10%    — PDF operator parsing
3. `fix_bare_struct_names`                      ~2.1%   — full-buffer naive substring scan
4. `scan_content_for_text_operators` (detect)   ~1.4%   — detection byte scan
5. font decoding / layout / tables              remainder

Key finding: content streams are **decompressed twice** per document —
once in `detector::analyze_page_content` (via `decompressed_content()`) and
again in `extractor::content_stream` (via `get_page_content()`), because
lopdf's `Stream::decompressed_content()` re-runs FlateDecode on every call.

---

# Performance ledger

## Experiment #01 — memchr substring search
--------------
Hypothesis: `fix_bare_struct_names` and `estimate_page_count_from_bytes` use
`slice::windows(n).position()` — O(n·m) byte scans — to find `/StructTreeRoot`,
`/S `, and `/Type` in raw PDF buffers on every load. SIMD `memchr::memmem` is
much faster.

Files changed: Cargo.toml, src/structure_tree.rs, src/detector.rs

Before (baseline): 434.6 ms  →  After: 420.8 ms (target speedup 1.033x)
Full 200-doc corpus: baseline 434.6ms, candidate 420.8ms → vs original 1.033x
Quality: unchanged (byte-identical markdown, overall 0.875690)
Decision: KEEP (b8b4b91)

## Experiment #02 — decompress each content stream once
--------------
Hypothesis: detection and extraction both call
`Stream::decompressed_content()`, which re-runs FlateDecode each call. Thread a
per-document cache keyed by stream object id through both phases.

Files changed: src/lib.rs, src/detector.rs, src/extractor/{mod,content_stream,xobjects}.rs

Before (Exp 1): 438.1 ms  →  After: 412.1 ms (target speedup 1.063x)
Full 200-doc corpus: Exp1 426.9ms, candidate 418.8ms → vs original 1.06x
Quality: unchanged (byte-identical markdown, overall 0.875690)
Decision: KEEP (a2738a9)

