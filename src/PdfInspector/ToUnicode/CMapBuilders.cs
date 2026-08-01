// Ported from reference/src/tounicode.rs
using PdfInspector.Fonts;
using PdfInspector.Pdf;
using PdfInspector.Text;

namespace PdfInspector.ToUnicode;

/// <summary>A character-code→CID mapping taken from a font's <c>/Encoding</c> CMap.</summary>
internal sealed class EncodingCMap
{
    public Dictionary<ushort, ushort> Map { get; init; } = [];

    public byte CodeByteLength { get; set; }

    public bool IsIdentity { get; init; }
}

/// <summary>
/// Builds ToUnicode CMaps from the places a PDF can carry that information other
/// than a <c>/ToUnicode</c> stream: embedded font <c>cmap</c> tables, glyph
/// names, encoding CMaps, and predefined character collections.
/// </summary>
internal static class CMapBuilders
{
    private const string Module = "tounicode";

    // ── TrueType-derived CMaps ───────────────────────────────────────────

    /// <summary>
    /// Builds a CMap from an embedded TrueType or OpenType <c>cmap</c> table.
    /// For Identity-H fonts CID equals GID, so reversing the table's
    /// Unicode→GID direction yields CID→Unicode.
    /// </summary>
    public static ToUnicodeCMap? FromTrueType(byte[] fontData)
    {
        var face = TrueTypeFace.Parse(fontData);
        if (face is null)
        {
            return null;
        }

        var gidToUnicode = BuildGidToUnicode(face);
        if (gidToUnicode is null)
        {
            return null;
        }

        Log.Debug(Module, () => $"TrueType cmap: {gidToUnicode.Count} GID→Unicode entries");

        var cmap = new ToUnicodeCMap();
        foreach (var (gid, ch) in gidToUnicode)
        {
            cmap.CharMap[gid] = ch.ToString();
        }

        cmap.CodeByteLength = 2; // Identity-H uses 2-byte CIDs.
        return cmap;
    }

    /// <summary>
    /// Builds a single-byte CMap for a simple font by resolving character codes
    /// through the embedded font's own encoding subtable. A best-effort fallback
    /// used when no usable ToUnicode stream exists.
    /// </summary>
    public static ToUnicodeCMap? SimpleFromTrueType(byte[] fontData)
    {
        var face = TrueTypeFace.Parse(fontData);
        if (face is null)
        {
            return null;
        }

        var gidToUnicode = BuildGidToUnicode(face);
        if (gidToUnicode is null)
        {
            return null;
        }

        var cmap = new ToUnicodeCMap();

        // In a subsetted font the glyph id is not the character code, so the
        // font's own cmap subtable is needed to translate content-stream bytes.
        var usedEncodingCMap = false;

        // Mac Roman (1,0) maps byte codes 0–255 straight to glyph ids.
        usedEncodingCMap = TryFillFromSubtable(
            face, cmap, gidToUnicode, TrueTypePlatform.Macintosh, 0, codeOffset: 0);

        // Windows Symbol (3,0) maps F000+byte.
        if (!usedEncodingCMap)
        {
            usedEncodingCMap = TryFillFromSubtable(
                face, cmap, gidToUnicode, TrueTypePlatform.Windows, 0, codeOffset: 0xF000);
        }

        // Windows Unicode BMP (3,1): try each byte value as a code point. Common
        // in OCR output where the declared encoding is wrong.
        if (!usedEncodingCMap)
        {
            usedEncodingCMap = TryFillFromSubtable(
                face, cmap, gidToUnicode, TrueTypePlatform.Windows, 1, codeOffset: 0);
        }

        if (!usedEncodingCMap)
        {
            // No encoding subtable at all — treat the glyph id as the code.
            foreach (var (gid, ch) in gidToUnicode)
            {
                if (gid <= 0xFF)
                {
                    cmap.CharMap[gid] = ch.ToString();
                }
            }

            // Fill the remaining single-byte codes from glyph names, which also
            // recovers ligature glyphs such as "t_i".
            for (var gid = 0; gid < face.NumberOfGlyphs && gid <= 0xFF; gid++)
            {
                var gidValue = (ushort)gid;
                if (cmap.CharMap.ContainsKey(gidValue))
                {
                    continue;
                }

                if (face.GlyphName(gidValue) is { } name && GlyphNameToString(name) is { } text)
                {
                    cmap.CharMap[gidValue] = text;
                }
            }
        }

        if (cmap.CharMap.Count == 0)
        {
            return null;
        }

        Log.Debug(Module, () => $"TrueType simple cmap: {cmap.CharMap.Count} code→Unicode entries");
        cmap.CodeByteLength = 1;
        return cmap;
    }

