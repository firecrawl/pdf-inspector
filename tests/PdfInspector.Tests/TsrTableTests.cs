// Ported from reference/tests/integration_tests.rs
using PdfInspector.Regions;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Covers region-scoped vector-grid detection.</summary>
public sealed class VectorGridDetectionTests
{
    private static void AssertClose(float actual, float expected) =>
        Assert.True(
            MathF.Abs(actual - expected) < 0.75f,
            $"expected {actual} to be close to {expected}");

    [Fact]
    public void ARuledTableIsDetectedAndFillsFromThePdfText()
    {
        var buf = SyntheticPdf.VectorGrid(false);
        float[] crop = [50.0f, 60.0f, 210.0f, 130.0f];
        var detected = VectorGrid.DetectVectorGridInRegionMem(buf, 0, crop, 72.0f);

        Assert.NotNull(detected);
        Assert.Equal(4, detected.CellBBoxes.Count);
        Assert.Equal(4, detected.StructureTokens.Count(t => t == "<td></td>"));
        Assert.Equal("<table>", detected.StructureTokens[0]);
        Assert.Equal("</table>", detected.StructureTokens[^1]);

        var first = detected.CellBBoxes[0];
        AssertClose(first[0], 0.0f);
        AssertClose(first[1], 0.0f);
        AssertClose(first[2], 80.0f);
        AssertClose(first[3], 30.0f);

        var markdown = TsrTables.ExtractTablesWithStructureMem(buf,
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = crop,
                RenderDpi = 72.0f,
                StructureTokens = detected.StructureTokens,
                CellBBoxes = detected.CellBBoxes,
            },
        ])[0];

        foreach (var token in new[] { "A1", "B1", "A2", "B2" })
        {
            Assert.Contains(token, markdown, StringComparison.Ordinal);
        }
    }

    [Fact]
    public void APlainTextPageHasNoVectorGrid() =>
        Assert.Null(VectorGrid.DetectVectorGridInRegionMem(
            TestDocuments.MakeMinimalTextPdf(), 0, [0.0f, 0.0f, 300.0f, 800.0f], 72.0f));

    [Fact]
    public void DetectionIsScopedToTheRequestedTable()
    {
        var buf = SyntheticPdf.VectorGrid(true);
        float[] secondTableCrop = [50.0f, 240.0f, 210.0f, 310.0f];
        var detected = VectorGrid.DetectVectorGridInRegionMem(buf, 0, secondTableCrop, 72.0f);

        Assert.NotNull(detected);
        Assert.Equal(4, detected.CellBBoxes.Count);

        var markdown = TsrTables.ExtractTablesWithStructureMem(buf,
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = secondTableCrop,
                RenderDpi = 72.0f,
                StructureTokens = detected.StructureTokens,
                CellBBoxes = detected.CellBBoxes,
            },
        ])[0];

        Assert.Contains("C1", markdown, StringComparison.Ordinal);
        Assert.Contains("D2", markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("A1", markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("B2", markdown, StringComparison.Ordinal);
    }
}

