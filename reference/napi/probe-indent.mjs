import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { detectVectorGridInRegion } = require("./index.js");

const pdfPath =
  process.argv[2] ?? "/tmp/pdf_inspector_indent_fixtures/cis_edge_benchmark.pdf";
const pdf = readFileSync(pdfPath);
const dpi = Number(process.argv[3] ?? 200);

const crops = [
  { pageIdx: 29, box: [0, 0, 612, 792], label: "page30-full" },
  { pageIdx: 16, box: [0, 0, 612, 792], label: "page17-full" },
  { pageIdx: 23, box: [0, 0, 612, 792], label: "page24-full" },
];

for (const { pageIdx, box, label } of crops) {
  const result = detectVectorGridInRegion(pdf, pageIdx, box, dpi);
  if (!result) {
    console.log(`${label}: null`);
    continue;
  }
  const rows = result.structureTokens.filter((token) => token === "<tr>").length;
  const cols = rows > 0 ? result.cellBboxes.length / rows : 0;
  console.log(
    `${label}: cells=${result.cellBboxes.length} rows=${rows} cols=${cols}`,
  );
}
