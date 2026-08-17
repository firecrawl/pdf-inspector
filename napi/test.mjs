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
  extractStructureElements,
  extractTextInRegions,
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
