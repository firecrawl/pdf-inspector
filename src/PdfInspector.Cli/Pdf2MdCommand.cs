// Ported from reference/src/bin/pdf2md.rs
using System.Globalization;
using PdfInspector.Detector;
using PdfInspector.Markdown;
using PdfInspector.Types;

namespace PdfInspector.Cli;

/// <summary>Converts a PDF to markdown, with smart type detection.</summary>
internal static class Pdf2MdCommand
{
    /// <summary>Runs the command and returns its process exit code.</summary>
    public static int Run(string[] args)
    {
        if (args.Length < 1)
        {
            PrintUsage();
            return 1;
        }

        var pdfPath = args[0];
        var jsonOutput = args.Contains("--json");
        var itemsJsonOutput = args.Contains("--items-json");
        var rawOutput = args.Contains("--raw");
        var compactOutput = args.Contains("--compact");
        var pageNumbers = args.Contains("--pages");
        var detectOnly = args.Contains("--detect-only");
        var analyze = args.Contains("--analyze");

        string? password;
        try
        {
            password = ValueAfter(args, "--password");
        }
        catch (ArgumentException)
        {
            Console.Error.WriteLine("Error: --password requires a value");
            return 1;
        }

        HashSet<uint>? pageFilter = null;
        try
        {
            var spec = ValueAfter(args, "--select-pages");
            if (spec is not null)
            {
                pageFilter = ParsePageSpec(spec);
            }
        }
        catch (ArgumentException)
        {
            Console.Error.WriteLine("Error: --select-pages requires a value (e.g. 1,3,5-10)");
            return 1;
        }
        catch (FormatException ex)
        {
            Console.Error.WriteLine($"Error: invalid --select-pages value: {ex.Message}");
            return 1;
        }

        if (itemsJsonOutput)
        {
            return WriteItemsJson(pdfPath, pageFilter, password);
        }

        var outputFile = args.Length >= 2 && !args[1].StartsWith("--", StringComparison.Ordinal) ? args[1] : null;

        var options = new PdfOptions
        {
            Mode = detectOnly ? ProcessMode.DetectOnly : analyze ? ProcessMode.Analyze : ProcessMode.Full,
            PageFilter = pageFilter,
            Password = password,
        };

        if (compactOutput)
        {
            options.Markdown.Profile = MarkdownProfile.Compact;
        }

        options.Markdown.IncludePageNumbers = pageNumbers;

        PdfProcessResult result;
        try
        {
            result = PdfProcessor.ProcessPdf(pdfPath, options);
        }
        catch (PdfException ex)
        {
            if (jsonOutput)
            {
                Console.WriteLine($"{{\"error\":\"{JsonWriter.Escape(ex.Message)}\"}}");
            }
            else
            {
                Console.Error.WriteLine($"Error: {ex.Message}");
            }

            return 1;
        }

        if (detectOnly || analyze)
        {
            WriteDetectionOutput(result, jsonOutput, analyze);
            return 0;
        }

        if (jsonOutput)
        {
            WriteFullJson(result);
            return 0;
        }

        return rawOutput ? WriteRaw(result) : WriteVerbose(result, pdfPath, outputFile);
    }

    /// <summary>Prints the usage banner the reference emits.</summary>
    private static void PrintUsage()
    {
        var err = Console.Error;
        err.WriteLine("Usage: pdf2md <pdf_file> [output_file]");
        err.WriteLine("       pdf2md <pdf_file> --json");
        err.WriteLine("       pdf2md <pdf_file> --items-json");
        err.WriteLine("       pdf2md <pdf_file> --raw");
        err.WriteLine();
        err.WriteLine("Converts PDF to Markdown with smart type detection.");
        err.WriteLine("Returns early if PDF is scanned (OCR needed).");
        err.WriteLine();
        err.WriteLine("Options:");
        err.WriteLine("  --json              Output result as JSON");
        err.WriteLine("  --items-json        Output positioned TextItem JSON");
        err.WriteLine("  --raw               Output only markdown (no headers)");
        err.WriteLine("  --compact           Collapse token-heavy source formatting such as dot leaders");
        err.WriteLine("  --pages             Insert page break markers (<!-- Page N -->)");
        err.WriteLine("  --select-pages N    Only process specified pages (e.g. 1,3,5-10)");
        err.WriteLine("  --password PW       Password for an encrypted PDF");
        err.WriteLine("  --detect-only       Only detect PDF type (no extraction)");
        err.WriteLine("  --analyze           Detect + extract + layout analysis (no markdown)");
    }

