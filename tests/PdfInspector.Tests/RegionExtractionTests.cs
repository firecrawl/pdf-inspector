// Ported from reference/tests/integration_tests.rs
using PdfInspector.Regions;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Covers region-scoped text extraction.</summary>
public sealed class RegionTextExtractionTests
{
    private static byte[] Fixture(string name) =>
        File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, name + ".pdf"));

    /// <summary>
    /// Full-page region arguments for each page, using a bbox generous enough
    /// to cover any page size.
    /// </summary>
    private static List<(uint Page, IReadOnlyList<float[]> Regions)> FullPageRegions(uint pageCount) =>
    [
        .. Enumerable.Range(0, (int)pageCount)
            .Select(p => ((uint)p, (IReadOnlyList<float[]>)new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })),
    ];

    /// <summary>Lowercased alphanumeric words longer than three characters.</summary>
    private static HashSet<string> NormalizeWords(string text) =>
    [
        .. text
            .Split(text.Where(c => !char.IsLetterOrDigit(c)).Distinct().ToArray())
            .Select(w => w.ToLowerInvariant())
            .Where(w => w.Length > 3),
    ];

    /// <summary>The fraction of <paramref name="a"/>'s words that also appear in <paramref name="b"/>.</summary>
    private static double WordOverlapRatio(string a, string b)
    {
        var wordsA = NormalizeWords(a);
        if (wordsA.Count == 0)
        {
            return NormalizeWords(b).Count == 0 ? 1.0 : 0.0;
        }

        var wordsB = NormalizeWords(b);
        return wordsA.Count(wordsB.Contains) / (double)wordsA.Count;
    }

    [Fact]
    public void EveryPageOfATextPdfYieldsARegion()
    {
        var buf = Fixture("nexo-price-en");
        var pageCount = PdfProcessor.ProcessPdfMem(buf).PageCount;

        var regions = RegionExtraction.ExtractTextInRegionsMem(buf, FullPageRegions(pageCount));
        Assert.Equal((int)pageCount, regions.Count);
        Assert.All(regions, r => Assert.Single(r.Regions));

        Assert.NotEmpty(regions[0].Regions[0].Text.Trim());
        Assert.Equal(0u, regions[0].Page);
    }

    [Fact]
    public void AnIdentityHPageNeedsOcr()
    {
        var regions = RegionExtraction.ExtractTextInRegionsMem(
            Fixture("shinagawa_identity_h"), [(0u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);
        Assert.Single(regions);
        Assert.True(regions[0].Regions[0].NeedsOcr);
    }

    [Fact]
    public void AFullPageRegionHoldsAtLeastAsMuchAsASmallOne()
    {
        var regions = RegionExtraction.ExtractTextInRegionsMem(
            Fixture("nexo-price-en"),
            [(0u, new[]
            {
                new[] { 0.0f, 0.0f, 300.0f, 100.0f },
                new[] { 0.0f, 0.0f, 1200.0f, 1200.0f },
            })]);

        Assert.Single(regions);
        Assert.Equal(2, regions[0].Regions.Count);
        Assert.True(regions[0].Regions[1].Text.Length >= regions[0].Regions[0].Text.Length);
    }

    [Fact]
    public void ANonexistentPageNeedsOcr()
    {
        var regions = RegionExtraction.ExtractTextInRegionsMem(
            Fixture("nexo-price-en"), [(9999u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);
        Assert.Single(regions);
        Assert.True(regions[0].Regions[0].NeedsOcr);
    }

    [Fact]
    public void AZeroAreaRegionNeedsOcr()
    {
        var regions = RegionExtraction.ExtractTextInRegionsMem(
            Fixture("nexo-price-en"), [(0u, new[] { new[] { 0.0f, 0.0f, 0.0f, 0.0f } })]);
        Assert.Single(regions);
        Assert.True(regions[0].Regions[0].NeedsOcr);
    }

    [Fact]
    public void ANonPdfIsRejected() =>
        Assert.Throws<PdfException>(() => RegionExtraction.ExtractTextInRegionsMem(
            System.Text.Encoding.ASCII.GetBytes("not a pdf"),
            [(0u, new[] { new[] { 0.0f, 0.0f, 100.0f, 100.0f } })]));

    [Fact]
    public void ARotatedPageIsNotFalselyEmpty()
    {
        var regions = RegionExtraction.ExtractTextInRegionsMem(
            Fixture("tnagriculture_06_12"), [(0u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);

        Assert.Single(regions);
        var region = Assert.Single(regions[0].Regions);
        Assert.NotEmpty(region.Text.Trim());
        Assert.False(region.NeedsOcr);
        Assert.Contains(
            "DISTRICT WISE PRODUCTION OF SPICES AND CONDIMENTS", region.Text, StringComparison.Ordinal);
    }

    [Fact]
    public void PartiallyOverlappingItemsAreKept()
    {
        var item = TestDocuments.MakeTextItem("EdgeWord", 100.0f, 700.0f, 12.0f, 1);

        // The region meets only the item's left edge: its centre at x=124 sits
        // outside x=[95,120], so a centre-only test would drop it.
        var text = RegionExtraction.CollectTextInRegion([item], 95.0f, 80.0f, 120.0f, 110.0f, 800.0f);
        Assert.Contains("EdgeWord", text, StringComparison.Ordinal);
    }

    [Fact]
    public void RegionTextReusesRightToLeftSorting()
    {
        List<TextItem> items =
        [
            TestDocuments.MakeTextItem("بكم", 240.0f, 700.0f, 12.0f, 1),
            TestDocuments.MakeTextItem("مرحبا", 300.0f, 700.0f, 12.0f, 1),
        ];
        Assert.Equal(
            "مرحبا بكم",
            RegionExtraction.CollectTextInRegion(items, 0.0f, 0.0f, 600.0f, 800.0f, 800.0f));
    }

    /// <summary>
    /// For each text-based fixture, compares the fast region path against the
    /// full pipeline. Where the fast path claims the text is trustworthy, its
    /// words must substantially appear in the full markdown — which catches a
    /// silent quality regression.
    /// </summary>
    [Theory]
    [InlineData("nexo-price-en")]
    [InlineData("td9264")]
    [InlineData("p1244-1996")]
    [InlineData("real-estate-pricing")]
    [InlineData("2013-app2")]
    [InlineData("firecrawl_docs_tagged")]
    [InlineData("thermo-freon12")]
    public void TheFastPathAgreesWithTheFullPipeline(string fixture)
    {
        var buf = Fixture(fixture);
        var normal = PdfProcessor.ProcessPdfMem(buf);
        var normalMarkdown = normal.Markdown ?? string.Empty;

        var regions = RegionExtraction.ExtractTextInRegionsMem(buf, FullPageRegions(normal.PageCount));
        Assert.Equal((int)normal.PageCount, regions.Count);

        foreach (var pageResult in regions)
        {
            var region = pageResult.Regions[0];
            if (region.NeedsOcr || region.Text.Trim().Length == 0)
            {
                // Flagging a page the full pipeline read fine is conservative,
                // not a defect.
                continue;
            }

            var overlap = WordOverlapRatio(region.Text, normalMarkdown);
            Assert.True(
                overlap >= 0.3,
                $"{fixture} page {pageResult.Page}: the fast path trusts its text but only "
                + $"{overlap * 100:F0}% of its words appear in the full extraction");
        }
    }
}

/// <summary>Covers region-scoped table extraction.</summary>
public sealed class RegionTableExtractionTests
{
    private static byte[] Fixture(string name) =>
        File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, name + ".pdf"));

    [Fact]
    public void ATableRegionRendersAPipeTable()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("tnagriculture_06_12"), [(0u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);

        Assert.Single(results);
        var region = Assert.Single(results[0].Regions);
        if (region.NeedsOcr)
        {
            return;
        }

        Assert.Contains('|', region.Text);
        Assert.Contains(region.Text.Split('\n'), l => l.Contains("---", StringComparison.Ordinal));
    }

    [Fact]
    public void ARegionWithTooFewItemsNeedsOcr()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("nexo-price-en"), [(0u, new[] { new[] { 0.0f, 0.0f, 50.0f, 50.0f } })]);

        Assert.Single(results);
        var region = Assert.Single(results[0].Regions);
        Assert.True(region.NeedsOcr);
        Assert.Empty(region.Text);
    }

    [Fact]
    public void AZeroAreaRegionNeedsOcr()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("nexo-price-en"), [(0u, new[] { new[] { 0.0f, 0.0f, 0.0f, 0.0f } })]);
        var region = Assert.Single(Assert.Single(results).Regions);
        Assert.True(region.NeedsOcr);
        Assert.Empty(region.Text);
    }

    [Fact]
    public void AnIdentityHRegionNeedsOcr()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("shinagawa_identity_h"), [(0u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);
        Assert.True(Assert.Single(Assert.Single(results).Regions).NeedsOcr);
    }

    [Fact]
    public void ANonPdfIsRejected() =>
        Assert.Throws<PdfException>(() => RegionExtraction.ExtractTablesInRegionsMem(
            System.Text.Encoding.ASCII.GetBytes("not a pdf"),
            [(0u, new[] { new[] { 0.0f, 0.0f, 100.0f, 100.0f } })]));

    [Fact]
    public void ANonexistentPageNeedsOcr()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("nexo-price-en"), [(9999u, new[] { new[] { 0.0f, 0.0f, 1200.0f, 1200.0f } })]);
        var region = Assert.Single(Assert.Single(results).Regions);
        Assert.True(region.NeedsOcr);
        Assert.Empty(region.Text);
    }

    /// <summary>
    /// Page 4 has multi-line wrapped headers over numeric data columns. The
    /// heuristic detector used to fail here twice over: header items at
    /// different x positions than the data produced six column clusters instead
    /// of four, and the spanning super-header row produced duplicate header
    /// cells the partial-table guard then rejected.
    /// </summary>
    [Fact]
    public void BitsPilaniPage4TableIsDetected()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("bits_pilani_feedback"), [(3u, new[] { new[] { 0.0f, 0.0f, 612.0f, 792.0f } })]);

        var region = Assert.Single(Assert.Single(results).Regions);
        Assert.False(region.NeedsOcr);
        Assert.Contains("BIO", region.Text, StringComparison.Ordinal);
        Assert.Contains("8.23", region.Text, StringComparison.Ordinal);
    }

    [Fact]
    public void BitsPilaniPage8TableIsStillDetected()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            Fixture("bits_pilani_feedback"), [(7u, new[] { new[] { 0.0f, 0.0f, 612.0f, 792.0f } })]);
        Assert.False(Assert.Single(Assert.Single(results).Regions).NeedsOcr);
    }

    /// <summary>
    /// A stroked-grid table. The heuristic text-only detector already handles
    /// these cells, so this guards that the line-backed path does not regress
    /// them away.
    /// </summary>
    [Fact]
    public void AStrokedGridTableIsExtracted()
    {
        var results = RegionExtraction.ExtractTablesInRegionsMem(
            SyntheticPdf.VectorGrid(false), [(0u, new[] { new[] { 40.0f, 50.0f, 220.0f, 760.0f } })]);

        var region = Assert.Single(Assert.Single(results).Regions);
        Assert.False(region.NeedsOcr);
        foreach (var token in new[] { "A1", "B1", "A2", "B2" })
        {
            Assert.Contains(token, region.Text, StringComparison.Ordinal);
        }

        Assert.Contains('|', region.Text);
    }
}
