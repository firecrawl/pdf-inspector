// Ported from reference/src/lib.rs
using PdfInspector.Detector;
using PdfInspector.Markdown;
using PdfInspector.Quality;
using PdfInspector.Types;

namespace PdfInspector;

/// <summary>A fast classification of a PDF, with no text extraction.</summary>
public sealed class PdfClassification
{
    /// <summary>The detected PDF type.</summary>
    public required PdfType PdfType { get; init; }

    /// <summary>The document's page count.</summary>
    public required uint PageCount { get; init; }

    /// <summary>The 0-indexed pages that need OCR: the scanned or image ones.</summary>
    public List<uint> PagesNeedingOcr { get; init; } = [];

    /// <summary>Detection confidence, from 0.0 to 1.0.</summary>
    public required float Confidence { get; init; }
}

/// <summary>Markdown for one page, with the reliability of its text layer.</summary>
public sealed class PageMarkdown
{
    /// <summary>The 0-indexed page number.</summary>
    public required uint Page { get; init; }

    /// <summary>The formatted markdown for this page.</summary>
    public required string Markdown { get; init; }

    /// <summary>
    /// True when this page's text is unreliable: glyph-id-encoded fonts,
    /// encoding issues, garbage text, or an empty extraction.
    /// </summary>
    public required bool NeedsOcr { get; init; }

    /// <summary>A machine-readable OCR reason, when the cause is known.</summary>
    public string? OcrReason { get; init; }
}

/// <summary>Per-page markdown plus the document's layout classification.</summary>
public sealed class PagesExtractionResult
{
    /// <summary>The per-page markdown results.</summary>
    public List<PageMarkdown> Pages { get; init; } = [];

    /// <summary>The 1-indexed pages where tables were detected.</summary>
    public List<uint> PagesWithTables { get; init; } = [];

    /// <summary>The 1-indexed pages where a multi-column layout was detected.</summary>
    public List<uint> PagesWithColumns { get; init; } = [];

    /// <summary>The 1-indexed pages that need OCR.</summary>
    public List<uint> PagesNeedingOcr { get; init; } = [];

    /// <summary>Why each flagged page needs OCR.</summary>
    public List<PageOcrReasons> OcrReasonsByPage { get; init; } = [];

    /// <summary>True when any page has tables or columns.</summary>
    public required bool IsComplex { get; init; }
}

public static partial class PdfProcessor
{
    /// <summary>
    /// Classifies an in-memory PDF without extracting its text, returning the
    /// type and which pages need OCR.
    /// </summary>
    public static PdfClassification ClassifyPdfMem(byte[] buffer)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = LoadDocumentOrThrow(buffer, null);
        var pageCount = (uint)doc.PageCount;
        var detection = PdfDetector.DetectFromDocument(doc, pageCount, new DetectionConfig());