    /// <summary>The argument that follows a flag, or null when the flag is absent.</summary>
    /// <exception cref="ArgumentException">When the flag is present but has no value.</exception>
    private static string? ValueAfter(string[] args, string flag)
    {
        var index = Array.IndexOf(args, flag);
        if (index < 0)
        {
            return null;
        }

        return index + 1 < args.Length ? args[index + 1] : throw new ArgumentException($"{flag} requires a value");
    }

    /// <summary>Parses a page specification such as "1,3,5-10,20".</summary>
    /// <exception cref="FormatException">When the specification is malformed.</exception>
    private static HashSet<uint> ParsePageSpec(string spec)
    {
        var pages = new HashSet<uint>();

        foreach (var rawPart in spec.Split(','))
        {
            var part = rawPart.Trim();
            var dash = part.IndexOf('-', StringComparison.Ordinal);

            if (dash >= 0)
            {
                var startText = part[..dash].Trim();
                var endText = part[(dash + 1)..].Trim();

                if (!uint.TryParse(startText, out var start))
                {
                    throw new FormatException($"invalid page number: {startText}");
                }

                if (!uint.TryParse(endText, out var end))
                {
                    throw new FormatException($"invalid page number: {endText}");
                }

                if (start == 0 || end == 0)
                {
                    throw new FormatException("page numbers are 1-indexed");
                }

                if (start > end)
                {
                    throw new FormatException($"invalid range: {start}-{end}");
                }

                for (var p = start; p <= end; p++)
                {
                    pages.Add(p);
                }
            }
            else
            {
                if (!uint.TryParse(part, out var p))
                {
                    throw new FormatException($"invalid page number: {part}");
                }

                if (p == 0)
                {
                    throw new FormatException("page numbers are 1-indexed");
                }

                pages.Add(p);
            }
        }

        return pages;
    }

    /// <summary>Emits the positioned text items as JSON.</summary>
    private static int WriteItemsJson(string pdfPath, HashSet<uint>? pageFilter, string? password)
    {
        try
        {
            var items = PdfProcessor.ExtractTextWithPositions(pdfPath, pageFilter, password);
            Console.WriteLine(FormatItemsJson(items));
            return 0;
        }
        catch (Exception ex)
        {
            Console.WriteLine($"{{\"error\":\"{JsonWriter.Escape(ex.Message)}\"}}");
            return 1;
        }
    }

    /// <summary>Renders the positioned items in the reference's JSON shape.</summary>
    internal static string FormatItemsJson(IReadOnlyList<TextItem> items)
    {
        var underlinedCount = items.Count(item => item.IsUnderline);

        var itemsJson = string.Join(',', items.Select(item =>
        {
            var mcid = item.Mcid?.ToString(CultureInfo.InvariantCulture) ?? "null";
            var linkUrl = item.Kind == ItemKind.Link && item.LinkUrl is not null
                ? $",\"url\":\"{JsonWriter.Escape(item.LinkUrl)}\""
                : string.Empty;

            return string.Create(CultureInfo.InvariantCulture,
                $"{{\"text\":\"{JsonWriter.Escape(item.Text)}\",\"page\":{item.Page}," +
                $"\"x\":{item.X:F2},\"y\":{item.Y:F2},\"width\":{item.Width:F2},\"height\":{item.Height:F2}," +
                $"\"font\":\"{JsonWriter.Escape(item.Font)}\",\"font_size\":{item.FontSize:F2}," +
                $"\"is_bold\":{JsonWriter.Bool(item.IsBold)},\"is_italic\":{JsonWriter.Bool(item.IsItalic)}," +
                $"\"is_underline\":{JsonWriter.Bool(item.IsUnderline)}," +
                $"\"is_strikeout\":{JsonWriter.Bool(item.IsStrikeout)}," +
                $"\"item_type\":\"{ItemTypeLabel(item.Kind)}\",\"mcid\":{mcid}{linkUrl}}}");
        }));

        return $"{{\"total_items\":{items.Count},\"underlined_count\":{underlinedCount},\"items\":[{itemsJson}]}}";
    }