/// <summary>Covers table extraction driven by external structure recovery.</summary>
public sealed class TsrStructureTests
{
    private static byte[] Fixture(string name) =>
        File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, name + ".pdf"));

    /// <summary>A two-row table with a header row, as a structure model emits it.</summary>
    private static List<string> TwoByTwoTokens() =>
    [
        "<table>", "<thead>", "<tr>", "<th></th>", "<th></th>", "</tr>", "</thead>",
        "<tbody>", "<tr>", "<td></td>", "<td></td>", "</tr>", "</tbody>", "</table>",
    ];

    /// <summary>A three-row table: a header row plus two data rows.</summary>
    private static List<string> ThreeRowTokens() =>
    [
        "<table>", "<thead>", "<tr>", "<th></th>", "<th></th>", "</tr>", "</thead>",
        "<tbody>",
        "<tr>", "<td></td>", "<td></td>", "</tr>",
        "<tr>", "<td></td>", "<td></td>", "</tr>",
        "</tbody>", "</table>",
    ];

    /// <summary>
    /// Cell boxes for the 2×2 table on page 4 of the feedback fixture, in crop
    /// pixels. At 72 dpi a pixel is a PDF point, so these are the hand-measured
    /// page coordinates offset by the crop origin. The y ranges are tightened
    /// against the neighbouring rows so each box covers only its target text.
    /// </summary>
    private static List<float[]> BitsPilaniCells() =>
    [
        SyntheticPdf.Polygon(10.0f, 7.0f, 100.0f, 18.0f),
        SyntheticPdf.Polygon(110.0f, 7.0f, 200.0f, 18.0f),
        SyntheticPdf.Polygon(10.0f, 35.0f, 100.0f, 60.0f),
        SyntheticPdf.Polygon(110.0f, 35.0f, 200.0f, 60.0f),
    ];

    private static readonly float[] BitsPilaniCrop = [80.0f, 170.0f, 280.0f, 240.0f];

    [Fact]
    public void ARealPageRendersTheExpectedMarkdown()
    {
        var markdown = TsrTables.ExtractTablesWithStructureMem(Fixture("bits_pilani_feedback"),
        [
            new TsrTableInput
            {
                Page = 3,
                CropPdfPtBBox = BitsPilaniCrop,
                RenderDpi = 72.0f,
                StructureTokens = TwoByTwoTokens(),
                CellBBoxes = BitsPilaniCells(),
            },
        ]);

        Assert.Equal("|Department|Core Courses|\n|---|---|\n|BIO|8.23|\n", Assert.Single(markdown));
    }

    [Fact]
    public void TheCellApiCarriesGridMetadata()
    {
        var lists = TsrTables.ExtractTablesWithStructureCellsMem(Fixture("bits_pilani_feedback"),
        [
            new TsrTableInput
            {
                Page = 3,
                CropPdfPtBBox = BitsPilaniCrop,
                RenderDpi = 72.0f,
                StructureTokens = TwoByTwoTokens(),
                CellBBoxes = BitsPilaniCells(),
            },
        ]);

        var cells = Assert.Single(lists);
        Assert.Equal(4, cells.Count);

        // The header row's cells came from <thead>/<th>.
        Assert.True(cells[0].IsHeader);
        Assert.True(cells[1].IsHeader);
        Assert.Equal((0, 0), (cells[0].Row, cells[0].Col));
        Assert.Equal((0, 1), (cells[1].Row, cells[1].Col));
        Assert.Equal("Department", cells[0].Text);
        Assert.Equal("Core Courses", cells[1].Text);

        Assert.False(cells[2].IsHeader);
        Assert.False(cells[3].IsHeader);
        Assert.Equal((1, 0), (cells[2].Row, cells[2].Col));
        Assert.Equal((1, 1), (cells[3].Row, cells[3].Col));
        Assert.Equal("BIO", cells[2].Text);
        Assert.Equal("8.23", cells[3].Text);

        foreach (var cell in cells)
        {
            Assert.True(
                cell.PagePtBBox[0] < cell.PagePtBBox[2] && cell.PagePtBBox[1] < cell.PagePtBBox[3],
                $"cell bbox should be non-empty: [{string.Join(", ", cell.PagePtBBox)}]");
        }
    }

    /// <summary>
    /// Rows sit 16.8pt apart while the model's boxes are 40pt tall and overlap
    /// their neighbours, yet each row's text must still land in one row only.
    /// </summary>
    [Fact]
    public void OverlappingCellBoxesStillPartitionTheRows()
    {
        var markdown = TsrTables.ExtractTablesWithStructureMem(SyntheticPdf.DenseTable(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 800.0f],
                RenderDpi = 72.0f,
                StructureTokens = ThreeRowTokens(),
                CellBBoxes =
                [
                    SyntheticPdf.Polygon(10.0f, 72.0f, 100.0f, 112.0f),
                    SyntheticPdf.Polygon(90.0f, 72.0f, 180.0f, 112.0f),
                    SyntheticPdf.Polygon(10.0f, 88.8f, 100.0f, 128.8f),
                    SyntheticPdf.Polygon(90.0f, 88.8f, 180.0f, 128.8f),
                    SyntheticPdf.Polygon(10.0f, 105.6f, 100.0f, 145.6f),
                    SyntheticPdf.Polygon(90.0f, 105.6f, 180.0f, 145.6f),
                ],
            },
        ])[0];

        Assert.Equal(
            "|Branch Name|Deposits|\n|---|---|\n|Oak Street|100|\n|Boardwalk|200|\n", markdown);
        Assert.DoesNotContain("Branch Name Oak Street", markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("Oak Street Boardwalk", markdown, StringComparison.Ordinal);
    }

    [Fact]
    public void ResultsFollowTheInputOrder()
    {
        TsrTableInput Input(float[] cell) => new()
        {
            Page = 3,
            CropPdfPtBBox = BitsPilaniCrop,
            RenderDpi = 72.0f,
            StructureTokens = ["<table>", "<tr>", "<td></td>", "</tr>", "</table>"],
            CellBBoxes = [cell],
        };

        var markdown = TsrTables.ExtractTablesWithStructureMem(Fixture("bits_pilani_feedback"),
        [
            Input(SyntheticPdf.Polygon(10.0f, 35.0f, 100.0f, 60.0f)),
            Input(SyntheticPdf.Polygon(110.0f, 35.0f, 200.0f, 60.0f)),
        ]);

        Assert.Equal(2, markdown.Count);
        Assert.Contains("BIO", markdown[0], StringComparison.Ordinal);
        Assert.Contains("8.23", markdown[1], StringComparison.Ordinal);
    }

    [Fact]
    public void AnOutOfRangePageYieldsEmptyMarkdown()
    {
        var markdown = TsrTables.ExtractTablesWithStructureMem(Fixture("bits_pilani_feedback"),
        [
            new TsrTableInput
            {
                Page = 9999,
                CropPdfPtBBox = [0.0f, 0.0f, 100.0f, 100.0f],
                RenderDpi = 72.0f,
                StructureTokens = ["<table>", "<tr>", "<td></td>", "</tr>", "</table>"],
                CellBBoxes = [SyntheticPdf.Polygon(0.0f, 0.0f, 50.0f, 50.0f)],
            },
        ]);

        Assert.Empty(Assert.Single(markdown));
    }

    [Fact]
    public void ANonPdfIsRejected() =>
        Assert.Throws<PdfException>(() => TsrTables.ExtractTablesWithStructureMem(
            System.Text.Encoding.ASCII.GetBytes("not a pdf"), []));

    [Fact]
    public void NoInputsYieldNoResults() =>
        Assert.Empty(TsrTables.ExtractTablesWithStructureMem(Fixture("bits_pilani_feedback"), []));
}

