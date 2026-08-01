// Ported from reference/src/extractor/fonts.rs
using PdfInspector.Pdf;

namespace PdfInspector.Extractor;

/// <summary>Glyph metrics read from a font dictionary.</summary>
public sealed class FontWidthInfo
{
    /// <summary>Glyph widths in font units, keyed by character code.</summary>
    public Dictionary<ushort, ushort> Widths { get; init; } = [];

    /// <summary>Width used for codes missing from <see cref="Widths"/>.</summary>
    public ushort DefaultWidth { get; init; }

    /// <summary>Width of the space character, measured or estimated.</summary>
    public ushort SpaceWidth { get; set; }

    /// <summary>True for a CID font, whose character codes are two bytes.</summary>
    public bool IsCid { get; init; }

    /// <summary>
    /// Converts font units to text-space units: 0.001 for Type1 and TrueType
    /// (widths in thousandths of an em), or FontMatrix[0] for Type3.
    /// </summary>
    public float UnitsScale { get; init; } = 0.001f;

    /// <summary>Writing mode: 0 for horizontal, 1 for vertical.</summary>
    public byte WMode { get; init; }
}

/// <summary>Reads glyph metrics out of font dictionaries.</summary>
internal static class FontWidths
{
    private const string Module = "fonts";

    /// <summary>Builds width info for every font on a page, keyed by resource name.</summary>
    public static Dictionary<string, FontWidthInfo> BuildPageFontWidths(
        PdfDocument doc,
        IReadOnlyDictionary<string, PdfDictionary> fonts)
    {
        var widths = new Dictionary<string, FontWidthInfo>(StringComparer.Ordinal);

        foreach (var (resourceName, fontDict) in fonts)
        {
            if (Log.IsEnabled(Module))
            {
                var subtype = fontDict.Get("Subtype")?.AsName() ?? string.Empty;
                var baseFont = fontDict.Get("BaseFont")?.AsName() ?? string.Empty;
                var hasToUnicode = fontDict.Get("ToUnicode") is not null;
                var hasDescendants = fontDict.Get("DescendantFonts") is not null;
                var encoding = fontDict.Get("Encoding") switch
                {
                    PdfName n => n.Value,
                    PdfReference => "ref(dict)",
                    PdfDictionary => "dict",
                    null => "none",
                    var other => other.ToString() ?? "none",
                };

                Log.Debug(Module,
                    $"font {resourceName,-10} sub={subtype,-12} base={baseFont,-45} " +
                    $"toUni={hasToUnicode,-6} enc={encoding,-20} cid={hasDescendants}");
            }

            if (Parse(doc, fontDict) is { } info)
            {
                widths[resourceName] = info;
            }
        }

        return widths;
    }

    /// <summary>Parses widths from a font dictionary, dispatching on <c>/Subtype</c>.</summary>
    public static FontWidthInfo? Parse(PdfDocument doc, PdfDictionary fontDict) =>
        fontDict.Get("Subtype")?.AsName() switch
        {
            "Type0" => ParseType0(doc, fontDict),
            "Type1" or "TrueType" or "MMType1" or "Type3" => ParseSimple(doc, fontDict),
            _ => null,
        };

    /// <summary>
    /// Parses widths for a simple font from FirstChar, LastChar, and Widths.
    /// Type3 fonts also carry a FontMatrix that sets the unit scale.
    /// </summary>
    public static FontWidthInfo? ParseSimple(PdfDocument doc, PdfDictionary fontDict)
    {
        if (doc.GetInteger(fontDict, "FirstChar") is not { } firstCharValue ||
            doc.GetInteger(fontDict, "LastChar") is not { } lastCharValue)
        {
            return null;
        }

        var widthsArray = doc.GetArray(fontDict, "Widths");
        if (widthsArray is null)
        {
            return null;
        }

        var firstChar = (ushort)firstCharValue;
        var lastChar = (ushort)lastCharValue;

        var widths = new Dictionary<ushort, ushort>();
        ushort spaceWidth = 0;

        for (var i = 0; i < widthsArray.Count; i++)
        {
            var code = (ushort)(firstChar + i);
            if (code > lastChar)
            {
                break;
            }

            if (doc.Resolve(widthsArray[i]).AsNumber() is not { } value)
            {
                continue;
            }

            var w = ToU16(value);
            if (code == 32)
            {
                spaceWidth = w;
            }

            widths[code] = w;
        }

        // Type3 fonts define their own glyph space through FontMatrix.
        var unitsScale = 0.001f;
        if (doc.GetArray(fontDict, "FontMatrix") is { Count: > 0 } matrix &&
            matrix[0].AsNumber() is { } scale)
        {
            unitsScale = MathF.Abs((float)scale);
        }

        // The default of 250 is calibrated for standard 1000-unit fonts. A Type3
        // font with a different glyph space needs an estimate from its own metrics.
        if (spaceWidth == 0)
        {
            if (widths.Count > 0 && MathF.Abs(unitsScale - 0.001f) > 0.0005f)
            {
                var sum = widths.Values.Aggregate(0u, (acc, w) => acc + w);
                var average = (float)sum / widths.Count;
                spaceWidth = (ushort)MathF.Max(average * 0.45f, 1.0f);
            }
            else
            {
                spaceWidth = 250;
            }
        }

        return new FontWidthInfo
        {
            Widths = widths,
            DefaultWidth = 0,
            SpaceWidth = spaceWidth,
            IsCid = false,
            UnitsScale = unitsScale,
            WMode = 0,
        };
    }

