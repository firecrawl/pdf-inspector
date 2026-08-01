// Ported from reference/src/detector.rs
namespace PdfInspector.Detector;

/// <summary>How a PDF's content is classified.</summary>
public enum PdfType
{
    /// <summary>The PDF has extractable text; Tj and TJ operators were found.</summary>
    TextBased,

    /// <summary>The PDF appears to be scanned: images only, with no text operators.</summary>
    Scanned,

    /// <summary>The PDF is mostly images, with little or no text.</summary>
    ImageBased,

    /// <summary>The PDF mixes text with image-heavy pages.</summary>
    Mixed,
}

/// <summary>Which pages detection scans.</summary>
public abstract record ScanStrategy
{
    private ScanStrategy()
    {
    }

    /// <summary>
    /// Scans pages in order and stops at the first non-text page. Best for
    /// pipelines that route text-based PDFs to fast extraction.
    /// </summary>
    public sealed record EarlyExit : ScanStrategy;

    /// <summary>
    /// Scans every page with no early exit. Best when Mixed and Scanned need to be
    /// told apart accurately.
    /// </summary>
    public sealed record Full : ScanStrategy;

    /// <summary>
    /// Samples up to <paramref name="MaxPages"/> evenly distributed pages, always
    /// including the first and last. Best for very large PDFs where speed matters
    /// more than precision.
    /// </summary>
    public sealed record Sample(uint MaxPages) : ScanStrategy;

    /// <summary>Scans only these 1-indexed page numbers.</summary>
    public sealed record Pages(IReadOnlyList<uint> PageNumbers) : ScanStrategy;
}

/// <summary>Configuration for PDF type detection.</summary>
public sealed class DetectionConfig
{
    /// <summary>
    /// Which pages to scan. Sampling eight pages is the default: early exit is too
    /// aggressive for a PDF with an image-only cover followed by text-heavy pages,
    /// as annual reports have.
    /// </summary>
    public ScanStrategy Strategy { get; set; } = new ScanStrategy.Sample(8);

    /// <summary>The text operators a page needs before it counts as text-based.</summary>
    public uint MinTextOpsPerPage { get; set; } = 3;

    /// <summary>The ratio of text pages to sampled pages that classifies a document as text-based.</summary>
    public float TextPageRatioThreshold { get; set; } = 0.6f;
}

/// <summary>The result of PDF type detection.</summary>
public sealed class PdfTypeResult
{
    /// <summary>The detected type.</summary>
    public required PdfType PdfType { get; init; }

    /// <summary>The document's page count.</summary>
    public required uint PageCount { get; init; }

    /// <summary>How many pages detection actually sampled.</summary>
    public required uint PagesSampled { get; init; }

    /// <summary>How many sampled pages carried usable text operators.</summary>
    public required uint PagesWithText { get; init; }

    /// <summary>Confidence in the classification, from 0.0 to 1.0.</summary>
    public required float Confidence { get; init; }

    /// <summary>The document title from its metadata, when it has one.</summary>
    public string? Title { get; init; }

    /// <summary>
    /// True when OCR would improve extraction, either because images carry
    /// essential context — as in a template-based PDF — or because the document is
    /// scanned.
    /// </summary>
    public required bool OcrRecommended { get; init; }

    /// <summary>
    /// The 1-indexed pages that need OCR because they are image-only or hold too
    /// little text. Empty for a text-based PDF, every page for a scanned or
    /// image-based one, and specific pages for a mixed one.
    /// </summary>
    public List<uint> PagesNeedingOcr { get; init; } = [];

    /// <summary>
    /// Why each flagged page needs OCR, keyed by 1-indexed page number. Only pages
    /// that need OCR appear.
    /// </summary>
    public SortedDictionary<uint, List<string>> OcrReasonsByPage { get; init; } = [];
}

/// <summary>The reason codes reported in <see cref="PdfTypeResult.OcrReasonsByPage"/>.</summary>
public static class OcrReason
{
    /// <summary>The page's fonts cannot map their codes to Unicode.</summary>
    public const string SuspectedGarbledText = "suspected_garbled_text";

    /// <summary>The page is backed by an image with no usable text layer.</summary>
    public const string Scanned = "scanned";

    /// <summary>The page carries no text and no image either.</summary>
    public const string NoText = "no_text";

    /// <summary>The page's text is drawn as vector outlines rather than glyphs.</summary>
    public const string VectorText = "vector_text";
}
