// Ported from the unit-test modules in reference/src/lib.rs
using PdfInspector.Regions;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Covers the x-cluster column-undercount guard.</summary>
public class TextClusterColumnUndercountTests
{
    private static TextItem Item(float x, float y, string text) => new()
    {
        Text = text,
        X = x,
        Y = y,
        Width = TextUtils.ByteLength(text) * 5.0f,
        Height = 10.0f,
        Font = "F",
        FontSize = 10.0f,
        Page = 1,
    };

    /// <summary>
    /// 22 rows of 4 columns of neutral synthetic content, simulating the
    /// "narrow numeric columns dropped" shape: the detector produced 2 markdown
    /// columns but the page geometry shows 4.
    /// </summary>
    private static List<TextItem> MakeFourColumnGrid()
    {
        float[] xs = [60.0f, 145.0f, 330.0f, 430.0f];
        var items = new List<TextItem>();
        for (var row = 0; row < 22; row++)
        {
            var y = 700.0f - (row * 20.0f);
            foreach (var x in xs)
            {
                items.Add(Item(x, y, "x"));
            }
        }

        return items;
    }

    private static MarkdownTableShape Shape(int cols) => new(22, cols, cols);

    [Fact]
    public void NarrowUndercountFiresWhenGeometryShowsTwiceTheColumns()
    {
        var items = MakeFourColumnGrid();
        Assert.True(TableCandidates.TextClusterColumnUndercount(items, Shape(2)));
    }

    [Fact]
    public void MatchingGeometryDoesNotFire()
    {
        var items = MakeFourColumnGrid();
        Assert.False(TableCandidates.TextClusterColumnUndercount(items, Shape(4)));
    }

    [Fact]
    public void SingleColumnIsSkipped()
    {
        var items = MakeFourColumnGrid();
        Assert.False(TableCandidates.TextClusterColumnUndercount(items, Shape(1)));
    }

    [Fact]
    public void InsufficientItemsAreSkipped()
    {
        List<TextItem> items =
        [
            Item(60.0f, 700.0f, "x"),
            Item(145.0f, 700.0f, "x"),
            Item(330.0f, 700.0f, "x"),
        ];
        Assert.False(TableCandidates.TextClusterColumnUndercount(items, Shape(2)));
    }

    [Fact]
    public void OutlierSingletonsAreFilteredOut()
    {
        var items = new List<TextItem>();
        for (var row = 0; row < 22; row++)
        {
            var y = 700.0f - (row * 20.0f);
            items.Add(Item(60.0f, y, "x"));
            items.Add(Item(330.0f, y, "x"));
        }

        // A handful of strays: continuation fragments, footnotes.
        items.Add(Item(145.0f, 100.0f, "footnote"));
        items.Add(Item(430.0f, 50.0f, "page#"));
        Assert.False(TableCandidates.TextClusterColumnUndercount(items, Shape(2)));
    }

    [Fact]
    public void WideTablePathStillFires()
    {
        var items = new List<TextItem>();
        var xs = Enumerable.Range(0, 12).Select(i => 50.0f + (i * 40.0f)).ToList();
        for (var row = 0; row < 22; row++)
        {
            var y = 700.0f - (row * 20.0f);
            foreach (var x in xs)
            {
                items.Add(Item(x, y, "x"));
            }
        }

        Assert.True(TableCandidates.TextClusterColumnUndercount(items, Shape(8)));
    }
}

/// <summary>Covers the region text-density floor that catches font-CMap failures.</summary>
public class RegionTextDensityTests
{
    [Fact]
    public void SmallRegionSkipsTheCheck() =>
        // 100×100 = 10,000 sq pt, below the 30,000 floor.
        Assert.False(TableCandidates.RegionTextDensityTooLow(5, 10_000.0f));

    [Fact]
    public void DenseFullTablePasses() =>
        // A full A4 ledger: 438,000 sq pt with 6,500 chars, density 0.015.
        Assert.False(TableCandidates.RegionTextDensityTooLow(6_500, 438_000.0f));

