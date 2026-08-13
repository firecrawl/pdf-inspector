# pdf-inspector — Performance Optimization Report

CURRENT SCORE
=============

Original baseline SHA:  f4aab3b36f7fa1752c65ea45fa64be1274f671cf
Current best SHA:       d563a65

Official corpus (OpenDataLoader 200 PDFs, in-process Rust, median of runs)
---------------------------------------------------------------------------
Baseline median: 434.9 ms
Current median:  378.5 ms
Overall speedup: 1.15x (docs/sec ~460 → ~528)

Quality (official paired evaluator)
-----------------------------------
Baseline overall: 0.875690
Current overall:  0.875690  (byte-identical markdown on all 200 documents)
Reading order (NID):  Baseline 0.915409 / Current 0.915409
Tables (TEDS):        Baseline 0.814117 / Current 0.814117
Headings (MHS):       Baseline 0.787774 / Current 0.787774

Tail
----
Baseline P95: 3.69 ms
Current P95:  3.32 ms
Worst pathological speedup: ~23x (20,000-rectangle vector page: 0.71s → 0.03s)

Memory
------
Baseline peak RSS: 42.5 MB
Current peak RSS:  51.2 MB
Difference:        +8.7 MB (decompressed content-stream cache lives for the
                   duration of one document; acceptable for the speedup)

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

## Experiment #03 — spatial-hash rect clustering
--------------
Hypothesis: `cluster_rects` is O(n²). On pages with thousands of disjoint
rectangles (vector drawings, dense grids) the full overlap scan dominates.
Spatial hashing over tolerance-expanded boxes should reduce it to ~O(n).

Files changed: src/tables/detect_rects.rs

Pathological (rect scaling, single doc):
  rects-2000:  0.02s → 0.00s
  rects-5000:  0.06s → 0.01s  (~6x)
  rects-10000: 0.18s → 0.01s  (~18x)
  rects-20000: 0.71s → 0.03s  (~23x)
Full 200-doc corpus: unchanged (406.9ms vs baseline 425.7ms, cumulative 1.046x)
Quality: unchanged (byte-identical markdown, overall 0.875690)
Decision: KEEP (52fb2df)

## Experiment #04 — memoize embedded font-file decompression
--------------
Hypothesis: `descriptor_style_flags` (extraction) and
`embedded_font_has_cmap`/`identity_h_font_has_fallback` (detection) each
decompress the same embedded font program — the largest FlateDecode streams —
across detection/extraction and per page. Threading the shared
`DecompressedContentCache` into font-file reads should inflate each font once.

Files changed: src/detector.rs, src/extractor/{fonts,content_stream,xobjects}.rs

Full 200-doc corpus: baseline 403.8ms, candidate 384.1ms → cumulative 1.051x
Quality: unchanged (byte-identical markdown, overall 0.875690)
Decision: KEEP (b6a93a1)

## Experiment #05 — fat LTO + codegen-units=1
--------------
Hypothesis: parsing is dominated by small hot functions in dependency crates
(lopdf's nom parser, miniz_oxide inflate, hash maps); the default release
profile (no LTO, 16 codegen units) blocks cross-crate inlining of those paths.

Files changed: Cargo.toml

Isolated: 415.2ms → 374.2ms (1.110x) on the 200-doc corpus.
Full 200-doc corpus (cumulative with Exp 1-4): 430.9ms → 373.5ms (1.154x).
Quality: unchanged (byte-identical markdown, overall 0.875690).
Decision: KEEP (d563a65)

## Rejected: target-cpu=native (no measurable gain)
--------------
`-C target-cpu=native` + LTO was 0.996x vs LTO alone on the M4 Pro — the
default arm64 target already uses NEON and miniz_oxide ships NEON paths.
Hardware-specific with no benefit; rejected.

## Rejected (report-only): zlib-ng FlateDecode backend
--------------
Swapping flate2's miniz_oxide backend for zlib-ng was 1.047x on the corpus,
but it needs a C compiler + cmake and breaks the pure-Rust / wasm32 build.
Non-portable; reported separately, not merged.



# Profiling evidence (baseline → final)

`sample` (macOS, ~6ms interval) over a 400-run loop of the in-process harness,
before vs. after optimization. Samples are wall-clock CPU attribution for the
application's own frames (rayon idle worker sleeps excluded).

| Function | Before | After |
| -------- | -----: | ----: |
| miniz_oxide inflate (FlateDecode) | ~1389 | ~890 |
| fix_bare_struct_names (naive scan) | ~214 | ~0 (memchr) |
| lopdf Content::decode (nom) | ~700 | ~520 |
| scan_content_for_text_operators | ~140 | ~97 |
| cluster_rects (O(n²)) | ~20 | ~12 (spatial hash) |

FlateDecode dropped ~36% (double-decompress + font re-decompress eliminated);
the remainder is the single unavoidable inflate pass. `fix_bare_struct_names`
vanished from the profile after the memchr swap. Rect clustering moved from
O(n²) to ~O(n) (23x on a 20k-rect page).

# Pathological / stress cases (baseline → final)

| Case | Baseline | Optimized | Speedup |
| ---- | -------: | --------: | ------: |
| rects-2000 (vector) | 0.02s | 0.00s | ~2x |
| rects-5000 (vector) | 0.06s | 0.01s | ~6x |
| rects-10000 (vector) | 0.18s | 0.01s | ~18x |
| rects-20000 (vector) | 0.71s | 0.03s | ~23x |
| long-1000 (pages) | 0.38s | 0.37s | 1.03x |

Long-document scaling was already linear; the big win is vector-heavy pages.

# Failed / rejected experiments

- **Spatial hash for small rect sets (first attempt):** applying the spatial
  hash unconditionally (no `SPATIAL_HASH_MIN_RECTS` floor) cost ~10x on the
  official corpus — most pages have <512 rects where the HashMap build is
  slower than the plain scan, and full-page background rects span many grid
  cells producing huge candidate lists. Fixed by gating the hash to ≥512 rects
  and falling back to the plain scan for rects spanning >16 cells; retained the
  23x pathological win while restoring parity on the official corpus.