/// <summary>Covers the self-healing structure-recovery wrapper.</summary>
public sealed class TsrAutoFallbackTests
{
    private static List<string> ThreeRowTokens() =>
    [
        "<table>", "<thead>", "<tr>", "<th></th>", "<th></th>", "</tr>", "</thead>",
        "<tbody>",
        "<tr>", "<td></td>", "<td></td>", "</tr>",
        "<tr>", "<td></td>", "<td></td>", "</tr>",
        "</tbody>", "</table>",
    ];

    private static List<string> TwoRowTokens() =>
    [
        "<table>", "<thead>", "<tr>", "<th></th>", "<th></th>", "</tr>", "</thead>",
        "<tbody>", "<tr>", "<td></td>", "<td></td>", "</tr>", "</tbody>", "</table>",
    ];

    /// <summary>Cell boxes that fit each visible row of the dense table cleanly.</summary>
    private static List<float[]> CleanDenseCells() =>
    [
        SyntheticPdf.Polygon(10.0f, 72.0f, 100.0f, 112.0f),
        SyntheticPdf.Polygon(90.0f, 72.0f, 180.0f, 112.0f),
        SyntheticPdf.Polygon(10.0f, 88.8f, 100.0f, 128.8f),
        SyntheticPdf.Polygon(90.0f, 88.8f, 180.0f, 128.8f),
        SyntheticPdf.Polygon(10.0f, 105.6f, 100.0f, 145.6f),
        SyntheticPdf.Polygon(90.0f, 105.6f, 180.0f, 145.6f),
    ];