    private static bool TryFillFromSubtable(
        TrueTypeFace face,
        ToUnicodeCMap cmap,
        Dictionary<ushort, char> gidToUnicode,
        TrueTypePlatform platform,
        ushort encoding,
        uint codeOffset)
    {
        foreach (var subtable in face.CmapSubtables)
        {
            if (subtable.PlatformId != platform || subtable.EncodingId != encoding)
            {
                continue;
            }

            for (uint code = 0x20; code <= 0xFF; code++)
            {
                if (subtable.GlyphIndex(code + codeOffset) is not { } gid)
                {
                    continue;
                }

                if (!gidToUnicode.TryGetValue(gid, out var ch))
                {
                    continue;
                }

                cmap.CharMap.TryAdd((ushort)code, StripPuaChar(ch).ToString());
            }

            return true;
        }

        return false;
    }

    /// <summary>Strips the Windows Symbol private-use offset from a character.</summary>
    private static char StripPuaChar(char ch) =>
        ch is >= '\uF000' and <= '\uF0FF' ? (char)(ch - 0xF000) : ch;

    /// <summary>
    /// Resolves a glyph name to text, handling variant suffixes and the
    /// underscore-joined names ligature glyphs use.
    /// </summary>
    private static string? GlyphNameToString(string name)
    {
        var dot = name.IndexOf('.', StringComparison.Ordinal);
        var baseName = dot >= 0 ? name[..dot] : name;

        if (GlyphNames.GlyphToChar(baseName) is { } direct)
        {
            return direct.ToString();
        }

        if (baseName.Contains('_', StringComparison.Ordinal))
        {
            var output = new System.Text.StringBuilder();
            foreach (var part in baseName.Split('_'))
            {
                if (part.Length == 0)
                {
                    return null;
                }

                if (GlyphNames.GlyphToChar(part) is { } ch)
                {
                    output.Append(ch);
                }
                else if (part.Length == 1)
                {
                    output.Append(part[0]);
                }
                else
                {
                    return null;
                }
            }

            if (output.Length > 0)
            {
                return output.ToString();
            }
        }

        return baseName is "ti" or "tt" or "tz" ? baseName : null;
    }

    /// <summary>Builds a CMap from the font's <c>post</c> glyph names via the Adobe Glyph List.</summary>
    private static ToUnicodeCMap? FromGlyphNames(TrueTypeFace face)
    {
        var cmap = new ToUnicodeCMap();

        for (var gid = 0; gid < face.NumberOfGlyphs; gid++)
        {
            var gidValue = (ushort)gid;
            if (face.GlyphName(gidValue) is { } name && GlyphNames.GlyphToChar(name) is { } ch)
            {
                cmap.CharMap[gidValue] = ch.ToString();
            }
        }

        if (cmap.CharMap.Count == 0)
        {
            return null;
        }

        Log.Debug(Module, () => $"TrueType post glyph names: {cmap.CharMap.Count} GID→Unicode entries");
        cmap.CodeByteLength = 2;
        return cmap;
    }

