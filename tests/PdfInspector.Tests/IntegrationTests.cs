// Ported from reference/tests/integration_tests.rs
using System.Text;
using PdfInspector.Detector;
using PdfInspector.Extractor;
using PdfInspector.Markdown;
using PdfInspector.Structure;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Shared builders for the synthetic PDFs and text items the tests use.</summary>
internal static class TestDocuments
{
    /// <summary>A three-line, single-page PDF with a classic xref table.</summary>
    public static byte[] MakeMinimalTextPdf()
    {
        var pdf = new List<byte>(Encoding.ASCII.GetBytes("%PDF-1.4\n"));
        var offsets = new List<int> { 0 };

        void AddObject(int id, string body)
        {
            offsets.Add(pdf.Count);
            pdf.AddRange(Encoding.ASCII.GetBytes($"{id} 0 obj\n"));
            pdf.AddRange(Encoding.ASCII.GetBytes(body));
            pdf.AddRange(Encoding.ASCII.GetBytes("\nendobj\n"));
        }

        AddObject(1, "<< /Type /Catalog /Pages 2 0 R >>");
        AddObject(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        AddObject(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            + "/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");

        const string content =
            "BT /F1 12 Tf 100 700 Td (Hello World) Tj 0 -14 Td (Second Line) Tj 0 -14 Td (Third Line) Tj ET";
        AddObject(4, $"<< /Length {content.Length} >>\nstream\n{content}\nendstream");
        AddObject(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

        var xrefStart = pdf.Count;
        pdf.AddRange(Encoding.ASCII.GetBytes($"xref\n0 {offsets.Count}\n"));
        pdf.AddRange(Encoding.ASCII.GetBytes("0000000000 65535 f \n"));
        foreach (var offset in offsets.Skip(1))
        {
            pdf.AddRange(Encoding.ASCII.GetBytes($"{offset:D10} 00000 n \n"));
        }

        pdf.AddRange(Encoding.ASCII.GetBytes(
            $"trailer\n<< /Size {offsets.Count} /Root 1 0 R >>\nstartxref\n{xrefStart}\n%%EOF"));

        return [.. pdf];
    }

    /// <summary>Drops the final byte of the end-of-file marker.</summary>
    public static byte[] TruncateEofMarker(byte[] pdf)
    {
        Assert.EndsWith("%%EOF", Encoding.ASCII.GetString(pdf), StringComparison.Ordinal);
        return pdf[..^1];
    }

    /// <summary>Prefixes the file with a stray tab, as some broken containers do.</summary>
    public static byte[] AddLeadingTab(byte[] pdf) => [(byte)'\t', .. pdf];

    public static TextItem MakeTextItem(string text, float x, float y, float fontSize, uint page) => new()
    {
        Text = text,
        X = x,
        Y = y,
        Width = Text.TextUtils.ByteLength(text) * fontSize * 0.5f,
        Height = fontSize,
        Font = "Helvetica",
        FontSize = fontSize,
        Page = page,
    };

    public static TextItem MakeTextItemWithFont(
        string text,
        float x,
        float y,
        float fontSize,
        string font,
        uint page) => new()
        {
            Text = text,
            X = x,
            Y = y,
            Width = Text.TextUtils.ByteLength(text) * fontSize * 0.5f,
            Height = fontSize,
            Font = font,
            FontSize = fontSize,
            Page = page,
            IsBold = Text.TextUtils.IsBoldFont(font),
            IsItalic = Text.TextUtils.IsItalicFont(font),
        };
}

public sealed class DetectionConfigTests
{
    [Fact]
    public void DefaultConfigMatchesTheReference()
    {
        var config = new DetectionConfig();
        var sample = Assert.IsType<ScanStrategy.Sample>(config.Strategy);
        Assert.Equal(8u, sample.MaxPages);
        Assert.Equal(3u, config.MinTextOpsPerPage);
        Assert.True(MathF.Abs(config.TextPageRatioThreshold - 0.6f) < 0.001f);
    }

    [Fact]
    public void CustomConfigRoundTrips()
    {
        var config = new DetectionConfig
        {
            Strategy = new ScanStrategy.Sample(10),
            MinTextOpsPerPage = 5,
            TextPageRatioThreshold = 0.8f,
        };
        var sample = Assert.IsType<ScanStrategy.Sample>(config.Strategy);
        Assert.Equal(10u, sample.MaxPages);
        Assert.Equal(5u, config.MinTextOpsPerPage);
        Assert.True(MathF.Abs(config.TextPageRatioThreshold - 0.8f) < 0.001f);
    }
}

public sealed class TextItemAndLineTests
{
    [Fact]
    public void TextItemCarriesItsPosition()
    {
        var item = TestDocuments.MakeTextItem("Hello", 100.0f, 700.0f, 12.0f, 1);
        Assert.Equal("Hello", item.Text);
        Assert.Equal(100.0f, item.X);
        Assert.Equal(700.0f, item.Y);
        Assert.Equal(12.0f, item.FontSize);
        Assert.Equal(1u, item.Page);
    }

    [Fact]
    public void CloningATextItemCopiesItsFields()
    {
        var item = TestDocuments.MakeTextItem("Test", 50.0f, 600.0f, 14.0f, 2);
        var cloned = item.Clone();
        Assert.Equal(item.Text, cloned.Text);
        Assert.Equal(item.X, cloned.X);
        Assert.Equal(item.Y, cloned.Y);
    }

    [Fact]
    public void LineTextJoinsItsItems()
    {
        var line = new TextLine
        {
            Items =
            [
                TestDocuments.MakeTextItem("Hello", 100.0f, 700.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("World", 160.0f, 700.0f, 12.0f, 1),
            ],
            Y = 700.0f,
            Page = 1,
            AdaptiveThreshold = 0.10f,
        };
        Assert.Equal("Hello World", line.Text());
    }

    [Fact]
    public void SingleItemLineRendersThatItem()
    {
        var line = new TextLine
        {
            Items = [TestDocuments.MakeTextItem("Single", 100.0f, 700.0f, 12.0f, 1)],
            Y = 700.0f,
            Page = 1,
            AdaptiveThreshold = 0.10f,
        };
        Assert.Equal("Single", line.Text());
    }

    [Fact]
    public void EmptyLineRendersEmpty()
    {
        var line = new TextLine { Items = [], Y = 700.0f, Page = 1, AdaptiveThreshold = 0.10f };
        Assert.Equal(string.Empty, line.Text());
    }
}

public sealed class GroupIntoLinesTests
{
    [Fact]
    public void EmptyInputProducesNoLines() => Assert.Empty(Layout.GroupIntoLines([]));

    [Fact]
    public void ItemsAtTheSameBaselineFormOneLine()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("A", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("B", 120.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("C", 140.0f, 700.0f, 12.0f, 1),
        ];
        var lines = Layout.GroupIntoLines(items);
        Assert.Single(lines);
        Assert.Equal(3, lines[0].Items.Count);
        Assert.Equal("A B C", lines[0].Text());
    }

    [Fact]
    public void ItemsAtDifferentBaselinesFormSeparateLines()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("Line1", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Line2", 100.0f, 680.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Line3", 100.0f, 660.0f, 12.0f, 1),
        ];
        var lines = Layout.GroupIntoLines(items);
        Assert.Equal(3, lines.Count);
        Assert.Equal("Line1", lines[0].Text());
        Assert.Equal("Line2", lines[1].Text());
        Assert.Equal("Line3", lines[2].Text());
    }

    [Fact]
    public void ItemsWithinTheYToleranceMerge()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("A", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("B", 150.0f, 700.0f, 12.0f, 1),
        ];
        var lines = Layout.GroupIntoLines(items);
        Assert.Single(lines);
        Assert.Equal("A B", lines[0].Text());
    }

