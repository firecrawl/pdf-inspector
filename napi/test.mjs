import { readFileSync } from 'fs';
import { strict as assert } from 'assert';
import {
  processPdf,
  processPdfAsync,
  processPdfWithOcr,
  detectPdf,
  classifyPdf,
  classifyPdfAsync,
  extractText,
  extractTextWithPositions,
  extractTextWithPositionsAndRotations,
  extractStructureElements,
  extractTextInRegions,
  extractTablesInRegions,
  detectVectorGridInRegion,
  extractPagesMarkdown,
  extractPagesMarkdownAsync,
} from './index.js';

const fixture = readFileSync('../tests/fixtures/thermo-freon12.pdf');
const taggedFixture = readFileSync('../tests/fixtures/firecrawl_docs_tagged.pdf');

// --- processPdf ---
console.log('Testing processPdf...');
const result = processPdf(fixture);
assert.equal(result.pdfType, 'TextBased');
assert.equal(result.pageCount, 3);
assert.ok(result.confidence > 0);
assert.ok(result.markdown && result.markdown.length > 0);
assert.equal(typeof result.isComplexLayout, 'boolean');
assert.ok(Array.isArray(result.pagesWithTables));
assert.ok(Array.isArray(result.pagesWithColumns));
assert.equal(typeof result.hasEncodingIssues, 'boolean');
console.log('  processPdf: OK');

// processPdf with pages
const result2 = processPdf(fixture, [1]);
assert.ok(result2.markdown && result2.markdown.length > 0);
console.log('  processPdf with pages: OK');

// --- detectPdf ---
console.log('Testing detectPdf...');
const detected = detectPdf(fixture);
assert.equal(detected.pdfType, 'TextBased');
assert.equal(detected.pageCount, 3);
assert.equal(detected.markdown, undefined);
console.log('  detectPdf: OK');

// --- classifyPdf ---
console.log('Testing classifyPdf...');
const classified = classifyPdf(fixture);
assert.equal(classified.pdfType, 'TextBased');
assert.equal(classified.pageCount, 3);
assert.ok(classified.confidence > 0);
assert.ok(Array.isArray(classified.pagesNeedingOcr));
console.log('  classifyPdf: OK');

// --- extractText ---
console.log('Testing extractText...');
const text = extractText(fixture);
assert.equal(typeof text, 'string');
assert.ok(text.length > 0);
console.log('  extractText: OK');

// --- extractTextWithPositions ---
console.log('Testing extractTextWithPositions...');
const items = extractTextWithPositions(fixture);
assert.ok(items.length > 0);
const item = items[0];
assert.equal(typeof item.text, 'string');
assert.equal(typeof item.x, 'number');
assert.equal(typeof item.y, 'number');
assert.equal(typeof item.width, 'number');
assert.equal(typeof item.height, 'number');
assert.equal(typeof item.rotation, 'number');
assert.equal(typeof item.advanceKnown, 'boolean');
assert.equal(typeof item.font, 'string');
assert.equal(typeof item.fontSize, 'number');
assert.equal(typeof item.page, 'number');
assert.equal(typeof item.isBold, 'boolean');
assert.equal(typeof item.isItalic, 'boolean');
assert.equal(typeof item.itemType, 'string');
console.log('  extractTextWithPositions: OK');

// with pages filter
const page1Items = extractTextWithPositions(fixture, [1]);
assert.ok(page1Items.length > 0);
assert.ok(page1Items.every(i => i.page === 1));
console.log('  extractTextWithPositions with pages: OK');

// mcid: undefined on untagged PDFs, numeric on tagged marked content
assert.ok(items.every(i => i.mcid === undefined || typeof i.mcid === 'number'));
const taggedItems = extractTextWithPositions(taggedFixture);
assert.ok(
  taggedItems.some(i => typeof i.mcid === 'number'),
  'tagged PDF text items should carry Marked Content IDs',
);
console.log('  extractTextWithPositions mcid: OK');