    /// <summary>The reference's snake_case name for an item kind.</summary>
    private static string ItemTypeLabel(ItemKind kind) => kind switch
    {
        ItemKind.Text => "text",
        ItemKind.Image => "image",
        ItemKind.Link => "link",
        ItemKind.FormField => "form_field",
        _ => "text",
    };

    /// <summary>Emits the detect-only or analyze output.</summary>
    private static void WriteDetectionOutput(PdfProcessResult result, bool jsonOutput, bool analyze)
    {
        if (jsonOutput)
        {
            Console.WriteLine(string.Create(CultureInfo.InvariantCulture,
                $"{{\"pdf_type\":\"{JsonWriter.PdfTypeName(result.PdfType)}\"," +
                $"\"page_count\":{result.PageCount},\"processing_time_ms\":{result.ProcessingTimeMs}," +
                $"\"pages_needing_ocr\":[{JsonWriter.PageList(result.PagesNeedingOcr)}]," +
                $"\"ocr_reasons_by_page\":[{JsonWriter.OcrReasons(result.OcrReasonsByPage)}]," +
                $"\"is_complex\":{JsonWriter.Bool(result.Layout.IsComplex)}," +
                $"\"pages_with_tables\":[{JsonWriter.PageList(result.Layout.PagesWithTables)}]," +
                $"\"pages_with_columns\":[{JsonWriter.PageList(result.Layout.PagesWithColumns)}]," +
                $"\"has_encoding_issues\":{JsonWriter.Bool(result.HasEncodingIssues)}}}"));
            return;
        }

        Console.Error.WriteLine($"Type: {JsonWriter.PdfTypeName(result.PdfType)}");
        Console.Error.WriteLine($"Pages: {result.PageCount}");
        Console.Error.WriteLine($"Processing time: {result.ProcessingTimeMs}ms");

        if (result.PagesNeedingOcr.Count > 0)
        {
            Console.Error.WriteLine($"Pages needing OCR: [{string.Join(", ", result.PagesNeedingOcr)}]");
        }

        if (analyze)
        {
            PrintLayoutInfo(result.Layout);
        }
    }

    /// <summary>Emits the full-mode JSON report.</summary>
    private static void WriteFullJson(PdfProcessResult result)
    {
        var markdown = result.Markdown ?? string.Empty;

        Console.WriteLine(string.Create(CultureInfo.InvariantCulture,
            $"{{\"pdf_type\":\"{JsonWriter.PdfTypeName(result.PdfType)}\"," +
            $"\"page_count\":{result.PageCount}," +
            $"\"has_text\":{JsonWriter.Bool(result.Markdown is not null)}," +
            $"\"processing_time_ms\":{result.ProcessingTimeMs}," +
            $"\"markdown_length\":{markdown.Length}," +
            $"\"pages_needing_ocr\":[{JsonWriter.PageList(result.PagesNeedingOcr)}]," +
            $"\"ocr_reasons_by_page\":[{JsonWriter.OcrReasons(result.OcrReasonsByPage)}]," +
            $"\"is_complex\":{JsonWriter.Bool(result.Layout.IsComplex)}," +
            $"\"pages_with_tables\":[{JsonWriter.PageList(result.Layout.PagesWithTables)}]," +
            $"\"pages_with_columns\":[{JsonWriter.PageList(result.Layout.PagesWithColumns)}]," +
            $"\"has_encoding_issues\":{JsonWriter.Bool(result.HasEncodingIssues)}," +
            $"\"markdown\":\"{JsonWriter.Escape(markdown)}\"}}"));
    }

