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
