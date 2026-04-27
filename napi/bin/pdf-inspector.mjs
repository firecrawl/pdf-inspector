#!/usr/bin/env node

import { readFileSync, writeFileSync } from "fs";
import { createRequire } from "module";

const require = createRequire(import.meta.url);
const { version } = require("../package.json");

const HELP = `pdf-inspector v${version} — Fast PDF text extraction to Markdown

Usage:
  pdf-inspector <file>                  Extract markdown (default)
  pdf-inspector detect <file>           Classify PDF type

Options:
  --json                  Output as JSON
  --pages <pages>         Comma-separated page numbers (e.g. 1,3,5)
  -o, --output <file>     Write output to file instead of stdout
  -h, --help              Show this help
  -v, --version           Show version

Examples:
  pdf-inspector document.pdf
  pdf-inspector document.pdf --json
  pdf-inspector document.pdf --pages 1,2,3
  pdf-inspector detect document.pdf --json
  cat document.pdf | pdf-inspector -`;

function die(msg) {
  process.stderr.write(`error: ${msg}\n`);
  process.exit(1);
}

function parseArgs(argv) {
  const opts = { json: false, pages: null, output: null, file: null, command: "extract" };
  let i = 0;

  // Check for subcommand
  if (argv[0] === "detect") {
    opts.command = "detect";
    i = 1;
  }

  while (i < argv.length) {
    const arg = argv[i];
    if (arg === "-h" || arg === "--help") {
      process.stdout.write(HELP + "\n");
      process.exit(0);
    } else if (arg === "-v" || arg === "--version") {
      process.stdout.write(`${version}\n`);
      process.exit(0);
    } else if (arg === "--json") {
      opts.json = true;
    } else if (arg === "--pages") {
      i++;
      if (!argv[i]) die("--pages requires a value (e.g. 1,3,5)");
      opts.pages = argv[i].split(",").map((p) => {
        const n = parseInt(p.trim(), 10);
        if (Number.isNaN(n) || n < 1) die(`invalid page number: ${p}`);
        return n;
      });
    } else if (arg === "-o" || arg === "--output") {
      i++;
      if (!argv[i]) die("-o requires a filename");
      opts.output = argv[i];
    } else if (arg === "-" || !arg.startsWith("-")) {
      if (opts.file) die(`unexpected argument: ${arg}`);
      opts.file = arg;
    } else {
      die(`unknown option: ${arg}`);
    }
    i++;
  }

  return opts;
}

function readInput(file) {
  if (file === "-") {
    return readFileSync(0); // stdin fd
  }
  try {
    return readFileSync(file);
  } catch (err) {
    if (err.code === "ENOENT") die(`file not found: ${file}`);
    die(err.message);
  }
}

function output(text, outputPath) {
  if (outputPath) {
    writeFileSync(outputPath, text);
  } else {
    process.stdout.write(text);
  }
}

// ---- main ----

const opts = parseArgs(process.argv.slice(2));

if (!opts.file) {
  // Check if stdin is piped
  if (process.stdin.isTTY !== false) {
    process.stderr.write(HELP + "\n");
    process.exit(1);
  }
  opts.file = "-";
}

const { processPdf, classifyPdf } = await import("../index.js");
const buffer = readInput(opts.file);

if (opts.command === "detect") {
  const result = classifyPdf(buffer);
  if (opts.json) {
    output(JSON.stringify(result, null, 2) + "\n", opts.output);
  } else {
    const ocr = result.pagesNeedingOcr.length > 0
      ? `, ${result.pagesNeedingOcr.length} pages need OCR`
      : "";
    output(`${result.pdfType} (${result.pageCount} pages, confidence: ${result.confidence.toFixed(2)}${ocr})\n`, opts.output);
  }
} else {
  const result = processPdf(buffer, opts.pages ?? undefined);
  if (opts.json) {
    output(JSON.stringify(result, null, 2) + "\n", opts.output);
  } else {
    output((result.markdown ?? "") + "\n", opts.output);
  }
}
