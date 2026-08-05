# Residual Review Findings

Run context:

- Run: ce-code-review 20260806-025419-eb4dedaf (LFG pipeline step 4)
- Branch: `feat/expose-detector-signals`
- Head: `cf0fe17`
- Plan: `docs/plans/2026-08-06-001-feat-expose-detector-signals-plan.md`
- Verdict: Ready with fixes; all substantive findings applied during review and validated (5/5); one actionable finding deferred.

## Findings

- **P2 - src/lib.rs:193 - Scanned/ImageBased `page_signals` empty case untested** - filed as https://github.com/firecrawl/pdf-inspector/issues/276. The Full-mode early return for Scanned/ImageBased PDFs produces an empty `page_signals` (documented contract), but no image-backed fixture exists to exercise the branch; the only empty-page_signals assertion is the DetectOnly test. Suggested fix: add a scanned/image-backed fixture test asserting `page_signals == []` in Full mode, or document the coverage gap.

## Report-only items (no ticket filed - advisory/release-owned)

- **P2 - napi/src/lib.rs:624** - napi-generated `index.d.ts` marks new fields required; TS construction/mock consumers break at compile time. Accepted per plan A6 (semver-minor). Release notes + regenerated `index.d.ts` via `napi prepublish`.
- **P2 - src/lib.rs:197** - Rust struct-literal construction of `PageMarkdown`/`PdfProcessResult` breaks at compile time. Accepted per plan A6 (semver-minor). Changelog note.
- **P2 - wasm/src/lib.rs:31** - wasm TS narrows `PageOcrReasons.reasons` to `OcrReason[]` (deliberate KTD5/G9); napi keeps `string[]` (cross-surface asymmetry). Release-note the narrowing.
- **P2 - napi/src/lib.rs:167 / docs/rust-api.md** - `0.15` is inexact after f32->f64 widening on Python/napi/wasm (0.15000000596046448); compare with `abs(diff) < 1e-3`. Documented.
- **P3 - src/text_quality.rs:391 / docs/rust-api.md** - run-length-rule short pages flag `needs_ocr` at confidence up to ~0.75; bands describe replacement density. Documented.
