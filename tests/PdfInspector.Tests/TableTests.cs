using PdfInspector.Structure;
using PdfInspector.Tables;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Shared item construction for the table detection suites.</summary>
internal static class TableTestItems
{
    public static TextItem Make(string text, float x, float y, float fontSize, uint page = 1, long? mcid = null) =>
        new()
        {
            Text = text,
            X = x,
            Y = y,
            Width = text.Length * fontSize * 0.5f,
            Height = fontSize,
            Font = "TestFont",
            FontSize = fontSize,
            Page = page,
            Mcid = mcid,
        };
}

public sealed class RectTableTests
{
    /// <summary>
    /// A stacked bar chart: an enclosing frame plus three columns of equal-width
    /// segments whose heights vary with the data, each holding a numeric label.
    /// </summary>
    private static List<PdfRect> ChartRects()
    {
        var rects = new List<PdfRect>
        {
            new() { X = 126.0f, Y = 548.0f, Width = 396.0f, Height = 216.0f, Page = 1 },
        };

        (float X, float Y, float H)[] bars =
        [
            (208.0f, 618.0f, 59.0f),
            (208.0f, 661.0f, 39.0f),
            (208.0f, 696.0f, 37.0f),
            (313.0f, 618.0f, 67.0f),
            (313.0f, 670.0f, 49.0f),
            (313.0f, 691.0f, 42.0f),
            (419.0f, 618.0f, 73.0f),
            (419.0f, 684.0f, 37.0f),
            (419.0f, 708.0f, 25.0f),
        ];

        foreach (var (x, y, h) in bars)
        {
            rects.Add(new PdfRect { X = x, Y = y, Width = 46.0f, Height = h, Page = 1 });
        }

        return rects;
    }

    [Fact]
    public void ChartBarsProduceRegionNotTable()
    {
        (string Text, float X, float Y)[] labels =
        [
            ("38", 228.0f, 638.0f),
            ("30", 228.0f, 676.0f),
            ("46", 333.0f, 643.0f),
            ("17", 333.0f, 679.0f),
            ("57", 438.0f, 650.0f),
            ("20", 438.0f, 694.0f),
        ];

        var items = labels.Select(l => TableTestItems.Make(l.Text, l.X, l.Y, 9.0f)).ToList();
        var rects = ChartRects();

        var regions = RectTables.DetectChartRegions(items, rects, 1);
        Assert.Single(regions);

        var (tables, hints) = RectTables.DetectTablesFromRects(items, rects, 1);
        Assert.Empty(tables);
        Assert.Empty(hints);
    }

    [Fact]
    public void UniformCellGridIsNotAChart()
    {
        // Touching, uniform-height cell rects — a real table — must not match:
        // there is no inter-column gap and no bar-length variation.
        var rects = new List<PdfRect>();
        for (var row = 0; row < 4; row++)
        {
            for (var col = 0; col < 3; col++)
            {
                rects.Add(new PdfRect
                {
                    X = 100.0f + (col * 80.0f),
                    Y = 600.0f - (row * 20.0f),
                    Width = 80.0f,
                    Height = 20.0f,
                    Page = 1,
                });
            }
        }

        var items = new List<TextItem>();
        for (var r = 0; r < 4; r++)
        {
            for (var c = 0; c < 3; c++)
            {
                items.Add(TableTestItems.Make("42", 100.0f + (c * 80.0f) + 10.0f, 605.0f - (r * 20.0f), 9.0f));
            }
        }

        Assert.Empty(RectTables.DetectChartRegions(items, rects, 1));
    }

