// Ported from reference/src/bin/pdf2md.rs and reference/src/bin/detect_pdf.rs
using System.Globalization;
using System.Text;
using PdfInspector.Detector;

namespace PdfInspector.Cli;

/// <summary>
/// The hand-rolled JSON emitter both commands share, matching the reference
/// binaries byte for byte.
/// </summary>
internal static class JsonWriter
{
    /// <summary>
    /// Escapes a string for a JSON string value: backslash, double quote and
    /// every control character below U+0020.
    /// </summary>
    public static string Escape(string s)
    {
        var output = new StringBuilder(s.Length + 16);
        foreach (var ch in s)
        {
            switch (ch)
            {
                case '\\':
                    output.Append("\\\\");
                    break;

                case '"':
                    output.Append("\\\"");
                    break;

                case '\n':
                    output.Append("\\n");
                    break;

                case '\r':
                    output.Append("\\r");
                    break;

                case '\t':
                    output.Append("\\t");
                    break;

                case '\b':
                    output.Append("\\b");
                    break;

                case '\f':
                    output.Append("\\f");
                    break;

                default:
                    if (ch < ' ')
                    {
                        output.Append(CultureInfo.InvariantCulture, $"\\u{(int)ch:x4}");
                    }
                    else
                    {
                        output.Append(ch);
                    }

                    break;
            }
        }

        return output.ToString();
    }

    /// <summary>The reference's snake_case name for a classification.</summary>
    public static string PdfTypeName(PdfType type) => type switch
    {
        PdfType.TextBased => "text_based",
        PdfType.Scanned => "scanned",
        PdfType.ImageBased => "image_based",
        PdfType.Mixed => "mixed",
        _ => "text_based",
    };

    /// <summary>Renders a list of page numbers as a bare JSON array body.</summary>
    public static string PageList(IEnumerable<uint> pages) => string.Join(',', pages);

    /// <summary>Renders per-page OCR reasons as a JSON array body.</summary>
    public static string OcrReasons(IEnumerable<PageOcrReasons> reasons) =>
        string.Join(',', reasons.Select(entry =>
        {
            var reasonsJson = string.Join(',', entry.Reasons.Select(r => $"\"{Escape(r)}\""));
            return $"{{\"page\":{entry.Page},\"reasons\":[{reasonsJson}]}}";
        }));

    /// <summary>Renders the detector's own per-page reason map as a JSON array body.</summary>
    public static string DetectorOcrReasons(SortedDictionary<uint, List<string>> reasons) =>
        string.Join(',', reasons.Select(kv =>
        {
            var reasonsJson = string.Join(',', kv.Value.Select(r => $"\"{Escape(r)}\""));
            return $"{{\"page\":{kv.Key},\"reasons\":[{reasonsJson}]}}";
        }));

    /// <summary>Renders a value as a JSON string, or the literal null.</summary>
    public static string NullableString(string? value) =>
        value is null ? "null" : $"\"{Escape(value)}\"";

    /// <summary>Renders a boolean the way the reference does.</summary>
    public static string Bool(bool value) => value ? "true" : "false";
}
