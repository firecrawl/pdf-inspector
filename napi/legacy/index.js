'use strict'

const scoped = require('@firecrawl/pdf-inspector')

module.exports = scoped
module.exports.classifyPdf = scoped.classifyPdf
module.exports.detectPdf = scoped.detectPdf
module.exports.detectVectorGridInRegion = scoped.detectVectorGridInRegion
module.exports.extractPagesMarkdown = scoped.extractPagesMarkdown
module.exports.extractTablesInRegions = scoped.extractTablesInRegions
module.exports.extractTablesWithStructure = scoped.extractTablesWithStructure
module.exports.extractTablesWithStructureAuto = scoped.extractTablesWithStructureAuto
module.exports.extractTablesWithStructureCells = scoped.extractTablesWithStructureCells
module.exports.extractText = scoped.extractText
module.exports.extractTextInRegions = scoped.extractTextInRegions
module.exports.extractTextWithPositions = scoped.extractTextWithPositions
module.exports.ItemType = scoped.ItemType
module.exports.PdfType = scoped.PdfType
module.exports.processPdf = scoped.processPdf

