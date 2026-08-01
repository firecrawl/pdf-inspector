// Ported from reference/src/tables/mod.rs
namespace PdfInspector.Tables;

/// <summary>What a detected table represents.</summary>
public enum TableKind
{
    /// <summary>A real data table, rendered as markdown table syntax.</summary>
    Data,

    /// <summary>
    /// A table of contents, rendered as a flat list with aligned page numbers.
    /// It is detected through the table pipeline because a contents page shares
    /// row and column structure with a table, but it is not a data table and
    /// must not appear in the layout complexity report.
    /// </summary>
    Toc,
}

/// <summary>Which font band a heuristic detection pass considers.</summary>
internal enum TableDetectionMode
{
    /// <summary>Items smaller than the page's body font — the usual table band.</summary>
    SmallFont,

    /// <summary>Items at the body font size, which needs stricter guards against prose.</summary>
    BodyFont,
}

/// <summary>A detected table: its grid, its cell text, and the items it consumed.</summary>
public sealed class Table
{
    /// <summary>Column boundaries, as x positions.</summary>
    public required List<float> Columns { get; init; }

    /// <summary>Row boundaries, as y positions in descending order.</summary>
    public required List<float> Rows { get; init; }

    /// <summary>Cell contents, indexed by row then column.</summary>
    public required List<List<string>> Cells { get; init; }

    /// <summary>Indices, into the page's item list, of the items this table consumed.</summary>
    public required List<int> ItemIndices { get; init; }

    /// <summary>Data table or table of contents, classified from the cells.</summary>
    public TableKind Kind { get; init; }

    /// <summary>Builds a table and classifies it from its cell contents.</summary>
    public static Table Create(
        List<float> columns,
        List<float> rows,
        List<List<string>> cells,
        List<int> itemIndices) => new()
        {
            Columns = columns,
            Rows = rows,
            Cells = cells,
            ItemIndices = itemIndices,
            Kind = TableOfContents.IsTableOfContents(cells) ? TableKind.Toc : TableKind.Data,
        };
}