        return new PdfClassification
        {
            PdfType = detection.PdfType,
            PageCount = pageCount,

            // The public surface is 0-indexed, for caller convenience.
            PagesNeedingOcr = [.. detection.PagesNeedingOcr.Select(p => p - 1)],
            Confidence = detection.Confidence,
        };
    }

    /// <summary>
    /// Extracts formatted markdown per page, with layout-classification metadata.
    /// </summary>
    /// <param name="buffer">The PDF file bytes.</param>
    /// <param name="pages">
    /// The 0-indexed pages to return, in the caller's order. Null returns every
    /// page in document order.
    /// </param>
    /// <remarks>
    /// Unlike <see cref="ProcessPdfMem"/>, which returns one concatenated
    /// markdown string, this returns markdown per page so callers can mix direct
    /// extraction for simple text pages with OCR for complex or scanned ones.
    /// Font statistics are computed over the whole document, so heading
    /// thresholds stay consistent regardless of which pages were requested, and
    /// layout complexity comes free since the items are already in memory.
    /// </remarks>
    public static PagesExtractionResult ExtractPagesMarkdownMem(byte[] buffer, IReadOnlyList<uint>? pages = null)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = LoadDocumentOrThrow(buffer, null);
        var pageCount = (uint)doc.PageCount;
        var fontCMaps = ToUnicode.FontCMaps.FromDocument(doc);

        // Every page is extracted, so the font statistics are document-wide.
        var extraction = Extractor.TextExtractor.ExtractPositionedText(doc, fontCMaps, null);
        var allItems = extraction.Items;
        var textQuality = TextQuality.AnalyzeTextQuality(allItems);

        var complexity = LayoutAnalysis.ComputeLayoutComplexity(allItems, extraction.Rects, extraction.Lines);
        var fontStats = Analysis.CalculateFontStatsFromItems(allItems);

        var pagesSlice = pages ?? [.. Enumerable.Range(0, (int)pageCount).Select(i => (uint)i)];

        var results = new List<PageMarkdown>(pagesSlice.Count);
        var pagesNeedingOcr = new List<uint>();
        var ocrReasonsByPage = new SortedDictionary<uint, List<string>>();

        foreach (var page0Idx in pagesSlice)
        {
            // An out-of-range page yields empty markdown and needs OCR.
            if (page0Idx >= pageCount)
            {
                pagesNeedingOcr.Add(page0Idx + 1);
                results.Add(new PageMarkdown
                {
                    Page = page0Idx,
                    Markdown = string.Empty,
                    NeedsOcr = true,
                });
                continue;
            }

            var page1Idx = page0Idx + 1;
            var pageItems = allItems.Where(i => i.Page == page1Idx).Select(i => i.Clone()).ToList();
            var pageRects = extraction.Rects.Where(r => r.Page == page1Idx).Select(r => r.Clone()).ToList();

            var hasGid = extraction.GidEncodedPages.Contains(page1Idx);
            var hasTextQualityIssue = textQuality.PagesNeedingOcr.Contains(page1Idx);

            var options = new MarkdownOptions
            {
                BaseFontSize = fontStats.MostCommonSize,
                IncludePageNumbers = false,
                StripHeadersFooters = false,
            };

            var md = hasTextQualityIssue
                ? string.Empty
                : MarkdownConverter.ToMarkdownFromItemsWithRectsAndLines(
                    pageItems, options, pageRects, [], extraction.PageThresholds, null, []);

            var hasDecodingIssue = hasTextQualityIssue
                || (md.Length > 0 && (TextQuality.IsCidGarbage(md) || TextQuality.DetectEncodingIssues(md)));
            if (hasDecodingIssue)
            {
                if (!ocrReasonsByPage.TryGetValue(page1Idx, out var reasons))
                {
                    reasons = [];
                    ocrReasonsByPage[page1Idx] = reasons;
                }

                if (!reasons.Contains(OcrReason.SuspectedGarbledText))
                {
                    reasons.Add(OcrReason.SuspectedGarbledText);
                }
            }

            var ocrReason = ocrReasonsByPage.TryGetValue(page1Idx, out var pageReasons) && pageReasons.Count > 0
                ? pageReasons[0]
                : null;

            var needsOcr = ocrReason is not null
                || md.Trim().Length == 0
                || hasGid
                || TextQuality.IsGarbageText(md);

            if (needsOcr)
            {
                pagesNeedingOcr.Add(page1Idx);
            }

            results.Add(new PageMarkdown
            {
                Page = page0Idx,
                Markdown = needsOcr ? string.Empty : md,
                NeedsOcr = needsOcr,
                OcrReason = ocrReason,
            });
        }

        return new PagesExtractionResult
        {
            Pages = results,
            PagesWithTables = complexity.PagesWithTables,
            PagesWithColumns = complexity.PagesWithColumns,
            PagesNeedingOcr = pagesNeedingOcr,
            OcrReasonsByPage = ToPageOcrReasons(ocrReasonsByPage),
            IsComplex = complexity.IsComplex,
        };
    }

    /// <summary>Reads a PDF from disk and extracts markdown per page.</summary>
    public static PagesExtractionResult ExtractPagesMarkdown(string path, IReadOnlyList<uint>? pages = null)
    {
        Validation.ValidatePdfFile(path);
        return ExtractPagesMarkdownMem(File.ReadAllBytes(path), pages);
    }
}