    [Fact]
    public void StackedBoxListBecomesSingleColumnTable()
    {
        var rects = new List<PdfRect>();
        var items = new List<TextItem>();
        for (var i = 0; i < 5; i++)
        {
            rects.Add(new PdfRect
            {
                X = 100.0f,
                Y = 600.0f - (i * 22.0f),
                Width = 300.0f,
                Height = 22.0f,
                Page = 1,
            });
            items.Add(TableTestItems.Make("#1: Recycling Basics", 120.0f, 605.0f - (i * 22.0f), 10.0f));
        }

        // Five boxes clear the six-rect cluster minimum only with the frame, so
        // add one more box to reach it.
        rects.Add(new PdfRect { X = 100.0f, Y = 600.0f - (5 * 22.0f), Width = 300.0f, Height = 22.0f, Page = 1 });
        items.Add(TableTestItems.Make("#6: Recycling Basics", 120.0f, 605.0f - (5 * 22.0f), 10.0f));

        var (tables, _) = RectTables.DetectTablesFromRects(items, rects, 1);
        var table = Assert.Single(tables);
        Assert.Single(table.Columns);
        Assert.Equal(6, table.Cells.Count);
    }
}

public sealed class StructTableTests
{
    private static StructTableCell Cell(bool isHeader, params long[] mcids) => new()
    {
        IsHeader = isHeader,
        Mcids = mcids.Select(m => (m, 1u)).ToList(),
    };

    [Fact]
    public void BasicStructTable()
    {
        var items = new List<TextItem>
        {
            TableTestItems.Make("Name", 50.0f, 700.0f, 10.0f, mcid: 10),
            TableTestItems.Make("Age", 200.0f, 700.0f, 10.0f, mcid: 11),
            TableTestItems.Make("Alice", 50.0f, 680.0f, 10.0f, mcid: 20),
            TableTestItems.Make("30", 200.0f, 680.0f, 10.0f, mcid: 21),
            TableTestItems.Make("Bob", 50.0f, 660.0f, 10.0f, mcid: 30),
            TableTestItems.Make("25", 200.0f, 660.0f, 10.0f, mcid: 31),
        };

        var structTables = new List<StructTable>
        {
            new()
            {
                Rows =
                [
                    new StructTableRow { Cells = { Cell(true, 10), Cell(true, 11) } },
                    new StructTableRow { Cells = { Cell(false, 20), Cell(false, 21) } },
                    new StructTableRow { Cells = { Cell(false, 30), Cell(false, 31) } },
                ],
            },
        };

        var tables = StructTables.DetectTablesFromStructTree(items, structTables, 1);
        var table = Assert.Single(tables);
        Assert.Equal(3, table.Cells.Count);
        Assert.Equal(["Name", "Age"], table.Cells[0]);
        Assert.Equal(["Alice", "30"], table.Cells[1]);
        Assert.Equal(["Bob", "25"], table.Cells[2]);
        Assert.Equal(6, table.ItemIndices.Count);
    }

    [Fact]
    public void RejectsLowMcidCoverage()
    {
        var items = new List<TextItem>
        {
            TableTestItems.Make("Orphan", 50.0f, 700.0f, 10.0f, mcid: 999),
            TableTestItems.Make("Text", 200.0f, 700.0f, 10.0f),
        };

        var structTables = new List<StructTable>
        {
            new()
            {
                Rows =
                [
                    new StructTableRow { Cells = { Cell(false, 10), Cell(false, 11) } },
                    new StructTableRow { Cells = { Cell(false, 20), Cell(false, 21) } },
                ],
            },
        };

        Assert.Empty(StructTables.DetectTablesFromStructTree(items, structTables, 1));
    }
}

public sealed class StructuredCellTests
{
    /// <summary>
    /// Tokens for a synthetic grid: one colspan-4 row plus two data rows of four
    /// cells each, so nine cells across three rows and four columns.
    /// </summary>
    private static readonly string[] Synthetic3X3Tokens =
    [
        "<html>", "<body>", "<table>", "<tbody>",
        "<tr>", "<td", " colspan=\"4\"", ">", "</td>", "</tr>",
        "<tr>", "<td></td>", "<td></td>", "<td></td>", "<td></td>", "</tr>",
        "<tr>", "<td></td>", "<td></td>", "<td></td>", "<td></td>", "</tr>",
        "</tbody>", "</table>", "</body>", "</html>",
    ];