    [Fact]
    public void PagesAreNeverMerged()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("Page1Text", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Page2Text", 100.0f, 700.0f, 12.0f, 2),
        ];
        var lines = Layout.GroupIntoLines(items);
        Assert.Equal(2, lines.Count);
        Assert.Equal(1u, lines[0].Page);
        Assert.Equal(2u, lines[1].Page);
    }

    [Fact]
    public void ItemsOnALineAreSortedByX()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("Third", 200.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("First", 50.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Second", 100.0f, 700.0f, 12.0f, 1),
        ];
        var lines = Layout.GroupIntoLines(items);
        Assert.Single(lines);
        Assert.Equal("First Second Third", lines[0].Text());
    }
}

public sealed class MarkdownOptionsTests
{
    [Fact]
    public void DefaultsMatchTheReference()
    {
        var opts = new MarkdownOptions();
        Assert.Equal(MarkdownProfile.Fidelity, opts.Profile);
        Assert.True(opts.DetectHeaders);
        Assert.True(opts.DetectLists);
        Assert.True(opts.DetectCode);
        Assert.Null(opts.BaseFontSize);
    }

    [Fact]
    public void CustomOptionsRoundTrip()
    {
        var opts = new MarkdownOptions
        {
            Profile = MarkdownProfile.Compact,
            DetectHeaders = false,
            DetectLists = true,
            DetectCode = false,
            BaseFontSize = 14.0f,
            RemovePageNumbers = false,
            FormatUrls = false,
            FixHyphenation = false,
            DetectBold = false,
            DetectItalic = false,
            IncludeImages = false,
            IncludeLinks = false,
            IncludePageNumbers = false,
        };
        Assert.False(opts.DetectHeaders);
        Assert.Equal(MarkdownProfile.Compact, opts.Profile);
        Assert.True(opts.DetectLists);
        Assert.False(opts.DetectCode);
        Assert.Equal(14.0f, opts.BaseFontSize);
        Assert.False(opts.RemovePageNumbers);
        Assert.False(opts.FormatUrls);
        Assert.False(opts.FixHyphenation);
        Assert.False(opts.DetectBold);
        Assert.False(opts.DetectItalic);
        Assert.False(opts.IncludeImages);
        Assert.False(opts.IncludeLinks);
    }

