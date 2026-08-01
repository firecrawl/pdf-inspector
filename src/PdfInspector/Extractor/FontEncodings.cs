// Ported from reference/src/extractor/fonts.rs
using PdfInspector.Pdf;
using PdfInspector.Text;
using PdfInspector.ToUnicode;

namespace PdfInspector.Extractor;

/// <summary>The result of reading a font's <c>/Differences</c> array.</summary>
internal sealed class EncodingResult
{
    /// <summary>Character code to Unicode, for the codes Differences names.</summary>
    public Dictionary<byte, char> Map { get; } = [];

    /// <summary>
    /// Codes whose glyph names match the <c>gidNNNNN</c> pattern. These name raw
    /// glyph ids in the original font's table and only decode when the font's
    /// ToUnicode CMap maps the code.
    /// </summary>
    public List<byte> GidCodes { get; } = [];
}

/// <summary>
/// A resolved single-byte font encoding: a 256-entry table built from the base
/// encoding and any <c>/Differences</c> overrides. This replaces the encoding
/// handling the Rust build inherits from lopdf.
/// </summary>
internal sealed class SimpleFontEncoding
{
    private readonly ToUnicodeCMap? _cidCMap;

    public SimpleFontEncoding(char?[] table) => Table = table;

    /// <summary>
    /// The Identity-H/V form: two-byte CIDs resolved through the font's own
    /// ToUnicode CMap. The reference inherits this case from lopdf, whose
    /// <c>get_font_encoding</c> turns an Identity encoding into a ToUnicode-backed
    /// one rather than a byte table.
    /// </summary>
    private SimpleFontEncoding(ToUnicodeCMap cidCMap)
    {
        Table = [];
        _cidCMap = cidCMap;
    }

    public char?[] Table { get; }

    /// <summary>
    /// Builds the encoding for a font, or returns null when the font declares
    /// none and has no implicit default worth applying.
    /// </summary>
    public static SimpleFontEncoding? Build(PdfDocument doc, PdfDictionary fontDict)
    {
        var subtype = fontDict.Get("Subtype")?.AsName();
        if (subtype == "Type0")
        {
            // A composite font has no byte table. An Identity encoding still
            // gets an encoding here, resolving two-byte CIDs through the font's
            // own ToUnicode CMap, because the reference's decoding chain
            // consults it before falling back to byte interpretation — and an
            // unmapped CID must surface as a replacement character there rather
            // than as a stray Latin-1 letter.
            return BuildIdentityCidEncoding(doc, fontDict);
        }

        var encodingObj = fontDict.Get("Encoding");
        var resolved = encodingObj is null ? null : doc.Resolve(encodingObj);

        char?[] table;
        PdfDictionary? encodingDict = null;

        switch (resolved)
        {
            case PdfName name:
                table = (char?[]?)StandardEncodings.ByName(name.Value)?.Clone()
                    ?? (char?[])DefaultTable(fontDict).Clone();
                break;

            case PdfDictionary dict:
            {
                encodingDict = dict;
                var baseName = doc.GetName(dict, "BaseEncoding");
                table = (char?[]?)(baseName is null ? null : StandardEncodings.ByName(baseName))?.Clone()
                    ?? (char?[])DefaultTable(fontDict).Clone();
                break;
            }

            default:
                table = (char?[])DefaultTable(fontDict).Clone();
                break;
        }

        if (encodingDict is not null && doc.GetArray(encodingDict, "Differences") is { } differences)
        {
            ApplyDifferences(differences, table);
        }

        return new SimpleFontEncoding(table);
    }

    /// <summary>
    /// Builds the ToUnicode-backed encoding for an Identity-H/V composite font,
    /// or null when the font declares another encoding or has no usable CMap.
    /// </summary>
    private static SimpleFontEncoding? BuildIdentityCidEncoding(PdfDocument doc, PdfDictionary fontDict)
    {
        if (doc.Resolve(fontDict.Get("Encoding")) is not PdfName encoding
            || encoding.Value is not ("Identity-H" or "Identity-V"))
        {
            return null;
        }

        if (doc.Resolve(fontDict.Get("ToUnicode")) is not PdfStream stream)
        {
            return null;
        }

        var cmap = ToUnicodeCMap.Parse(stream.DecompressedContent() ?? stream.RawData);
        return cmap is null ? null : new SimpleFontEncoding(cmap);
    }

