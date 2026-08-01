// Ported from reference/src/tounicode.rs
using System.Globalization;
using System.Text;

namespace PdfInspector.ToUnicode;

/// <summary>A parsed ToUnicode CMap, mapping character codes to Unicode strings.</summary>
public sealed class ToUnicodeCMap
{
    /// <summary>Direct mappings from code to Unicode text.</summary>
    public Dictionary<ushort, string> CharMap { get; } = [];

    /// <summary>Range mappings: start code, end code, and the base Unicode scalar.</summary>
    public List<(ushort Start, ushort End, uint Base)> Ranges { get; } = [];

    /// <summary>Byte width of source codes (1 or 2), from the codespace or inferred from entries.</summary>
    public byte CodeByteLength { get; set; }

    /// <summary>
    /// When true, unmapped codes are read as Unicode code points directly. A last
    /// resort for Identity-H fonts with no ToUnicode, cmap, or glyph names.
    /// </summary>
    public bool CidPassthrough { get; set; }

    public ToUnicodeCMap Clone()
    {
        var copy = new ToUnicodeCMap
        {
            CodeByteLength = CodeByteLength,
            CidPassthrough = CidPassthrough,
        };

        // A bundled Adobe CMap runs to tens of thousands of entries, so both
        // containers are sized before they are filled.
        copy.CharMap.EnsureCapacity(CharMap.Count);
        foreach (var (key, value) in CharMap)
        {
            copy.CharMap[key] = value;
        }

        copy.Ranges.Capacity = Ranges.Count;
        copy.Ranges.AddRange(Ranges);
        return copy;
    }

    public int EntryCount => CharMap.Count + Ranges.Count;

    // ── Parsing ──────────────────────────────────────────────────────────

    /// <summary>Parses a ToUnicode CMap from its decoded stream contents.</summary>
    public static ToUnicodeCMap? Parse(byte[] content)
    {
        // The reference build reads this lossily; invalid sequences become U+FFFD
        // and simply fail to match any keyword.
        var text = Encoding.UTF8.GetString(content);
        var cmap = new ToUnicodeCMap();
        var srcHexLengths = new List<int>();

        // The codespace range declares the source byte width.
        byte? codespaceByteLength = null;
        var csStart = text.IndexOf("begincodespacerange", StringComparison.Ordinal);
        if (csStart >= 0)
        {
            var sectionStart = csStart + "begincodespacerange".Length;
            var csEnd = text.IndexOf("endcodespacerange", sectionStart, StringComparison.Ordinal);
            if (csEnd >= 0)
            {
                var section = text[sectionStart..csEnd];
                var inHex = false;
                var hexLen = 0;
                foreach (var c in section)
                {
                    if (c == '<')
                    {
                        inHex = true;
                        hexLen = 0;
                    }
                    else if (c == '>')
                    {
                        if (inHex && hexLen > 0)
                        {
                            codespaceByteLength = (byte)((hexLen + 1) / 2);
                        }

                        inHex = false;
                    }
                    else if (inHex && char.IsAsciiHexDigit(c))
                    {
                        hexLen++;
                    }
                }
            }
        }

        var useCMapName = FindUseCMapName(text);

        var pos = 0;
        while (true)
        {
            var start = text.IndexOf("beginbfchar", pos, StringComparison.Ordinal);
            if (start < 0)
            {
                break;
            }

            var sectionStart = start + "beginbfchar".Length;
            var end = text.IndexOf("endbfchar", sectionStart, StringComparison.Ordinal);
            if (end < 0)
            {
                break;
            }

            cmap.ParseBfCharSection(text[sectionStart..end], srcHexLengths);
            pos = end;
        }

        pos = 0;
        while (true)
        {
            var start = text.IndexOf("beginbfrange", pos, StringComparison.Ordinal);
            if (start < 0)
            {
                break;
            }

            var sectionStart = start + "beginbfrange".Length;
            var end = text.IndexOf("endbfrange", sectionStart, StringComparison.Ordinal);
            if (end < 0)
            {
                break;
            }

            cmap.ParseBfRangeSection(text[sectionStart..end], srcHexLengths);
            pos = end;
        }

        if (cmap.CharMap.Count == 0 && cmap.Ranges.Count == 0)
        {
            return null;
        }

        if (codespaceByteLength is { } csLen)
        {
            // A 2-byte codespace whose entries are all 1-byte is really 1-byte:
            // the common case of <0000><FFFF> paired with <20>, <41>, ... entries.
            cmap.CodeByteLength = csLen == 2 && srcHexLengths.Count > 0 && srcHexLengths.All(l => l <= 2)
                ? (byte)1
                : csLen;
        }
        else if (srcHexLengths.Count > 0)
        {
            cmap.CodeByteLength = srcHexLengths.Max() <= 2 ? (byte)1 : (byte)2;
        }
        else
        {
            cmap.CodeByteLength = 2;
        }

        // Sorted for the binary search in Lookup.
        cmap.Ranges.Sort((a, b) => a.Start.CompareTo(b.Start));

        if (useCMapName is not null)
        {
            var mapBase = BuiltinCMaps.LoadByName(useCMapName);
            if (mapBase is not null)
            {
                cmap = MergeCMaps(mapBase, cmap);
            }
            else
            {
                Log.Warn("tounicode", $"usecmap={useCMapName} could not be loaded");
            }
        }

        return cmap;
    }