    /// <summary>
    /// A header row plus one data row whose cells are tall enough to swallow
    /// both data lines — the row-undercount pattern.
    /// </summary>
    private static List<float[]> UnderCountedDenseCells() =>
    [
        SyntheticPdf.Polygon(10.0f, 88.0f, 100.0f, 105.0f),
        SyntheticPdf.Polygon(90.0f, 88.0f, 180.0f, 105.0f),
        SyntheticPdf.Polygon(10.0f, 105.0f, 100.0f, 145.0f),
        SyntheticPdf.Polygon(90.0f, 105.0f, 180.0f, 145.0f),
    ];

    [Fact]
    public void CleanOutputPassesStraightThrough()
    {
        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 800.0f],
                RenderDpi = 72.0f,
                StructureTokens = ThreeRowTokens(),
                CellBBoxes = CleanDenseCells(),
            },
        ]);

        var result = Assert.Single(results);
        Assert.Null(result.FallbackReason);
        Assert.Contains("Oak Street", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("Boardwalk", result.Markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("Oak Street Boardwalk", result.Markdown, StringComparison.Ordinal);
    }

    [Fact]
    public void AnOverStuffedRowIsExpandedInPlace()
    {
        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 800.0f],
                RenderDpi = 72.0f,
                StructureTokens = TwoRowTokens(),
                CellBBoxes = UnderCountedDenseCells(),
            },
        ]);

        var result = Assert.Single(results);
        Assert.Equal("multi_row_in_cell_expanded", result.FallbackReason);

        // The expansion must preserve all three PDF rows.
        foreach (var token in new[] { "Oak Street", "Boardwalk", "100", "200" })
        {
            Assert.Contains(token, result.Markdown, StringComparison.Ordinal);
        }

        Assert.DoesNotContain("Oak Street Boardwalk", result.Markdown, StringComparison.Ordinal);
    }

    [Fact]
    public void AnUnderCountedVectorGridIsExpanded()
    {
        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.VectorGridThreeRows(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [50.0f, 60.0f, 210.0f, 150.0f],
                RenderDpi = 72.0f,
                StructureTokens = TwoRowTokens(),
                CellBBoxes =
                [
                    SyntheticPdf.Polygon(0.0f, 0.0f, 80.0f, 30.0f),
                    SyntheticPdf.Polygon(80.0f, 0.0f, 160.0f, 30.0f),
                    SyntheticPdf.Polygon(0.0f, 30.0f, 80.0f, 90.0f),
                    SyntheticPdf.Polygon(80.0f, 30.0f, 160.0f, 90.0f),
                ],
            },
        ]);

        var result = Assert.Single(results);
        Assert.Equal("multi_row_in_cell_expanded", result.FallbackReason);
        Assert.Contains("|Branch|Deposits|", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("|Oak|100|", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("|Boardwalk|200|", result.Markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("Oak Boardwalk", result.Markdown, StringComparison.Ordinal);
    }

    /// <summary>
    /// A real vector grid whose header and row labels wrap over two lines. The
    /// wrap is legitimate, so it must not trigger the heuristic fallback — which
    /// would split the header row.
    /// </summary>
    [Fact]
    public void AWrappedHeaderVectorGridKeepsTheStructurePath()
    {
        var buf = File.ReadAllBytes(Path.Combine(TestPaths.Fixtures, "government_positions_women.pdf"));
        float[] crop = [0.0f, 0.0f, 612.0f, 792.0f];

        var grid = VectorGrid.DetectVectorGridInRegionMem(buf, 0, crop, 200.0f);
        Assert.NotNull(grid);
        Assert.Equal(36, grid.CellBBoxes.Count);

        var results = TsrTables.ExtractTablesWithStructureAutoMem(buf,
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = crop,
                RenderDpi = 200.0f,
                StructureTokens = grid.StructureTokens,
                CellBBoxes = grid.CellBBoxes,
            },
        ]);

        var result = Assert.Single(results);
        Assert.Null(result.FallbackReason);
        Assert.Contains("Government Position", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("Aquino Administration", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("Ramos Administration", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("City Municipal Councilor", result.Markdown, StringComparison.Ordinal);
        Assert.DoesNotContain("|Position||Administration", result.Markdown, StringComparison.Ordinal);
    }

    [Fact]
    public void NoInputsYieldNoResults() =>
        Assert.Empty(TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(), []));

    /// <summary>
    /// A cell declared with a row span legitimately covers two visual lines, so
    /// the multi-row-in-cell check must not fire on it.
    /// </summary>
    [Fact]
    public void ALegitimateRowSpanDoesNotTriggerAFallback()
    {
        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 800.0f],
                RenderDpi = 72.0f,
                StructureTokens =
                [
                    "<table>", "<thead>", "<tr>", "<th></th>", "<th></th>", "</tr>", "</thead>",
                    "<tbody>",
                    "<tr>", "<td", " rowspan=\"2\"", ">", "</td>", "<td></td>", "</tr>",
                    "<tr>", "<td></td>", "</tr>",
                    "</tbody>", "</table>",
                ],
                CellBBoxes =
                [
                    SyntheticPdf.Polygon(10.0f, 88.0f, 100.0f, 105.0f),
                    SyntheticPdf.Polygon(90.0f, 88.0f, 180.0f, 105.0f),

                    // The row-spanning cell covers both data lines.
                    SyntheticPdf.Polygon(10.0f, 105.0f, 100.0f, 145.0f),
                    SyntheticPdf.Polygon(90.0f, 105.0f, 180.0f, 122.0f),
                    SyntheticPdf.Polygon(90.0f, 122.0f, 180.0f, 145.0f),
                ],
            },
        ]);

        Assert.Null(Assert.Single(results).FallbackReason);
    }

    /// <summary>
    /// The cell boxes overlap real text, so the multi-row check fires, but the
    /// crop handed to the heuristic points at an empty strip of the page. In
    /// place expansion works from the cell boxes and does not need the crop.
    /// </summary>
    [Fact]
    public void ExpansionWorksEvenWhenTheHeuristicRegionIsEmpty()
    {
        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(),
        [
            new TsrTableInput
            {
                Page = 0,
                CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 50.0f],
                RenderDpi = 72.0f,
                StructureTokens = TwoRowTokens(),
                CellBBoxes = UnderCountedDenseCells(),
            },
        ]);

        var result = Assert.Single(results);
        Assert.Equal("multi_row_in_cell_expanded", result.FallbackReason);
        Assert.Contains("|Oak Street|100|", result.Markdown, StringComparison.Ordinal);
        Assert.Contains("|Boardwalk|200|", result.Markdown, StringComparison.Ordinal);
    }

    [Fact]
    public void OneInputsOutcomeDoesNotAffectAnother()
    {
        var good = new TsrTableInput
        {
            Page = 0,
            CropPdfPtBBox = [0.0f, 0.0f, 200.0f, 800.0f],
            RenderDpi = 72.0f,
            StructureTokens = ThreeRowTokens(),
            CellBBoxes = CleanDenseCells(),
        };

        // Targets a page that does not exist. Detection short-circuits on a
        // missing page, but pairing it with a real input exercises the
        // per-input control flow.
        var bad = new TsrTableInput
        {
            Page = 9999,
            CropPdfPtBBox = [0.0f, 0.0f, 100.0f, 100.0f],
            RenderDpi = 72.0f,
            StructureTokens = ["<table>", "<tr>", "<td></td>", "</tr>", "</table>"],
            CellBBoxes = [SyntheticPdf.Polygon(0.0f, 0.0f, 50.0f, 50.0f)],
        };

        var results = TsrTables.ExtractTablesWithStructureAutoMem(SyntheticPdf.DenseTable(), [good, bad]);

        Assert.Equal(2, results.Count);
        Assert.Null(results[0].FallbackReason);
        Assert.Contains("Oak Street", results[0].Markdown, StringComparison.Ordinal);
        Assert.Equal(string.Empty, results[1].Markdown);
    }
}