    [Fact]
    public void ImagesAreExcludedByDefault() => Assert.False(new MarkdownOptions().IncludeImages);
}

public sealed class MarkdownConversionTests
{
    private static string Convert(string text, MarkdownOptions? options = null) =>
        MarkdownConverter.ToMarkdown(text, options ?? new MarkdownOptions());

    [Fact]
    public void PlainTextSurvives() => Assert.Contains("Hello World", Convert("Hello World"), StringComparison.Ordinal);

    [Fact]
    public void MultipleLinesSurvive()
    {
        var md = Convert("Line one\nLine two\nLine three");
        Assert.Contains("Line one", md, StringComparison.Ordinal);
        Assert.Contains("Line two", md, StringComparison.Ordinal);
        Assert.Contains("Line three", md, StringComparison.Ordinal);
    }

    [Fact]
    public void BulletListsBecomeDashes()
    {
        var md = Convert("• First\n• Second\n• Third");
        Assert.Contains("- First", md, StringComparison.Ordinal);
        Assert.Contains("- Second", md, StringComparison.Ordinal);
        Assert.Contains("- Third", md, StringComparison.Ordinal);
    }

    [Fact]
    public void DashListsStayAsTheyAre()
    {
        var md = Convert("- One\n- Two\n- Three");
        Assert.Contains("- One", md, StringComparison.Ordinal);
        Assert.Contains("- Two", md, StringComparison.Ordinal);
    }

    [Fact]
    public void NumberedListsStayNumbered()
    {
        var md = Convert("1. First\n2. Second\n3. Third");
        Assert.Contains("1. First", md, StringComparison.Ordinal);
        Assert.Contains("2. Second", md, StringComparison.Ordinal);
    }

    [Fact]
    public void CodeIsFenced() =>
        Assert.Contains("```", Convert("const x = 5;\nlet y = 10;"), StringComparison.Ordinal);

    [Fact]
    public void CodeDetectionCanBeDisabled() =>
        Assert.DoesNotContain(
            "```",
            Convert("const x = 5;", new MarkdownOptions { DetectCode = false }),
            StringComparison.Ordinal);

    [Fact]
    public void ListDetectionCanBeDisabled() =>
        // The original bullet character is kept.
        Assert.Contains(
            "•",
            Convert("• Item", new MarkdownOptions { DetectLists = false }),
            StringComparison.Ordinal);

    [Fact]
    public void BlankLinesSeparateParagraphs()
    {
        var md = Convert("Para one\n\nPara two");
        Assert.Contains("Para one", md, StringComparison.Ordinal);
        Assert.Contains("Para two", md, StringComparison.Ordinal);
    }

    [Fact]
    public void WhitespaceOnlyLinesAreHarmless()
    {
        var md = Convert("Content\n   \nMore content");
        Assert.Contains("Content", md, StringComparison.Ordinal);
        Assert.Contains("More content", md, StringComparison.Ordinal);
    }

    [Fact]
    public void ExcessiveNewlinesKeepTheirParagraphs()
    {
        var md = Convert("Para one\n\n\n\n\nPara two");
        Assert.Contains("Para one", md, StringComparison.Ordinal);
        Assert.Contains("Para two", md, StringComparison.Ordinal);
    }

    [Fact]
    public void OutputEndsWithExactlyOneNewline()
    {
        var md = Convert("Content");
        Assert.EndsWith("\n", md, StringComparison.Ordinal);
        Assert.False(md.EndsWith("\n\n", StringComparison.Ordinal));
    }

