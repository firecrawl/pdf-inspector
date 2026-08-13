# Evidence: real-world logical-order RTL via `TJ` positioning

This folder holds a real production PDF used to validate the fix in #303
and to check the assumption both #331 and #334 make: that PDF text-showing
operators always paint left-to-right by natural glyph advance, and RTL text
must therefore always be stored character-reversed to render correctly.

## The file

`rtl_logical_order_gov_form.pdf` is a real Israel National Insurance
Institute (Bituach Leumi) account letter, shared and approved for this use
by its owner. Personal fields (name, address, ID numbers) were redacted by
deleting the corresponding glyph-showing operators from the content
stream outright, not replaced with placeholder characters. Everything
else — layout, fonts, dates, government branch details, and the two
account amounts — is untouched, since those are exactly what demonstrate
the construct.

## What it shows

The Hebrew in this document is stored in the content stream in ordinary
logical (first-letter-first) order. It renders correctly right-to-left
purely because its `TJ` operators walk the pen leftward between glyphs.

Per the `TJ` operand semantics (ISO 32000-1 §9.4.3 — number-then-string
pairs; each number is subtracted from the pen position before the next
glyph paints), a producer can lay out RTL text with unreversed glyph order
by supplying the right adjustments. This file does exactly that. Pulling
the font's actual `/W` glyph widths and a run's adjustments:

| prev glyph (width) | next glyph (width) | adjustment | w_prev + w_next | net step |
|---|---|---|---|---|
| 0x00f7 (658) | 0x00f5 (260) | 917.97 | 918 | **-259.97** |
| 0x00f5 (260) | 0x00fc (530) | 789.55 | 790 | **-529.55** |
| 0x00fc (530) | 0x00f9 (260) | 789.55 | 790 | **-259.55** |

Each adjustment equals `w(prev) + w(next)`, so the net step is exactly
`-w(next)`: every glyph origin lands one glyph-width to the left of the
one before it, with the character sequence never reversed.

`tests/fixtures/rtl_logical_order.pdf` (see `tests/integration_tests.rs`,
`fixture_logical_order_rtl_pdf_is_left_untouched` and
`synthetic_rtl_pdf_pages_position_adjusted`) is a 2 KB synthetic reduction
of this exact construct, built for the test suite so the regression does
not depend on a real document. This file is the proof the construct is
not hypothetical: a real government PDF generator produces it.

## Extraction comparison

Final-form Hebrew letters (ך ם ן ף ץ) are legal only as the last letter of
a word, which gives a cheap objective signal for "is this text backward"
without needing to read Hebrew:

| Build | words starting with a final-form letter | words ending with one | verdict |
|---|---|---|---|
| `main` (as of this writing) | 0 | 45 | correct |
| #303 | 0 | 45 | correct |
| #331 | 45 | 0 | every word reversed |
| #334 | 45 | 0 | every word reversed |

`main` already extracts this file correctly. #331 and #334 are each a
regression against it on this document, not merely "worse than #303" on a
hard case.

Read directly rather than through the heuristic, #303's output opens:

> בשנת הכספים 2025 נרשמו בחשבונך בביטוח לאומי (כולל קיזוזים מגמלאות)
> הסכומים הבאים:

#331 and #334 both produce the character-reversed version of the same
line.

## Why both PRs get this document wrong

Both make the reverse/don't-reverse decision from the extracted text
string alone, after per-glyph position data has already been discarded:

- #334's `normalize_show_text` (`src/text_utils.rs`) routes any non-ASCII
  string without Arabic presentation forms into `restore_item_text`
  (`src/rtl.rs`), which reorders it unconditionally.
- #331's `merge_text_items` (`src/extractor/mod.rs`) computes one
  document-wide `dominant_direction` and applies `visual_to_logical`
  (`src/bidi_order.rs`) to every item once that direction is RTL, with no
  per-item or per-producer escape hatch.

Neither has a code path that looks at glyph advances or `TJ` adjustments
before deciding whether to reorder. #331's own module doc comment states
the premise directly: PDF text-showing operators always paint left to
right by glyph width, so RTL text must always be stored visually
reversed. This document is a direct counter-example to that premise.

#303 avoids the failure without reading position data either — it checks
an orthographic property of the *extracted characters themselves*
(`VisualOrderEvidence` in `src/text_utils.rs`: are Hebrew words spelled
correctly, final forms only at word-end), which is invariant to whichever
layout mechanism the producer used. Because this document's underlying
character sequence is genuinely in logical order, the evidence check
correctly reports "not reversed," regardless of how `TJ` laid it out
visually.
