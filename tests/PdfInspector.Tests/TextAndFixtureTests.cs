using PdfInspector.Extractor;
using PdfInspector.Pdf;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

public sealed class TextUtilsTests
{
    [Theory]
    [InlineData("Arial-BoldMT", true)]
    [InlineData("NimbusRomNo9L-Medi", true)]
    [InlineData("Helvetica", false)]
    [InlineData("Arial-MediumItalic", false)]
    public void DetectsBoldFonts(string name, bool expected) =>
        Assert.Equal(expected, TextUtils.IsBoldFont(name));

    [Theory]
    [InlineData("Times-Italic", true)]
    [InlineData("Helvetica-Oblique", true)]
    [InlineData("Arial", false)]
    public void DetectsItalicFonts(string name, bool expected) =>
        Assert.Equal(expected, TextUtils.IsItalicFont(name));

    [Fact]
    public void ExpandsLigaturesAndStripsInvisibles()
    {
        // Ligatures decompose; the soft hyphen and zero-width space vanish.
        Assert.Equal("efficient", TextUtils.ExpandLigatures("eﬃcient"));
        Assert.Equal("ab", TextUtils.ExpandLigatures("a­b"));
        Assert.Equal("ab", TextUtils.ExpandLigatures("a​b"));
    }

    [Fact]
    public void NormalizesTypographicSpacesButKeepsNoBreakSpace()
    {
        Assert.Equal("a b", TextUtils.ExpandLigatures("a b"));
        Assert.Equal("a b", TextUtils.ExpandLigatures("a b"));
    }

    [Fact]
    public void DecodesUtf16TextStrings()
    {
        byte[] utf16 = [0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        Assert.Equal("Hi", TextUtils.DecodeTextString(utf16));
    }

    [Fact]
    public void ClassifiesRightToLeftText()
    {
        Assert.True(TextUtils.IsRtlText(["שלום"]));
        Assert.False(TextUtils.IsRtlText(["hello"]));
    }

    [Fact]
    public void JoinsNumericContinuations()
    {
        var prev = new TextItem { Text = "34,20", X = 0, Width = 20, FontSize = 10 };
        var next = new TextItem { Text = "8", X = 20.5f, Width = 5, FontSize = 10 };

        Assert.True(TextUtils.ShouldJoinItems(prev, next, TextUtils.DefaultJoinThreshold));
    }

    [Fact]
    public void SeparatesWordsAcrossAWideGap()
    {
        var prev = new TextItem { Text = "word", X = 0, Width = 20, FontSize = 10 };
        var next = new TextItem { Text = "next", X = 25, Width = 20, FontSize = 10 };

        Assert.False(TextUtils.ShouldJoinItems(prev, next, TextUtils.DefaultJoinThreshold));
    }
}

public sealed class GlyphNameTests
{
    [Theory]
    [InlineData("A", 'A')]
    [InlineData("adieresis", 'ä')]
    [InlineData("uni0041", 'A')]
    // Windows Symbol fonts map ASCII into the private-use F000 block.
    [InlineData("uniF041", 'A')]
    [InlineData("zero.tf", '0')]
    public void ResolvesGlyphNames(string name, char expected) =>
        Assert.Equal(expected, GlyphNames.GlyphToChar(name));

    [Fact]
    public void ReturnsNullForUnknownNames() => Assert.Null(GlyphNames.GlyphToChar("notaglyph"));
}

public sealed class ToUnicodeCMapTests
{
    [Fact]
    public void ParsesBfCharAndBfRange()
    {
        const string source = """
            /CIDInit /ProcSet findresource begin
            1 begincodespacerange <0000> <FFFF> endcodespacerange
            1 beginbfchar <0003> <0020> endbfchar
            1 beginbfrange <0024> <0026> <0041> endbfrange
            endcmap
            """;

        var cmap = ToUnicodeCMap.Parse(System.Text.Encoding.ASCII.GetBytes(source))!;

        Assert.Equal(2, cmap.CodeByteLength);
        Assert.Equal(" ", cmap.Lookup(0x0003));
        Assert.Equal("A", cmap.Lookup(0x0024));
        Assert.Equal("C", cmap.Lookup(0x0026));
        Assert.Null(cmap.Lookup(0x0027));
    }

    [Fact]
    public void InfersSingleByteWidthWhenEntriesAreOneByte()
    {
        const string source = """
            1 begincodespacerange <0000> <FFFF> endcodespacerange
            1 beginbfchar <41> <0041> endbfchar
            """;

        var cmap = ToUnicodeCMap.Parse(System.Text.Encoding.ASCII.GetBytes(source))!;

        Assert.Equal(1, cmap.CodeByteLength);
    }

    [Fact]
    public void DecodesSurrogatePairDestinations()
    {
        const string source = "1 beginbfchar <0001> <D83CDF1F> endbfchar";
        var cmap = ToUnicodeCMap.Parse(System.Text.Encoding.ASCII.GetBytes(source))!;

        Assert.Equal("\U0001F31F", cmap.Lookup(1));
    }