    /// <summary>Emits just the markdown, with no headers.</summary>
    private static int WriteRaw(PdfProcessResult result)
    {
        if (result.PdfType is PdfType.Scanned or PdfType.ImageBased)
        {
            Console.Error.WriteLine($"Error: PDF requires OCR (type: {result.PdfType})");
            return 2;
        }

        if (result.Markdown is { } markdown)
        {
            Console.Out.Write(markdown);
        }

        return 0;
    }

    /// <summary>Emits the human-readable report.</summary>
    private static int WriteVerbose(PdfProcessResult result, string pdfPath, string? outputFile)
    {
        var err = Console.Error;
        err.WriteLine("PDF to Markdown Conversion");
        err.WriteLine("==========================");
        err.WriteLine($"File: {pdfPath}");
        err.WriteLine();

        switch (result.PdfType)
        {
            case PdfType.TextBased:
                err.WriteLine("Type: TEXT-BASED (direct extraction)");
                err.WriteLine($"Pages: {result.PageCount}");
                err.WriteLine($"Processing time: {result.ProcessingTimeMs}ms");
                PrintLayoutInfo(result.Layout);
                if (result.PagesNeedingOcr.Count > 0)
                {
                    err.WriteLine($"Pages needing OCR: [{string.Join(", ", result.PagesNeedingOcr)}]");
                }

                WriteMarkdown(result.Markdown, outputFile);
                return 0;

            case PdfType.Scanned:
            case PdfType.ImageBased:
                err.WriteLine(
                    $"Type: {(result.PdfType == PdfType.Scanned ? "SCANNED" : "IMAGE-BASED")} (OCR required)");
                err.WriteLine($"Pages: {result.PageCount}");
                err.WriteLine($"Processing time: {result.ProcessingTimeMs}ms");
                err.WriteLine();
                err.WriteLine("This PDF requires OCR for text extraction.");
                err.WriteLine("Consider using MinerU or similar OCR tool.");
                return 2;

            default:
                err.WriteLine("Type: MIXED (partial text extraction)");
                err.WriteLine($"Pages: {result.PageCount}");
                err.WriteLine($"Processing time: {result.ProcessingTimeMs}ms");
                PrintLayoutInfo(result.Layout);

                if (result.Markdown is not null)
                {
                    err.WriteLine();
                    err.WriteLine(result.PagesNeedingOcr.Count == 0
                        ? "Note: Some pages may contain images that require OCR."
                        : $"Pages needing OCR: [{string.Join(", ", result.PagesNeedingOcr)}]");
                    err.WriteLine();
                    WriteMarkdown(result.Markdown, outputFile, leadingBlankLine: false);
                }

                return 0;
        }
    }

    /// <summary>Writes markdown to a file or to standard output.</summary>
    private static void WriteMarkdown(string? markdown, string? outputFile, bool leadingBlankLine = true)
    {
        if (markdown is null)
        {
            return;
        }

        var err = Console.Error;

        if (outputFile is not null)
        {
            File.WriteAllText(outputFile, markdown);
            if (leadingBlankLine)
            {
                err.WriteLine();
            }

            err.WriteLine($"Markdown written to: {outputFile}");
            err.WriteLine($"Length: {markdown.Length} characters");
            return;
        }

        if (leadingBlankLine)
        {
            err.WriteLine();
        }

        err.WriteLine("--- Markdown Output ---");
        err.WriteLine();
        Console.Out.WriteLine(markdown);
    }

    /// <summary>Prints the layout summary to standard error.</summary>
    private static void PrintLayoutInfo(LayoutComplexity layout)
    {
        var err = Console.Error;

        if (!layout.IsComplex)
        {
            err.WriteLine("Layout: simple");
            return;
        }

        err.WriteLine("Layout: COMPLEX");
        if (layout.PagesWithTables.Count > 0)
        {
            err.WriteLine($"  Pages with tables: [{string.Join(", ", layout.PagesWithTables)}]");
        }

        if (layout.PagesWithColumns.Count > 0)
        {
            err.WriteLine($"  Pages with columns: [{string.Join(", ", layout.PagesWithColumns)}]");
        }
    }
}