    /// <summary>
    /// Reverses the font's Unicode subtables into GID→Unicode, preferring the
    /// lowest code point when several map to one glyph.
    /// </summary>
    private static Dictionary<ushort, char>? BuildGidToUnicode(TrueTypeFace face)
    {
        var gidToUnicode = new Dictionary<ushort, char>();

        foreach (var subtable in face.CmapSubtables)
        {
            var isSymbol = subtable.PlatformId == TrueTypePlatform.Windows && subtable.EncodingId == 0;
            if (!subtable.IsUnicode && !isSymbol)
            {
                continue;
            }

            foreach (var cp in subtable.CodePoints())
            {
                if (cp > 0xFFFF || cp is >= 0xD800 and <= 0xDFFF)
                {
                    continue;
                }

                if (subtable.GlyphIndex(cp) is { } gid)
                {
                    gidToUnicode.TryAdd(gid, (char)cp);
                }
            }
        }

        if (gidToUnicode.Count > 0)
        {
            return gidToUnicode;
        }

        var byName = FromGlyphNames(face);
        if (byName is null)
        {
            return null;
        }

        var fromNames = new Dictionary<ushort, char>();
        foreach (var (gid, text) in byName.CharMap)
        {
            if (text.Length > 0)
            {
                fromNames[gid] = text[0];
            }
        }

        return fromNames;
    }

    // ── Font dictionary navigation ───────────────────────────────────────

    /// <summary>The first <c>/DescendantFonts</c> entry of a Type0 font.</summary>
    public static PdfDictionary? GetDescendantCidFont(PdfDocument doc, PdfDictionary fontDict)
    {
        var array = doc.GetDeref(fontDict, "DescendantFonts")?.AsArray();
        if (array is null || array.Count == 0)
        {
            return null;
        }

        return doc.Resolve(array[0]).AsDictionary();
    }

    /// <summary>The font descriptor of a font or CIDFont dictionary.</summary>
    public static PdfDictionary? GetFontDescriptor(PdfDocument doc, PdfDictionary dict) =>
        doc.GetDict(dict, "FontDescriptor");

    /// <summary>The reference to the embedded font program, preferring FontFile2 over FontFile3.</summary>
    public static PdfObjectId? GetFontFileReference(PdfDictionary? descriptor) =>
        descriptor?.Get("FontFile2")?.AsReference() ?? descriptor?.Get("FontFile3")?.AsReference();

    /// <summary>Decoded bytes of an embedded font program.</summary>
    public static byte[]? ReadFontFile(PdfDocument doc, PdfObjectId id)
    {
        if (doc.GetObject(id)?.AsStream() is not { } stream)
        {
            return null;
        }

        return stream.DecompressedContent() ?? stream.RawData;
    }

    /// <summary>The starting CID of a CIDFont's <c>/W</c> array.</summary>
    private static ushort? GetWArrayStartCid(PdfDocument doc, PdfDictionary cidFontDict)
    {
        var array = doc.GetDeref(cidFontDict, "W")?.AsArray();
        if (array is null || array.Count == 0)
        {
            return null;
        }

        return doc.Resolve(array[0]).AsInteger() is { } value ? (ushort)value : null;
    }

    /// <summary>
    /// True when the <c>/W</c> array explicitly covers <paramref name="target"/>.
    /// The array uses two forms: <c>c [w1 … wn]</c> and <c>c_first c_last w</c>.
    /// </summary>
    private static bool WArrayCoversCid(PdfDocument doc, PdfDictionary cidFontDict, ushort target)
    {
        var array = doc.GetDeref(cidFontDict, "W")?.AsArray();
        if (array is null)
        {
            return false;
        }

        long targetValue = target;
        var i = 0;

        while (i < array.Count)
        {
            if (doc.Resolve(array[i]).AsInteger() is not { } first)
            {
                break;
            }

            i++;
            if (i >= array.Count)
            {
                break;
            }

            if (doc.Resolve(array[i]).AsArray() is { } widths)
            {
                var last = first + widths.Count - 1;
                if (targetValue >= first && targetValue <= last)
                {
                    return true;
                }

                i++;
            }
            else if (doc.Resolve(array[i]).AsInteger() is { } last)
            {
                i++;
                if (i < array.Count)
                {
                    i++; // Skip the shared width.
                }

                if (targetValue >= first && targetValue <= last)
                {
                    return true;
                }
            }
            else
            {
                // Unrecognised token; stop rather than misread the rest.
                break;
            }
        }

        return false;
    }

