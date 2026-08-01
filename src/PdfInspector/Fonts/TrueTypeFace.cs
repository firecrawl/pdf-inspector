// Replaces the Rust build's `ttf-parser` dependency.
using System.Buffers.Binary;
using System.Text;

namespace PdfInspector.Fonts;

/// <summary>Platform identifiers used by the <c>cmap</c> table.</summary>
public enum TrueTypePlatform
{
    Unicode = 0,
    Macintosh = 1,
    Iso = 2,
    Windows = 3,
    Custom = 4,
    Unknown = -1,
}

/// <summary>
/// A minimal sfnt reader covering the tables this port needs: <c>cmap</c> for
/// code→glyph mappings, <c>maxp</c> for the glyph count, and <c>post</c> for
/// glyph names. Outline and metric tables are deliberately not parsed.
/// </summary>
public sealed class TrueTypeFace
{
    private readonly byte[] _data;
    private readonly Dictionary<string, (int Offset, int Length)> _tables = new(StringComparer.Ordinal);
    private string[]? _glyphNames;

    private TrueTypeFace(byte[] data) => _data = data;

    /// <summary>Number of glyphs, from the <c>maxp</c> table.</summary>
    public int NumberOfGlyphs { get; private set; }

    /// <summary>The <c>cmap</c> subtables, in the order the font declares them.</summary>
    public IReadOnlyList<CmapSubtable> CmapSubtables { get; private set; } = [];

    /// <summary>Parses a font, or returns null when the container is not a recognisable sfnt.</summary>
    public static TrueTypeFace? Parse(byte[] data, int faceIndex = 0)
    {
        if (data.Length < 12)
        {
            return null;
        }

        var face = new TrueTypeFace(data);
        var offset = 0;

        var tag = ReadUInt32(data, 0);

        // 'ttcf' — a collection; jump to the requested face's table directory.
        if (tag == 0x74746366)
        {
            if (data.Length < 16)
            {
                return null;
            }

            var numFonts = (int)ReadUInt32(data, 8);
            if (faceIndex >= numFonts)
            {
                return null;
            }

            var entry = 12 + (faceIndex * 4);
            if (entry + 4 > data.Length)
            {
                return null;
            }

            offset = (int)ReadUInt32(data, entry);
            if (offset + 12 > data.Length)
            {
                return null;
            }

            tag = ReadUInt32(data, offset);
        }

        // 0x00010000 (TrueType), 'true', 'OTTO' (CFF outlines). A bare CFF font
        // has no sfnt directory and is rejected here, matching ttf-parser.
        if (tag is not (0x00010000 or 0x74727565 or 0x4F54544F))
        {
            return null;
        }

        var numTables = ReadUInt16(data, offset + 4);
        var directory = offset + 12;

        for (var i = 0; i < numTables; i++)
        {
            var record = directory + (i * 16);
            if (record + 16 > data.Length)
            {
                break;
            }

            var name = Encoding.ASCII.GetString(data, record, 4);
            var tableOffset = (int)ReadUInt32(data, record + 8);
            var tableLength = (int)ReadUInt32(data, record + 12);

            if (tableOffset < 0 || tableLength < 0 || tableOffset > data.Length)
            {
                continue;
            }

            // Clamp lengths that overrun the file rather than discarding the table.
            tableLength = Math.Min(tableLength, data.Length - tableOffset);
            face._tables[name] = (tableOffset, tableLength);
        }

        face.ReadMaxp();
        face.ReadCmap();
        return face;
    }

    private void ReadMaxp()
    {
        if (_tables.TryGetValue("maxp", out var maxp) && maxp.Length >= 6)
        {
            NumberOfGlyphs = ReadUInt16(_data, maxp.Offset + 4);
        }
    }

    private void ReadCmap()
    {
        if (!_tables.TryGetValue("cmap", out var cmap) || cmap.Length < 4)
        {
            return;
        }

        var numTables = ReadUInt16(_data, cmap.Offset + 2);
        var subtables = new List<CmapSubtable>(numTables);

        for (var i = 0; i < numTables; i++)
        {
            var record = cmap.Offset + 4 + (i * 8);
            if (record + 8 > _data.Length)
            {
                break;
            }

            var platform = ReadUInt16(_data, record);
            var encoding = ReadUInt16(_data, record + 2);
            var subtableOffset = cmap.Offset + (int)ReadUInt32(_data, record + 4);

            if (subtableOffset < 0 || subtableOffset + 2 > _data.Length)
            {
                continue;
            }

            var subtable = CmapSubtable.Parse(_data, subtableOffset, platform, encoding);
            if (subtable is not null)
            {
                subtables.Add(subtable);
            }
        }

        CmapSubtables = subtables;
    }

