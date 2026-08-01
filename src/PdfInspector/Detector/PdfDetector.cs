// Ported from reference/src/detector.rs
using PdfInspector.Pdf;

namespace PdfInspector.Detector;

/// <summary>
/// Smart PDF type detection without a full document load. Content streams are
/// sampled for text operators, so a text-based PDF can be told from a scanned or
/// image-based one without resolving every object.
/// </summary>
public static class PdfDetector
{
    private const string Module = "detector";

    /// <summary>Detects a PDF's type from a file path.</summary>
    public static PdfTypeResult DetectPdfType(string path, DetectionConfig? config = null)
    {
        Validation.ValidatePdfFile(path);
        var doc = PdfDocument.LoadFile(path);
        return DetectFromDocument(doc, (uint)doc.PageCount, config ?? new DetectionConfig());
    }

    /// <summary>Detects a PDF's type from an in-memory buffer.</summary>
    public static PdfTypeResult DetectPdfTypeMem(byte[] buffer, DetectionConfig? config = null)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = PdfDocument.Load(buffer);
        return DetectFromDocument(doc, (uint)doc.PageCount, config ?? new DetectionConfig());
    }

    /// <summary>
    /// A heuristic page count for a malformed PDF that cannot be parsed, scanning
    /// the raw bytes for page dictionaries while excluding the page-tree node. It
    /// is a low-confidence diagnostic hint; a parsed page-tree count remains
    /// authoritative.
    /// </summary>
    public static uint EstimatePageCountFromBytes(byte[] buffer)
    {
        var count = 0u;
        var pos = 0;
        var needle = "/Type"u8.ToArray();

        while (true)
        {
            var relIdx = FindBytes(buffer.AsSpan(pos), needle);
            if (relIdx < 0)
            {
                break;
            }

            var valuePos = SkipPdfWhitespace(buffer, pos + relIdx + needle.Length);

            if (valuePos < buffer.Length && buffer[valuePos] == (byte)'/')
            {
                var nameStart = valuePos + 1;
                var nameEnd = nameStart + 4;
                if (nameEnd <= buffer.Length
                    && buffer.AsSpan(nameStart, 4).SequenceEqual("Page"u8)
                    && (nameEnd >= buffer.Length || IsPdfNameDelimiter(buffer[nameEnd])))
                {
                    count++;
                }
            }

            pos += relIdx + needle.Length;
        }

        return count;
    }

    /// <summary>The index of a byte sequence within a span, or -1 when absent.</summary>
    private static int FindBytes(ReadOnlySpan<byte> haystack, ReadOnlySpan<byte> needle) =>
        haystack.IndexOf(needle);

    /// <summary>Advances past PDF whitespace.</summary>
    private static int SkipPdfWhitespace(byte[] buffer, int pos)
    {
        while (pos < buffer.Length && IsPdfWhitespace(buffer[pos]))
        {
            pos++;
        }

        return pos;
    }

    /// <summary>True for the six bytes the PDF syntax treats as whitespace.</summary>
    private static bool IsPdfWhitespace(byte b) =>
        b is 0 or (byte)'\t' or (byte)'\n' or 0x0C or (byte)'\r' or (byte)' ';

    /// <summary>True for a byte that ends a PDF name token.</summary>
    private static bool IsPdfNameDelimiter(byte b) =>
        IsPdfWhitespace(b)
        || b is (byte)'(' or (byte)')' or (byte)'<' or (byte)'>' or (byte)'['
            or (byte)']' or (byte)'{' or (byte)'}' or (byte)'/' or (byte)'%';

    /// <summary>Runs detection over a loaded document.</summary>
    internal static PdfTypeResult DetectFromDocument(PdfDocument doc, uint pageCount, DetectionConfig config)
    {
        var totalPages = (uint)doc.PageCount;
        var (sampleIndices, allowEarlyExit) = SelectPages(config.Strategy, totalPages);

        var pagesWithText = 0u;
        var pagesWithImages = 0u;
        var pagesWithTemplateImages = 0u;
        var pagesWithVectorText = 0u;
        var totalTextOps = 0u;

        // The first-phase results are cached so the second phase does not
        // re-analyse a page it already saw.
        var analysisCache = new Dictionary<uint, PageAnalysis>();
        var pagesActuallySampled = 0u;

        foreach (var pageNum in sampleIndices)
        {
            var page = doc.GetPage((int)pageNum);
            if (page is null)
            {
                continue;
            }

            var analysis = PageAnalyzer.AnalyzePageContent(doc, page);
            pagesActuallySampled++;

            Log.Debug(Module, () =>
                $"page {pageNum}: text_ops={analysis.TextOperatorCount} images={analysis.HasImages} " +
                $"image_count={analysis.ImageCount} template={analysis.HasTemplateImage} " +
                $"unique_chars={analysis.UniqueTextChars} alphanum={analysis.UniqueAlphanumChars} " +
                $"path_ops={analysis.PathOpCount} vector_text={analysis.HasVectorText} " +
                $"image_area={analysis.TotalImageArea} " +
                $"identity_h_no_tounicode={analysis.HasIdentityHNoToUnicode} " +
                $"type3_only={analysis.HasOnlyType3Fonts} font_changes={analysis.FontChangeCount} " +
                $"decodable_fonts={analysis.HasDecodableTextFonts}");

            var isImageDominated = analysis.ImageCount > 10
                && analysis.ImageCount > analysis.TextOperatorCount * 3;

            var effectiveMinOps = analysis.HasImages || analysis.ImageCount > 0
                ? Math.Max(config.MinTextOpsPerPage, 10u)
                : config.MinTextOpsPerPage;

            if (analysis.TextOperatorCount >= effectiveMinOps
                && !isImageDominated
                && analysis.UniqueTextChars >= 5
                && !analysis.HasVectorText
                && !analysis.HasOnlyType3Fonts)
            {
                pagesWithText++;
            }

            if (analysis.HasImages)
            {
                pagesWithImages++;
            }

            if (analysis.HasTemplateImage && LooksLikeScan(analysis))
            {
                pagesWithTemplateImages++;
            }

            if (analysis.HasVectorText)
            {
                pagesWithVectorText++;
            }

            totalTextOps += analysis.TextOperatorCount;
            analysisCache[pageNum] = analysis;

            // Early exit: a page with too little meaningful text but with images
            // means this PDF is not purely text-based.
            if (allowEarlyExit
                && (analysis.TextOperatorCount < config.MinTextOpsPerPage
                    || isImageDominated
                    || analysis.UniqueTextChars < 5)
                && (analysis.HasImages || analysis.HasTemplateImage))
            {
                break;
            }
        }

        var pagesSampled = pagesActuallySampled;
        var textRatio = pagesSampled > 0 ? pagesWithText / (float)pagesSampled : 0.0f;

        // A template-based PDF has text AND large background images on most pages,
        // so its images carry essential context that text alone misses.
        var hasTemplateImages = pagesWithTemplateImages > 0;
        var templateRatio = pagesSampled > 0 ? pagesWithTemplateImages / (float)pagesSampled : 0.0f;

        PdfType pdfType;
        float confidence;
        bool ocrRecommended;

        if (hasTemplateImages && pagesWithText > 0)
        {
            ocrRecommended = true;
            pdfType = PdfType.Mixed;
            confidence = 0.5f + (0.3f * (1.0f - templateRatio));
        }
        else if (textRatio >= config.TextPageRatioThreshold)
        {
            ocrRecommended = false;
            pdfType = PdfType.TextBased;
            confidence = textRatio;
        }
        else if (pagesWithText == 0 && (pagesWithImages > 0 || pagesWithVectorText > 0))
        {
            // No extractable text, but images or vector-outlined text are present.
            ocrRecommended = true;
            if (totalTextOps == 0 && pagesWithVectorText == 0)
            {
                pdfType = PdfType.Scanned;
                confidence = 0.95f;
            }
            else
            {
                pdfType = PdfType.ImageBased;
                confidence = 0.8f;
            }
        }
        else if (pagesWithText > 0 && (pagesWithImages > 0 || pagesWithVectorText > 0))
        {
            ocrRecommended = true;
            pdfType = PdfType.Mixed;
            confidence = 0.7f;
        }
        else if (totalTextOps == 0)
        {
            ocrRecommended = true;
            pdfType = PdfType.Scanned;
            confidence = 0.9f;
        }
        else
        {
            ocrRecommended = false;
            pdfType = PdfType.TextBased;
            confidence = MathF.Max(textRatio, 0.5f);
        }

        ocrRecommended = ApplyNewspaperHeuristic(pdfType, pagesSampled, analysisCache, ocrRecommended);

        var pagesNeedingOcr = BuildPagesNeedingOcr(doc, pdfType, totalPages, config, analysisCache);

        // Explain each flagged page. A page that was analysed gets a signal-derived
        // reason; a page flagged only by whole-document classification — an
        // unsampled page of a scanned document — defaults to "scanned".
        var ocrReasonsByPage = new SortedDictionary<uint, List<string>>();
        foreach (var pageNum in pagesNeedingOcr)
        {
            ocrReasonsByPage[pageNum] = analysisCache.TryGetValue(pageNum, out var analysis)
                ? PageAnalyzer.PageOcrReasons(analysis)
                : [OcrReason.Scanned];
        }

        return new PdfTypeResult
        {
            PdfType = pdfType,
            PageCount = pageCount,
            PagesSampled = pagesSampled,
            PagesWithText = pagesWithText,
            Confidence = confidence,
            Title = doc.TryGetTitle(out var title) ? title : null,
            OcrRecommended = ocrRecommended,
            PagesNeedingOcr = pagesNeedingOcr,
            OcrReasonsByPage = ocrReasonsByPage,
        };
    }

    /// <summary>
    /// True when a page with a template image reads as a scan — a single full-page
    /// image — rather than as a text page with figures. A scanned-with-OCR PDF has
    /// one large image per page plus a text overlay, while a text PDF with figures
    /// has several smaller images alongside real text. CID-encoded fonts with a
    /// ToUnicode CMap yield few distinct raw bytes yet decode fully, so a page with
    /// decodable fonts and enough text operators is never treated as a scan.
    /// </summary>
    private static bool LooksLikeScan(PageAnalysis analysis)
    {
        var alphanumLow = analysis.UniqueAlphanumChars < 10
            && !(analysis.HasDecodableTextFonts && analysis.TextOperatorCount >= 10);
        return analysis.ImageCount <= 1 && analysis.TextOperatorCount < 50 && alphanumLow;
    }

    /// <summary>Selects the pages a strategy scans, and whether it may stop early.</summary>
    private static (List<uint> Pages, bool AllowEarlyExit) SelectPages(ScanStrategy strategy, uint totalPages)
    {
        switch (strategy)
        {
            case ScanStrategy.EarlyExit:
                return ([.. Range(1, totalPages)], true);

            case ScanStrategy.Full:
                return ([.. Range(1, totalPages)], false);

            case ScanStrategy.Sample sample:
                return (DistributePages(Math.Min(sample.MaxPages, totalPages), totalPages), false);

            case ScanStrategy.Pages pages:
                return (
                    [.. pages.PageNumbers.Where(p => p >= 1 && p <= totalPages).Distinct().OrderBy(p => p)],
                    false);

            default:
                return ([], false);
        }
    }

    /// <summary>An inclusive range of page numbers.</summary>
    private static IEnumerable<uint> Range(uint from, uint to)
    {
        for (var p = from; p <= to; p++)
        {
            yield return p;
        }
    }

    /// <summary>
    /// Distributes <paramref name="n"/> page indices evenly across the document,
    /// always including the first and last page and spacing the rest between them.
    /// </summary>
    private static List<uint> DistributePages(uint n, uint total)
    {
        if (n == 0)
        {
            return [];
        }

        if (n >= total)
        {
            return [.. Range(1, total)];
        }

        var indices = new List<uint>((int)n) { 1 };

        if (n > 1)
        {
            indices.Add(total);
        }

        var remaining = n >= 2 ? n - 2 : 0;
        if (remaining > 0 && total > 2)
        {
            var step = (total - 2) / (remaining + 1);
            for (var i = 1u; i <= remaining; i++)
            {
                var idx = 1 + (step * i);
                if (idx > 1 && idx < total && !indices.Contains(idx))
                {
                    indices.Add(idx);
                }
            }
        }

        return [.. indices.Distinct().OrderBy(p => p)];
    }

    /// <summary>
    /// Recommends OCR for a dense multi-column newspaper. Those have extractable
    /// text but produce poor output because of their interleaved article layouts,
    /// and they show up as consistently high text density with moderate font
    /// switching and a low font-change to text-operator ratio.
    /// </summary>
    /// <remarks>
    /// The ratio is what separates a newspaper from a richly styled legal or
    /// business document: newspapers run 0.02–0.06, per-character-styled documents
    /// 0.25–0.35. Thresholds were calibrated against a 50-page WSJ issue, DPA and
    /// contract documents, SEC filings, and ordinary documents.
    /// </remarks>
    private static bool ApplyNewspaperHeuristic(
        PdfType pdfType,
        uint pagesSampled,
        Dictionary<uint, PageAnalysis> analysisCache,
        bool ocrRecommended)
    {
        if (pdfType != PdfType.TextBased || pagesSampled < 3)
        {
            return ocrRecommended;
        }

        var newspaperPages = 0u;
        foreach (var analysis in analysisCache.Values)
        {
            var ratio = analysis.TextOperatorCount > 0
                ? analysis.FontChangeCount / (float)analysis.TextOperatorCount
                : 1.0f;

            if (analysis.TextOperatorCount >= 1500 && analysis.FontChangeCount >= 50 && ratio < 0.15f)
            {
                newspaperPages++;
            }
        }

        if (newspaperPages / (float)pagesSampled < 0.5f)
        {
            return ocrRecommended;
        }

        Log.Debug(Module, () =>
            $"newspaper layout detected: {newspaperPages}/{pagesSampled} pages with high text_ops + " +
            "font_changes → OCR recommended");
        return true;
    }

    /// <summary>Builds the sorted list of pages that need OCR.</summary>
    private static List<uint> BuildPagesNeedingOcr(
        PdfDocument doc,
        PdfType pdfType,
        uint totalPages,
        DetectionConfig config,
        Dictionary<uint, PageAnalysis> analysisCache)
    {
        var pagesNeedingOcr = new List<uint>();

        switch (pdfType)
        {
            case PdfType.TextBased:
                break;

            case PdfType.Scanned:
            case PdfType.ImageBased:
                pagesNeedingOcr.AddRange(Range(1, totalPages));
                break;

            case PdfType.Mixed:
                for (var pageNum = 1u; pageNum <= totalPages; pageNum++)
                {
                    if (!analysisCache.TryGetValue(pageNum, out var analysis))
                    {
                        var page = doc.GetPage((int)pageNum);
                        if (page is null)
                        {
                            continue;
                        }

                        // Caching the fresh analysis lets the reason pass see the real
                        // signals rather than defaulting to "scanned".
                        analysis = PageAnalyzer.AnalyzePageContent(doc, page);
                        analysisCache[pageNum] = analysis;
                    }

                    if ((analysis.HasTemplateImage && LooksLikeScan(analysis))
                        || analysis.HasVectorText
                        || (analysis.TextOperatorCount < config.MinTextOpsPerPage && analysis.HasImages))
                    {
                        pagesNeedingOcr.Add(pageNum);
                    }
                }

                break;
        }

        // Pages whose fonts cannot reach Unicode need OCR whatever the document's
        // classification: Identity-H without a ToUnicode CMap cannot map raw CIDs,
        // and a Type3-only page cannot map its glyph bitmaps.
        foreach (var (pageNum, analysis) in analysisCache)
        {
            if ((analysis.HasIdentityHNoToUnicode || analysis.HasOnlyType3Fonts)
                && !pagesNeedingOcr.Contains(pageNum))
            {
                pagesNeedingOcr.Add(pageNum);
            }
        }

        // Unsampled pages need the same check, using the usage-based font analysis.
        if (pagesNeedingOcr.Count < totalPages)
        {
            for (var pageNum = 1u; pageNum <= totalPages; pageNum++)
            {
                if (analysisCache.ContainsKey(pageNum) || pagesNeedingOcr.Contains(pageNum))
                {
                    continue;
                }

                var page = doc.GetPage((int)pageNum);
                if (page is null)
                {
                    continue;
                }

                var analysis = PageAnalyzer.AnalyzePageContent(doc, page);
                if (analysis.HasIdentityHNoToUnicode || analysis.HasOnlyType3Fonts)
                {
                    pagesNeedingOcr.Add(pageNum);

                    // Cached so the reason pass reports the garbled-text signal rather
                    // than defaulting to "scanned".
                    analysisCache[pageNum] = analysis;
                }
            }
        }

        return [.. pagesNeedingOcr.Distinct().OrderBy(p => p)];
    }
}