    /// <summary>Reads <c>/CIDToGIDMap</c> as glyph ids indexed by CID. Identity maps return null.</summary>
    private static ushort[]? GetCidToGidMap(PdfDocument doc, PdfDictionary cidFontDict)
    {
        var obj = cidFontDict.Get("CIDToGIDMap");
        if (obj is null)
        {
            return null;
        }

        var resolved = doc.Resolve(obj);
        if (resolved.AsName() == "Identity")
        {
            return null;
        }

        if (resolved.AsStream()?.DecompressedContent() is not { } data || data.Length < 2)
        {
            return null;
        }

        var map = new ushort[data.Length / 2];
        for (var i = 0; i < map.Length; i++)
        {
            map[i] = (ushort)((data[i * 2] << 8) | data[(i * 2) + 1]);
        }

        return map;
    }

    /// <summary>Applies a CIDToGIDMap to a GID→Unicode CMap, producing CID→Unicode.</summary>
    private static ToUnicodeCMap? ApplyCidToGidMap(ToUnicodeCMap cmap, ushort[] cidToGid)
    {
        var result = new ToUnicodeCMap();

        for (var cid = 0; cid < cidToGid.Length; cid++)
        {
            if (cmap.Lookup(cidToGid[cid]) is { } text)
            {
                result.CharMap[(ushort)cid] = text;
            }
        }

        if (result.CharMap.Count == 0)
        {
            return null;
        }

        result.CodeByteLength = 2;
        return result;
    }

    // ── Subset remapping ─────────────────────────────────────────────────

    /// <summary>
    /// Detects a subset font whose glyph ids were renumbered sequentially without
    /// the ToUnicode CMap being updated, and produces a repaired alternative.
    /// </summary>
    public static (ToUnicodeCMap Primary, ToUnicodeCMap? Remapped) TryRemapSubsetCMap(
        ToUnicodeCMap cmap,
        PdfDictionary fontDict,
        PdfDocument doc,
        int objNum)
    {
        var encoding = doc.GetName(fontDict, "Encoding");
        if (encoding is not ("Identity-H" or "Identity-V"))
        {
            return (cmap, null);
        }

        // A minimum source code above 2 indicates pre-subsetting glyph ids.
        if (cmap.MinSourceCid() is not { } minCid || minCid <= 2)
        {
            return (cmap, null);
        }

        var cidFontDict = GetDescendantCidFont(doc, fontDict);
        if (cidFontDict is null)
        {
            return (cmap, null);
        }

        // An explicit CIDToGIDMap gives an exact repair.
        if (GetCidToGidMap(doc, cidFontDict) is { } cidToGid &&
            ApplyCidToGidMap(cmap, cidToGid) is { } repaired)
        {
            Log.Debug(Module, () =>
                $"CIDToGIDMap repair applied for obj={objNum}: {repaired.CharMap.Count} entries");
            return (cmap, repaired);
        }

        // Otherwise the /W array must start low, indicating sequential post-subset ids.
        if (GetWArrayStartCid(doc, cidFontDict) is not { } wStart || wStart > 2)
        {
            return (cmap, null);
        }

        // If /W actually covers the CMap's highest source code, the CMap is
        // aligned with the font and no renumbering happened.
        if (cmap.MaxSourceCid() is { } maxCid && WArrayCoversCid(doc, cidFontDict, maxCid))
        {
            Log.Debug(Module, () =>
                $"Subset remap skipped for obj={objNum}: W array covers CMap max CID {maxCid}");
            return (cmap, null);
        }

        Log.Debug(Module, () =>
            $"Subset GID mismatch detected for obj={objNum}: W starts at CID {wStart}, " +
            $"CMap min CID {minCid}. Remapping to sequential.");

        return (cmap, cmap.RemapToSequential());
    }