    /// <summary>Parses a bfchar section of <c>&lt;src&gt; &lt;dst&gt;</c> pairs.</summary>
    private void ParseBfCharSection(string section, List<int> srcHexLengths)
    {
        var cursor = new HexCursor(section);

        while (true)
        {
            cursor.SkipWhitespace();
            if (!cursor.TryConsume('<'))
            {
                break;
            }

            var srcHex = cursor.ReadUntil('>');
            cursor.TryConsume('>');

            var trimmedSrc = srcHex.Trim();
            if (trimmedSrc.Length > 0)
            {
                srcHexLengths.Add(trimmedSrc.Length);
            }

            cursor.SkipWhitespace();
            if (!cursor.TryConsume('<'))
            {
                continue;
            }

            var dstHex = cursor.ReadUntil('>');
            cursor.TryConsume('>');

            if (ParseHexU16(srcHex) is { } src && HexToUnicodeString(dstHex) is { } dst)
            {
                CharMap[src] = dst;
            }
        }
    }

    /// <summary>
    /// Parses a bfrange section of <c>&lt;start&gt; &lt;end&gt; &lt;base&gt;</c> or
    /// <c>&lt;start&gt; &lt;end&gt; [&lt;u1&gt; &lt;u2&gt; ...]</c> triples.
    /// </summary>
    private void ParseBfRangeSection(string section, List<int> srcHexLengths)
    {
        var cursor = new HexCursor(section);

        while (true)
        {
            cursor.SkipWhitespace();
            if (!cursor.TryConsume('<'))
            {
                break;
            }

            var startHex = cursor.ReadUntil('>');
            cursor.TryConsume('>');

            var trimmedStart = startHex.Trim();
            if (trimmedStart.Length > 0)
            {
                srcHexLengths.Add(trimmedStart.Length);
            }

            cursor.SkipWhitespace();
            if (!cursor.TryConsume('<'))
            {
                continue;
            }

            var endHex = cursor.ReadUntil('>');
            cursor.TryConsume('>');

            cursor.SkipWhitespace();

            if (cursor.Peek() == '<')
            {
                cursor.Advance();
                var baseHex = cursor.ReadUntil('>');
                cursor.TryConsume('>');

                if (ParseHexU16(startHex) is { } start &&
                    ParseHexU16(endHex) is { } end &&
                    HexToUnicodeScalar(baseHex) is { } baseValue)
                {
                    Ranges.Add((start, end, baseValue));
                }
            }
            else if (cursor.Peek() == '[')
            {
                cursor.Advance();

                if (ParseHexU16(startHex) is { } start && ParseHexU16(endHex) is { } end)
                {
                    var cid = start;
                    while (true)
                    {
                        cursor.SkipWhitespace();
                        if (cursor.TryConsume(']'))
                        {
                            break;
                        }

                        if (!cursor.TryConsume('<'))
                        {
                            break;
                        }

                        var hex = cursor.ReadUntil('>');
                        cursor.TryConsume('>');

                        if (HexToUnicodeString(hex) is { } unicode)
                        {
                            CharMap[cid] = unicode;
                        }

                        if (cid >= end)
                        {
                            cursor.SkipUntil(']');
                            cursor.TryConsume(']');
                            break;
                        }

                        cid = cid == ushort.MaxValue ? cid : (ushort)(cid + 1);
                    }
                }
                else
                {
                    cursor.SkipUntil(']');
                    cursor.TryConsume(']');
                }
            }
        }
    }

