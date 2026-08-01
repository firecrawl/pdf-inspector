// Ported from reference/src/extractor/fonts.rs
using System.Text;
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;

namespace PdfInspector.Extractor;

/// <summary>Which of a font's two CMap variants a page has settled on.</summary>
internal enum CMapChoice
{
    Primary,
    Remapped,
}

/// <summary>
/// Accumulates decoded samples per font so the primary and remapped CMaps can be
/// compared once enough text has been seen, then reuses that verdict for the
/// rest of the page rather than re-scoring every operand.
/// </summary>
internal sealed class CMapDecisionCache
{
    private const int SampleTargetBytes = 240;

    private sealed class Decision
    {
        public StringBuilder PrimarySample { get; } = new();
        public StringBuilder RemappedSample { get; } = new();
        public int SampleBytes;
        public CMapChoice? Choice;
    }

    private readonly Dictionary<int, Decision> _decisions = [];

    public CMapChoice? GetChoice(int objNum) =>
        _decisions.TryGetValue(objNum, out var decision) ? decision.Choice : null;

    public CMapChoice? Consider(int objNum, string primary, string remapped, int bytesLength)
    {
        if (!_decisions.TryGetValue(objNum, out var entry))
        {
            entry = new Decision();
            _decisions[objNum] = entry;
        }

        entry.SampleBytes += bytesLength;
        entry.PrimarySample.Append(primary);
        entry.RemappedSample.Append(remapped);

        if (entry.Choice is null && entry.SampleBytes >= SampleTargetBytes)
        {
            var scorePrimary = TextDecoder.ScoreText(entry.PrimarySample.ToString());
            var scoreRemap = TextDecoder.ScoreText(entry.RemappedSample.ToString());
            entry.Choice = scoreRemap > scorePrimary + 5 ? CMapChoice.Remapped : CMapChoice.Primary;
        }

        return entry.Choice;
    }
}

/// <summary>Everything the decoder needs about the fonts on one page.</summary>
internal sealed class PageFontContext
{
    /// <summary>ToUnicode CMaps shared across the document, keyed by object number.</summary>
    public required FontCMaps DocumentCMaps { get; init; }

    /// <summary>Object number of each font's CMap, by resource name.</summary>
    public Dictionary<string, int> ToUnicodeRefs { get; } = [];

    /// <summary>CMaps built for fonts reached only through a Form XObject.</summary>
    public Dictionary<string, CMapEntry> InlineCMaps { get; } = [];

    /// <summary>Differences-derived code→character maps, by resource name.</summary>
    public Dictionary<string, Dictionary<byte, char>> Encodings { get; init; } = [];

    /// <summary>Resolved single-byte encodings, by resource name.</summary>
    public Dictionary<string, SimpleFontEncoding> SimpleEncodings { get; } = [];

    /// <summary>Glyph metrics, by resource name.</summary>
    public Dictionary<string, FontWidthInfo> Widths { get; init; } = [];

    public CMapDecisionCache CMapDecisions { get; } = new();
}

/// <summary>
/// Turns a content-stream string operand into text, trying the font's CMaps,
/// encoding differences, and a series of fallbacks in the order the reference
/// implementation established.
/// </summary>
internal static class TextDecoder
{
    private const string Module = "fonts";

    /// <summary>Decodes a string operand for the current font, or returns null when nothing decodes.</summary>
    public static string? ExtractTextFromOperand(
        PdfObject obj,
        string currentFont,
        string? baseFontName,
        PageFontContext context)
    {
        if (obj is not PdfString str)
        {
            return null;
        }

        var bytes = str.Bytes;
        var isType0CidFont = context.Widths.TryGetValue(currentFont, out var widthInfo) && widthInfo.IsCid;
        var useCp1252Fallback = ShouldUseCp1252SingleByteFallback(baseFontName, isType0CidFont);

        var result = Decode(bytes, currentFont, baseFontName, context, isType0CidFont, useCp1252Fallback);
        if (result is null)
        {
            return null;
        }

        result = CleanSymbolPua(result);
        result = RemapTexCmMathSymbols(result, baseFontName);
        return NormalizeCp1252Controls(result, useCp1252Fallback);
    }

