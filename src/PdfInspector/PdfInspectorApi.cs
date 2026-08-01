// Ported from reference/src/lib.rs
using System.Diagnostics;
using PdfInspector.Detector;
using PdfInspector.Markdown;
using PdfInspector.Pdf;
using PdfInspector.Types;

namespace PdfInspector;

/// <summary>What went wrong while reading a PDF.</summary>
public sealed class PdfException : Exception
{
    /// <summary>The category of failure.</summary>
    public enum FailureKind
    {
        /// <summary>The file could not be read.</summary>
        Io,

        /// <summary>The PDF's syntax could not be parsed.</summary>
        Parse,

        /// <summary>The PDF is encrypted and the supplied password did not open it.</summary>
        Encrypted,

        /// <summary>The PDF's structure is invalid: a broken xref, a missing object.</summary>
        InvalidStructure,

        /// <summary>The bytes are not a PDF at all.</summary>
        NotAPdf,
    }

    public PdfException(FailureKind kind, string message)
        : base(message) => Kind = kind;

    public PdfException(FailureKind kind, string message, Exception inner)
        : base(message, inner) => Kind = kind;

    /// <summary>The category of failure.</summary>
    public FailureKind Kind { get; }
}

/// <summary>The OCR reasons recorded for one 1-indexed page.</summary>
public sealed record PageOcrReasons(uint Page, List<string> Reasons);

/// <summary>The result of high-level PDF processing.</summary>
public sealed class PdfProcessResult
{
    /// <summary>The detected PDF type.</summary>
    public required PdfType PdfType { get; init; }

    /// <summary>The markdown output. Populated in <see cref="ProcessMode.Full"/> only.</summary>
    public string? Markdown { get; init; }

    /// <summary>The document's page count.</summary>
    public required uint PageCount { get; init; }

    /// <summary>How long processing took, in milliseconds.</summary>
    public required long ProcessingTimeMs { get; init; }

    /// <summary>The 1-indexed pages that need OCR.</summary>
    public List<uint> PagesNeedingOcr { get; init; } = [];

    /// <summary>Why each flagged page needs OCR.</summary>
    public List<PageOcrReasons> OcrReasonsByPage { get; init; } = [];

    /// <summary>The title from the PDF's metadata, when it has one.</summary>
    public string? Title { get; init; }

    /// <summary>Detection confidence, from 0.0 to 1.0.</summary>
    public required float Confidence { get; init; }

    /// <summary>The layout complexity analysis: tables and multi-column detection.</summary>
    public LayoutComplexity Layout { get; init; } = new();

    /// <summary>
    /// True when broken font encodings were detected — garbled text or
    /// replacement characters. Callers should fall back to OCR.
    /// </summary>
    public bool HasEncodingIssues { get; init; }
}

/// <summary>Configuration for the processing entry points.</summary>
public sealed class PdfOptions
{
    /// <summary>How far the pipeline should run.</summary>
    public ProcessMode Mode { get; set; } = ProcessMode.Full;

    /// <summary>Detection configuration.</summary>
    public DetectionConfig Detection { get; set; } = new();

    /// <summary>Markdown formatting options, used in <see cref="ProcessMode.Full"/> only.</summary>
    public MarkdownOptions Markdown { get; set; } = new();

    /// <summary>The 1-indexed pages to process; null processes them all.</summary>
    public HashSet<uint>? PageFilter { get; set; }

    /// <summary>
    /// The password for an encrypted PDF. Null falls back to the empty password,
    /// which opens an owner-only-encrypted file.
    /// </summary>
    public string? Password { get; set; }

    /// <summary>Options for detection only, skipping extraction entirely.</summary>
    public static PdfOptions DetectOnly() => new() { Mode = ProcessMode.DetectOnly };

    /// <summary>
    /// A string that never leaks the password, so debug logging and formatted
    /// exceptions stay safe.
    /// </summary>
    public override string ToString() =>
        $"PdfOptions {{ Mode = {Mode}, PageFilter = " +
        $"{(PageFilter is null ? "null" : $"[{string.Join(',', PageFilter.Order())}]")}, " +
        $"Password = {(Password is null ? "null" : "[REDACTED]")} }}";
}

/// <summary>The library's high-level entry points.</summary>
public static partial class PdfProcessor
{
    /// <summary>
    /// Processes a PDF file end to end: detect, extract, convert to markdown.
    /// </summary>
    public static PdfProcessResult ProcessPdf(string path, PdfOptions? options = null)
    {
        options ??= new PdfOptions();
        var timer = Stopwatch.StartNew();
        var bytes = ReadFile(path);
        Validation.ValidatePdfBytes(bytes);
        var doc = LoadDocument(bytes, options.Password);
        return ProcessDocument(doc, (uint)doc.PageCount, options, timer);
    }

    /// <summary>Detects a PDF file's type without extracting its text.</summary>
    public static PdfProcessResult DetectPdf(string path) => ProcessPdf(path, PdfOptions.DetectOnly());

    /// <summary>Processes a PDF from an in-memory buffer.</summary>
    public static PdfProcessResult ProcessPdfMem(byte[] buffer, PdfOptions? options = null)
    {
        options ??= new PdfOptions();
        var timer = Stopwatch.StartNew();
        Validation.ValidatePdfBytes(buffer);
        var doc = LoadDocument(buffer, options.Password);
        return ProcessDocument(doc, (uint)doc.PageCount, options, timer);
    }

    /// <summary>Detects an in-memory PDF's type without extracting its text.</summary>
    public static PdfProcessResult DetectPdfMem(byte[] buffer) =>
        ProcessPdfMem(buffer, PdfOptions.DetectOnly());