    // ── Lookup ───────────────────────────────────────────────────────────

    /// <summary>Resolves a code to its Unicode text, or null when unmapped.</summary>
    public string? Lookup(ushort cid)
    {
        if (CharMap.TryGetValue(cid, out var direct))
        {
            return direct;
        }

        // Binary search for the first range whose start is >= cid.
        var lo = 0;
        var hi = Ranges.Count;
        while (lo < hi)
        {
            var mid = (lo + hi) / 2;
            if (Ranges[mid].Start < cid)
            {
                lo = mid + 1;
            }
            else
            {
                hi = mid;
            }
        }

        var index = lo;

        if (index < Ranges.Count && Resolve(Ranges[index], cid) is { } atIndex)
        {
            return atIndex;
        }

        // The containing range may begin before cid.
        if (index > 0 && Resolve(Ranges[index - 1], cid) is { } before)
        {
            return before;
        }

        return null;
    }

    private static string? Resolve((ushort Start, ushort End, uint Base) range, ushort cid)
    {
        if (cid < range.Start || cid > range.End)
        {
            return null;
        }

        var unicode = range.Base + (uint)(cid - range.Start);
        return CodePointToString(unicode);
    }

    /// <summary>
    /// Per-byte lookup without the Latin-1 fallback, returning each raw byte
    /// alongside its mapping. Only meaningful for single-byte CMaps.
    /// </summary>
    public List<(byte Raw, string? Mapped)> LookupBytes(ReadOnlySpan<byte> bytes)
    {
        var result = new List<(byte, string?)>(bytes.Length);
        foreach (var b in bytes)
        {
            var mapped = Lookup(b);
            if (mapped is not null && mapped.Contains('�'))
            {
                mapped = null;
            }

            result.Add((b, mapped));
        }

        return result;
    }

    /// <summary>Decodes a byte string, respecting the CMap's code byte width.</summary>
    public string DecodeCids(ReadOnlySpan<byte> bytes)
    {
        var result = new StringBuilder();
        var unmappedCount = 0;

        if (CodeByteLength == 1)
        {
            foreach (var b in bytes)
            {
                var mapped = Lookup(b);
                if (mapped is not null && !mapped.Contains('�'))
                {
                    result.Append(mapped);
                    continue;
                }

                // In most legacy encodings the byte is itself the character code.
                if (b >= 0x20)
                {
                    result.Append((char)b);
                }

                unmappedCount++;
            }
        }
        else
        {
            for (var i = 0; i + 1 < bytes.Length; i += 2)
            {
                var cid = (ushort)((bytes[i] << 8) | bytes[i + 1]);
                var mapped = Lookup(cid);
                if (mapped is not null && !mapped.Contains('�'))
                {
                    result.Append(mapped);
                    continue;
                }

                if (CidPassthrough)
                {
                    // Valid where the producer used Unicode values as CIDs but
                    // stripped the cmap.
                    var ch = (char)cid;
                    if (!char.IsControl(ch) || ch == '\t' || ch == '\n')
                    {
                        result.Append(ch);
                    }
                    else
                    {
                        unmappedCount++;
                    }
                }
                else
                {
                    // CIDs are font-internal indices, not Unicode. Skipping
                    // unmapped ones avoids emitting CJK garbage.
                    unmappedCount++;
                }
            }
        }

        // Too many unmapped codes: report failure so the caller can try another
        // decoding path.
        var total = CodeByteLength == 1 ? bytes.Length : bytes.Length / 2;
        if (total > 0 && unmappedCount > total / 2)
        {
            return string.Empty;
        }

        return result.ToString();
    }