    private static string? Decode(
        byte[] bytes,
        string currentFont,
        string? baseFontName,
        PageFontContext context,
        bool isType0CidFont,
        bool useCp1252Fallback)
    {
        var hasCMap = false;

        if (context.InlineCMaps.TryGetValue(currentFont, out var inlineEntry))
        {
            hasCMap = true;
            if (DecodeWithEntry(inlineEntry, bytes, currentFont, context, useCp1252Fallback) is { } inlineText)
            {
                return inlineText;
            }
        }

        if (context.ToUnicodeRefs.TryGetValue(currentFont, out var objNum) &&
            context.DocumentCMaps.GetByObject(objNum) is { } entry)
        {
            hasCMap = true;
            if (DecodeWithEntry(entry, bytes, currentFont, context, useCp1252Fallback) is { } cmapText)
            {
                return cmapText;
            }
        }

        // A CID font whose CMap could not decode has a genuinely unmapped CID.
        // Falling through to the text-interpretation fallbacks would read CID
        // bytes as character codes — CID 0x01A9 becoming Latin-1 "©", say — so
        // markers are emitted instead and the quality checks downstream fire.
        if (isType0CidFont && bytes.Any(b => b > 0x7F))
        {
            var cidCount = Math.Max(bytes.Length / 2, 1);
            return new string('�', cidCount);
        }

        // The Differences array overrides specific codes in a base encoding, so
        // its entries must be combined with that base rather than replacing it.
        if (context.Encodings.TryGetValue(currentFont, out var encodingMap))
        {
            var hasDiffMatch = bytes.Any(encodingMap.ContainsKey);
            if (hasDiffMatch)
            {
                var builder = new StringBuilder(bytes.Length);
                foreach (var b in bytes)
                {
                    if (encodingMap.TryGetValue(b, out var ch))
                    {
                        builder.Append(ch);
                    }
                    else if (b >= 0x20)
                    {
                        builder.Append(DecodeSingleByteFallbackChar(b, useCp1252Fallback));
                    }

                    // Unmapped control characters are dropped.
                }

                if (builder.Length > 0)
                {
                    return builder.ToString();
                }
            }
        }

        // A byte-order mark makes the intent explicit.
        if (bytes.Length >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF)
        {
            var text = DecodeUtf16BeLossy(bytes.AsSpan(2));
            if (text.Contains('�'))
            {
                Log.Debug(Module, () =>
                    $"utf16 loss produced replacement for font={currentFont} bytes_len={bytes.Length}");
            }

            return text;
        }

        // Null-heavy even-length runs are UTF-16BE without a mark.
        if (bytes.Length >= 4 && bytes.Length % 2 == 0)
        {
            var nulls = bytes.Count(b => b == 0);
            if (nulls * 4 > bytes.Length)
            {
                var text = DecodeUtf16BeLossy(bytes);
                if (ScoreText(text) > 0)
                {
                    return text;
                }
            }
        }

        // Some producers embed UTF-8 in a single-byte encoded font, writing
        // "José" as the two bytes C3 A9 rather than WinAnsi's E9.
        if (bytes.Any(b => b > 0x7F) && TryDecodeStrictUtf8(bytes) is { } utf8Text)
        {
            return utf8Text;
        }

        if (context.SimpleEncodings.TryGetValue(currentFont, out var encoding) &&
            encoding.Decode(bytes) is { } encoded)
        {
            var text = NormalizeCp1252Controls(encoded, useCp1252Fallback);
            if (!text.Contains('�'))
            {
                return text;
            }

            Log.Debug(Module, () =>
                $"decode_text produced replacement for font={currentFont} bytes_len={bytes.Length}");

            if (bytes.All(b => b is >= 0x20 and <= 0x7E))
            {
                var ascii = new StringBuilder(bytes.Length);
                foreach (var b in bytes)
                {
                    ascii.Append((char)b);
                }

                return ascii.ToString();
            }

            if (DecodeSymbolFallback(bytes, baseFontName) is { } symbolText)
            {
                return symbolText;
            }

            // For a font with a CMap the code is genuinely unmapped; returning
            // null avoids a Latin-1 fallback misreading CID bytes.
            if (hasCMap || context.ToUnicodeRefs.ContainsKey(currentFont))
            {
                return null;
            }

            // Other fonts fall through to the remaining strategies.
        }

        if (DecodeSymbolFallback(bytes, baseFontName) is { } lateSymbolText)
        {
            return lateSymbolText;
        }

        // Simple fonts use single-byte encodings. In practice the fallback should
        // follow Windows-1252 across 0x80–0x9F, so a byte like 0x92 becomes smart
        // punctuation rather than a C1 control that looks like CID mojibake.
        return DecodeSingleByteFallback(bytes, useCp1252Fallback);
    }

