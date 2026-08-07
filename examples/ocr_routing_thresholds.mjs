#!/usr/bin/env node
// Minimal OCR routing example for pdf-inspector.
import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { classifyPdf, processPdf } from '../napi/index.js';

const here = dirname(fileURLToPath(import.meta.url));
const pdfPath = process.argv[2] || join(here, '..', 'tests', 'fixtures', 'thermo-freon12.pdf');
const buffer = readFileSync(pdfPath);

const thresholds = {
  textBasedMinConfidence: 0.85,
  needsOcrMaxConfidence: 0.35,
  mixedPageThreshold: 0.25,
};

let result;
try {
  result = classifyPdf(buffer, { thresholds });
} catch {
  result = processPdf(buffer);
}

const pdfType = result.pdfType || result.pdf_type || 'unknown';
const confidence = result.confidence;
const pages = result.pagesNeedingOcr || result.pages_needing_ocr || [];

console.log(`PDF: ${pdfPath}`);
console.log(`Document class: ${pdfType}`);
console.log(`Confidence: ${confidence}`);
console.log(`Pages needing OCR: ${JSON.stringify(pages)}`);
console.log(`Thresholds requested: ${JSON.stringify(thresholds)}`);
