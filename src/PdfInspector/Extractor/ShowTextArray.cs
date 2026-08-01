// Ported from reference/src/extractor/content_stream.rs
using System.Text;
using PdfInspector.Pdf;

namespace PdfInspector.Extractor;

/// <summary>
/// One run of text from a TJ array, with the text-space offsets at which it
/// starts and ends relative to the operator's origin.
/// </summary>
internal readonly record struct TextRun(string Text, float StartWidthTs, float EndWidthTs);

/// <summary>
/// Splits a TJ array into runs. The array interleaves strings with numeric
/// adjustments; a large negative adjustment is a column-sized gap that should
/// break the text into separate items rather than merely inserting a space.
/// </summary>
internal static class ShowTextArray
{
    public readonly record struct Result(List<TextRun> Runs, float TotalWidthTs);

    /// <summary>
    /// Walks the array, accumulating decoded text and text-space advance.
    /// </summary>
    /// <param name="suppressText">
    /// When true the text is not collected — used for invisible text and for
    /// spans replaced by ActualText — but the advance is still accumulated so
    /// later operators stay positioned.
    /// </param>
    public static Result Segment(
        PdfArray array,
        FontWidthInfo? fontInfo,
        float fontSize,
        float charSpacing,
        float wordSpacing,
        bool suppressText,
        Func<PdfObject, string?> decode)
    {
        // The gap that counts as a word space, derived from the font's own
        // space width where available.
        float spaceThreshold;
        if (fontInfo is not null)
        {
            var spaceEm = fontInfo.SpaceWidth * fontInfo.UnitsScale;
            spaceThreshold = MathF.Max(spaceEm * 1000.0f * 0.4f, 80.0f);
        }
        else
        {
            spaceThreshold = 120.0f;
        }

        var columnGapThreshold = spaceThreshold * 4.0f;

        var runs = new List<TextRun>();
        var currentText = new StringBuilder();
        var subStartWidthTs = 0.0f;
        var totalWidthTs = 0.0f;

        foreach (var element in array)
        {
            if (element.AsNumber() is { } adjustment && element is not PdfString)
            {
                var value = (float)adjustment;
                var displacement = -value / 1000.0f * fontSize;

                if (!suppressText && value < -columnGapThreshold && currentText.Length > 0)
                {
                    // A column-sized gap ends the run.
                    runs.Add(new TextRun(currentText.ToString(), subStartWidthTs, totalWidthTs));
                    currentText.Clear();
                    totalWidthTs += displacement;
                    subStartWidthTs = totalWidthTs;
                }
                else
                {
                    totalWidthTs += displacement;
                    if (!suppressText && value < -spaceThreshold && currentText.Length > 0 &&
                        currentText[^1] != ' ')
                    {
                        currentText.Append(' ');
                    }
                }

                continue;
            }

            if (fontInfo is not null && element is PdfString str)
            {
                totalWidthTs += FontWidths.ComputeStringWidthTs(
                    str.Bytes, fontInfo, fontSize, charSpacing, wordSpacing);
            }

            if (!suppressText && decode(element) is { } text)
            {
                currentText.Append(text);
            }
        }

        if (!suppressText && currentText.ToString().Trim().Length > 0)
        {
            runs.Add(new TextRun(currentText.ToString(), subStartWidthTs, totalWidthTs));
        }

        return new Result(runs, totalWidthTs);
    }
}