// rotation: a 90° margin stamp keeps a tall, thin axis-aligned box instead of
// collapsing to width 0, and reports its baseline angle
const rotatedFixture = readFileSync('../tests/fixtures/rotated_margin_stamp.pdf');
const rotatedItems = extractTextWithPositions(rotatedFixture);
const stamp = rotatedItems.find(i => i.text.startsWith('arXiv:'));
assert.ok(stamp, 'rotated stamp item should be extracted');
assert.ok(Math.abs(stamp.rotation - 90) < 1e-3, `stamp rotation ${stamp.rotation}`);
assert.ok(
  stamp.height > 10 * stamp.width,
  `stamp box should be tall and thin, got ${stamp.width}x${stamp.height}`,
);
assert.ok(
  rotatedItems.every(i => i.text.trim() === '' || i.width > 0),
  'no run with glyphs may be zero-width',
);
assert.ok(rotatedItems.filter(i => !i.text.startsWith('arXiv:')).every(i => i.rotation === 0));
assert.ok(rotatedItems.every(i => i.advanceKnown === true), 'Helvetica carries metrics for every run');
console.log('  extractTextWithPositions rotation: OK');

// the stamp belongs to the margin box only, never to the body paragraph
const stampRegions = extractTextInRegions(rotatedFixture, [
  { page: 0, regions: [[0, 0, 50, 792], [60, 0, 612, 792]] },
]);
assert.equal(stampRegions[0].regions[0].text.trim(), 'arXiv:2301.00001v1 [cs.CL] 1 Jan 2023');
assert.ok(!stampRegions[0].regions[1].text.includes('arXiv'), 'stamp leaked into body region');
assert.ok(stampRegions[0].regions[1].text.includes('The quick brown fox'));
console.log('  extractTextInRegions rotated margin run: OK');

// page frames: an upright page reports none; a page whose text is rotated
// 90° counter-clockwise is re-based and reported as 'ccw'
const upright = extractTextWithPositionsAndRotations(fixture);
assert.ok(upright.items.length > 0);
assert.deepEqual(upright.pageRotations, []);
const rotatedPageFixture = readFileSync('../tests/fixtures/tnagriculture_06_12.pdf');
const turned = extractTextWithPositionsAndRotations(rotatedPageFixture);
assert.ok(turned.items.length > 0);
assert.deepEqual(turned.pageRotations, [{ page: 1, rotation: 'ccw' }]);
assert.ok(turned.items.every(i => i.page !== 1 || i.rotation === 0 || i.rotation === 270));
console.log('  extractTextWithPositionsAndRotations: OK');

// --- extractStructureElements ---
console.log('Testing extractStructureElements...');
const structureElements = extractStructureElements(taggedFixture);
assert.ok(structureElements.length > 0);
assert.ok(structureElements.every(e => typeof e.page === 'number'));
assert.ok(structureElements.every(e => typeof e.mcid === 'number'));
assert.ok(structureElements.every(e => typeof e.role === 'string' && e.role.length > 0));
assert.ok(
  structureElements.some(e => e.role === 'H1'),
  'tagged fixture should surface H1 heading roles',
);

// (page, mcid) joins against extractTextWithPositions to recover heading text
const h1Refs = new Set(
  structureElements.filter(e => e.role === 'H1').map(e => `${e.page}:${e.mcid}`),
);
const h1Text = taggedItems
  .filter(i => typeof i.mcid === 'number' && h1Refs.has(`${i.page}:${i.mcid}`))
  .map(i => i.text)
  .join('');
assert.ok(h1Text.trim().length > 0, 'H1 join should recover heading text');

// pages filter is 1-indexed, matching TextItem.page
const page1Elements = extractStructureElements(taggedFixture, [1]);
assert.ok(page1Elements.length > 0);
assert.ok(page1Elements.every(e => e.page === 1));

// untagged PDFs yield an empty array
assert.deepEqual(extractStructureElements(fixture), []);
console.log('  extractStructureElements: OK');

// --- extractTextInRegions ---
console.log('Testing extractTextInRegions...');
const regionResults = extractTextInRegions(fixture, [
  { page: 0, regions: [[0, 0, 600, 100]] },
]);
assert.equal(regionResults.length, 1);
assert.equal(regionResults[0].page, 0);
assert.equal(regionResults[0].regions.length, 1);
assert.equal(typeof regionResults[0].regions[0].text, 'string');
assert.equal(typeof regionResults[0].regions[0].needsOcr, 'boolean');
console.log('  extractTextInRegions: OK');

