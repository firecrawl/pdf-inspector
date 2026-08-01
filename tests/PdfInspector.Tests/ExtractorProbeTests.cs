using PdfInspector.Extractor;
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;
using Xunit;
using Xunit.Abstractions;

namespace PdfInspector.Tests;

/// <summary>
/// Temporary scaffold used while porting: dumps a page's extracted items so the
/// output can be diffed against the Rust reference. Replaced by the real
/// differential suite in the validation phase.
/// </summary>
public sealed class ExtractorProbeTests(ITestOutputHelper output)
{
    [Theory]
    [InlineData("nexo-price-en.pdf", 1)]
    [InlineData("wireless_two_col_no_rects.pdf", 1)]
    [InlineData("shinagawa_identity_h.pdf", 1)]
    public void DumpPageItems(string fixture, int pageNum)
    {
        var path = Path.Combine(TestPaths.Fixtures, fixture);
        var doc = PdfDocument.LoadFile(path);
        var cmaps = FontCMaps.FromDocumentPages(doc, [(uint)pageNum]);
        var page = doc.GetPage(pageNum)!;

        var extraction = ContentStreamExtractor.ExtractPageTextItems(
            doc, page, (uint)pageNum, cmaps, includeInvisible: false, new FontStyleCache());

        output.WriteLine($"items={extraction.Items.Count} rects={extraction.Rects.Count}");
        foreach (var item in extraction.Items.Take(40))
        {
            output.WriteLine($"{item.X,8:F2} {item.Y,8:F2} {item.Width,7:F2}  {item.Text}");
        }

        Assert.NotEmpty(extraction.Items);
    }
}
