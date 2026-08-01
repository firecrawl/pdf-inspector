// Ported from reference/src/process_mode.rs
namespace PdfInspector;

/// <summary>Controls how far the PDF processing pipeline runs.</summary>
public enum ProcessMode
{
    /// <summary>Only detect the PDF type. Very fast — no text extraction.</summary>
    DetectOnly,

    /// <summary>Detect the type, extract text, and compute layout complexity. Skips markdown.</summary>
    Analyze,

    /// <summary>The full pipeline: detect, extract, and convert to markdown.</summary>
    Full,
}
