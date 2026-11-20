# @firecrawl/pdf-inspector-wasm

Browser WebAssembly bindings for [pdf-inspector](https://github.com/firecrawl/pdf-inspector). Classify PDFs and extract structured Markdown locally from a `Uint8Array`, using the same Rust core as the native Node.js, Python, and Rust packages.

## Install

```bash
npm install @firecrawl/pdf-inspector-wasm
```

## Usage

```ts
import init, { processPdf } from "@firecrawl/pdf-inspector-wasm";

await init();

const response = await fetch("/annual-report.pdf");
const pdf = new Uint8Array(await response.arrayBuffer());
const result = processPdf(pdf);

console.log(result.pdfType);
console.log(result.markdown);
```

Pass options when you need selected pages or compact Markdown:

```ts
const result = processPdf(pdf, {
  pages: [1, 3, 5],
  profile: "compact",
  includePageMarkers: true,
});
```

`processPdf` parses the whole file on every call. To process pages of one PDF repeatedly, open it once with `PdfDocument`:

```ts
import init, { PdfDocument } from "@firecrawl/pdf-inspector-wasm";

await init();

const doc = new PdfDocument(pdf); // pass a password as the second argument for encrypted PDFs
for (let page = 1; page <= doc.pageCount; page++) {
  console.log(doc.processPages({ pages: [page] }).markdown);
}
doc.free(); // release the parsed document
```

`processPages` takes the same options as `processPdf` (`password` is ignored: the document is already open). `pageCount`, the document type and the OCR page list still describe the whole document in every result.

The package also exports:

- `detectPdf(pdf, options?)` for detection without extraction.
- `classifyPdf(pdf)` for the lightweight result shape shared with the native Node.js API.
- `extractText(pdf)` for plain text.
- `version()` for the WASM package version.

## Browser behavior

- Parsing runs locally. PDF bytes are not uploaded anywhere.
- The build is single-threaded and does not require cross-origin isolation.
- CMaps are embedded so CJK font decoding does not depend on a filesystem.
- Extraction is synchronous after `init()`. For large documents, call it from a Web Worker to keep the UI responsive.
- Image-only documents still require a separate OCR step.

## Build from source

```bash
cargo install wasm-pack --version 0.15.0 --locked
wasm-pack build wasm --target web --scope firecrawl --release
```

## License

MIT
