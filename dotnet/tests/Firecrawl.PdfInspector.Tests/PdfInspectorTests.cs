using System.Text;
using Xunit;

namespace Firecrawl.PdfInspector.Tests;

public sealed class PdfInspectorTests
{
    private static byte[] ReadFixture() =>
        File.ReadAllBytes(Path.Combine(AppContext.BaseDirectory, "fixtures", "thermo-freon12.pdf"));

    [Fact]
    public void ProcessPdfMatchesWasmContract()
    {
        var result = PdfInspector.ProcessPdf(ReadFixture());

        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.True(result.PageCount > 0);
        Assert.False(string.IsNullOrWhiteSpace(result.Markdown));
        Assert.NotNull(result.Layout);
    }

    [Fact]
    public void DetectPdfDoesNotProduceMarkdown()
    {
        var result = PdfInspector.DetectPdf(ReadFixture());

        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.Null(result.Markdown);
    }

    [Fact]
    public void ClassifyAndExtractTextMatchWasmSemantics()
    {
        var pdf = ReadFixture();
        var classification = PdfInspector.ClassifyPdf(pdf);
        var text = PdfInspector.ExtractText(pdf);

        Assert.Equal(PdfType.TextBased, classification.PdfType);
        Assert.NotEmpty(text);
    }

    [Fact]
    public async Task StreamAndAsyncOverloadsWork()
    {
        using var stream = new MemoryStream(ReadFixture());
        var result = PdfInspector.ProcessPdf(stream);
        var asyncResult = await PdfInspector.ClassifyPdfAsync(ReadFixture());

        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.Equal(PdfType.TextBased, asyncResult.PdfType);
    }

    [Fact]
    public void RejectsPageZero()
    {
        var error = Assert.Throws<PdfInspectorException>(() =>
            PdfInspector.ProcessPdf(ReadFixture(), new ProcessOptions { Pages = new uint[] { 0 } }));

        Assert.Equal(1, error.NativeStatus);
        Assert.Contains("1-indexed", error.Message);
    }

    [Fact]
    public void OcrOffDoesNotRequireExternalRuntime()
    {
        var result = PdfInspector.ProcessPdfWithOcr(
            ReadFixture(),
            new OcrOptions { Mode = OcrMode.Off });

        Assert.True(result.PageCount > 0);
        Assert.Empty(result.PagesRoutedToOcr);
        Assert.NotEmpty(result.Markdown);
    }

    [Fact]
    public void OcrAutoProcessesRoutedPageWhenRuntimeIsConfigured()
    {
        if (Environment.GetEnvironmentVariable("PDF_INSPECTOR_DOTNET_OCR_RUNTIME") != "1")
        {
            return;
        }

        var pdf = File.ReadAllBytes(
            Path.Combine(
                AppContext.BaseDirectory,
                "fixtures",
                "scan_with_native_header_text.pdf"));
        var result = PdfInspector.ProcessPdfWithOcr(
            pdf,
            new OcrOptions { Mode = OcrMode.Auto, Offline = true });

        Assert.Equal(new uint[] { 1 }, result.PagesRoutedToOcr);
        Assert.Empty(result.PagesRecommendingHosted);
        Assert.Contains(
            result.Pages[0].Provenance.Source,
            new[] { PageContentSource.Ocr, PageContentSource.Fused });
        Assert.NotEmpty(result.Pages[0].Markdown);
    }

    [Fact]
    public void InvalidPdfReturnsNativeError()
    {
        var error = Assert.Throws<PdfInspectorException>(() =>
            PdfInspector.ProcessPdf(Encoding.UTF8.GetBytes("not a PDF")));

        Assert.Equal(2, error.NativeStatus);
    }

    [Fact]
    public void VersionMatchesPackageVersion()
    {
        var assemblyVersion = typeof(PdfInspector).Assembly.GetName().Version
            ?? throw new InvalidOperationException("Missing managed assembly version.");

        Assert.Equal(
            $"{assemblyVersion.Major}.{assemblyVersion.Minor}.{assemblyVersion.Build}",
            PdfInspector.Version());
    }
}