    // ── Encoding CMaps ───────────────────────────────────────────────────

    /// <summary>Builds a ToUnicode CMap by composing the font's encoding CMap with a UCS-2 collection map.</summary>
    public static ToUnicodeCMap? FallbackFromEncoding(PdfDocument doc, PdfDictionary fontDict)
    {
        var encoding = BuildEncodingCMap(doc, fontDict);
        if (encoding is null)
        {
            return null;
        }

        var ordering = GetCidSystemInfoOrdering(doc, fontDict);
        if (ordering is null)
        {
            return null;
        }

        var ucs2 = BuiltinCMaps.ForOrdering(ordering);
        if (ucs2 is null)
        {
            return null;
        }

        if (encoding.IsIdentity)
        {
            return ucs2;
        }

        var cmap = new ToUnicodeCMap();
        foreach (var (charCode, cid) in encoding.Map)
        {
            if (ucs2.Lookup(cid) is { } text)
            {
                cmap.CharMap[charCode] = text;
            }
        }

        if (cmap.CharMap.Count == 0)
        {
            return null;
        }

        cmap.CodeByteLength = encoding.CodeByteLength;
        return cmap;
    }

    private static string? GetCidSystemInfoOrdering(PdfDocument doc, PdfDictionary fontDict)
    {
        var cidFontDict = GetDescendantCidFont(doc, fontDict);
        return cidFontDict is null ? null : GetOrdering(doc, cidFontDict);
    }

    private static string? GetOrdering(PdfDocument doc, PdfDictionary cidFontDict)
    {
        var csi = doc.GetDict(cidFontDict, "CIDSystemInfo");
        if (csi is null)
        {
            return null;
        }

        return doc.GetDeref(csi, "Ordering") is PdfString ordering
            ? System.Text.Encoding.UTF8.GetString(ordering.Bytes)
            : null;
    }

    private static EncodingCMap? BuildEncodingCMap(PdfDocument doc, PdfDictionary fontDict)
    {
        var encodingObj = fontDict.Get("Encoding");
        if (encodingObj is null)
        {
            return null;
        }

        if (encodingObj is PdfName name)
        {
            if (name.Value is "Identity-H" or "Identity-V")
            {
                return new EncodingCMap { CodeByteLength = 2, IsIdentity = true };
            }

            var data = BuiltinCMaps.ReadFile($"{name.Value}.bcmap");
            return data is null ? null : ParseBinaryEncodingCMap(data);
        }

        var resolved = doc.Resolve(encodingObj);
        return resolved.AsStream()?.DecompressedContent() is { } content
            ? ParseEncodingCMapStream(content)
            : null;
    }

    /// <summary>Parses a textual CMap's <c>cidchar</c> and <c>cidrange</c> sections.</summary>
    private static EncodingCMap? ParseEncodingCMapStream(byte[] data)
    {
        var text = System.Text.Encoding.UTF8.GetString(data);
        var srcHexLengths = new List<int>();
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

        var map = new Dictionary<ushort, ushort>();

        ForEachSection(text, "begincidchar", "endcidchar",
            section => ParseCidCharSection(section, map, srcHexLengths));
        ForEachSection(text, "begincidrange", "endcidrange",
            section => ParseCidRangeSection(section, map, srcHexLengths));

        if (map.Count == 0)
        {
            return null;
        }

        byte codeByteLength;
        if (codespaceByteLength is { } csLen)
        {
            codeByteLength = csLen;
        }
        else if (srcHexLengths.Count > 0)
        {
            codeByteLength = srcHexLengths.Max() <= 2 ? (byte)1 : (byte)2;
        }
        else
        {
            codeByteLength = 2;
        }

        return new EncodingCMap { Map = map, CodeByteLength = codeByteLength, IsIdentity = false };
    }

