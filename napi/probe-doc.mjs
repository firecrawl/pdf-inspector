#!/usr/bin/env node
// Generalized one-shot probe: run detectVectorGridInRegion against a single
// PDF + region without spinning up the api or the bench.
//
// Usage:
//   node probe-doc.mjs <pdf-path> [page] [bbox] [dpi]
//
// pdf-path : path to a PDF on disk (required)
// page     : 0-indexed page number (default 0)
// bbox     : "x0,y0,x1,y1" in PDF points (default = full standard letter)
// dpi      : render dpi (default 200)
//
// Examples:
//   node probe-doc.mjs ~/Code/opendataloader-bench/pdfs/01030000000127.pdf 0
//   node probe-doc.mjs /tmp/foo.pdf 3 "50,100,560,700" 200
//   node probe-doc.mjs /tmp/foo.pdf       # page 0, full page, dpi 200
//
// Prints: cells / rows / cols / null. Useful for fast pdf-inspector loops:
//   cargo build --release && bun run build:debug && node probe-doc.mjs ...

import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { detectVectorGridInRegion } = require("./index.js");

const [, , pdfPath, pageArg, bboxArg, dpiArg] = process.argv;

if (!pdfPath) {
  console.error("usage: node probe-doc.mjs <pdf-path> [page] [bbox] [dpi]");
  process.exit(2);
}
if (!existsSync(pdfPath)) {
  console.error(`pdf not found: ${pdfPath}`);
  process.exit(2);
}

const page = pageArg !== undefined ? Number(pageArg) : 0;
const bbox = bboxArg
  ? bboxArg.split(",").map(Number)
  : [0, 0, 612, 792]; // standard US letter
const dpi = dpiArg !== undefined ? Number(dpiArg) : 200;

if (bbox.length !== 4 || bbox.some(Number.isNaN)) {
  console.error(`invalid bbox "${bboxArg}", expected "x0,y0,x1,y1"`);
  process.exit(2);
}

const pdf = readFileSync(pdfPath);

const t0 = Date.now();
const result = detectVectorGridInRegion(pdf, page, bbox, dpi);
const ms = Date.now() - t0;

if (!result) {
  console.log(`null (${ms}ms)  page=${page} bbox=${bbox.join(",")} dpi=${dpi}`);
  process.exit(0);
}

const rows = result.structureTokens.filter((t) => t === "<tr>").length;
const cols = rows > 0 ? result.cellBboxes.length / rows : 0;
console.log(
  `cells=${result.cellBboxes.length} rows=${rows} cols=${cols} (${ms}ms)  page=${page} bbox=${bbox.join(",")} dpi=${dpi}`,
);