// --- coordinate frame: positions and regions share the visible page box ---
console.log('Testing visible page box coordinate frame...');
// MediaBox [0 0 400 500], CropBox [50 60 350 460]; the glyph is written at
// raw (120, 300), so a CropBox render puts it at (70, 240) from the box's
// lower-left corner.
const cropFixture = readFileSync('../tests/fixtures/cropbox_offset_origin.pdf');
const cropItems = extractTextWithPositions(cropFixture);
const glyph = cropItems.find(i => i.text.trim() === 'Visible glyph');
assert.ok(glyph, 'fixture glyph should be extracted');
assert.ok(Math.abs(glyph.x - 70) < 0.01, `glyph.x should be 70, got ${glyph.x}`);
assert.ok(Math.abs(glyph.y - 240) < 0.01, `glyph.y should be 240, got ${glyph.y}`);
// The region API reads the same frame: the glyph's own box in the visible
// box's top-left space (300 x 400) yields exactly that line.
const visibleHeight = 400;
const glyphRegion = extractTextInRegions(cropFixture, [
  {
    page: 0,
    regions: [[
      glyph.x,
      visibleHeight - glyph.y - glyph.height,
      glyph.x + glyph.width,
      visibleHeight - glyph.y,
    ]],
  },
]);
const glyphText = glyphRegion[0].regions[0].text;
assert.ok(glyphText.includes('Visible glyph'), `region should hold the glyph, got ${glyphText}`);
assert.ok(!glyphText.includes('Second line'), `region should not spill, got ${glyphText}`);
console.log('  visible page box frame: OK');

// --- display frame: positions and regions on the rendered page ---
console.log('Testing display frame...');