    private static void ForEachSection(string text, string begin, string end, Action<string> handler)
    {
        var pos = 0;
        while (true)
        {
            var start = text.IndexOf(begin, pos, StringComparison.Ordinal);
            if (start < 0)
            {
                break;
            }

            var sectionStart = start + begin.Length;
            var sectionEnd = text.IndexOf(end, sectionStart, StringComparison.Ordinal);
            if (sectionEnd < 0)
            {
                break;
            }

            handler(text[sectionStart..sectionEnd]);
            pos = sectionEnd;
        }
    }

    private static void ParseCidCharSection(
        string section,
        Dictionary<ushort, ushort> map,
        List<int> srcHexLengths)
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

            var trimmed = srcHex.Trim();
            if (trimmed.Length > 0)
            {
                srcHexLengths.Add(trimmed.Length);
            }

            cursor.SkipWhitespace();
            var cidText = cursor.ReadWhileNotWhitespace();

            if (ToUnicodeCMap.ParseHexU16(srcHex) is { } code && ushort.TryParse(cidText, out var cid))
            {
                map[code] = cid;
            }
        }
    }

    private static void ParseCidRangeSection(
        string section,
        Dictionary<ushort, ushort> map,
        List<int> srcHexLengths)
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

            var trimmed = startHex.Trim();
            if (trimmed.Length > 0)
            {
                srcHexLengths.Add(trimmed.Length);
            }

            cursor.SkipWhitespace();
            if (!cursor.TryConsume('<'))
            {
                continue;
            }

            var endHex = cursor.ReadUntil('>');
            cursor.TryConsume('>');

            cursor.SkipWhitespace();
            var cidText = cursor.ReadWhileNotWhitespace();

            if (ToUnicodeCMap.ParseHexU16(startHex) is not { } start ||
                ToUnicodeCMap.ParseHexU16(endHex) is not { } end ||
                !ushort.TryParse(cidText, out var startCid))
            {
                continue;
            }

            var cid = startCid;
            for (var code = (uint)start; code <= end; code++)
            {
                map[(ushort)code] = cid;
                cid = cid == ushort.MaxValue ? cid : (ushort)(cid + 1);
            }
        }
    }

    /// <summary>Parses the binary CMap format's <c>cidchar</c> and <c>cidrange</c> records.</summary>
    private static EncodingCMap? ParseBinaryEncodingCMap(byte[] data)
    {
        var stream = new BinaryCMapStream(data);
        if (stream.ReadByte() is null)
        {
            return null;
        }

        var map = new Dictionary<ushort, ushort>();
        byte maxCodeSize = 1;
        string? useCMap = null;

        try
        {
            while (stream.ReadByte() is { } b)
            {
                var type = b >> 5;

                if (type == 7)
                {
                    switch (b & 0x1F)
                    {
                        case 0:
                            stream.ReadString();
                            break;
                        case 1:
                            useCMap = stream.ReadString();
                            break;
                    }

                    continue;
                }

                var sequence = (b & 0x10) != 0;
                var dataSize = b & 0x0F;
                if (dataSize + 1 > 16)
                {
                    return null;
                }

                maxCodeSize = Math.Max(maxCodeSize, (byte)(dataSize + 1));
                var subitems = (int)stream.ReadNumber();

                switch (type)
                {
                    case 2: // cidchar
                    {
                        uint prevCode = 0;
                        for (var i = 0; i < subitems; i++)
                        {
                            var codeBytes = stream.ReadHexNumber(dataSize);
                            var code = HexToUInt32(codeBytes);
                            var cid = (ushort)stream.ReadNumber();

                            if (i == 0)
                            {
                                prevCode = code;
                                map[(ushort)code] = cid;
                                continue;
                            }

                            if (sequence)
                            {
                                prevCode = prevCode == uint.MaxValue ? prevCode : prevCode + 1;
                                map[(ushort)prevCode] = cid;
                            }
                            else
                            {
                                map[(ushort)code] = cid;
                                prevCode = code;
                            }
                        }

                        break;
                    }

                    case 3: // cidrange
                        for (var i = 0; i < subitems; i++)
                        {
                            var start = stream.ReadHexNumber(dataSize);
                            var endDelta = stream.ReadHexNumber(dataSize);
                            var end = (byte[])start.Clone();
                            AddHex(end, endDelta);
                            var cidStart = (ushort)stream.ReadNumber();

                            var startCode = (ushort)HexToUInt32(start);
                            var endCode = (ushort)HexToUInt32(end);

                            var cid = cidStart;
                            for (var code = (uint)startCode; code <= endCode; code++)
                            {
                                map[(ushort)code] = cid;
                                cid = cid == ushort.MaxValue ? cid : (ushort)(cid + 1);
                            }
                        }

                        break;

                    default:
                        for (var i = 0; i < subitems; i++)
                        {
                            stream.ReadHexNumber(dataSize);
                            stream.ReadHexNumber(dataSize);
                            stream.ReadNumber();
                        }

                        break;
                }
            }
        }
        catch (EndOfStreamException)
        {
            // Keep what parsed cleanly.
        }

        if (useCMap is not null)
        {
            var baseData = BuiltinCMaps.ReadFile($"{useCMap}.bcmap");
            if (baseData is not null && ParseBinaryEncodingCMap(baseData) is { } baseMap)
            {
                foreach (var (code, cid) in map)
                {
                    baseMap.Map[code] = cid;
                }

                baseMap.CodeByteLength = Math.Max(baseMap.CodeByteLength, maxCodeSize);
                return baseMap;
            }
        }

        return new EncodingCMap { Map = map, CodeByteLength = maxCodeSize, IsIdentity = false };
    }

    private static uint HexToUInt32(ReadOnlySpan<byte> bytes)
    {
        uint n = 0;
        foreach (var b in bytes)
        {
            n = (n << 8) | b;
        }

        return n;
    }

    private static void AddHex(byte[] a, byte[] b)
    {
        ushort carry = 0;
        for (var i = a.Length - 1; i >= 0; i--)
        {
            carry += (ushort)(a[i] + (i < b.Length ? b[i] : 0));
            a[i] = (byte)(carry & 0xFF);
            carry >>= 8;
        }
    }

    // ── Predefined character collections ─────────────────────────────────

    /// <summary>Builds a CMap from a predefined CID→Unicode collection named by <c>/CIDSystemInfo</c>.</summary>
    public static ToUnicodeCMap? FromCidSystemInfo(PdfDocument doc, PdfDictionary cidFontDict)
    {
        var ordering = GetOrdering(doc, cidFontDict);
        if (ordering is null)
        {
            return null;
        }

        if (ordering == "Korea1")
        {
            var cmap = new ToUnicodeCMap();
            foreach (var (cid, ch) in AdobeKorea1.Entries())
            {
                cmap.CharMap[cid] = ch.ToString();
            }

            cmap.CodeByteLength = 2;
            Log.Debug(Module, () => $"Adobe-Korea1 predefined CMap: {cmap.CharMap.Count} entries");
            return cmap;
        }

        return ordering is "Japan1" or "GB1" or "CNS1" ? BuiltinCMaps.ForOrdering(ordering) : null;
    }

    /// <summary>
    /// True when a CIDFont's <c>/W</c> array holds values that look like Unicode
    /// code points rather than low-valued glyph ids, judged by the median.
    /// </summary>
    public static bool CidValuesLookLikeUnicode(PdfDictionary cidFontDict)
    {
        // Deliberately not resolved: the reference build only inspects a direct array.
        if (cidFontDict.Get("W")?.AsArray() is not { } wArray)
        {
            return false;
        }

        var cids = new List<ushort>();
        var i = 0;

        while (i < wArray.Count)
        {
            if (wArray[i].AsInteger() is not { } cid)
            {
                i++;
                continue;
            }

            cids.Add((ushort)cid);

            if (i + 1 >= wArray.Count)
            {
                i++;
                continue;
            }

            if (wArray[i + 1].AsArray() is { } widths)
            {
                // c [w1 … wn] — the CIDs are c, c+1, …, c+n-1.
                for (var j = 1; j < widths.Count; j++)
                {
                    cids.Add((ushort)(cid + j));
                }

                i += 2;
            }
            else if (i + 2 < wArray.Count)
            {
                // c_first c_last w — a contiguous range.
                if (wArray[i + 1].AsInteger() is { } cidEnd)
                {
                    for (var c = (ushort)cid; c <= (ushort)cidEnd; c++)
                    {
                        cids.Add(c);
                        if (c == ushort.MaxValue)
                        {
                            break;
                        }
                    }
                }

                i += 3;
            }
            else
            {
                i++;
            }
        }

        if (cids.Count == 0)
        {
            return false;
        }

        cids.Sort();
        var median = cids[cids.Count / 2];

        // Unicode text codes are typically at or above 'A'; subset glyph ids
        // start near zero.
        return median >= 0x41;
    }

    // ── Fallback entry points ────────────────────────────────────────────

    /// <summary>Fallback CMap for a Type0 Identity-encoded font, from its embedded program or collection.</summary>
    public static ToUnicodeCMap? FallbackForType0(PdfDocument doc, PdfDictionary fontDict)
    {
        if (doc.GetName(fontDict, "Subtype") != "Type0")
        {
            return null;
        }

        var encoding = doc.GetName(fontDict, "Encoding");
        if (encoding is not ("Identity-H" or "Identity-V"))
        {
            return null;
        }

        var cidFontDict = GetDescendantCidFont(doc, fontDict);
        if (cidFontDict is null)
        {
            return null;
        }

        var fontFile = GetFontFileReference(GetFontDescriptor(doc, cidFontDict));
        if (fontFile is not null && ReadFontFile(doc, fontFile.Value) is { } data &&
            FromTrueType(data) is { } cmap)
        {
            if (GetCidToGidMap(doc, cidFontDict) is { } cidToGid &&
                ApplyCidToGidMap(cmap, cidToGid) is { } repaired)
            {
                Log.Debug(Module, () =>
                    $"Fallback TrueType CMap repaired with CIDToGIDMap: {repaired.CharMap.Count} entries");
                return repaired;
            }

            Log.Debug(Module, () => $"Fallback TrueType CMap (Type0+ToUnicode) char_map={cmap.CharMap.Count}");
            return cmap;
        }

        var predefined = FromCidSystemInfo(doc, cidFontDict);
        if (predefined is not null)
        {
            Log.Debug(Module, () =>
                $"Fallback CIDSystemInfo CMap (Type0+ToUnicode) char_map={predefined.CharMap.Count}");
        }

        return predefined;
    }

    /// <summary>Fallback CMap for a simple (non-Type0) font, from its embedded program.</summary>
    public static ToUnicodeCMap? FallbackForSimple(PdfDocument doc, PdfDictionary fontDict)
    {
        var subtype = doc.GetName(fontDict, "Subtype");
        if (subtype is null || subtype == "Type0")
        {
            return null;
        }

        var fontFile = GetFontFileReference(GetFontDescriptor(doc, fontDict));
        if (fontFile is null)
        {
            return null;
        }

        if (doc.GetObject(fontFile.Value)?.AsStream()?.DecompressedContent() is not { } data)
        {
            return null;
        }

        var cmap = SimpleFromTrueType(data);
        if (cmap is not null)
        {
            Log.Debug(Module, () =>
                $"Fallback simple font cmap (ToUnicode present) char_map={cmap.CharMap.Count}");
        }

        return cmap;
    }
}