    /// <summary>Decodes through one font's CMap entry, using its byte width and alternatives.</summary>
    private static string? DecodeWithEntry(
        CMapEntry entry,
        byte[] bytes,
        string currentFont,
        PageFontContext context,
        bool useCp1252Fallback)
    {
        if (entry.Primary.CodeByteLength == 1)
        {
            return DecodeSingleByteCMap(entry, bytes, currentFont, context, useCp1252Fallback);
        }

        // Some files emit one-byte codes even for a Type0 font.
        if (bytes.Length % 2 == 1)
        {
            var builder = new StringBuilder();
            foreach (var (_, mapped) in entry.Primary.LookupBytes(bytes))
            {
                if (mapped is not null)
                {
                    builder.Append(mapped);
                }
            }

            if (builder.Length > 0)
            {
                return builder.ToString();
            }
        }

        var decodedPrimary = entry.Primary.DecodeCids(bytes);

        if (entry.Remapped is { } remapped)
        {
            var decodedRemap = remapped.DecodeCids(bytes);
            var decodedFallback = entry.Fallback?.DecodeCids(bytes);
            var objNum = context.ToUnicodeRefs.GetValueOrDefault(currentFont, 0);

            if (context.CMapDecisions.GetChoice(objNum) is { } settled)
            {
                var chosen = settled == CMapChoice.Primary ? decodedPrimary : decodedRemap;
                if (chosen.Length > 0)
                {
                    return chosen;
                }
            }

            var choice = context.CMapDecisions.Consider(objNum, decodedPrimary, decodedRemap, bytes.Length);
            var decoded = choice switch
            {
                CMapChoice.Primary => decodedPrimary,
                CMapChoice.Remapped => decodedRemap,
                _ => ChooseBestCMapDecode(decodedPrimary, decodedRemap),
            };

            if (decodedFallback is { } fb)
            {
                var expected = bytes.Length / 2;
                var decodedLength = Text.TextUtils.CharCount(decoded);
                var preferFallback = (fb.Length > 0 && decoded.Length == 0)
                    || (fb.Length > 0 && expected > 0 && decodedLength * 2 < expected);

                if (preferFallback || ScoreText(fb) > ScoreText(decoded) + 3)
                {
                    decoded = fb;
                }
            }

            return decoded.Length > 0 ? decoded : null;
        }

        if (decodedPrimary.Length > 0)
        {
            if (entry.Fallback?.DecodeCids(bytes) is { } fb)
            {
                var expected = bytes.Length / 2;
                var decodedLength = Text.TextUtils.CharCount(decodedPrimary);
                var preferFallback = (fb.Length > 0 && decodedPrimary.Length == 0)
                    || (fb.Length > 0 && expected > 0 && decodedLength * 2 < expected);

                if (preferFallback || ScoreText(fb) > ScoreText(decodedPrimary) + 3)
                {
                    return fb;
                }
            }

            return decodedPrimary;
        }

        return null;
    }

    /// <summary>
    /// Merges the CMap and Differences at byte level for a single-byte CMap: the
    /// CMap first, then the fallback CMap, then Differences, then a printable
    /// fallback. Doing this per byte keeps a partial CMap result from blocking
    /// the Differences path.
    /// </summary>
    private static string? DecodeSingleByteCMap(
        CMapEntry entry,
        byte[] bytes,
        string currentFont,
        PageFontContext context,
        bool useCp1252Fallback)
    {
        context.Encodings.TryGetValue(currentFont, out var encodingMap);

        var builder = new StringBuilder(bytes.Length);
        foreach (var b in bytes)
        {
            if (entry.Primary.Lookup(b) is { } primary && !primary.Contains('�'))
            {
                builder.Append(primary);
                continue;
            }

            if (entry.Fallback?.Lookup(b) is { } fallback && !fallback.Contains('�'))
            {
                builder.Append(fallback);
                continue;
            }

            if (encodingMap is not null && encodingMap.TryGetValue(b, out var ch))
            {
                builder.Append(ch);
                continue;
            }

            if (b >= 0x20)
            {
                builder.Append(DecodeSingleByteFallbackChar(b, useCp1252Fallback));
            }
        }

        return builder.Length > 0 ? builder.ToString() : null;
    }

    // ── Fallbacks and repairs ────────────────────────────────────────────

    private static string DecodeSingleByteFallback(byte[] bytes, bool useCp1252Fallback)
    {
        var builder = new StringBuilder(bytes.Length);
        foreach (var b in bytes)
        {
            builder.Append(DecodeSingleByteFallbackChar(b, useCp1252Fallback));
        }

        return builder.ToString();
    }

