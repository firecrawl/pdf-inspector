using System;
using System.Text.Json.Serialization;

namespace Firecrawl.PdfInspector;

/// <summary>PDF document classification.</summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum PdfType
{
    TextBased,
    Scanned,
    ImageBased,
    Mixed,
}

/// <summary>Markdown output profile.</summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum MarkdownProfile
{
    Fidelity,
    Compact,
}

/// <summary>Controls when local OCR runs.</summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum OcrMode
{
    Off,
    Auto,
    Force,
}

/// <summary>How a page's final content was produced.</summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum PageContentSource
{
    Native,
    Ocr,
    Fused,
}

/// <summary>Options shared by PDF processing and detection.</summary>
public sealed class ProcessOptions
{
    /// <summary>Optional 1-indexed page numbers.</summary>
    public uint[]? Pages { get; set; }

    /// <summary>Password for an encrypted PDF.</summary>
    public string? Password { get; set; }

    /// <summary>Markdown output profile.</summary>
    public MarkdownProfile? Profile { get; set; }

    /// <summary>Insert HTML page markers between pages.</summary>
    public bool? IncludePageMarkers { get; set; }

    /// <summary>Include image placeholders in Markdown output.</summary>
    public bool? IncludeImages { get; set; }
}

/// <summary>Machine-readable OCR reasons for one 1-indexed page.</summary>
public sealed class PageOcrReasons
{
    public uint Page { get; set; }

    public string[] Reasons { get; set; } = Array.Empty<string>();
}

/// <summary>Detected layout features.</summary>
public sealed class LayoutComplexity
{
    public bool IsComplex { get; set; }

    public uint[] PagesWithTables { get; set; } = Array.Empty<uint>();

    public uint[] PagesWithColumns { get; set; } = Array.Empty<uint>();
}

/// <summary>Full PDF extraction result.</summary>
public sealed class PdfProcessResult
{
    public PdfType PdfType { get; set; }

    public string? Markdown { get; set; }

    public uint PageCount { get; set; }

    public ulong ProcessingTimeMs { get; set; }

    /// <summary>1-indexed pages that need OCR.</summary>
    public uint[] PagesNeedingOcr { get; set; } = Array.Empty<uint>();

    public PageOcrReasons[] OcrReasonsByPage { get; set; } = Array.Empty<PageOcrReasons>();

    public string? Title { get; set; }

    public double Confidence { get; set; }

    public LayoutComplexity Layout { get; set; } = new LayoutComplexity();

    public bool HasEncodingIssues { get; set; }
}

/// <summary>Lightweight classification result.</summary>
public sealed class PdfClassification
{
    public PdfType PdfType { get; set; }

    public uint PageCount { get; set; }

    /// <summary>0-indexed pages that need OCR.</summary>
    public uint[] PagesNeedingOcr { get; set; } = Array.Empty<uint>();

    public double Confidence { get; set; }
}

/// <summary>Options for native extraction with selective OCR.</summary>
public sealed class OcrOptions
{
    public OcrMode? Mode { get; set; }

    /// <summary>Optional 1-indexed page numbers.</summary>
    public uint[]? PageNumbers { get; set; }

    public string? Password { get; set; }

    public double? Dpi { get; set; }

    public double? MinimumConfidence { get; set; }

    public double? HostedRecommendationConfidence { get; set; }

    public string? ModelDirectory { get; set; }

    public bool? Offline { get; set; }
}

public sealed class OcrModelIdentity
{
    public string Name { get; set; } = string.Empty;

    public string Revision { get; set; } = string.Empty;
}

public sealed class OcrTimings
{
    public ulong RenderMs { get; set; }

    public ulong OcrMs { get; set; }

    public ulong AssemblyMs { get; set; }
}

public sealed class OcrPageProvenance
{
    /// <summary>1-indexed PDF page number.</summary>
    public uint PageNumber { get; set; }

    public PageContentSource Source { get; set; }

    public OcrModelIdentity? OcrModel { get; set; }

    public double? RenderDpi { get; set; }

    public double? OcrConfidence { get; set; }

    public OcrTimings Timings { get; set; } = new OcrTimings();

    public string[] Warnings { get; set; } = Array.Empty<string>();

    public bool HostedRecommended { get; set; }
}

public sealed class OcrPageResult
{
    /// <summary>1-indexed PDF page number.</summary>
    public uint PageNumber { get; set; }

    public string Markdown { get; set; } = string.Empty;

    public OcrPageProvenance Provenance { get; set; } = new OcrPageProvenance();
}

public sealed class OcrPdfResult
{
    public string Markdown { get; set; } = string.Empty;

    /// <summary>Page results whose PageNumber values are 1-indexed.</summary>
    public OcrPageResult[] Pages { get; set; } = Array.Empty<OcrPageResult>();

    public uint PageCount { get; set; }

    /// <summary>1-indexed pages recommended for OCR by native extraction.</summary>
    public uint[] PagesRecommendedForOcr { get; set; } = Array.Empty<uint>();

    /// <summary>1-indexed pages that were routed to OCR.</summary>
    public uint[] PagesRoutedToOcr { get; set; } = Array.Empty<uint>();

    /// <summary>1-indexed pages recommended for hosted processing after local OCR.</summary>
    public uint[] PagesRecommendingHosted { get; set; } = Array.Empty<uint>();

    public PageOcrReasons[] OcrReasonsByPage { get; set; } = Array.Empty<PageOcrReasons>();

    /// <summary>1-indexed pages containing tables.</summary>
    public uint[] PagesWithTables { get; set; } = Array.Empty<uint>();

    /// <summary>1-indexed pages containing columns.</summary>
    public uint[] PagesWithColumns { get; set; } = Array.Empty<uint>();

    public bool IsComplex { get; set; }

    public ulong ProcessingTimeMs { get; set; }

    public ulong RenderTimeMs { get; set; }

    public ulong OcrTimeMs { get; set; }
}