    /// <summary>
    /// The implicit encoding for a font with no explicit one. Non-symbolic fonts
    /// use StandardEncoding; the reference build's decoding path then falls back
    /// to Windows-1252 semantics for the C1 range, which
    /// <see cref="TextDecoder"/> applies separately.
    /// </summary>
    private static char?[] DefaultTable(PdfDictionary fontDict)
    {
        _ = fontDict;
        return StandardEncodings.Standard;
    }

    private static void ApplyDifferences(PdfArray differences, char?[] table)
    {
        var currentCode = 0;

        foreach (var item in differences)
        {
            switch (item)
            {
                case PdfInteger n:
                    currentCode = (int)n.Value;
                    break;

                case PdfName name:
                    if (currentCode is >= 0 and < 256)
                    {
                        table[currentCode] = GlyphNames.GlyphToChar(name.Value);
                    }

                    currentCode++;
                    break;
            }
        }
    }

    /// <summary>
    /// Decodes bytes through the table. Returns null when no byte resolves,
    /// letting the caller fall through to the other decoding strategies.
    /// </summary>
    public string? Decode(ReadOnlySpan<byte> bytes)
    {
        if (_cidCMap is not null)
        {
            return DecodeCidCodes(bytes);
        }

        var builder = new System.Text.StringBuilder(bytes.Length);
        var resolved = false;

        foreach (var b in bytes)
        {
            var ch = Table[b];
            if (ch is not null)
            {
                builder.Append(ch.Value);
                resolved = true;
            }
            else
            {
                // An undefined code becomes a replacement character, which the
                // caller treats as a decode failure worth retrying elsewhere.
                builder.Append('�');
            }
        }

        return resolved || bytes.Length == 0 ? builder.ToString() : null;
    }

    /// <summary>
    /// Decodes two-byte CIDs through the ToUnicode CMap, emitting a replacement
    /// character for each unmapped code. Unlike the byte-table path this always
    /// returns a string: the caller distinguishes success from failure by
    /// looking for the replacement character, exactly as the reference does.
    /// </summary>
    private string DecodeCidCodes(ReadOnlySpan<byte> bytes)
    {
        var builder = new System.Text.StringBuilder(bytes.Length / 2);

        for (var i = 0; i + 1 < bytes.Length; i += 2)
        {
            var cid = (ushort)((bytes[i] << 8) | bytes[i + 1]);
            var mapped = _cidCMap!.Lookup(cid);
            builder.Append(mapped is null || mapped.Contains('\uFFFD') ? "\uFFFD" : mapped);
        }

        return builder.ToString();
    }
}

/// <summary>Builds the per-page encoding structures the text decoder needs.</summary>
internal static class FontEncodings
{
    private const string Module = "fonts";

    /// <summary>
    /// Builds Differences-derived encoding maps for every font on a page.
    /// </summary>
    /// <returns>
    /// The maps by resource name, and whether any font uses raw glyph-id names
    /// that cannot be decoded. Gid names whose codes the font's own ToUnicode
    /// CMap covers are decodable and do not set the flag — LibreOffice subsets
    /// write <c>/gidNNNN</c> Differences names alongside a complete CMap.
    /// </returns>
    public static (Dictionary<string, Dictionary<byte, char>> Encodings, bool HasGidFonts) BuildPageFontEncodings(
        PdfDocument doc,
        IReadOnlyDictionary<string, PdfDictionary> fonts,
        FontCMaps cmaps)
    {
        var encodings = new Dictionary<string, Dictionary<byte, char>>(StringComparer.Ordinal);
        var hasGidFonts = false;

        foreach (var (resourceName, fontDict) in fonts)
        {
            var result = ParseFontEncoding(doc, fontDict);
            if (result is null)
            {
                continue;
            }

            if (result.GidCodes.Count > 0 && !ToUnicodeMapsCodes(fontDict, cmaps, result.GidCodes))
            {
                hasGidFonts = true;
            }

            if (result.Map.Count > 0)
            {
                encodings[resourceName] = result.Map;
            }
        }

        return (encodings, hasGidFonts);
    }

    /// <summary>
    /// True when the font's ToUnicode CMap maps at least one of the gid-named
    /// codes, so the Differences entries still decode through the CMap. The
    /// remaining unmapped codes are subset leftovers — the component glyphs of
    /// an emoji sequence mapped whole on its first code, for instance. A mapping
    /// counts only when extraction would accept it, so empty and replacement
    /// results are rejected here as they are there.
    /// </summary>
    private static bool ToUnicodeMapsCodes(PdfDictionary fontDict, FontCMaps cmaps, List<byte> codes)
    {
        if (fontDict.Get("ToUnicode")?.AsReference() is not { } objRef)
        {
            return false;
        }

        var entry = cmaps.GetByObject(objRef.Number);
        if (entry is null)
        {
            return false;
        }

        return codes.Any(code =>
            entry.Primary.Lookup(code) is { Length: > 0 } mapped && !mapped.Contains('�'));
    }

