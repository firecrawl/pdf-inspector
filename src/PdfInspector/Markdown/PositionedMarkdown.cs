// Ported from reference/src/markdown/convert.rs
namespace PdfInspector.Markdown;

/// <summary>
/// The logical stream geometry of a page where one full-width chart separates
/// two prose columns. Positioned non-text blocks use this same ordering, so a
/// right-column table or image cannot jump ahead of left-column prose.
/// </summary>
internal readonly record struct ChartProseOrder(float SplitX, ChartRegion Region);

/// <summary>
/// A markdown block with its physical position and, on chart pages, its logical
/// stream. Tables and images share this representation because both are removed
/// before text-line grouping and reinserted during conversion.
/// </summary>
internal sealed class PositionedMarkdown
{
    public required float Y { get; init; }

    public required float X { get; init; }

    public required string Markdown { get; set; }

    /// <summary>The page's chart stream, when it has one.</summary>
    public ChartProseOrder? ChartOrder { get; init; }
}