    [Fact]
    public void RemapsToSequentialCodes()
    {
        const string source = "2 beginbfchar <0064> <0041> <00C8> <0042> endbfchar";
        var cmap = ToUnicodeCMap.Parse(System.Text.Encoding.ASCII.GetBytes(source))!;

        var remapped = cmap.RemapToSequential();

        // Source codes are sorted and reassigned from 1; glyph 0 is .notdef.
        Assert.Equal("A", remapped.Lookup(1));
        Assert.Equal("B", remapped.Lookup(2));
    }
}

public sealed class TextQualityTests
{
    [Fact]
    public void FlagsReplacementCharacters() =>
        Assert.True(PdfInspector.Quality.TextQuality.DetectEncodingIssues("some � text"));

    [Fact]
    public void FlagsDollarAsSpacePattern()
    {
        var text = string.Concat(Enumerable.Repeat("word$next$more$", 10));
        Assert.True(PdfInspector.Quality.TextQuality.DetectEncodingIssues(text));
    }

    [Fact]
    public void AcceptsOrdinaryProse() =>
        Assert.False(PdfInspector.Quality.TextQuality.DetectEncodingIssues(
            "The quick brown fox jumps over the lazy dog, and the price is $5.00 for a copy."));

    [Fact]
    public void DetectsSymbolSoupAsGarbage() =>
        Assert.True(PdfInspector.Quality.TextQuality.IsGarbageText(
            "~!@^&()_+=[]{};:'\"<>?/\\`~!@^&()_+=[]{};:'\"<>?/\\`~!@^&()_+="));
}

/// <summary>
/// End-to-end checks against the reference crate's fixture corpus. These are
/// the strongest signal that the port matches upstream behaviour.
/// </summary>
public sealed class FixtureExtractionTests
{
    public static TheoryData<string> AllFixtures()
    {
        var data = new TheoryData<string>();
        foreach (var path in Directory.GetFiles(TestPaths.Fixtures, "*.pdf").OrderBy(p => p))
        {
            data.Add(Path.GetFileName(path));
        }

        return data;
    }

    [Theory]
    [MemberData(nameof(AllFixtures))]
    public void EveryFixtureLoadsAndDecodes(string fixture)
    {
        var path = Path.Combine(TestPaths.Fixtures, fixture);
        var password = fixture.Contains("secret123", StringComparison.Ordinal) ? "secret123" : null;

        var doc = PdfDocument.Load(File.ReadAllBytes(path), password);

        Assert.True(doc.PageCount > 0, $"{fixture} reported no pages");

        // Decoding the first page's content exercises the filter chain, the
        // object streams, and the operator parser.
        var content = doc.GetPageContent(1);
        Assert.NotNull(content);
        Assert.NotEmpty(ContentStream.Decode(content));
    }

    [Fact]
    public void EncryptedFixtureNeedsTheCorrectPassword()
    {
        var path = Path.Combine(TestPaths.Fixtures, "encrypted-secret123.pdf");
        var bytes = File.ReadAllBytes(path);

        Assert.Throws<PdfEncryptedException>(() => PdfDocument.Load(bytes));
        Assert.Throws<PdfEncryptedException>(() => PdfDocument.Load(bytes, "wrong"));

        var doc = PdfDocument.Load(bytes, "secret123");
        Assert.Equal(8, doc.PageCount);
    }

    // These phrases sit inside a single extracted item. Multi-line phrases are
    // only joined later, when the table and markdown passes assemble cells.
    [Theory]
    [InlineData("nexo-price-en.pdf", 1, "Selling price")]
    [InlineData("wireless_two_col_no_rects.pdf", 1, "3-Year")]
    [InlineData("2013-app2.pdf", 1, "Procurement")]
    public void ExtractsExpectedTextFromPage(string fixture, int pageNum, string expected)
    {
        var items = ExtractPage(fixture, pageNum, password: null);
        var joined = string.Join(" ", items.Select(i => i.Text));

        Assert.Contains(expected, joined, StringComparison.Ordinal);
    }

    [Fact]
    public void DecryptedContentMatchesThePlaintextFixture()
    {
        // The encrypted fixture is the same document as 2013-app2.pdf, so the
        // decrypted text must agree.
        var plain = ExtractPage("2013-app2.pdf", 1, password: null);
        var decrypted = ExtractPage("encrypted-secret123.pdf", 1, password: "secret123");

        Assert.Equal(
            plain.Select(i => i.Text).ToList(),
            decrypted.Select(i => i.Text).ToList());
    }

    [Fact]
    public void ReadsKoreanAndEnglishFromTheSamePage()
    {
        var items = ExtractPage("nexo-price-en.pdf", 1, password: null);
        var joined = string.Join(" ", items.Select(i => i.Text));

        Assert.Contains("한국어", joined, StringComparison.Ordinal);
        Assert.Contains("The final sales price may vary", joined, StringComparison.Ordinal);
    }

    private static List<TextItem> ExtractPage(string fixture, int pageNum, string? password)
    {
        var path = Path.Combine(TestPaths.Fixtures, fixture);
        var doc = PdfDocument.Load(File.ReadAllBytes(path), password);
        var cmaps = FontCMaps.FromDocumentPages(doc, [(uint)pageNum]);
        var page = doc.GetPage(pageNum)!;

        return ContentStreamExtractor.ExtractPageTextItems(
            doc, page, (uint)pageNum, cmaps, includeInvisible: false, new FontStyleCache()).Items;
    }
}