    /// <summary>
    /// Reads a font's <c>/Differences</c> overrides. Fonts whose encoding is a
    /// plain name have no Differences, and decode through
    /// <see cref="SimpleFontEncoding"/> instead.
    /// </summary>
    public static EncodingResult? ParseFontEncoding(PdfDocument doc, PdfDictionary fontDict)
    {
        var encodingObj = fontDict.Get("Encoding");
        if (encodingObj is null)
        {
            return null;
        }

        var baseFontName = fontDict.Get("BaseFont")?.AsName();
        var resolved = doc.Resolve(encodingObj);

        return resolved.AsDictionary() is { } encodingDict
            ? ParseEncodingDictionary(doc, encodingDict, baseFontName)
            : null;
    }

    public static EncodingResult? ParseEncodingDictionary(
        PdfDocument doc,
        PdfDictionary encodingDict,
        string? baseFontName)
    {
        var differences = doc.GetArray(encodingDict, "Differences");
        if (differences is null)
        {
            return null;
        }

        var result = new EncodingResult();
        var currentCode = 0;
        var ligatureCount = 0;

        foreach (var item in differences)
        {
            switch (item)
            {
                case PdfInteger n:
                    currentCode = (int)n.Value;
                    break;

                case PdfName nameObj:
                {
                    var glyphName = nameObj.Value;
                    var mapped = GlyphNames.GlyphToChar(glyphName)
                        ?? PrivateGlyphToChar(glyphName, baseFontName);

                    if (currentCode is >= 0 and < 256)
                    {
                        var code = (byte)currentCode;

                        if (mapped is { } ch && IsLigatureChar(ch))
                        {
                            Log.Debug(Module, () =>
                                $"  Differences: code=0x{code:X2} glyph=\"{glyphName}\" (ligature)");
                            ligatureCount++;
                        }

                        // Raw glyph-id names cannot map to Unicode without the
                        // original font's cmap table.
                        if (IsGidName(glyphName))
                        {
                            result.GidCodes.Add(code);
                        }

                        if (mapped is { } value)
                        {
                            result.Map[code] = value;
                        }
                        else
                        {
                            Log.Debug(Module, () =>
                                $"  Differences: code=0x{code:X2} glyph=\"{glyphName}\" (unmapped)");
                        }
                    }

                    currentCode = currentCode >= 255 ? 0 : currentCode + 1;
                    break;
                }
            }
        }

        if (ligatureCount > 0)
        {
            Log.Debug(Module, () =>
                $"  Differences: {result.Map.Count} total entries, {ligatureCount} ligatures");
        }

        if (result.GidCodes.Count > 0)
        {
            Log.Debug(Module, () =>
                $"  Differences: {result.GidCodes.Count} gid-encoded glyphs (decodable only via ToUnicode)");
        }

        return result;
    }

    private static bool IsGidName(string glyphName) =>
        glyphName.StartsWith("gid", StringComparison.Ordinal)
        && glyphName.Length >= 4
        && glyphName.AsSpan(3).ToArray().All(char.IsAsciiDigit);

    /// <summary>
    /// Aptos CFF subsets from Office PDFs expose the ff ligature as
    /// <c>/g431</c> with no ToUnicode map. This stays font-scoped because
    /// <c>/gNNN</c> names are private to the font that defines them.
    /// </summary>
    private static char? PrivateGlyphToChar(string glyphName, string? baseFontName)
    {
        if (baseFontName is null)
        {
            return null;
        }

        var stripped = StripSubsetPrefix(baseFontName);
        return stripped.Equals("Aptos", StringComparison.OrdinalIgnoreCase) && glyphName == "g431"
            ? '\uFB00'
            : null;
    }

    /// <summary>Removes the six-letter subset tag a font name may carry, as in "ABCDEF+Arial".</summary>
    public static string StripSubsetPrefix(string fontName)
    {
        var plus = fontName.IndexOf('+', StringComparison.Ordinal);
        return plus >= 0 ? fontName[(plus + 1)..] : fontName;
    }

    private static bool IsLigatureChar(char ch) => ch is >= '\uFB00' and <= '\uFB04';
}
