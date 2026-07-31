# Language bindings

The Rust crate at the repository root owns PDF parsing, extraction, and Markdown conversion. Each directory here is a thin package adapter that translates the core API into the conventions of its runtime.

| Binding | Public package | Implementation |
|---|---|---|
| [Node.js](node/README.md) | `@firecrawl/pdf-inspector` | napi-rs |
| [Python](python/README.md) | `pdf-inspector` | PyO3 + maturin |
| [Browser WebAssembly](wasm/README.md) | `@firecrawl/pdf-inspector-wasm` | wasm-bindgen |

The public package names and APIs are independent from these repository paths. Binding-specific metadata, tests, examples, and release inputs live with their adapter; shared PDF fixtures remain in [`tests/fixtures`](../tests/fixtures).

Versions remain independent for now. A later release-policy change can align them without mixing that change into the structural reorganization.