    private static char DecodeSingleByteFallbackChar(byte b, bool useCp1252Fallback)
    {
        if (!useCp1252Fallback)
        {
            return (char)b;
        }

        return b switch
        {
            0x80 => '€',
            0x82 => '‚',
            0x83 => 'ƒ',
            0x84 => '„',
            0x85 => '…',
            0x86 => '†',
            0x87 => '‡',
            0x88 => 'ˆ',
            0x89 => '‰',
            0x8A => 'Š',
            0x8B => '‹',
            0x8C => 'Œ',
            0x8E => 'Ž',
            0x91 => '‘',
            0x92 => '’',
            0x93 => '“',
            0x94 => '”',
            0x95 => '•',
            0x96 => '–',
            0x97 => '—',
            0x98 => '˜',
            0x99 => '™',
            0x9A => 'š',
            0x9B => '›',
            0x9C => 'œ',
            0x9E => 'ž',
            0x9F => 'Ÿ',
            _ => (char)b,
        };
    }

    private static string NormalizeCp1252Controls(string text, bool useCp1252Fallback)
    {
        if (!useCp1252Fallback)
        {
            return text;
        }

        var hasC1 = false;
        foreach (var ch in text)
        {
            if (ch is >= '\u0080' and <= '\u009F')
            {
                hasC1 = true;
                break;
            }
        }

        if (!hasC1)
        {
            return text;
        }

        var builder = new StringBuilder(text.Length);
        foreach (var ch in text)
        {
            builder.Append(ch is >= '\u0080' and <= '\u009F'
                ? DecodeSingleByteFallbackChar((byte)ch, true)
                : ch);
        }

        return builder.ToString();
    }

    private static bool ShouldUseCp1252SingleByteFallback(string? baseFontName, bool isType0CidFont)
    {
        if (isType0CidFont)
        {
            return false;
        }

        if (baseFontName is null)
        {
            return true;
        }

        var fontName = FontEncodings.StripSubsetPrefix(baseFontName).ToLowerInvariant();

        // TeX and Computer Modern faces, and math and symbol fonts generally,
        // place ligatures or symbols in the C1 byte range. Reading those as
        // Windows-1252 turns "deficiente" into "de…ciente" and "fluid" into "‡uid".
        string[] nonCp1252Prefixes =
        [
            "cmr", "cmb", "cmmi", "cmsy", "cmex", "cmtt", "cmss", "cmti",
            "ecrm", "ecbx", "ecti", "tcrm", "tctt", "msam", "msbm", "ttdc",
        ];

        if (nonCp1252Prefixes.Any(prefix => fontName.StartsWith(prefix, StringComparison.Ordinal)))
        {
            return false;
        }

        string[] nonCp1252Names = ["math", "symbol", "dingbat", "emoji"];
        return !nonCp1252Names.Any(name => fontName.Contains(name, StringComparison.Ordinal));
    }

    /// <summary>
    /// Replaces private-use characters in the F000–F0FF range with standard
    /// equivalents. They come from Symbol and Wingdings fonts whose ToUnicode
    /// CMaps map into the private-use area.
    /// </summary>
    private static string CleanSymbolPua(string text)
    {
        var hasPua = false;
        foreach (var c in text)
        {
            if (c is >= '\uF000' and <= '\uF0FF')
            {
                hasPua = true;
                break;
            }
        }

        if (!hasPua)
        {
            return text;
        }

        var builder = new StringBuilder(text.Length);
        foreach (var c in text)
        {
            if (c is < '\uF000' or > '\uF0FF')
            {
                builder.Append(c);
                continue;
            }

            var low = c - 0xF000;
            builder.Append(low switch
            {
                0xA1 or 0xA7 or 0xB7 => '•', // bullets
                0xFC => '✓',                 // checkmark
                >= 0x20 and <= 0xFF => (char)low, // strip the offset
                _ => c,
            });
        }

        return builder.ToString();
    }

    private static string? DecodeSymbolFallback(byte[] bytes, string? baseFontName)
    {
        if (baseFontName is null)
        {
            return null;
        }

        var name = baseFontName.ToLowerInvariant();
        if (!name.Contains("symbol", StringComparison.Ordinal)
            && !name.Contains("wingdings", StringComparison.Ordinal)
            && !name.Contains("zapfdingbats", StringComparison.Ordinal))
        {
            return null;
        }

        var builder = new StringBuilder();
        foreach (var b in bytes)
        {
            if (b >= 0x20)
            {
                builder.Append((char)(0xF000 + b));
            }
        }

        return builder.Length > 0 ? builder.ToString() : null;
    }