    /// <summary>Reads a file, translating an IO failure into a PDF exception.</summary>
    private static byte[] ReadFile(string path)
    {
        try
        {
            return File.ReadAllBytes(path);
        }
        catch (IOException ex)
        {
            throw new PdfException(PdfException.FailureKind.Io, $"IO error: {ex.Message}", ex);
        }
        catch (UnauthorizedAccessException ex)
        {
            throw new PdfException(PdfException.FailureKind.Io, $"IO error: {ex.Message}", ex);
        }
    }

    /// <summary>Loads a document, translating parse and decryption failures.</summary>
    internal static PdfDocument LoadDocumentOrThrow(byte[] bytes, string? password) =>
        LoadDocument(bytes, password);

    /// <summary>Loads a document, translating parse and decryption failures.</summary>
    private static PdfDocument LoadDocument(byte[] bytes, string? password)
    {
        try
        {
            return PdfDocument.Load(bytes, password);
        }
        catch (PdfEncryptedException ex)
        {
            throw new PdfException(PdfException.FailureKind.Encrypted, "PDF is encrypted", ex);
        }
        catch (PdfException)
        {
            throw;
        }
        catch (Exception ex)
        {
            throw new PdfException(PdfException.FailureKind.Parse, $"PDF parsing error: {ex.Message}", ex);
        }
    }

    /// <summary>Runs the pipeline over a loaded document.</summary>
    private static PdfProcessResult ProcessDocument(
        PdfDocument doc,
        uint pageCount,
        PdfOptions options,
        Stopwatch timer)
    {
        // Step 1 — detection, which only scans content streams for text operators.
        var detection = PdfDetector.DetectFromDocument(doc, pageCount, options.Detection);
        var pdfType = detection.PdfType;
        var pagesNeedingOcr = detection.PagesNeedingOcr;
        var detectionOcrReasons = new SortedDictionary<uint, List<string>>(detection.OcrReasonsByPage);

        if (options.Mode == ProcessMode.DetectOnly
            || pdfType is PdfType.Scanned or PdfType.ImageBased)
        {
            // Detection-only stops here, and a scanned or image-based PDF has
            // nothing to extract.
            return new PdfProcessResult
            {
                PdfType = pdfType,
                Markdown = null,
                PageCount = pageCount,
                ProcessingTimeMs = timer.ElapsedMilliseconds,
                PagesNeedingOcr = pagesNeedingOcr,
                OcrReasonsByPage = ToPageOcrReasons(detectionOcrReasons),
                Title = detection.Title,
                Confidence = detection.Confidence,
                Layout = new LayoutComplexity(),
                HasEncodingIssues = false,
            };
        }

        var pipeline = new ProcessingPipeline(doc, options, pdfType, pageCount, pagesNeedingOcr);
        return pipeline.Run(detection, detectionOcrReasons, timer);
    }

    /// <summary>
    /// Extracts positioned text items from a PDF file, optionally limited to a
    /// set of 1-indexed pages.
    /// </summary>
    public static List<TextItem> ExtractTextWithPositions(
        string path,
        IReadOnlySet<uint>? pageFilter = null,
        string? password = null)
    {
        Validation.ValidatePdfFile(path);
        var doc = LoadDocument(ReadFile(path), password);
        var cmaps = ToUnicode.FontCMaps.FromDocument(doc);
        return Extractor.TextExtractor.ExtractPositionedText(doc, cmaps, pageFilter).Items;
    }

    /// <summary>
    /// Extracts positioned text items from an in-memory PDF, optionally limited
    /// to a set of 1-indexed pages.
    /// </summary>
    public static List<TextItem> ExtractTextWithPositionsMem(
        byte[] buffer,
        IReadOnlySet<uint>? pageFilter = null,
        string? password = null)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = LoadDocument(buffer, password);
        var cmaps = ToUnicode.FontCMaps.FromDocument(doc);
        return Extractor.TextExtractor.ExtractPositionedText(doc, cmaps, pageFilter).Items;
    }

    /// <summary>Extracts a PDF file's text as one plain string.</summary>
    public static string ExtractText(string path, string? password = null)
    {
        Validation.ValidatePdfFile(path);
        return ExtractTextFromDocument(LoadDocument(ReadFile(path), password));
    }

    /// <summary>Extracts an in-memory PDF's text as one plain string.</summary>
    public static string ExtractTextMem(byte[] buffer, string? password = null)
    {
        Validation.ValidatePdfBytes(buffer);
        return ExtractTextFromDocument(LoadDocument(buffer, password));
    }

    /// <summary>
    /// Flattens a document to plain text, one line per grouped baseline.
    /// </summary>
    /// <remarks>
    /// The Rust original delegates this to lopdf's own simple extractor. With
    /// the PDF core ported in-tree there is no separate simple path, so the
    /// positioned extractor's line grouping supplies the lines — which gives
    /// the same content with better spacing than a raw operand dump.
    /// </remarks>
    private static string ExtractTextFromDocument(PdfDocument doc)
    {
        var cmaps = ToUnicode.FontCMaps.FromDocument(doc);
        var items = Extractor.TextExtractor.ExtractPositionedText(doc, cmaps, null).Items;
        return string.Join('\n', Extractor.Layout.GroupIntoLines(items).Select(line => line.Text()));
    }

    /// <summary>Flattens the per-page reason map into the public list form.</summary>
    internal static List<PageOcrReasons> ToPageOcrReasons(SortedDictionary<uint, List<string>> reasonsByPage) =>
        [.. reasonsByPage.Select(kv => new PageOcrReasons(kv.Key, kv.Value))];
}