    /// <summary>
    /// True when the font declares itself italic or oblique in OS/2, or carries
    /// a non-zero <c>post</c> italic angle.
    /// </summary>
    /// <remarks>
    /// The <c>head</c> table's macStyle is deliberately not consulted. The Rust
    /// build reads these flags through ttf-parser, which reports a face with no
    /// OS/2 table as neither italic nor bold; falling back to macStyle would
    /// style text the reference leaves plain.
    /// </remarks>
    public bool IsItalic
    {
        get
        {
            if (SelectionFlags is { } flags)
            {
                // Bit 0 is italic; bit 9 is oblique, and only meaningful from
                // OS/2 version 4 onwards.
                if ((flags & 0x0001) != 0 || (Os2Version >= 4 && (flags & 0x0200) != 0))
                {
                    return true;
                }
            }

            return ItalicAngle != 0f;
        }
    }

    /// <summary>True when the font declares itself bold in OS/2 fsSelection.</summary>
    public bool IsBold
    {
        get
        {
            // As with italic, a face with no OS/2 table reports plain.
            return SelectionFlags is { } flags && (flags & 0x0020) != 0;
        }
    }

    /// <summary>The OS/2 table's <c>fsSelection</c> field, or null when the table is absent.</summary>
    private ushort? SelectionFlags =>
        _tables.TryGetValue("OS/2", out var os2) && os2.Length >= 64
            ? ReadUInt16(_data, os2.Offset + 62)
            : null;

    /// <summary>The OS/2 table's version, or zero when the table is absent.</summary>
    private ushort Os2Version =>
        _tables.TryGetValue("OS/2", out var os2) && os2.Length >= 2
            ? ReadUInt16(_data, os2.Offset)
            : (ushort)0;

    /// <summary>The <c>post</c> table's italic angle in degrees, zero when absent.</summary>
    public float ItalicAngle
    {
        get
        {
            if (!_tables.TryGetValue("post", out var post) || post.Length < 12)
            {
                return 0f;
            }

            // A 16.16 signed fixed-point value.
            var raw = (int)ReadUInt32(_data, post.Offset + 4);
            return raw / 65536f;
        }
    }

    /// <summary>The glyph's name from the <c>post</c> table, or null when unavailable.</summary>
    public string? GlyphName(ushort glyphId)
    {
        _glyphNames ??= ReadPostGlyphNames();
        return glyphId < _glyphNames.Length && _glyphNames[glyphId].Length > 0 ? _glyphNames[glyphId] : null;
    }

    /// <summary>Reads version 2.0 <c>post</c> glyph names. Other versions carry no names.</summary>
    private string[] ReadPostGlyphNames()
    {
        if (!_tables.TryGetValue("post", out var post) || post.Length < 34)
        {
            return [];
        }

        var version = ReadUInt32(_data, post.Offset);
        if (version != 0x00020000 || post.Length < 34)
        {
            return [];
        }

        var numGlyphs = ReadUInt16(_data, post.Offset + 32);
        var indexEnd = post.Offset + 34 + (numGlyphs * 2);
        if (indexEnd > post.Offset + post.Length)
        {
            return [];
        }

        var indices = new ushort[numGlyphs];
        for (var i = 0; i < numGlyphs; i++)
        {
            indices[i] = ReadUInt16(_data, post.Offset + 34 + (i * 2));
        }

        // Custom names follow the index array as Pascal strings.
        var custom = new List<string>();
        var cursor = indexEnd;
        var limit = post.Offset + post.Length;
        while (cursor < limit)
        {
            var length = _data[cursor];
            cursor++;
            if (cursor + length > limit)
            {
                break;
            }

            custom.Add(Encoding.ASCII.GetString(_data, cursor, length));
            cursor += length;
        }

        var names = new string[numGlyphs];
        for (var i = 0; i < numGlyphs; i++)
        {
            var index = indices[i];
            if (index < 258)
            {
                names[i] = MacGlyphNames.Standard[index];
            }
            else
            {
                var customIndex = index - 258;
                names[i] = customIndex < custom.Count ? custom[customIndex] : string.Empty;
            }
        }

        return names;
    }