    /// <summary>Lowest source code across direct mappings and ranges.</summary>
    public ushort? MinSourceCid()
    {
        ushort? min = null;
        foreach (var key in CharMap.Keys)
        {
            min = min is null ? key : Math.Min(min.Value, key);
        }

        foreach (var (start, _, _) in Ranges)
        {
            min = min is null ? start : Math.Min(min.Value, start);
        }

        return min;
    }

    /// <summary>Highest source code across direct mappings and ranges.</summary>
    public ushort? MaxSourceCid()
    {
        ushort? max = null;
        foreach (var key in CharMap.Keys)
        {
            max = max is null ? key : Math.Max(max.Value, key);
        }

        foreach (var (_, end, _) in Ranges)
        {
            max = max is null ? end : Math.Max(max.Value, end);
        }

        return max;
    }

    /// <summary>
    /// Rewrites a CMap that references pre-subsetting glyph ids onto sequential
    /// post-subsetting ids: source codes are sorted and reassigned to 1, 2, 3, …
    /// </summary>
    public ToUnicodeCMap RemapToSequential()
    {
        var cidToUnicode = new Dictionary<ushort, string>();

        foreach (var (start, end, baseValue) in Ranges)
        {
            for (var cid = (uint)start; cid <= end; cid++)
            {
                var codePoint = baseValue + (cid - start);
                if (CodePointToString(codePoint) is { } text)
                {
                    cidToUnicode[(ushort)cid] = text;
                }
            }
        }

        // Direct mappings take precedence over range mappings.
        foreach (var (cid, unicode) in CharMap)
        {
            cidToUnicode[cid] = unicode;
        }

        var oldCids = cidToUnicode.Keys.ToList();
        oldCids.Sort();

        var newCMap = new ToUnicodeCMap { CodeByteLength = CodeByteLength };
        for (var i = 0; i < oldCids.Count; i++)
        {
            // Glyph 0 is .notdef, so content codes start at 1.
            newCMap.CharMap[(ushort)(i + 1)] = cidToUnicode[oldCids[i]];
        }

        return newCMap;
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    public static ToUnicodeCMap MergeCMaps(ToUnicodeCMap mapBase, ToUnicodeCMap overlay)
    {
        var merged = mapBase.Clone();

        foreach (var (cid, unicode) in overlay.CharMap)
        {
            merged.CharMap[cid] = unicode;
        }

        merged.Ranges.AddRange(overlay.Ranges);
        merged.Ranges.Sort((a, b) => a.Start.CompareTo(b.Start));
        merged.CodeByteLength = Math.Max(merged.CodeByteLength, overlay.CodeByteLength);

        return merged;
    }

    internal static ushort? ParseHexU16(string hex) =>
        ushort.TryParse(hex.Trim(), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var value)
            ? value
            : null;

    /// <summary>
    /// Converts a ToUnicode destination hex string. Destinations are UTF-16BE, so
    /// supplementary characters arrive as surrogate pairs and must not be split
    /// into separate scalars.
    /// </summary>
    internal static string? HexToUnicodeString(string hex)
    {
        var compact = new StringBuilder(hex.Length);
        foreach (var c in hex)
        {
            // Matches Rust's char::is_ascii_whitespace: space, tab, LF, FF, CR.
            if (c is not (' ' or '\t' or '\n' or '\f' or '\r'))
            {
                compact.Append(c);
            }
        }

        var clean = compact.ToString();
        if (clean.Length == 0 || clean.Length % 2 != 0)
        {
            return null;
        }

        var bytes = new byte[clean.Length / 2];
        for (var i = 0; i < bytes.Length; i++)
        {
            if (!byte.TryParse(clean.AsSpan(i * 2, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out bytes[i]))
            {
                return null;
            }
        }

        if (bytes.Length % 2 == 0)
        {
            var units = new char[bytes.Length / 2];
            for (var i = 0; i < units.Length; i++)
            {
                units[i] = (char)((bytes[i * 2] << 8) | bytes[(i * 2) + 1]);
            }

            var candidate = new string(units);
            if (candidate.Length > 0 && IsWellFormedUtf16(candidate))
            {
                return NormalizeDestination(candidate);
            }
        }

        // Be permissive about non-standard one-byte destinations.
        if (bytes.Length == 1)
        {
            var ch = (char)bytes[0];
            if (!char.IsControl(ch) || ch == '\t' || ch == '\n')
            {
                return ch.ToString();
            }
        }

        return null;
    }

    /// <summary>Rejects unpaired surrogates, matching Rust's <c>String::from_utf16</c>.</summary>
    private static bool IsWellFormedUtf16(string text)
    {
        for (var i = 0; i < text.Length; i++)
        {
            if (char.IsHighSurrogate(text[i]))
            {
                if (i + 1 >= text.Length || !char.IsLowSurrogate(text[i + 1]))
                {
                    return false;
                }

                i++;
            }
            else if (char.IsLowSurrogate(text[i]))
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>
    /// Collapses the malformed destinations some producers emit, where a single
    /// mapping lists several alternative whitespace or hyphen code points.
    /// </summary>
    private static string NormalizeDestination(string text)
    {
        var isMultiChar = text.Length > 1;
        if (!isMultiChar)
        {
            return text;
        }

        if (text.All(char.IsWhiteSpace) && text.Any(ch => ch is '\t' or '\n' or '\r'))
        {
            return text.Contains('\t') ? "\t" : " ";
        }

        if (text.Contains('­') &&
            text.All(ch => ch is '-' or '­' or '‐' or '‑' or '‒' or '–' or '−'))
        {
            return "-";
        }

        return text;
    }

    internal static uint? HexToUnicodeScalar(string hex)
    {
        var text = HexToUnicodeString(hex);
        if (text is null || text.Length == 0)
        {
            return null;
        }

        // Only a single scalar qualifies as a range base.
        if (char.IsHighSurrogate(text[0]))
        {
            return text.Length == 2 && char.IsLowSurrogate(text[1])
                ? (uint)char.ConvertToUtf32(text[0], text[1])
                : null;
        }

        return text.Length == 1 ? text[0] : null;
    }

    /// <summary>Renders a code point as text, rejecting surrogates and out-of-range values.</summary>
    internal static string? CodePointToString(uint codePoint)
    {
        if (codePoint > 0x10FFFF || codePoint is >= 0xD800 and <= 0xDFFF)
        {
            return null;
        }

        return char.ConvertFromUtf32((int)codePoint);
    }

    private static string? FindUseCMapName(string text)
    {
        foreach (var line in text.Split('\n'))
        {
            if (!line.Contains("usecmap", StringComparison.Ordinal))
            {
                continue;
            }

            var parts = line.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
            for (var i = 0; i < parts.Length; i++)
            {
                if (parts[i] == "usecmap" && i > 0)
                {
                    var name = parts[i - 1].Trim();
                    if (name.StartsWith('/'))
                    {
                        return name[1..];
                    }
                }
            }
        }

        return null;
    }
}

/// <summary>A small character cursor for the angle-bracket syntax CMap sections use.</summary>
internal struct HexCursor(string text)
{
    private readonly string _text = text;
    private int _position = 0;

    public readonly char? Peek() => _position < _text.Length ? _text[_position] : null;

    public void Advance() => _position++;

    public void SkipWhitespace()
    {
        while (_position < _text.Length && char.IsWhiteSpace(_text[_position]))
        {
            _position++;
        }
    }

    public bool TryConsume(char c)
    {
        if (_position < _text.Length && _text[_position] == c)
        {
            _position++;
            return true;
        }

        return false;
    }

    public string ReadUntil(char terminator)
    {
        var start = _position;
        while (_position < _text.Length && _text[_position] != terminator)
        {
            _position++;
        }

        return _text[start.._position];
    }

    /// <summary>Reads a run of non-whitespace characters — used for decimal CID operands.</summary>
    public string ReadWhileNotWhitespace()
    {
        var start = _position;
        while (_position < _text.Length && !char.IsWhiteSpace(_text[_position]))
        {
            _position++;
        }

        return _text[start.._position];
    }

    public void SkipUntil(char terminator)
    {
        while (_position < _text.Length && _text[_position] != terminator)
        {
            _position++;
        }
    }
}
