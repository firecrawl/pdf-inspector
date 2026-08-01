// Ported from reference/src/bin/detect_pdf.rs
using System.Diagnostics;
using System.Globalization;
using PdfInspector.Detector;
using PdfInspector.Types;

namespace PdfInspector.Cli;

/// <summary>Detects whether a PDF is text-based or scanned.</summary>
internal static class DetectPdfCommand
{
    /// <summary>Runs the command and returns its process exit code.</summary>
    public static int Run(string[] args)
    {
        if (args.Length < 1)
        {
            var err = Console.Error;
            err.WriteLine("Usage: detect-pdf <pdf_file>");
            err.WriteLine("       detect-pdf <pdf_file> --json");
            err.WriteLine("       detect-pdf <pdf_file> --analyze");
            err.WriteLine();
            err.WriteLine("Options:");
            err.WriteLine("  --json       Output result as JSON");
            err.WriteLine("  --analyze    Also run layout analysis (tables, columns)");
            return 1;
        }

        var pdfPath = args[0];
        var jsonOutput = args.Contains("--json");
        var analyze = args.Contains("--analyze");

        var timer = Stopwatch.StartNew();
        return analyze ? RunAnalyze(pdfPath, jsonOutput, timer) : RunDetectOnly(pdfPath, jsonOutput, timer);
    }

    /// <summary>Runs detection plus layout analysis.</summary>
    private static int RunAnalyze(string pdfPath, bool jsonOutput, Stopwatch timer)
    {
        PdfProcessResult result;
        try
        {
            result = PdfProcessor.ProcessPdf(pdfPath, new PdfOptions { Mode = ProcessMode.Analyze });
        }
        catch (Exception ex)
        {
            PrintError(ex, pdfPath, jsonOutput);
            return 1;
        }

        var elapsed = timer.ElapsedMilliseconds;

        if (jsonOutput)
        {
            Console.WriteLine(string.Create(CultureInfo.InvariantCulture,
                $"{{\"pdf_type\":\"{JsonWriter.PdfTypeName(result.PdfType)}\"," +
                $"\"page_count\":{result.PageCount}," +
                $"\"pages_needing_ocr\":[{JsonWriter.PageList(result.PagesNeedingOcr)}]," +
                $"\"ocr_reasons_by_page\":[{JsonWriter.OcrReasons(result.OcrReasonsByPage)}]," +
                $"\"is_complex\":{JsonWriter.Bool(result.Layout.IsComplex)}," +
                $"\"pages_with_tables\":[{JsonWriter.PageList(result.Layout.PagesWithTables)}]," +
                $"\"pages_with_columns\":[{JsonWriter.PageList(result.Layout.PagesWithColumns)}]," +
                $"\"detection_time_ms\":{elapsed}}}"));
            return 0;
        }

        Console.WriteLine("PDF Type Detection + Layout Analysis");
        Console.WriteLine("=====================================");
        Console.WriteLine($"File: {pdfPath}");
        Console.WriteLine();
        Console.WriteLine($"Type: {TypeDescription(result.PdfType)}");
        Console.WriteLine($"Page count: {result.PageCount}");

        if (result.PagesNeedingOcr.Count > 0)
        {
            Console.WriteLine($"Pages needing OCR: [{string.Join(", ", result.PagesNeedingOcr)}]");
            foreach (var entry in result.OcrReasonsByPage)
            {
                Console.WriteLine($"  page {entry.Page}: {string.Join(", ", entry.Reasons)}");
            }
        }

        Console.WriteLine();
        PrintLayout(result.Layout);
        Console.WriteLine();
        Console.WriteLine($"Detection time: {elapsed}ms");
        return 0;
    }