    internal static ushort ReadUInt16(byte[] data, int offset) =>
        offset + 2 <= data.Length ? BinaryPrimitives.ReadUInt16BigEndian(data.AsSpan(offset)) : (ushort)0;

    internal static uint ReadUInt32(byte[] data, int offset) =>
        offset + 4 <= data.Length ? BinaryPrimitives.ReadUInt32BigEndian(data.AsSpan(offset)) : 0u;
}

/// <summary>A single <c>cmap</c> subtable: one code-point space mapped to glyph ids.</summary>
public sealed class CmapSubtable
{
    private readonly byte[] _data;
    private readonly int _offset;
    private readonly int _format;

    private CmapSubtable(byte[] data, int offset, int format, ushort platformId, ushort encodingId)
    {
        _data = data;
        _offset = offset;
        _format = format;
        PlatformId = platformId switch
        {
            0 => TrueTypePlatform.Unicode,
            1 => TrueTypePlatform.Macintosh,
            2 => TrueTypePlatform.Iso,
            3 => TrueTypePlatform.Windows,
            4 => TrueTypePlatform.Custom,
            _ => TrueTypePlatform.Unknown,
        };
        EncodingId = encodingId;
    }

    public TrueTypePlatform PlatformId { get; }

    public ushort EncodingId { get; }

    /// <summary>True when the subtable's code points are Unicode.</summary>
    public bool IsUnicode =>
        PlatformId == TrueTypePlatform.Unicode
        || (PlatformId == TrueTypePlatform.Windows && EncodingId is 1 or 10)
        // (3,0) symbol subtables are treated as Unicode by ttf-parser only when
        // the caller opts in; they are excluded here to match its `is_unicode`.
        || (PlatformId == TrueTypePlatform.Iso && EncodingId is 1);

    internal static CmapSubtable? Parse(byte[] data, int offset, ushort platformId, ushort encodingId)
    {
        var format = TrueTypeFace.ReadUInt16(data, offset);
        return format is 0 or 4 or 6 or 12
            ? new CmapSubtable(data, offset, format, platformId, encodingId)
            : null;
    }

    /// <summary>Maps a code point to a glyph id, or null when the subtable has no entry.</summary>
    public ushort? GlyphIndex(uint codePoint) => _format switch
    {
        0 => Format0(codePoint),
        4 => Format4(codePoint),
        6 => Format6(codePoint),
        12 => Format12(codePoint),
        _ => null,
    };

    private ushort? Format0(uint codePoint)
    {
        if (codePoint > 0xFF)
        {
            return null;
        }

        var index = _offset + 6 + (int)codePoint;
        if (index >= _data.Length)
        {
            return null;
        }

        var gid = _data[index];
        return gid == 0 ? null : gid;
    }

    private ushort? Format4(uint codePoint)
    {
        if (codePoint > 0xFFFF)
        {
            return null;
        }

        var code = (ushort)codePoint;
        var segCountX2 = TrueTypeFace.ReadUInt16(_data, _offset + 6);
        var segCount = segCountX2 / 2;
        if (segCount == 0)
        {
            return null;
        }

        var endCodes = _offset + 14;
        var startCodes = endCodes + segCountX2 + 2;
        var idDeltas = startCodes + segCountX2;
        var idRangeOffsets = idDeltas + segCountX2;

        for (var i = 0; i < segCount; i++)
        {
            var end = TrueTypeFace.ReadUInt16(_data, endCodes + (i * 2));
            if (code > end)
            {
                continue;
            }

            var start = TrueTypeFace.ReadUInt16(_data, startCodes + (i * 2));
            if (code < start)
            {
                return null;
            }

            var idDelta = TrueTypeFace.ReadUInt16(_data, idDeltas + (i * 2));
            var idRangeOffset = TrueTypeFace.ReadUInt16(_data, idRangeOffsets + (i * 2));

            if (idRangeOffset == 0)
            {
                var gid = (ushort)((code + idDelta) & 0xFFFF);
                return gid == 0 ? null : gid;
            }

            // idRangeOffset is a byte offset from its own slot into glyphIdArray.
            var glyphAddress = idRangeOffsets + (i * 2) + idRangeOffset + ((code - start) * 2);
            if (glyphAddress + 2 > _data.Length)
            {
                return null;
            }

            var raw = TrueTypeFace.ReadUInt16(_data, glyphAddress);
            if (raw == 0)
            {
                return null;
            }

            return (ushort)((raw + idDelta) & 0xFFFF);
        }

        return null;
    }

