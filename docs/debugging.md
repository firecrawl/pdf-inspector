# Debugging with RUST_LOG

Structured logging via `RUST_LOG` replaces the former debug binaries. Set the environment variable to control which sections emit debug output on stderr:

```bash
# Raw PDF content stream operators (replaces dump_ops)
RUST_LOG=pdf_inspector::extractor::content_stream=trace cargo run --bin pdf2md -- file.pdf > /dev/null

# Font metadata, encodings, ligatures (replaces debug_fonts / debug_ligatures)
RUST_LOG=pdf_inspector::extractor::fonts=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# ToUnicode CMap parsing
RUST_LOG=pdf_inspector::tounicode=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# Text items per page with x/y/width (replaces debug_spaces / debug_pages)
RUST_LOG=pdf_inspector::extractor=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# Column detection and reading order (replaces debug_order)
RUST_LOG=pdf_inspector::extractor::layout=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# Y-gap analysis and paragraph thresholds (replaces debug_ygaps)
RUST_LOG=pdf_inspector::markdown::analysis=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# Table detection
RUST_LOG=pdf_inspector::tables=debug cargo run --bin pdf2md -- file.pdf > /dev/null

# Everything
RUST_LOG=pdf_inspector=debug cargo run --bin pdf2md -- file.pdf > /dev/null
```

## Debug shifted cells in a tagged PDF table

A tagged PDF contains hidden labels for rows and cells. A blank cell can look right on the page even when its label is missing. If the extractor places the remaining cells in order and adds blanks at the end, the balance can move into the Deposits column. The wider print of the example statement showed this on its first transaction.

| First transaction | Withdrawals | Deposits | Balance |
| --- | ---: | ---: | ---: |
| Printed statement | 2,675.12 | | 15,777.25 |
| Incorrect extraction | 2,675.12 | 15,777.25 | |
| Correct extraction | 2,675.12 | | 15,777.25 |

Run the parser on the [synthetic Chrome PDF](../tests/fixtures/chrome_compact_tagged_statement.pdf) from the repository root. The fixture contains two fictional transactions, one withdrawal and one deposit.

```bash
cargo run --bin pdf2md -- tests/fixtures/chrome_compact_tagged_statement.pdf statement.md
```

`statement.md` should show a blank Deposits cell for `LOAN PMT` and a blank Withdrawals cell for `MOBILE DEPOSIT`. Both balances should appear in the last column.

```text
|Date|Description|Withdrawals|Deposits|Balance|
|---|---|---|---|---|
|10/01|LOAN PMT|2,675.12||15,777.25|
|10/06|MOBILE DEPOSIT||6,146.48|17,352.24|
```

Check the Rust integration test if the columns move or nearby cells are joined into one text run.

```bash
cargo test --test integration_tests compact_tagged_statement_preserves_empty_cells
```

For other PDFs, set `RUST_LOG=pdf_inspector::tables=debug` to see how many cells could be matched to the original text. The parser uses the positions of a complete tagged header to restore missing cells when the match is clear. An explicit blank tag reserves a column while the surrounding values are aligned.