    /// <summary>Parses widths for a Type0 font from its descendant CIDFont's W array and DW.</summary>
    public static FontWidthInfo? ParseType0(PdfDocument doc, PdfDictionary fontDict)
    {
        var descFonts = doc.GetArray(fontDict, "DescendantFonts");
        if (descFonts is null || descFonts.Count == 0)
        {
            return null;
        }

        var cidFontDict = doc.Resolve(descFonts[0]).AsDictionary();
        if (cidFontDict is null)
        {
            return null;
        }

        var defaultWidth = cidFontDict.Get("DW")?.AsNumber() is { } dw ? ToU16(dw) : (ushort)1000;

        var widths = new Dictionary<ushort, ushort>();
        if (doc.GetArray(cidFontDict, "W") is { } wArray)
        {
            ParseCidWArray(doc, wArray, widths);
        }

        // CID 32 is the usual space; CID 3 is the convention in several
        // character collections.
        ushort spaceWidth;
        if (widths.TryGetValue(32, out var w32))
        {
            spaceWidth = w32;
        }
        else if (widths.TryGetValue(3, out var w3))
        {
            spaceWidth = w3;
        }
        else
        {
            spaceWidth = defaultWidth > 0 ? (ushort)(defaultWidth / 4) : (ushort)250;
        }

        var wmode = fontDict.Get("WMode")?.AsInteger() is { } mode ? (byte)mode : (byte)0;

        return new FontWidthInfo
        {
            Widths = widths,
            DefaultWidth = defaultWidth,
            SpaceWidth = spaceWidth,
            IsCid = true,
            UnitsScale = 0.001f, // CID fonts always use the 1000-unit system.
            WMode = wmode,
        };
    }

    /// <summary>
    /// Parses a CID <c>/W</c> array. Two forms interleave: <c>c [w1 w2 …]</c>
    /// assigns consecutive widths from c, and <c>c_first c_last w</c> assigns one
    /// width across a range.
    /// </summary>
    public static void ParseCidWArray(PdfDocument doc, PdfArray wArray, Dictionary<ushort, ushort> widths)
    {
        var i = 0;
        while (i < wArray.Count)
        {
            if (wArray[i].AsNumber() is not { } startValue)
            {
                i++;
                continue;
            }

            var startCid = ToU16(startValue);
            i++;
            if (i >= wArray.Count)
            {
                break;
            }

            var next = wArray[i];

            // A reference here may resolve to either form.
            if (next is PdfReference)
            {
                if (doc.Resolve(next).AsArray() is { } resolvedArray)
                {
                    FillConsecutive(resolvedArray, startCid, widths);
                }

                i++;
                continue;
            }

            if (next.AsArray() is { } inlineArray)
            {
                FillConsecutive(inlineArray, startCid, widths);
                i++;
                continue;
            }

            if (next.AsNumber() is { } endValue)
            {
                var endCid = ToU16(endValue);
                i++;
                if (i >= wArray.Count)
                {
                    break;
                }

                if (wArray[i].AsNumber() is not { } widthValue)
                {
                    i++;
                    continue;
                }

                var w = ToU16(widthValue);
                for (var cid = (uint)startCid; cid <= endCid; cid++)
                {
                    widths[(ushort)cid] = w;
                }

                i++;
                continue;
            }

            i++;
        }
    }

    private static void FillConsecutive(PdfArray array, ushort startCid, Dictionary<ushort, ushort> widths)
    {
        for (var j = 0; j < array.Count; j++)
        {
            if (array[j].AsNumber() is { } value)
            {
                widths[(ushort)(startCid + j)] = ToU16(value);
            }
        }
    }

    /// <summary>Truncates toward zero and wraps, matching Rust's <c>as u16</c> cast.</summary>
    private static ushort ToU16(double value)
    {
        if (double.IsNaN(value))
        {
            return 0;
        }

        var truncated = Math.Truncate(value);
        if (truncated <= 0)
        {
            return 0;
        }

        return truncated >= ushort.MaxValue ? ushort.MaxValue : (ushort)truncated;
    }

    /// <summary>
    /// Computes the width of a string in text-space units. Character spacing
    /// (Tc) is added per glyph and word spacing (Tw) per space character, which
    /// is what the rendering model specifies: tx = (w0 × Tfs + Tc + Tw) per glyph.
    /// </summary>
    public static float ComputeStringWidthTs(
        ReadOnlySpan<byte> bytes,
        FontWidthInfo fontInfo,
        float fontSize,
        float charSpacing,
        float wordSpacing)
    {
        var total = 0.0f;
        var numSpaces = 0;
        int numChars;

        if (fontInfo.IsCid)
        {
            var count = 0;
            for (var j = 0; j + 1 < bytes.Length; j += 2)
            {
                var cid = (ushort)((bytes[j] << 8) | bytes[j + 1]);
                total += fontInfo.Widths.TryGetValue(cid, out var w) ? w : fontInfo.DefaultWidth;

                // CID 32 is the space in most CID fonts.
                if (cid == 32)
                {
                    numSpaces++;
                }

                count++;
            }

            numChars = count;
        }
        else
        {
            foreach (var b in bytes)
            {
                total += fontInfo.Widths.TryGetValue(b, out var w) ? w : fontInfo.DefaultWidth;
                if (b == 0x20)
                {
                    numSpaces++;
                }
            }

            numChars = bytes.Length;
        }

        return (total * fontInfo.UnitsScale * fontSize)
            + (numChars * charSpacing)
            + (numSpaces * wordSpacing);
    }
}
