// Ported from reference/src/markdown/mod.rs
namespace PdfInspector.Markdown;

/// <summary>
/// The output policy for markdown post-processing.
/// </summary>
public enum MarkdownProfile
{
    /// <summary>Preserve the source text's characters wherever possible. The default.</summary>
    Fidelity,

    /// <summary>
    /// Prefer token-efficient output, including collapsing long dot leaders. Useful
    /// for agent context windows, but not byte-faithful to the PDF.
    /// </summary>
    Compact,
}

/// <summary>Options for markdown conversion.</summary>
public sealed class MarkdownOptions
{
    /// <summary>Source-fidelity versus token-efficient post-processing.</summary>
    public MarkdownProfile Profile { get; set; } = MarkdownProfile.Fidelity;

    /// <summary>Detect headers by font size.</summary>
    public bool DetectHeaders { get; set; } = true;

    /// <summary>Detect list items.</summary>
    public bool DetectLists { get; set; } = true;

    /// <summary>Detect code blocks.</summary>
    public bool DetectCode { get; set; } = true;

    /// <summary>The base font size to compare against; measured from the document when unset.</summary>
    public float? BaseFontSize { get; set; }

    /// <summary>Remove standalone page numbers.</summary>
    public bool RemovePageNumbers { get; set; } = true;

    /// <summary>Convert URLs into markdown links.</summary>
    public bool FormatUrls { get; set; } = true;

    /// <summary>Fix hyphenation, where a word breaks across lines.</summary>
    public bool FixHyphenation { get; set; } = true;

    /// <summary>Detect and format bold text from font names.</summary>
    public bool DetectBold { get; set; } = true;

    /// <summary>Detect and format italic text from font names.</summary>
    public bool DetectItalic { get; set; } = true;

    /// <summary>Emit <c>&lt;u&gt;</c> runs for text with a geometrically detected underline.</summary>
    public bool DetectUnderline { get; set; } = true;

    /// <summary>
    /// Include image placeholders in the output. Off by default: the
    /// content-stream walker emits an image item for every Image XObject it meets,
    /// and rendering those would insert <c>![Image: Im0](image)</c> placeholders
    /// throughout every existing caller's output — a silent regression on upgrade.
    /// Image boxes remain available through the positioned-text API for callers,
    /// such as layout-aware pipelines, that want to crop and caption figures
    /// themselves.
    /// </summary>
    public bool IncludeImages { get; set; }

    /// <summary>Include extracted hyperlinks.</summary>
    public bool IncludeLinks { get; set; } = true;

    /// <summary>Insert page break markers between pages.</summary>
    public bool IncludePageNumbers { get; set; }

    /// <summary>Strip repeated headers and footers that appear on many pages.</summary>
    public bool StripHeadersFooters { get; set; } = true;

    /// <summary>A copy of these options, so a caller can vary one field safely.</summary>
    public MarkdownOptions Clone() => (MarkdownOptions)MemberwiseClone();
}