    [Fact]
    public void ModerateDensityTablePasses() =>
        // Key/value tables with multi-line cells sit around 0.005 chars/sq pt.
        Assert.False(TableCandidates.RegionTextDensityTooLow(1_600, 291_000.0f));

    [Fact]
    public void FontDecodeFailureIsCaught() =>
        // A big region with almost no extractable text: density 0.0005.
        Assert.True(TableCandidates.RegionTextDensityTooLow(46, 102_000.0f));

    [Fact]
    public void SparseGlyphRepeatIsCaught() =>
        // A full page where every glyph decoded to one letter: density 0.00025.
        Assert.True(TableCandidates.RegionTextDensityTooLow(89, 353_000.0f));

    [Fact]
    public void SparseLayoutBandIsCaught() =>
        // A horizontal band whose table extends below the bbox: density 0.0013.
        Assert.True(TableCandidates.RegionTextDensityTooLow(96, 73_000.0f));

    [Fact]
    public void BoundaryAtTheDensityFloor()
    {
        // Exactly at the floor: 90 / 30,000 = 0.003. The check rejects only
        // when the density is strictly below, so the boundary is acceptable.
        Assert.False(TableCandidates.RegionTextDensityTooLow(90, 30_000.0f));
        Assert.True(TableCandidates.RegionTextDensityTooLow(89, 30_000.0f));
    }

    [Fact]
    public void WholePageBBoxSkipsTheCheck() =>
        // Near-whole-A4 bboxes include large white-space margins, so density is
        // unreliable. 612×792 = 484,704 sq pt.
        Assert.False(TableCandidates.RegionTextDensityTooLow(423, 484_704.0f));

    [Fact]
    public void TinyCharacterCountSkipsTheCheck() =>
        // Fewer than 20 characters is a fixture artefact, not a decode failure.
        Assert.False(TableCandidates.RegionTextDensityTooLow(8, 127_800.0f));
}

/// <summary>Covers the captured-fragment guard.</summary>
public class CapturedOnlyAFragmentTests
{
    [Fact]
    public void SmallRegionSkipsTheCheck()
    {
        const string md = "|Year|Value|\n|---|---|\n|2024|10|";
        Assert.False(TableCandidates.CapturedOnlyAFragment(md, 50));
    }

    [Fact]
    public void FullTablePasses()
    {
        const string md =
            "|Name|Year|Country|\n|---|---|---|\n|Alice|2020|US|\n|Bob|2021|UK|\n|Carol|2019|FR|";
        Assert.False(TableCandidates.CapturedOnlyAFragment(md, 50));
        Assert.False(TableCandidates.CapturedOnlyAFragment(md, TextUtils.ByteLength(md)));
    }

    [Fact]
    public void HeaderOnlyExtractionIsRejected()
    {
        // The header band was captured while the region holds many data rows.
        const string md = "|Description|Year|Amount|\n|---|---|---|";
        Assert.True(TableCandidates.CapturedOnlyAFragment(md, 1500));
    }

    [Fact]
    public void SparseFragmentIsRejected()
    {
        const string md = "|percent|for|\n|---|---|\n|sites|15|";
        Assert.True(TableCandidates.CapturedOnlyAFragment(md, 2000));
    }

    [Fact]
    public void BoundaryAtThe25PercentFloor()
    {
        // Exactly at the line: 250 of 1,000 chars. The check rejects when
        // captured × 4 is below the region, so the boundary is acceptable.
        Assert.False(TableCandidates.CapturedOnlyAFragment(new string('x', 250), 1000));
        Assert.True(TableCandidates.CapturedOnlyAFragment(new string('x', 249), 1000));
    }
}

/// <summary>Covers how a region picks between competing detectors.</summary>
public class TableCandidateSelectionTests
{
    private static TableCandidate Candidate(
        TableCandidateSource source,
        int rows,
        int cols,
        TableCandidateIssue? issue = null) =>
        new($"{source}-{rows}x{cols}", source, new MarkdownTableShape(rows, cols, cols), issue);