    [Theory]
    [InlineData("• Item")]
    [InlineData("○ Item")]
    [InlineData("● Item")]
    [InlineData("◦ Item")]
    public void UnicodeBulletsBecomeDashes(string bullet) =>
        Assert.Contains("- Item", Convert(bullet), StringComparison.Ordinal);

    [Theory]
    [InlineData("- Item")]
    [InlineData("* Item")]
    public void MarkdownBulletsAreLeftAlone(string bullet) =>
        Assert.Contains(bullet, Convert(bullet), StringComparison.Ordinal);

    [Theory]
    [InlineData("1. First")]
    [InlineData("2) Second")]
    [InlineData("10. Tenth")]
    public void NumberedListVariationsProduceOutput(string item) =>
        Assert.NotEmpty(Convert(item).Trim());

    [Fact]
    public void LetteredListItemsSurvive() =>
        Assert.Contains("a. Letter item", Convert("a. Letter item"), StringComparison.Ordinal);

    [Theory]
    [InlineData("import foo")]
    [InlineData("export default")]
    [InlineData("const x = 5;")]
    [InlineData("let y = 10;")]
    [InlineData("function test() {")]
    [InlineData("class MyClass {")]
    [InlineData("def func():")]
    [InlineData("pub fn main() {")]
    [InlineData("async fn process() {")]
    [InlineData("impl Trait {")]
    public void CodeKeywordsAreDetected(string code) =>
        Assert.Contains("```", Convert(code), StringComparison.Ordinal);

    [Theory]
    [InlineData("=> value")]
    [InlineData("-> Result")]
    [InlineData(":: io::Result")]
    public void CodeSyntaxPatternsAreDetected(string code) =>
        Assert.Contains("```", Convert(code), StringComparison.Ordinal);

    [Fact]
    public void PunctuationHeavyLinesReadAsCode() =>
        Assert.Contains("```", Convert("if (x > 0) { return y; }"), StringComparison.Ordinal);

    [Fact]
    public void ProseAboutCodeIsNotCode() =>
        Assert.DoesNotContain(
            "```", Convert("This is regular text about programming."), StringComparison.Ordinal);
}

public sealed class MarkdownFromItemsTests
{
    private static string Convert(List<TextItem> items) =>
        MarkdownConverter.ToMarkdownFromItems(items, new MarkdownOptions());

    [Fact]
    public void EmptyItemsProduceEmptyMarkdown() => Assert.Empty(Convert([]));

    [Fact]
    public void SingleItemSurvives() =>
        Assert.Contains(
            "Hello",
            Convert([TestDocuments.MakeTextItem("Hello", 100.0f, 700.0f, 12.0f, 1)]),
            StringComparison.Ordinal);

    [Fact]
    public void LargestFontBecomesH1()
    {
        // Several body items are needed to establish the base font size.
        var md = Convert(
        [
            TestDocuments.MakeTextItem("Title", 100.0f, 750.0f, 24.0f, 1),
            TestDocuments.MakeTextItem("Body text one", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Body text two", 100.0f, 680.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Body text three", 100.0f, 660.0f, 12.0f, 1),
        ]);
        Assert.Contains("# Title", md, StringComparison.Ordinal);
        Assert.Contains("Body text", md, StringComparison.Ordinal);
    }

    [Fact]
    public void SecondTierBecomesH2()
    {
        var md = Convert(
        [
            TestDocuments.MakeTextItem("Title", 100.0f, 800.0f, 24.0f, 1),
            TestDocuments.MakeTextItem("Subtitle", 100.0f, 750.0f, 18.0f, 1),
            TestDocuments.MakeTextItem("Body text one", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Body text two", 100.0f, 680.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Body text three", 100.0f, 660.0f, 12.0f, 1),
        ]);
        Assert.Contains("## Subtitle", md, StringComparison.Ordinal);
    }

    [Fact]
    public void MonospaceFontsProduceCodeFences()
    {
        var md = Convert(
            [TestDocuments.MakeTextItemWithFont("let x = 5", 100.0f, 700.0f, 12.0f, "Courier", 1)]);
        Assert.Contains("```", md, StringComparison.Ordinal);
        Assert.Contains("let x = 5", md, StringComparison.Ordinal);
    }