    private ushort? Format6(uint codePoint)
    {
        var first = TrueTypeFace.ReadUInt16(_data, _offset + 6);
        var count = TrueTypeFace.ReadUInt16(_data, _offset + 8);

        if (codePoint < first || codePoint >= (uint)(first + count))
        {
            return null;
        }

        var index = _offset + 10 + ((int)(codePoint - first) * 2);
        if (index + 2 > _data.Length)
        {
            return null;
        }

        var gid = TrueTypeFace.ReadUInt16(_data, index);
        return gid == 0 ? null : gid;
    }

    private ushort? Format12(uint codePoint)
    {
        var numGroups = (int)TrueTypeFace.ReadUInt32(_data, _offset + 12);
        var groups = _offset + 16;

        var lo = 0;
        var hi = numGroups - 1;
        while (lo <= hi)
        {
            var mid = (lo + hi) / 2;
            var record = groups + (mid * 12);
            if (record + 12 > _data.Length)
            {
                return null;
            }

            var startChar = TrueTypeFace.ReadUInt32(_data, record);
            var endChar = TrueTypeFace.ReadUInt32(_data, record + 4);

            if (codePoint < startChar)
            {
                hi = mid - 1;
            }
            else if (codePoint > endChar)
            {
                lo = mid + 1;
            }
            else
            {
                var startGlyph = TrueTypeFace.ReadUInt32(_data, record + 8);
                var gid = startGlyph + (codePoint - startChar);
                return gid is 0 or > 0xFFFF ? null : (ushort)gid;
            }
        }

        return null;
    }

    /// <summary>Enumerates every code point the subtable maps.</summary>
    public IEnumerable<uint> CodePoints()
    {
        switch (_format)
        {
            case 0:
                for (uint c = 0; c <= 0xFF; c++)
                {
                    yield return c;
                }

                break;

            case 4:
            {
                var segCountX2 = TrueTypeFace.ReadUInt16(_data, _offset + 6);
                var segCount = segCountX2 / 2;
                var endCodes = _offset + 14;
                var startCodes = endCodes + segCountX2 + 2;

                for (var i = 0; i < segCount; i++)
                {
                    var start = TrueTypeFace.ReadUInt16(_data, startCodes + (i * 2));
                    var end = TrueTypeFace.ReadUInt16(_data, endCodes + (i * 2));

                    // 0xFFFF terminates the segment list and is not a real mapping.
                    if (start == 0xFFFF)
                    {
                        continue;
                    }

                    for (uint c = start; c <= end; c++)
                    {
                        yield return c;
                        if (c == 0xFFFF)
                        {
                            break;
                        }
                    }
                }

                break;
            }

            case 6:
            {
                var first = TrueTypeFace.ReadUInt16(_data, _offset + 6);
                var count = TrueTypeFace.ReadUInt16(_data, _offset + 8);
                for (uint c = first; c < (uint)(first + count); c++)
                {
                    yield return c;
                }

                break;
            }

            case 12:
            {
                var numGroups = (int)TrueTypeFace.ReadUInt32(_data, _offset + 12);
                var groups = _offset + 16;

                for (var i = 0; i < numGroups; i++)
                {
                    var record = groups + (i * 12);
                    if (record + 12 > _data.Length)
                    {
                        break;
                    }

                    var startChar = TrueTypeFace.ReadUInt32(_data, record);
                    var endChar = TrueTypeFace.ReadUInt32(_data, record + 4);

                    // Guard against a corrupt group claiming a huge span.
                    if (endChar < startChar || endChar - startChar > 0x10FFFF)
                    {
                        continue;
                    }

                    for (var c = startChar; c <= endChar; c++)
                    {
                        yield return c;
                    }
                }

                break;
            }
        }
    }
}