    private static TextItem Item(string text, float y) => ItemAt(text, 10.0f, y);

    private static TextItem ItemAt(string text, float x, float y) => new()
    {
        Text = text,
        X = x,
        Y = y,
        Width = 50.0f,
        Height = 10.0f,
        Font = "F1",
        FontSize = 10.0f,
        Page = 1,
    };

    [Fact]
    public void MarkdownShapeCountsRowsAndNonEmptyColumns()
    {
        const string md = "|A|B||D|\n|---|---|---|---|\n|one|two|three||";
        var shape = TableCandidates.MarkdownTableShapeOf(md);
        Assert.Equal(2, shape.Rows);
        Assert.Equal(3, shape.Cols);
        Assert.Equal(4, shape.RawCols);
    }

    [Fact]
    public void PrefersCleanHeuristicWhenSubstantiallyWiderThanVector()
    {
        List<TableCandidate> candidates =
        [
            Candidate(TableCandidateSource.Rect, 5, 3),
            Candidate(TableCandidateSource.Heuristic, 5, 4),
        ];
        var selected = TableCandidates.SelectTableCandidate(candidates);
        Assert.NotNull(selected);
        Assert.Equal(TableCandidateSource.Heuristic, selected.Source);
    }

    [Fact]
    public void LineRowUndercountRoutesToOcrWithoutAWiderHeuristic()
    {
        List<TableCandidate> candidates =
        [
            Candidate(TableCandidateSource.Line, 2, 3, TableCandidateIssue.LineRowUndercount),
            Candidate(TableCandidateSource.Heuristic, 4, 3),
        ];
        Assert.Null(TableCandidates.SelectTableCandidate(candidates));
    }

    [Fact]
    public void LineRowUndercountCanUseAClearlyWiderHeuristic()
    {
        List<TableCandidate> candidates =
        [
            Candidate(TableCandidateSource.Line, 2, 3, TableCandidateIssue.LineRowUndercount),
            Candidate(TableCandidateSource.Heuristic, 2, 4),
        ];
        var selected = TableCandidates.SelectTableCandidate(candidates);
        Assert.NotNull(selected);
        Assert.Equal(TableCandidateSource.Heuristic, selected.Source);
    }

    [Fact]
    public void PrefersCleanColumnFallbackWhenItRecoversMoreRows()
    {
        List<TableCandidate> candidates =
        [
            Candidate(TableCandidateSource.Heuristic, 6, 5),
            Candidate(TableCandidateSource.Column, 7, 5),
        ];
        var selected = TableCandidates.SelectTableCandidate(candidates);
        Assert.NotNull(selected);
        Assert.Equal(TableCandidateSource.Column, selected.Source);
    }

    [Fact]
    public void LineCandidateCollapsingCapturedYClustersIsSuspicious()
    {
        const string longText =
            "value value value value value value value value value value value value";
        List<TextItem> items =
        [
            Item(longText, 100.0f),
            Item(longText, 112.0f),
            Item(longText, 124.0f),
            Item(longText, 136.0f),
        ];
        var table = Table.Create(
            [0.0f, 100.0f, 200.0f, 300.0f],
            [150.0f, 120.0f],
            [["A", "B", "C"], ["D", "E", "F"]],
            [0, 1, 2, 3]);
        var shape = new MarkdownTableShape(2, 3, 3);
        Assert.True(TableCandidates.LineTableCollapsesTextRows(table, items, shape));
    }

    [Fact]
    public void WideSparsePrefixWithBlankHeaderIsSuspicious()
    {
        const string md =
            "|Name|Flag A|Flag B||Metric A|Metric B|Metric C|Metric D|\n"
            + "|---|---|---|---|---|---|---|---|\n"
            + "|Row 1|Y|||1|2|3|4|\n"
            + "|Row 2|||N|5|6|7|8|\n"
            + "|Row 3||||9|10|11|12|";
        Assert.True(TableCandidates.WideTableSparsePrefixUndercount(md));
    }