    [Fact]
    public void PagesAreSeparatedWithoutRules()
    {
        var md = Convert(
        [
            TestDocuments.MakeTextItem("Content on first page", 100.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("Content on second page", 100.0f, 700.0f, 12.0f, 2),
        ]);
        Assert.DoesNotContain("---", md, StringComparison.Ordinal);
        Assert.Contains("Content on first page", md, StringComparison.Ordinal);
        Assert.Contains("Content on second page", md, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("Courier")]
    [InlineData("Consolas")]
    [InlineData("Monaco")]
    [InlineData("Menlo")]
    [InlineData("Fira Code")]
    [InlineData("JetBrains Mono")]
    [InlineData("Inconsolata")]
    [InlineData("DejaVu Sans Mono")]
    [InlineData("Liberation Mono")]
    [InlineData("Fixed")]
    [InlineData("Terminal")]
    public void MonospaceFontNamesAreRecognised(string font)
    {
        var md = Convert([TestDocuments.MakeTextItemWithFont("code", 100.0f, 700.0f, 12.0f, font, 1)]);
        Assert.Contains("```", md, StringComparison.Ordinal);
    }

    [Fact]
    public void DoubleTheBaseFontIsH1() =>
        Assert.Contains(
            "# H1 Title",
            Convert(
            [
                TestDocuments.MakeTextItem("H1 Title", 100.0f, 700.0f, 24.0f, 1),
                TestDocuments.MakeTextItem("body text one", 100.0f, 650.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text two", 100.0f, 630.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text three", 100.0f, 610.0f, 12.0f, 1),
            ]),
            StringComparison.Ordinal);

    [Fact]
    public void ASingleHeadingTierBecomesH1() =>
        // 18pt over a 12pt base is the only tier, so it is H1 rather than H2.
        Assert.Contains(
            "# Section Title",
            Convert(
            [
                TestDocuments.MakeTextItem("Section Title", 100.0f, 700.0f, 18.0f, 1),
                TestDocuments.MakeTextItem("body text one", 100.0f, 650.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text two", 100.0f, 630.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text three", 100.0f, 610.0f, 12.0f, 1),
            ]),
            StringComparison.Ordinal);

    [Fact]
    public void TwoTiersBecomeH1AndH2()
    {
        var md = Convert(
        [
            TestDocuments.MakeTextItem("H1 Title", 100.0f, 750.0f, 24.0f, 1),
            TestDocuments.MakeTextItem("H2 Title", 100.0f, 700.0f, 18.0f, 1),
            TestDocuments.MakeTextItem("body text one", 100.0f, 650.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("body text two", 100.0f, 630.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("body text three", 100.0f, 610.0f, 12.0f, 1),
        ]);
        Assert.Contains("# H1 Title", md, StringComparison.Ordinal);
        Assert.Contains("## H2 Title", md, StringComparison.Ordinal);
    }

    [Fact]
    public void ThreeTiersReachH3() =>
        Assert.Contains(
            "### H3 Title",
            Convert(
            [
                TestDocuments.MakeTextItem("H1 Title", 100.0f, 800.0f, 24.0f, 1),
                TestDocuments.MakeTextItem("H2 Title", 100.0f, 750.0f, 18.0f, 1),
                TestDocuments.MakeTextItem("H3 Title", 100.0f, 700.0f, 15.0f, 1),
                TestDocuments.MakeTextItem("body text one", 100.0f, 650.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text two", 100.0f, 630.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text three", 100.0f, 610.0f, 12.0f, 1),
            ]),
            StringComparison.Ordinal);

    [Fact]
    public void FourTiersReachH4() =>
        Assert.Contains(
            "#### H4 Title",
            Convert(
            [
                TestDocuments.MakeTextItem("H1 Title", 100.0f, 850.0f, 24.0f, 1),
                TestDocuments.MakeTextItem("H2 Title", 100.0f, 800.0f, 18.0f, 1),
                TestDocuments.MakeTextItem("H3 Title", 100.0f, 750.0f, 15.0f, 1),
                TestDocuments.MakeTextItem("H4 Title", 100.0f, 700.0f, 14.5f, 1),
                TestDocuments.MakeTextItem("body text one", 100.0f, 650.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text two", 100.0f, 630.0f, 12.0f, 1),
                TestDocuments.MakeTextItem("body text three", 100.0f, 610.0f, 12.0f, 1),
            ]),
            StringComparison.Ordinal);
}

public sealed class MarkdownFromLinesTests
{
    [Fact]
    public void EmptyLinesProduceEmptyMarkdown() =>
        Assert.Empty(MarkdownConverter.ToMarkdownFromLines([], new MarkdownOptions()));

    [Fact]
    public void LinesAreRenderedInOrder()
    {
        List<TextLine> lines =
        [
            new()
            {
                Items = [TestDocuments.MakeTextItem("First", 100.0f, 700.0f, 12.0f, 1)],
                Y = 700.0f,
                Page = 1,
                AdaptiveThreshold = 0.10f,
            },
            new()
            {
                Items = [TestDocuments.MakeTextItem("Second", 100.0f, 680.0f, 12.0f, 1)],
                Y = 680.0f,
                Page = 1,
                AdaptiveThreshold = 0.10f,
            },
        ];
        var md = MarkdownConverter.ToMarkdownFromLines(lines, new MarkdownOptions());
        Assert.Contains("First", md, StringComparison.Ordinal);
        Assert.Contains("Second", md, StringComparison.Ordinal);
    }
}

public sealed class ErrorHandlingTests
{
    [Fact]
    public void ExtractTextFailsOnAMissingFile() =>
        Assert.ThrowsAny<Exception>(() => PdfProcessor.ExtractText("/nonexistent/file.pdf"));

    [Fact]
    public void DetectionFailsOnAMissingFile() =>
        Assert.ThrowsAny<Exception>(() => PdfDetector.DetectPdfType("/nonexistent/file.pdf"));

    [Fact]
    public void PositionedExtractionFailsOnAMissingFile() =>
        Assert.ThrowsAny<Exception>(() => PdfProcessor.ExtractTextWithPositions("/nonexistent/file.pdf"));
}

public sealed class NotAPdfTests
{
    /// <summary>Asserts the call fails as not-a-PDF, mentioning the given hint.</summary>
    private static void AssertNotAPdf(Action action, string expectedHint)
    {
        var ex = Assert.Throws<PdfException>(action);
        Assert.Equal(PdfException.FailureKind.NotAPdf, ex.Kind);
        Assert.Contains(expectedHint, ex.Message, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void HtmlInputIsRejected() => AssertNotAPdf(
        () => PdfProcessor.ProcessPdfMem(Encoding.ASCII.GetBytes(
            "<!DOCTYPE html><html><body>Hello</body></html>")),
        "HTML");

    [Fact]
    public void XmlInputIsRejected() => AssertNotAPdf(
        () => PdfProcessor.ProcessPdfMem(Encoding.ASCII.GetBytes(
            "<?xml version=\"1.0\"?><root><item>data</item></root>")),
        "XML");

    [Fact]
    public void JsonInputIsRejected() => AssertNotAPdf(
        () => PdfProcessor.ProcessPdfMem(Encoding.ASCII.GetBytes("{\"error\": \"download failed\"}")),
        "JSON");

    [Fact]
    public void PlainTextInputIsRejected() => AssertNotAPdf(
        () => PdfProcessor.ProcessPdfMem(Encoding.ASCII.GetBytes(
            "This is a plain text file that is not a PDF at all.")),
        "plain text");

    [Fact]
    public void EmptyBufferIsRejected() =>
        AssertNotAPdf(() => PdfProcessor.ProcessPdfMem([]), "empty");

    [Fact]
    public void DetectionRejectsHtml() => AssertNotAPdf(
        () => PdfDetector.DetectPdfTypeMem(Encoding.ASCII.GetBytes(
            "<html><head><title>Not a PDF</title></head></html>")),
        "HTML");

    [Fact]
    public void PositionedExtractionRejectsHtml() => AssertNotAPdf(
        () => PdfProcessor.ExtractTextWithPositionsMem(Encoding.ASCII.GetBytes(
            "<!DOCTYPE html><html><body>content</body></html>")),
        "HTML");

    [Fact]
    public void PlainTextExtractionRejectsXml() => AssertNotAPdf(
        () => PdfProcessor.ExtractTextMem(Encoding.ASCII.GetBytes("<?xml version=\"1.0\"?><data/>")),
        "XML");

    [Fact]
    public void AValidHeaderIsNeverRejectedAsNotAPdf()
    {
        // A truncated but valid header should fail as a parse or structure
        // problem, never as not-a-PDF.
        try
        {
            PdfProcessor.ProcessPdfMem(Encoding.ASCII.GetBytes("%PDF-1.4\ntruncated content"));
        }
        catch (PdfException ex)
        {
            Assert.NotEqual(PdfException.FailureKind.NotAPdf, ex.Kind);
        }
    }

    [Fact]
    public void ABomPrefixedHeaderIsNeverRejectedAsNotAPdf()
    {
        byte[] bomPdf = [0xEF, 0xBB, 0xBF, .. Encoding.ASCII.GetBytes("%PDF-1.7\ntruncated")];
        try
        {
            PdfProcessor.ProcessPdfMem(bomPdf);
        }
        catch (PdfException ex)
        {
            Assert.NotEqual(PdfException.FailureKind.NotAPdf, ex.Kind);
        }
    }
}

public sealed class ContainerRepairTests
{
    [Fact]
    public void ATruncatedEofMarkerIsRepaired()
    {
        var pdf = TestDocuments.TruncateEofMarker(TestDocuments.MakeMinimalTextPdf());
        var result = PdfProcessor.ProcessPdfMem(pdf);

        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.Equal(1u, result.PageCount);
        Assert.Contains("Hello World", result.Markdown ?? string.Empty, StringComparison.Ordinal);
    }

    [Fact]
    public void ALeadingTabAndTruncatedEofAreBothRepaired()
    {
        var pdf = TestDocuments.AddLeadingTab(
            TestDocuments.TruncateEofMarker(TestDocuments.MakeMinimalTextPdf()));
        var result = PdfProcessor.ProcessPdfMem(pdf);

        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.Equal(1u, result.PageCount);
        Assert.Contains("Hello World", result.Markdown ?? string.Empty, StringComparison.Ordinal);
    }

    [Fact]
    public void TheDetectorUsesTheSameRepairLoader()
    {
        var pdf = TestDocuments.AddLeadingTab(
            TestDocuments.TruncateEofMarker(TestDocuments.MakeMinimalTextPdf()));
        var path = Path.Combine(Path.GetTempPath(), $"broken-container-{Guid.NewGuid():N}.pdf");
        try
        {
            File.WriteAllBytes(path, pdf);
            var result = PdfDetector.DetectPdfType(path);
            Assert.Equal(PdfType.TextBased, result.PdfType);
            Assert.Equal(1u, result.PageCount);
            Assert.Equal(1u, result.PagesWithText);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void PlainTextExtractionUsesTheSameRepairLoader()
    {
        var pdf = TestDocuments.TruncateEofMarker(TestDocuments.MakeMinimalTextPdf());
        Assert.Contains("Hello World", PdfProcessor.ExtractTextMem(pdf), StringComparison.Ordinal);
    }

    [Fact]
    public void PageCountEstimationExcludesThePagesTree()
    {
        var pdf = TestDocuments.AddLeadingTab(
            TestDocuments.TruncateEofMarker(TestDocuments.MakeMinimalTextPdf()));
        Assert.Equal(1u, PdfDetector.EstimatePageCountFromBytes(pdf));
    }
}

public sealed class ProcessResultTests
{
    [Fact]
    public void OcrFieldsAreReachableOnBothResultTypes()
    {
        var detection = new PdfTypeResult
        {
            PdfType = PdfType.TextBased,
            PageCount = 1,
            PagesSampled = 1,
            PagesWithText = 1,
            Confidence = 1.0f,
            OcrRecommended = false,
        };
        Assert.Empty(detection.PagesNeedingOcr);

        var process = new PdfProcessResult
        {
            PdfType = PdfType.TextBased,
            PageCount = 1,
            ProcessingTimeMs = 0,
            PagesNeedingOcr = [1, 3],
            Confidence = 1.0f,
        };
        Assert.Equal([1u, 3u], process.PagesNeedingOcr);
    }

    /// <summary>
    /// The reference writes this as "the minimal PDF may fail to parse, but if
    /// it succeeds, pages_needing_ocr must be empty" — and lopdf does reject the
    /// buffer ("invalid file trailer"), so the assertion never runs there. This
    /// port's recovery scan reads the page, which puts the sparse-extraction
    /// rule in charge: 14 bytes of markdown over one page is under the 50
    /// chars-per-page floor, so OCR is recommended. That is the ported rule
    /// working, so the assertion here is on the extracted content instead.
    /// </summary>
    [Fact]
    public void ATextPdfExtractsItsContent()
    {
        var pdfBytes = Encoding.ASCII.GetBytes(
            "%PDF-1.0\n"
            + "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n"
            + "2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n"
            + "3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Contents 4 0 R>>endobj\n"
            + "4 0 obj<</Length 44>>\nstream\nBT /F1 12 Tf 100 700 Td (Hello World) Tj ET\nendstream\nendobj\n"
            + "xref\n0 5\n0000000000 65535 f\n0000000009 00000 n\n0000000058 00000 n\n"
            + "0000000115 00000 n\n0000000206 00000 n\n"
            + "trailer<</Size 5/Root 1 0 R>>\nstartxref\n300\n%%EOF");

        var result = PdfProcessor.ProcessPdfMem(pdfBytes);
        Assert.Equal(PdfType.TextBased, result.PdfType);
        Assert.Equal(1u, result.PageCount);
        Assert.Contains("Hello World", result.Markdown ?? string.Empty, StringComparison.Ordinal);

        // OCR is recommended because the page is sparse, not because anything
        // failed to decode.
        Assert.False(result.HasEncodingIssues);
        Assert.Empty(result.OcrReasonsByPage);
    }

    [Fact]
    public void OptionsNeverLeakThePassword()
    {
        var options = new PdfOptions { Password = "hunter2" };
        var text = options.ToString();
        Assert.Contains("[REDACTED]", text, StringComparison.Ordinal);
        Assert.DoesNotContain("hunter2", text, StringComparison.Ordinal);
    }
}

public sealed class FixtureBehaviourTests
{
    private static byte[] Fixture(string name) =>
        File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, name + ".pdf"));

    [Fact]
    public void TaggedPdfExposesItsStructureTree()
    {
        var path = Path.Combine(TestPaths.Fixtures, "firecrawl_docs_tagged.pdf");
        var doc = Pdf.PdfDocument.Load(File.ReadAllBytes(path));
        var tree = StructTree.FromDocument(doc);
        Assert.NotNull(tree);

        var roles = tree.McidToRoles(doc.PageIds);
        Assert.NotEmpty(roles);

        var flat = tree.Flatten();
        Assert.Contains(flat, e => e.Role.Role == StructRole.Kind.Code);
        Assert.Contains(flat, e => e.Role.Role == StructRole.Kind.H1);
        Assert.Contains(flat, e => e.Role.Role == StructRole.Kind.Li);
        Assert.Contains(flat, e => e.Role.Role == StructRole.Kind.Caption);

        // Code fences should be generated from the tagged Code elements, and
        // fences always come in open/close pairs.
        var md = PdfProcessor.ProcessPdfMem(File.ReadAllBytes(path)).Markdown ?? string.Empty;
        var fenceCount = md.Split("```").Length - 1;
        Assert.True(fenceCount > 0);
        Assert.Equal(0, fenceCount % 2);
    }

    [Fact]
    public void IdentityHWithoutToUnicodeSuppressesGarbage()
    {
        // This fixture uses an Identity-H font with no usable ToUnicode CMap.
        // The Type0/CID guard emits one U+FFFD per CID rather than Latin-1
        // mojibake, which the encoding-issue check then suppresses. Pinning
        // both halves keeps a regression that re-enables mojibake loud.
        var buf = Fixture("shinagawa_identity_h");

        var items = PdfProcessor.ExtractTextWithPositionsMem(buf);
        var combined = string.Concat(items.Select(i => i.Text));
        Assert.Contains('�', combined);
        Assert.DoesNotContain(combined, c => c is >= '' and <= 'ÿ');

        var result = PdfProcessor.ProcessPdfMem(buf);
        Assert.Contains(1u, result.PagesNeedingOcr);
        Assert.Empty((result.Markdown ?? string.Empty).Trim());
    }

    [Fact]
    public void RotatedTableLayoutIsCorrected()
    {
        // This fixture holds landscape content in a portrait page via a 90°
        // counter-clockwise text matrix. Without rotation correction the table
        // reads sideways.
        var path = Path.Combine(TestPaths.Fixtures, "tnagriculture_06_12.pdf");
        var md = PdfProcessor.ProcessPdf(path).Markdown ?? string.Empty;

        Assert.Contains("DISTRICT WISE PRODUCTION OF SPICES AND CONDIMENTS", md, StringComparison.Ordinal);
        Assert.Contains("Ariyalur", md, StringComparison.Ordinal);
        Assert.Contains("Coimbatore", md, StringComparison.Ordinal);
        Assert.Contains("CARDAMOM", md, StringComparison.Ordinal);
        Assert.Contains("RED CHILLIES", md, StringComparison.Ordinal);
        Assert.Contains(
            md.Split('\n'),
            l => l.Contains('|') && l.Contains("Ariyalur", StringComparison.Ordinal));
    }

    [Fact]
    public void EncryptedFixtureOpensWithItsPassword()
    {
        var path = Path.Combine(TestPaths.Fixtures, "encrypted-secret123.pdf");
        var result = PdfProcessor.ProcessPdf(path, new PdfOptions { Password = "secret123" });
        Assert.True(result.PageCount >= 1);
    }
}