// One-page PDF with Helvetica text; `rotate` becomes the page's /Rotate.
function syntheticPdf(content, rotate) {
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]${rotate === undefined ? '' : ` /Rotate ${rotate}`} /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>`,
    `<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}\nendstream`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
  let pdf = '%PDF-1.4\n';
  const offsets = [];
  objects.forEach((body, index) => {
    offsets.push(Buffer.byteLength(pdf));
    pdf += `${index + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = Buffer.byteLength(pdf);
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) pdf += `${String(offset).padStart(10, '0')} 00000 n \n`;
  pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF`;
  return Buffer.from(pdf, 'latin1');
}
const close = (actual, expected, what) =>
  assert.ok(Math.abs(actual - expected) < 0.75, `${what}: expected ${actual} to be close to ${expected}`);

// Without a /Rotate both frames agree, and the default is the sheet frame.
const uprightPdf = syntheticPdf('BT /F1 12 Tf 72 700 Td (Anchor) Tj ET\nBT /F1 12 Tf 72 680 Td (Second) Tj ET');
const uprightSheet = extractTextWithPositions(uprightPdf);
const uprightExplicit = extractTextWithPositions(uprightPdf, undefined, { frame: 'sheet' });
const uprightDisplay = extractTextWithPositions(uprightPdf, undefined, { frame: 'display' });
assert.deepEqual(uprightExplicit, uprightSheet);
assert.deepEqual(uprightDisplay, uprightSheet);
const uprightAnchor = uprightSheet.find(i => i.text.trim() === 'Anchor');
close(uprightAnchor.x, 72, 'anchor.x');
close(uprightAnchor.y, 700, 'anchor.y');
assert.throws(
  () => extractTextWithPositions(uprightPdf, undefined, { frame: 'rendered' }),
  /unknown frame "rendered"/,
);
assert.throws(() => extractTextInRegions(uprightPdf, [], { frame: 'page' }), /unknown frame "page"/);
console.log('  display frame defaults and validation: OK');

// Two lines reading bottom-to-top on a page whose /Rotate 90 displays them
// upright: the sheet frame turns the page (reported as 'ccw'), the display
// frame puts each line where a renderer draws it on the 792 x 612 page.
const sidewaysPdf = syntheticPdf(
  'BT /F1 12 Tf 0 1 -1 0 40 420 Tm (HELLO) Tj ET\nBT /F1 12 Tf 0 1 -1 0 70 420 Tm (WORLD) Tj ET',
  90,
);
const sidewaysSheet = extractTextWithPositionsAndRotations(sidewaysPdf);
assert.deepEqual(sidewaysSheet.pageRotations, [{ page: 1, rotation: 'ccw' }]);
const sidewaysDisplay = extractTextWithPositionsAndRotations(sidewaysPdf, [1], { frame: 'display' });
assert.deepEqual(sidewaysDisplay.pageRotations, [{ page: 1, rotation: 'ccw' }]);
const hello = sidewaysDisplay.items.find(i => i.text.trim() === 'HELLO');
const world = sidewaysDisplay.items.find(i => i.text.trim() === 'WORLD');
assert.ok(hello && world, 'both lines should be extracted');
close(hello.x, 420, 'hello.x');
close(hello.y, 612 - 40, 'hello.y');
close(hello.height, 12, 'hello.height');
assert.equal(hello.rotation, 0);
close(world.x, 420, 'world.x');
close(world.y, 612 - 70, 'world.y');
assert.ok(hello.y > world.y, 'HELLO renders above WORLD');
assert.deepEqual(
  extractTextWithPositionsAndRotations(sidewaysPdf, [2], { frame: 'display' }),
  { items: [], pageRotations: [] },
);

// Region bboxes on the rendered page (top-left origin) pick exactly the line
// they cover: HELLO occupies y ∈ [28, 40], WORLD y ∈ [58, 70].
const sidewaysRegions = extractTextInRegions(
  sidewaysPdf,
  [{ page: 0, regions: [[400, 20, 700, 45], [400, 55, 700, 75]] }],
  { frame: 'display' },
);
assert.equal(sidewaysRegions[0].regions[0].text.trim(), 'HELLO');
assert.equal(sidewaysRegions[0].regions[1].text.trim(), 'WORLD');
// The same bboxes read in the default sheet frame land on empty paper.
const sidewaysSheetRegions = extractTextInRegions(sidewaysPdf, [
  { page: 0, regions: [[400, 20, 700, 45]] },
]);
assert.equal(sidewaysSheetRegions[0].regions[0].text.trim(), '');

// Tables take the same option: a grid on a sideways page reads identically
// through a sheet-frame bbox and through the matching display-frame bbox.
const gridPdf = syntheticPdf(
  [
    ['Name', 'Qty', 'Price'],
    ['Apple', '3', '1.50'],
    ['Pear', '5', '2.25'],
  ]
    .flatMap((row, r) => row.map((cell, c) => `BT /F1 12 Tf ${[72, 200, 330][c]} ${700 - 20 * r} Td (${cell}) Tj ET`))
    .join('\n'),
  90,
);
const gridFromSheet = extractTablesInRegions(gridPdf, [{ page: 0, regions: [[60, 80, 400, 137]] }]);
const gridFromDisplay = extractTablesInRegions(
  gridPdf,
  [{ page: 0, regions: [[792 - 137, 60, 792 - 80, 400]] }],
  { frame: 'display' },
);
assert.equal(gridFromSheet[0].regions[0].text, '|Name|Qty|Price|\n|---|---|---|\n|Apple|3|1.50|\n|Pear|5|2.25|\n');
assert.deepEqual(gridFromDisplay, gridFromSheet);
console.log('  display frame positions and regions: OK');

// --- detectVectorGridInRegion ---
console.log('Testing detectVectorGridInRegion...');
const vectorGrid = detectVectorGridInRegion(fixture, 0, [0, 0, 600, 800], 72);
assert.ok(vectorGrid === null || typeof vectorGrid === 'object');
if (vectorGrid) {
  assert.ok(Array.isArray(vectorGrid.structureTokens));
  assert.ok(Array.isArray(vectorGrid.cellBboxes));
  assert.ok(vectorGrid.cellBboxes.every(bbox => Array.isArray(bbox) && bbox.length === 4));
}
console.log('  detectVectorGridInRegion: OK');

// --- extractPagesMarkdown ---
console.log('Testing extractPagesMarkdown...');

// omit pages → every page in document order
const allPages = extractPagesMarkdown(fixture);
assert.equal(allPages.pages.length, 3);
assert.deepEqual(allPages.pages.map(p => p.page), [0, 1, 2]);
assert.ok(typeof allPages.pages[0].markdown === 'string');
assert.equal(typeof allPages.pages[0].needsOcr, 'boolean');
assert.ok(Array.isArray(allPages.pagesWithTables));
assert.ok(Array.isArray(allPages.pagesWithColumns));
assert.ok(Array.isArray(allPages.pagesNeedingOcr));
assert.equal(typeof allPages.isComplex, 'boolean');
console.log('  extractPagesMarkdown (no pages arg): OK');

// selected pages preserve caller order
const picked = extractPagesMarkdown(fixture, [2, 0]);
assert.equal(picked.pages.length, 2);
assert.equal(picked.pages[0].page, 2);
assert.equal(picked.pages[1].page, 0);
console.log('  extractPagesMarkdown with pages: OK');

// --- Async variants ---
console.log('Testing async variants...');

// processPdfAsync returns a promise and matches the sync result
const asyncResultPromise = processPdfAsync(fixture);
assert.ok(asyncResultPromise instanceof Promise);
const asyncResult = await asyncResultPromise;
assert.equal(asyncResult.pdfType, result.pdfType);
assert.equal(asyncResult.pageCount, result.pageCount);
assert.equal(asyncResult.markdown, result.markdown);
console.log('  processPdfAsync: OK');

// processPdfAsync with pages
const asyncResult2 = await processPdfAsync(fixture, [1]);
assert.equal(asyncResult2.markdown, result2.markdown);
console.log('  processPdfAsync with pages: OK');

// classifyPdfAsync matches the sync result
const asyncClassified = await classifyPdfAsync(fixture);
assert.equal(asyncClassified.pdfType, classified.pdfType);
assert.equal(asyncClassified.pageCount, classified.pageCount);
assert.equal(asyncClassified.confidence, classified.confidence);
assert.deepEqual(asyncClassified.pagesNeedingOcr, classified.pagesNeedingOcr);
console.log('  classifyPdfAsync: OK');

// extractPagesMarkdownAsync matches the sync result
const asyncAllPages = await extractPagesMarkdownAsync(fixture);
assert.equal(asyncAllPages.pages.length, allPages.pages.length);
assert.deepEqual(
  asyncAllPages.pages.map(p => p.markdown),
  allPages.pages.map(p => p.markdown),
);
assert.equal(asyncAllPages.isComplex, allPages.isComplex);
console.log('  extractPagesMarkdownAsync: OK');

// selected pages preserve caller order
const asyncPicked = await extractPagesMarkdownAsync(fixture, [2, 0]);
assert.equal(asyncPicked.pages.length, 2);
assert.equal(asyncPicked.pages[0].page, 2);
assert.equal(asyncPicked.pages[1].page, 0);
console.log('  extractPagesMarkdownAsync with pages: OK');

// input buffer is copied at call time: mutating it immediately after the
// call must not affect the in-flight parse
const scratch = Buffer.from(fixture);
const inFlight = processPdfAsync(scratch);
scratch.fill(0);
const fromMutated = await inFlight;
assert.equal(fromMutated.markdown, result.markdown);
console.log('  processPdfAsync input copied at call time: OK');

// --- Selective OCR ---
console.log('Testing processPdfWithOcr...');

// Off exercises the complete result/provenance contract without loading
// external PDFium, ONNX Runtime, or model artifacts.
const ocrOff = await processPdfWithOcr(fixture, { mode: 'Off' });
assert.equal(ocrOff.pageCount, 3);
assert.equal(ocrOff.pages.length, 3);
assert.deepEqual(ocrOff.pagesRoutedToOcr, []);
assert.ok(ocrOff.pages.every(page => page.provenance.source === 'Native'));
assert.ok(ocrOff.pages.every(page => page.provenance.ocrModel === undefined));
assert.ok(ocrOff.markdown.length > 0);

// Auto must preserve the lightweight path for clean text PDFs.
const ocrAuto = await processPdfWithOcr(fixture);
assert.deepEqual(ocrAuto.pagesRoutedToOcr, []);
assert.equal(ocrAuto.renderTimeMs, 0);
assert.equal(ocrAuto.ocrTimeMs, 0);

const ocrSelected = await processPdfWithOcr(fixture, {
  mode: 'Off',
  pageNumbers: [2],
});
assert.deepEqual(ocrSelected.pages.map(page => page.pageNumber), [2]);

await assert.rejects(
  processPdfWithOcr(fixture, { mode: 'Off', pageNumbers: [0] }),
  /page 0/,
);
console.log('  processPdfWithOcr: OK');

// concurrent async calls all settle
const [c1, c2, c3] = await Promise.all([
  processPdfAsync(fixture),
  classifyPdfAsync(fixture),
  extractPagesMarkdownAsync(fixture),
]);
assert.equal(c1.pdfType, 'TextBased');
assert.equal(c2.pdfType, 'TextBased');
assert.equal(c3.pages.length, 3);
console.log('  concurrent async calls: OK');

// --- Error handling ---
console.log('Testing error handling...');
assert.throws(() => processPdf(Buffer.from('not a pdf')), /process_pdf/);
assert.throws(() => classifyPdf(Buffer.from('')), /classify_pdf/);
await assert.rejects(processPdfAsync(Buffer.from('not a pdf')), /process_pdf/);
await assert.rejects(classifyPdfAsync(Buffer.from('')), /classify_pdf/);
await assert.rejects(extractPagesMarkdownAsync(Buffer.from('')), /extract_pages_markdown/);
console.log('  error handling: OK');

console.log('\nAll NAPI tests passed!');