    [Fact]
    public void WideBlankHeaderWithDenseBodyIsAllowed()
    {
        const string md =
            "|Name|Flag A|Flag B||Metric A|Metric B|Metric C|Metric D|\n"
            + "|---|---|---|---|---|---|---|---|\n"
            + "|Row 1|Y|N|Y|1|2|3|4|\n"
            + "|Row 2|N|Y|N|5|6|7|8|\n"
            + "|Row 3|Y|Y|N|9|10|11|12|";
        Assert.False(TableCandidates.WideTableSparsePrefixUndercount(md));
    }

    [Fact]
    public void XClustersCanSignalWideColumnUndercount()
    {
        var items = new List<TextItem>();
        foreach (var y in new[] { 100.0f, 112.0f, 124.0f })
        {
            foreach (var x in new[] { 10.0f, 45.0f, 80.0f, 115.0f, 150.0f, 185.0f, 220.0f, 255.0f, 290.0f, 325.0f })
            {
                items.Add(ItemAt("value", x, y));
            }
        }

        Assert.True(TableCandidates.TextClusterColumnUndercount(items, new MarkdownTableShape(4, 8, 8)));
    }

    [Fact]
    public void WrappedProseGridFragmentIsSuspicious()
    {
        const string md =
            "|A useful capability with several words|Another descriptive capability column|A final descriptive capability column|\n"
            + "|---|---|---|\n"
            + "|The group includes experienced specialists|received a strong neutral recommendation|system in a neutral evaluation setting|\n"
            + "|presented several papers in public venues|shown stronger performance than alternatives|ranked highly in a neutral benchmark|\n"
            + "|recognized by external reviewers|delivered useful results for operators|used in production style workflows|";
        Assert.True(TableCandidates.ProseGridFragmentNeedsOcr(md));
    }

    [Fact]
    public void CompactIdentifierColumnsAreNotProseFragments()
    {
        const string md =
            "|Box 1, F-7|Description|Date|\n"
            + "|---|---|---|\n"
            + "|Box 1, F-8|Long neutral description with several words for one record|2020 Jan 1|\n"
            + "|Box 1, F-9|Another neutral description with several words for another record|2021 Feb 2|\n"
            + "|Box 1, F-10|Additional neutral description with several words for a record|n.d.|";
        Assert.False(TableCandidates.ProseGridFragmentNeedsOcr(md));
    }

    [Fact]
    public void TwoColumnAllProseFragmentIsSuspicious()
    {
        const string md =
            "|A long descriptive header fragment|Another long descriptive header fragment|\n"
            + "|---|---|\n"
            + "|Long wrapped prose content from one visual cell|More wrapped prose content from a neighboring visual cell|";
        Assert.True(TableCandidates.ProseGridFragmentNeedsOcr(md));
    }

    [Fact]
    public void TwoColumnKeyValueTableIsAllowed()
    {
        const string md =
            "|Field|Detail|\n"
            + "|---|---|\n"
            + "|Status|Long neutral explanation with several words for this value|\n"
            + "|Owner|Another neutral explanation with several words for this value|";
        Assert.False(TableCandidates.ProseGridFragmentNeedsOcr(md));
    }
}