    /// <summary>Runs detection alone, which is the richer report.</summary>
    private static int RunDetectOnly(string pdfPath, bool jsonOutput, Stopwatch timer)
    {
        PdfTypeResult result;
        try
        {
            result = PdfDetector.DetectPdfType(pdfPath);
        }
        catch (Exception ex)
        {
            PrintError(ex, pdfPath, jsonOutput);
            return 1;
        }

        var elapsed = timer.ElapsedMilliseconds;

        if (jsonOutput)
        {
            Console.WriteLine(string.Create(CultureInfo.InvariantCulture,
                $"{{\"pdf_type\":\"{JsonWriter.PdfTypeName(result.PdfType)}\"," +
                $"\"page_count\":{result.PageCount},\"pages_sampled\":{result.PagesSampled}," +
                $"\"pages_with_text\":{result.PagesWithText}," +
                $"\"confidence\":{result.Confidence:F2}," +
                $"\"title\":{JsonWriter.NullableString(result.Title)}," +
                $"\"ocr_recommended\":{JsonWriter.Bool(result.OcrRecommended)}," +
                $"\"pages_needing_ocr\":[{JsonWriter.PageList(result.PagesNeedingOcr)}]," +
                $"\"ocr_reasons_by_page\":[{JsonWriter.DetectorOcrReasons(result.OcrReasonsByPage)}]," +
                $"\"detection_time_ms\":{elapsed}}}"));
            return 0;
        }

        Console.WriteLine("PDF Type Detection Results");
        Console.WriteLine("==========================");
        Console.WriteLine($"File: {pdfPath}");
        Console.WriteLine();
        Console.WriteLine($"Type: {TypeDescription(result.PdfType)}");
        Console.WriteLine(string.Create(CultureInfo.InvariantCulture, $"Confidence: {result.Confidence * 100.0f:F0}%"));
        Console.WriteLine();
        Console.WriteLine($"Page count: {result.PageCount}");
        Console.WriteLine($"Pages sampled: {result.PagesSampled}");
        Console.WriteLine($"Pages with text: {result.PagesWithText}");
        Console.WriteLine($"OCR recommended: {(result.OcrRecommended ? "YES" : "NO")}");

        if (result.PagesNeedingOcr.Count > 0)
        {
            Console.WriteLine(result.PagesNeedingOcr.Count == result.PageCount
                ? $"Pages needing OCR: all (of {result.PageCount})"
                : $"Pages needing OCR: [{string.Join(", ", result.PagesNeedingOcr)}] (of {result.PageCount})");

            foreach (var (page, reasons) in result.OcrReasonsByPage)
            {
                Console.WriteLine($"  page {page}: {string.Join(", ", reasons)}");
            }
        }

        if (result.Title is { } title)
        {
            Console.WriteLine($"Title: {title}");
        }

        Console.WriteLine();
        Console.WriteLine($"Detection time: {elapsed}ms");
        Console.WriteLine();

        if (!result.OcrRecommended)
        {
            Console.WriteLine("Recommendation: Use direct text extraction (fast)");
            return 0;
        }

        Console.WriteLine(result.PdfType switch
        {
            PdfType.Mixed => "Recommendation: Use OCR - images provide essential context (template PDF)",
            PdfType.Scanned => "Recommendation: Use OCR (MinerU or similar)",
            PdfType.ImageBased => "Recommendation: Use OCR for best results",
            _ => "Recommendation: Use OCR for complete extraction",
        });

        return 0;
    }

    /// <summary>The long-form description of a classification.</summary>
    private static string TypeDescription(PdfType type) => type switch
    {
        PdfType.TextBased => "TEXT-BASED (extractable text)",
        PdfType.Scanned => "SCANNED (OCR needed)",
        PdfType.ImageBased => "IMAGE-BASED (mostly images, OCR may help)",
        _ => "MIXED (some text, some images)",
    };

    /// <summary>Prints the layout summary.</summary>
    private static void PrintLayout(LayoutComplexity layout)
    {
        if (!layout.IsComplex)
        {
            Console.WriteLine("Layout: simple");
            return;
        }

        Console.WriteLine("Layout: COMPLEX");
        if (layout.PagesWithTables.Count > 0)
        {
            Console.WriteLine($"  Pages with tables: [{string.Join(", ", layout.PagesWithTables)}]");
        }

        if (layout.PagesWithColumns.Count > 0)
        {
            Console.WriteLine($"  Pages with columns: [{string.Join(", ", layout.PagesWithColumns)}]");
        }
    }

    /// <summary>
    /// Reports a failure, adding the raw-byte page-count hint that helps when a
    /// malformed or encrypted PDF cannot be opened.
    /// </summary>
    private static void PrintError(Exception ex, string pdfPath, bool jsonOutput)
    {
        var hint = PageCountHint(pdfPath);

        if (jsonOutput)
        {
            Console.WriteLine(hint is { } count
                ? $"{{\"error\":\"{JsonWriter.Escape(ex.Message)}\",\"page_count_hint\":{count}}}"
                : $"{{\"error\":\"{JsonWriter.Escape(ex.Message)}\"}}");
            return;
        }

        Console.Error.WriteLine($"Error: {ex.Message}");
        if (hint is { } hintCount)
        {
            Console.Error.WriteLine($"Page count hint: {hintCount}");
        }
    }

    /// <summary>The heuristic page count from a file's raw bytes, when it finds any.</summary>
    private static uint? PageCountHint(string pdfPath)
    {
        try
        {
            var count = PdfDetector.EstimatePageCountFromBytes(File.ReadAllBytes(pdfPath));
            return count > 0 ? count : null;
        }
        catch (IOException)
        {
            return null;
        }
    }
}
