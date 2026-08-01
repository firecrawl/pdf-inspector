// Ported from reference/tests/integration_tests.rs
using PdfInspector.Detector;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Covers the per-page markdown API and the fast classification path.</summary>
public sealed class PagesMarkdownTests
{
    private static byte[] Fixture(string name) =>
        File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, name + ".pdf"));

    [Fact]
    public void EveryPageIsReturnedInDocumentOrder()
    {
        var buf = Fixture("nexo-price-en");
        var result = PdfProcessor.ExtractPagesMarkdownMem(buf);
        var pageCount = PdfProcessor.ProcessPdfMem(buf).PageCount;

        Assert.Equal((int)pageCount, result.Pages.Count);
        Assert.Equal([.. Enumerable.Range(0, (int)pageCount).Select(i => (uint)i)],
            result.Pages.Select(p => p.Page));
        Assert.False(result.Pages[0].NeedsOcr);
        Assert.NotEmpty(result.Pages[0].Markdown.Trim());
    }

    [Fact]
    public void RequestedPagesComeBackInTheCallersOrder()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("nexo-price-en"), [2, 0, 1]);
        Assert.Equal([2u, 0u, 1u], result.Pages.Select(p => p.Page));
    }

    [Fact]
    public void AnOutOfRangePageIsEmptyAndNeedsOcr()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("nexo-price-en"), [9999]);
        var page = Assert.Single(result.Pages);
        Assert.Equal(9999u, page.Page);
        Assert.Empty(page.Markdown);
        Assert.True(page.NeedsOcr);
        Assert.Contains(10000u, result.PagesNeedingOcr);
    }

    [Fact]
    public void AnEmptyPageListReturnsNoPages() =>
        Assert.Empty(PdfProcessor.ExtractPagesMarkdownMem(Fixture("nexo-price-en"), []).Pages);

    [Fact]
    public void ASinglePageCanBeRequested()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("nexo-price-en"), [0]);
        var page = Assert.Single(result.Pages);
        Assert.Equal(0u, page.Page);
        Assert.NotEmpty(page.Markdown.Trim());
    }

    [Fact]
    public void AnInvalidBufferIsRejected() =>
        Assert.Throws<PdfException>(() => PdfProcessor.ExtractPagesMarkdownMem([1, 2, 3, 4]));

    [Fact]
    public void GlyphIdPagesNeedOcr()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("shinagawa_identity_h"));
        Assert.All(result.Pages, page => Assert.True(page.NeedsOcr));
        Assert.All(result.Pages, page => Assert.Empty(page.Markdown));
    }

    /// <summary>
    /// This fixture's producer authored a broken ToUnicode CMap that shifts every
    /// character by a per-range constant, and the embedded subset font has no
    /// cmap table to recover from. The resulting ciphertext is entirely printable
    /// ASCII, so it has to be caught by the substitution-cipher statistics and
    /// routed to OCR rather than served silently.
    /// </summary>
    [Fact]
    public void AShiftedCipherToUnicodePageIsRoutedToOcr()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("shifted_cipher_tounicode"));

        var page = Assert.Single(result.Pages);
        Assert.True(page.NeedsOcr);
        Assert.Empty(page.Markdown);
        Assert.Equal([1u], result.PagesNeedingOcr);
        Assert.Equal(OcrReason.SuspectedGarbledText, page.OcrReason);
    }

    [Fact]
    public void LayoutComplexityIsReportedForATablePdf()
    {
        var result = PdfProcessor.ExtractPagesMarkdownMem(Fixture("tnagriculture_06_12"));
        Assert.True(result.IsComplex);
        Assert.NotEmpty(result.PagesWithTables);
    }

    [Fact]
    public void ClassificationMatchesTheFullPipeline()
    {
        var buf = Fixture("nexo-price-en");
        var pages = PdfProcessor.ExtractPagesMarkdownMem(buf);
        var full = PdfProcessor.ProcessPdfMem(buf);

        Assert.Equal(full.Layout.IsComplex, pages.IsComplex);
        Assert.Equal(full.Layout.PagesWithTables, pages.PagesWithTables);
        Assert.Equal(full.Layout.PagesWithColumns, pages.PagesWithColumns);
    }

    [Fact]
    public void ThePathApiMatchesTheBufferApi()
    {
        var path = Path.Combine(TestPaths.Fixtures, "nexo-price-en.pdf");
        var fromPath = PdfProcessor.ExtractPagesMarkdown(path);
        var fromBuffer = PdfProcessor.ExtractPagesMarkdownMem(File.ReadAllBytes(path));

        Assert.Equal(fromBuffer.Pages.Count, fromPath.Pages.Count);
        Assert.Equal(
            fromBuffer.Pages.Select(p => p.Markdown),
            fromPath.Pages.Select(p => p.Markdown));
    }

    [Fact]
    public void ClassificationReportsZeroIndexedOcrPages()
    {
        var classification = PdfProcessor.ClassifyPdfMem(Fixture("shinagawa_identity_h"));
        Assert.Equal(1u, classification.PageCount);
        Assert.True(classification.Confidence is >= 0.0f and <= 1.0f);

        // The public surface is 0-indexed, unlike the 1-indexed internal lists.
        Assert.All(classification.PagesNeedingOcr, page => Assert.True(page < classification.PageCount));
    }

    [Fact]
    public void ClassificationAgreesWithTheDetector()
    {
        var buf = Fixture("nexo-price-en");
        var classification = PdfProcessor.ClassifyPdfMem(buf);
        var detection = PdfDetector.DetectPdfTypeMem(buf);

        Assert.Equal(detection.PdfType, classification.PdfType);
        Assert.Equal(detection.PageCount, classification.PageCount);
        Assert.Equal(detection.Confidence, classification.Confidence);
        Assert.Equal(
            detection.PagesNeedingOcr.Select(p => p - 1),
            classification.PagesNeedingOcr);
    }

    [Fact]
    public void ClassificationRejectsANonPdf() =>
        Assert.Throws<PdfException>(() => PdfProcessor.ClassifyPdfMem([1, 2, 3, 4]));
}
