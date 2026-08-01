import { readFileSync } from 'fs';
import { strict as assert } from 'assert';
import {
  processPdf,
  detectPdf,
  classifyPdf,
  extractText,
  extractTextWithPositions,
  extractTextInRegions,
  detectVectorGridInRegion,
  extractPagesMarkdown,
} from './index.js';

const fixture = readFileSync('../tests/fixtures/thermo-freon12.pdf');

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

// --- Error handling ---
console.log('Testing error handling...');
assert.throws(() => processPdf(Buffer.from('not a pdf')), /process_pdf/);
assert.throws(() => classifyPdf(Buffer.from('')), /classify_pdf/);
console.log('  error handling: OK');

console.log('\nAll NAPI tests passed!');