    /// <summary>
    /// Fixes a known producer bug in "TeXCMMathsSymbols" subset fonts, where the
    /// Computer Modern symbol glyphs are misnamed after Latin lookalikes (equal
    /// as /onequarter, plus as /thorn, and so on) and the generated ToUnicode
    /// faithfully propagates the wrong names. The remap applies only to text
    /// decoded from that font, keyed on the glyphs' observed misnames.
    /// </summary>
    private static string RemapTexCmMathSymbols(string text, string? baseFontName)
    {
        if (baseFontName is null)
        {
            return text;
        }

        var stripped = FontEncodings.StripSubsetPrefix(baseFontName);
        if (!stripped.Equals("TeXCMMathsSymbols", StringComparison.OrdinalIgnoreCase))
        {
            return text;
        }

        var builder = new StringBuilder(text.Length);
        foreach (var ch in text)
        {
            builder.Append(ch switch
            {
                '\u00BC' => '=', // onequarter
                '\u00BD' => '-', // onehalf
                '\u00FE' => '+', // thorn
                '\u00F0' => '(', // eth
                '\u00DE' => ')', // Thorn
                _ => ch,
            });
        }

        return builder.ToString();
    }

    private static string ChooseBestCMapDecode(string primary, string remapped)
    {
        if (primary.Length == 0)
        {
            return remapped;
        }

        if (remapped.Length == 0)
        {
            return primary;
        }

        return ScoreText(remapped) > ScoreText(primary) + 3 ? remapped : primary;
    }

    // ── Text scoring ─────────────────────────────────────────────────────

    private static readonly string[] CommonWords =
    [
        "the", "and", "of", "to", "in", "a", "is", "that", "for", "with", "on",
        "as", "by", "from", "this", "be", "are", "at", "or", "not", "it", "our",
    ];

    /// <summary>
    /// Scores how much a string looks like real text, used to pick between
    /// competing decodes. Common words dominate, letters and spaces contribute,
    /// and control or replacement characters count heavily against.
    /// </summary>
    public static int ScoreText(string text)
    {
        var letters = 0;
        var spaces = 0;
        var digits = 0;
        var other = 0;
        var wordHits = 0;

        var current = new StringBuilder();

        void FlushWord()
        {
            if (current.Length == 0)
            {
                return;
            }

            var word = current.ToString();
            if (CommonWords.Contains(word, StringComparer.Ordinal))
            {
                wordHits++;
            }

            current.Clear();
        }

        foreach (var ch in text)
        {
            if (char.IsAsciiLetter(ch))
            {
                letters++;
                current.Append(char.ToLowerInvariant(ch));
                continue;
            }

            FlushWord();

            if (ch == ' ')
            {
                spaces++;
            }
            else if (char.IsAsciiDigit(ch))
            {
                digits++;
            }
            else if (char.IsControl(ch) || ch == '�')
            {
                other += 3;
            }
            else if (ch is >= '一' and <= '鿿'
                or >= '぀' and <= 'ゟ'
                or >= '゠' and <= 'ヿ'
                or >= '㐀' and <= '䶿'
                or >= '豈' and <= '﫿')
            {
                // CJK ideographs and kana are valid text.
                letters++;
            }
            else
            {
                other++;
            }
        }

        FlushWord();

        var score = (wordHits * 10) + letters + (spaces * 2) + digits - (other * 2);
        if (letters > 15 && wordHits == 0)
        {
            score -= 15;
        }

        return score;
    }

    // ── Encoding helpers ─────────────────────────────────────────────────

    /// <summary>Decodes UTF-16BE, substituting the replacement character for unpaired surrogates.</summary>
    private static string DecodeUtf16BeLossy(ReadOnlySpan<byte> bytes)
    {
        var units = bytes.Length / 2;
        var builder = new StringBuilder(units);

        for (var i = 0; i < units; i++)
        {
            var unit = (char)((bytes[i * 2] << 8) | bytes[(i * 2) + 1]);

            if (char.IsHighSurrogate(unit))
            {
                if (i + 1 < units)
                {
                    var next = (char)((bytes[(i + 1) * 2] << 8) | bytes[((i + 1) * 2) + 1]);
                    if (char.IsLowSurrogate(next))
                    {
                        builder.Append(unit).Append(next);
                        i++;
                        continue;
                    }
                }

                builder.Append('�');
                continue;
            }

            builder.Append(char.IsLowSurrogate(unit) ? '�' : unit);
        }

        return builder.ToString();
    }

    /// <summary>Decodes strict UTF-8, returning null when the bytes are not valid.</summary>
    private static string? TryDecodeStrictUtf8(byte[] bytes)
    {
        try
        {
            return new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true)
                .GetString(bytes);
        }
        catch (DecoderFallbackException)
        {
            return null;
        }
    }
}