/// <summary>Covers the partial-table guard in both strict and layout-assisted modes.</summary>
public class LooksLikePartialTableTests
{
    [Fact]
    public void GoodTablePasses()
    {
        const string md = "|Name|Year|Country|\n|---|---|---|\n|Alice|2020|US|\n|Bob|2021|UK|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void HeaderStartingWithANumberIsPartial()
    {
        const string md = "|2|Cambodian Women for Peace|9,835|\n|---|---|---|\n|3|Association|711|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void HeaderWithEmptyCellsInAThreeColumnTableIsPartial()
    {
        const string md =
            "|Position||Administration|Administration|\n|---|---|---|---|\n|Senate|24|8.3|16.7|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void HeaderWithDuplicateCellsIsPartial()
    {
        const string md =
            "|Position|Administration|Administration|Notes|\n|---|---|---|---|\n|Senate|24|16|x|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void TwoColumnWithOneEmptyCellPasses()
    {
        // Many real two-column tables have key-only rows; don't penalise them.
        const string md = "|Key||\n|---|---|\n|Alice|123|\n|Bob|456|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void SingleColumnTableIsKept()
    {
        const string md = "|Item|\n|---|\n|First|\n|Second|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void NoTableAtAllIsNotFlagged()
    {
        // No line starts with a pipe, so there is no header to inspect.
        const string md = "Just some text\nWith multiple lines";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void FirstDataRowWithManyEmptyCellsIsPartial()
    {
        const string md =
            "|Government|No. of Seats|Aquino|Ramos|\n|---|---|---|---|\n"
            + "|Position|||(1986-1992)|\n|Senate|24|8.3|16.7|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void FirstDataRowWithOneEmptyCellInFourColumnsPasses()
    {
        const string md = "|A|B|C|D|\n|---|---|---|---|\n|x|y||z|\n|p|q|r|s|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void ParagraphMisreadAsTwoColumnTableIsPartial()
    {
        const string md =
            "|Approval is needed from the|Acquisitions of|\n"
            + "|---|---|\n"
            + "|Treasurer if the acquisition|residential and|\n"
            + "|constitutes a \"significant|agricultural|\n"
            + "|action,\" including acquiring an|land by foreign|\n"
            + "|interest in different types of|persons must be|\n"
            + "|land where the monetary|reported to the|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void RealMultiWordTableIsKept()
    {
        const string md =
            "|Country|Capital|Notes|\n"
            + "|---|---|---|\n"
            + "|United States|Washington DC|Federal capital|\n"
            + "|United Kingdom|London|City of London is a separate|\n"
            + "|France|Paris|Île-de-France region|\n"
            + "|Germany|Berlin|Reunified 1990|\n"
            + "|Spain|Madrid|Largest city in Spain|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));
    }

    [Fact]
    public void NumericHeaderIsAcceptedWhenLayoutAssisted()
    {
        const string md = "|2024|Revenue|Growth|\n|---|---|---|\n|Q1|1.2M|5%|\n|Q2|1.4M|8%|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void OneEmptyHeaderIsAcceptedWhenLayoutAssisted()
    {
        const string md = "|Position||Senate|House|\n|---|---|---|---|\n|Chair|1|2|3|\n|Vice|4|5|6|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void TwoEmptyHeadersAreStillRejectedWhenLayoutAssisted()
    {
        // A single tidy row is not enough evidence to trust a badly gapped header.
        const string md = "|A|||D|\n|---|---|---|---|\n|x|y|z|w|";
        Assert.True(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void DenseBodyWithEmptyMergedHeaderPassesWhenLayoutAssisted()
    {
        const string md =
            "|Year||Unadjusted Basis|||\n"
            + "|---|---|---|---|---|\n"
            + "|1|.1667|$100,000|$16,670|$16,670|\n"
            + "|2|.3333|$100,000|$33,330|$50,000|\n"
            + "|3|.3333|$100,000|$33,330|$88,330|\n"
            + "|4|.1667|$100,000|$16,670|$100,000|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void SparseFirstRowIsRelaxedWhenLayoutAssisted()
    {
        // 1 of 4 empty is 25%, below the strict 33% threshold.
        const string md = "|A|B|C|D|\n|---|---|---|---|\n|x||y|z|\n|p|q|r|s|";
        Assert.False(TableCandidates.LooksLikePartialTable(md));

        // 2 of 4 is 50%: both thresholds flag it.
        const string md2 = "|A|B|C|D|\n|---|---|---|---|\n|||y|z|\n|p|q|r|s|";
        Assert.True(TableCandidates.LooksLikePartialTable(md2));
        Assert.True(TableCandidates.LooksLikePartialTableEx(md2, true));

        // 2 of 6 is 33%: strict flags it, layout-assisted does not.
        const string md3 = "|A|B|C|D|E|F|\n|---|---|---|---|---|---|\n|x|||y|z|w|\n|a|b|c|d|e|f|";
        Assert.True(TableCandidates.LooksLikePartialTable(md3));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md3, true));
    }

    [Fact]
    public void SparseFirstRowWithHeaderSpacerPassesWhenLayoutAssisted()
    {
        const string md =
            "|Properties|Instruction||Training Datasets Alignment|\n"
            + "|---|---|---|---|\n"
            + "||Alpaca-GPT4 OpenOrca Synth. Math-Instruct||Orca DPO Pairs Ultrafeedback Cleaned|\n"
            + "|Total # Samples|52K 2.91M 126K||12.9K 60.8K 126K|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void SparseSectionRowPassesWhenLayoutAssistedBodyIsDense()
    {
        const string md =
            "|Properties|Conditions|Method|Typical values|Units|\n"
            + "|---|---|---|---|---|\n"
            + "|Rheology|||||\n"
            + "|Melt Flow Rate|230 C/2.16 kg|ASTM D1238|3.0|g/10 min|\n"
            + "|Tensile Stress at Yield|50 mm/min|ASTM D638|31|MPa|\n"
            + "|Elongation at Yield|50 mm/min|ASTM D638|8|%|";
        Assert.True(TableCandidates.LooksLikePartialTable(md));
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void ParagraphIsStillRejectedWhenLayoutAssisted()
    {
        // Paragraph detection is not relaxed — it is a genuine extraction issue.
        const string md =
            "|Approval is needed from the|Acquisitions of|\n"
            + "|---|---|\n"
            + "|Treasurer if the acquisition|residential and|\n"
            + "|constitutes a \"significant|agricultural|\n"
            + "|action,\" including acquiring an|land by foreign|\n"
            + "|interest in different types of|persons must be|\n"
            + "|land where the monetary|reported to the|";
        Assert.True(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void NumberedRowspanHierarchyIsRejectedWhenLayoutAssisted()
    {
        const string md =
            "|Group|Task|Detail|Benefit|\n"
            + "|---|---|---|---|\n"
            + "|1. Group alpha|Task setup|Begin setup|Faster start|\n"
            + "|2. Group beta|Storage setup|Provides tools||\n"
            + "||Label workspace|Creates review sets|Lets teams review|\n"
            + "||Model training|Builds model|Supports rollout|\n"
            + "|3. Group gamma|Pipeline setup|Configures flow|Improves control|";
        Assert.True(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void PlainNumberedTableIsKeptWhenLayoutAssisted()
    {
        // Numbered rows alone are fine; the guard needs blank first-column sub-rows.
        const string md =
            "|Step|Task|Detail|Benefit|\n"
            + "|---|---|---|---|\n"
            + "|1. Group alpha|Task setup|Begin setup|Faster start|\n"
            + "|2. Group beta|Storage setup|Provides tools|Easier review|\n"
            + "|3. Group gamma|Pipeline setup|Configures flow|Improves control|";
        Assert.False(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void DuplicateHeadersAreStillRejectedWhenLayoutAssisted()
    {
        const string md =
            "|Position|Administration|Administration|Notes|\n|---|---|---|---|\n|Senate|24|16|x|";
        Assert.True(TableCandidates.LooksLikePartialTableEx(md, true));
    }

    [Fact]
    public void DenseNumericTableBodyIsStructurallyTrusted()
    {
        const string md =
            "|Year|3-Year|5-Year|7-Year|\n"
            + "|---|---|---|---|\n"
            + "|1|33.0%|20.00%|14.29%|\n"
            + "|2|44.45%|32.00%|24.49%|\n"
            + "|3|14.81%|19.20%|17.49%|\n"
            + "|4|7.41%|11.52%|12.49%|";
        Assert.True(TableCandidates.MarkdownTableBodyIsDense(md));
    }

    [Fact]
    public void SparseMarkdownFragmentIsNotStructurallyTrusted()
    {
        const string md =
            "|A|B|C|D|\n"
            + "|---|---|---|---|\n"
            + "|x||||\n"
            + "|||y||\n"
            + "||||z|";
        Assert.False(TableCandidates.MarkdownTableBodyIsDense(md));
    }
}