    [Fact]
    public void ParseStructureSynthetic3X3()
    {
        var slots = StructuredCells.ParseStructure(Synthetic3X3Tokens);

        Assert.Equal(9, slots.Count);

        Assert.Equal(0, slots[0].Row);
        Assert.Equal(0, slots[0].Col);
        Assert.Equal(4, slots[0].ColSpan);
        Assert.Equal(1, slots[0].RowSpan);

        for (var i = 1; i <= 4; i++)
        {
            Assert.Equal(1, slots[i].Row);
            Assert.Equal(i - 1, slots[i].Col);
            Assert.Equal(1, slots[i].ColSpan);
            Assert.Equal(1, slots[i].RowSpan);
        }

        for (var i = 5; i <= 8; i++)
        {
            Assert.Equal(2, slots[i].Row);
            Assert.Equal(i - 5, slots[i].Col);
            Assert.Equal(1, slots[i].ColSpan);
        }
    }

    [Fact]
    public void PolygonToAabbEightElements() =>
        Assert.Equal<float[]>(
            [3.0f, 2.0f, 396.0f, 59.0f],
            StructuredCells.PolygonToAabb([3.0f, 2.0f, 395.0f, 2.0f, 396.0f, 59.0f, 3.0f, 59.0f]));

    [Fact]
    public void PolygonToAabbFourElements() =>
        Assert.Equal<float[]>([5.0f, 10.0f, 50.0f, 60.0f], StructuredCells.PolygonToAabb([5.0f, 10.0f, 50.0f, 60.0f]));

    [Fact]
    public void PolygonToAabbFourElementsUnordered() =>
        Assert.Equal<float[]>([5.0f, 10.0f, 50.0f, 60.0f], StructuredCells.PolygonToAabb([50.0f, 60.0f, 5.0f, 10.0f]));

    [Fact]
    public void PolygonToAabbRejectsInvalidLength()
    {
        Assert.Null(StructuredCells.PolygonToAabb([1.0f, 2.0f, 3.0f]));
        Assert.Null(StructuredCells.PolygonToAabb([1.0f, 1.0f, 1.0f, 1.0f, 1.0f, 1.0f]));
        Assert.Null(StructuredCells.PolygonToAabb([]));
    }

    [Fact]
    public void NormalizeCellBandsSplitsOverlappingRows()
    {
        var cells = new List<StructuredCell>
        {
            new() { Row = 0, Col = 0, IsHeader = true, PagePtBBox = [10.0f, 100.0f, 90.0f, 120.0f] },
            new() { Row = 0, Col = 1, IsHeader = true, PagePtBBox = [90.0f, 100.0f, 170.0f, 120.0f] },
            new() { Row = 1, Col = 0, PagePtBBox = [10.0f, 116.0f, 90.0f, 136.0f] },
            new() { Row = 1, Col = 1, PagePtBBox = [90.0f, 116.0f, 170.0f, 136.0f] },
        };

        StructuredCells.NormalizeCellBands(cells);

        Assert.Equal(cells[2].PagePtBBox[1], cells[0].PagePtBBox[3]);
        Assert.Equal(cells[3].PagePtBBox[1], cells[1].PagePtBBox[3]);

        // The separator lands on the midpoint between the two row centers.
        Assert.True(MathF.Abs(cells[0].PagePtBBox[3] - 118.0f) < 0.01f);
    }

    [Fact]
    public void CellsToMarkdownEmitsSeparatorAfterLastHeaderRow()
    {
        var cells = new List<StructuredCell>
        {
            new() { Row = 0, Col = 0, IsHeader = true, Text = "Name" },
            new() { Row = 0, Col = 1, IsHeader = true, Text = "Age" },
            new() { Row = 1, Col = 0, Text = "Alice" },
            new() { Row = 1, Col = 1, Text = "30" },
        };

        Assert.Equal("|Name|Age|\n|---|---|\n|Alice|30|\n", StructuredCells.CellsToMarkdown(cells));
    }

    [Fact]
    public void CellsToMarkdownEscapesPipesAndCollapsesWhitespace()
    {
        var cells = new List<StructuredCell>
        {
            new() { Row = 0, Col = 0, IsHeader = true, Text = " a | b " },
            new() { Row = 1, Col = 0, Text = "line\none" },
        };

        Assert.Equal("|a \\| b|\n|---|\n|line one|\n", StructuredCells.CellsToMarkdown(cells));
    }
}
