// Ported from reference/src/tounicode.rs
using System.Reflection;
using System.Text;

namespace PdfInspector.ToUnicode;

/// <summary>
/// Loads the bundled Adobe CMap binaries. They ship as embedded resources; the
/// <c>PDF_INSPECTOR_BCMAPS_DIR</c> environment variable overrides that with a
/// directory on disk, matching the Rust build.
/// </summary>
internal static class BuiltinCMaps
{
    private static readonly Assembly Assembly = typeof(BuiltinCMaps).Assembly;

    private static readonly Dictionary<string, ToUnicodeCMap?> Cache = [];

    private static readonly Dictionary<string, ToUnicodeCMap?> OrderingCache = [];

    private static readonly Lock CacheLock = new();

    /// <summary>Guards against a cycle in <c>usecmap</c> chains.</summary>
    [ThreadStatic]
    private static HashSet<string>? _loading;

    public static byte[]? ReadFile(string name)
    {
        var directory = Environment.GetEnvironmentVariable("PDF_INSPECTOR_BCMAPS_DIR");
        if (!string.IsNullOrEmpty(directory) && Directory.Exists(directory))
        {
            var path = Path.Combine(directory, name);
            if (File.Exists(path))
            {
                return File.ReadAllBytes(path);
            }
        }

        using var stream = Assembly.GetManifestResourceStream($"bcmaps/{name}");
        if (stream is null)
        {
            return null;
        }

        using var buffer = new MemoryStream();
        stream.CopyTo(buffer);
        return buffer.ToArray();
    }

    /// <summary>Builds a UCS-2 CMap for a character-collection ordering such as "Japan1".</summary>
    /// <remarks>
    /// Cached like <see cref="LoadByName"/>, and for the same reason: these files
    /// hold tens of thousands of mappings, and reading and parsing one per
    /// document was over a sixth of the run on a CJK fixture. Callers may adjust
    /// what they get back, so the cache hands out clones.
    /// </remarks>
    public static ToUnicodeCMap? ForOrdering(string ordering)
    {
        var name = $"Adobe-{ordering}-UCS2.bcmap";

        lock (CacheLock)
        {
            if (OrderingCache.TryGetValue(ordering, out var hit))
            {
                return hit?.Clone();
            }
        }

        var cmap = BuildForOrdering(name);

        lock (CacheLock)
        {
            OrderingCache[ordering] = cmap;
        }

        return cmap?.Clone();
    }

    private static ToUnicodeCMap? BuildForOrdering(string name)
    {
        var data = ReadFile(name);
        if (data is null)
        {
            return null;
        }

        var cmap = ParseBinary(data);
        if (cmap is null || (cmap.CharMap.Count == 0 && cmap.Ranges.Count == 0))
        {
            return null;
        }

        cmap.CodeByteLength = 2;
        Log.Debug("tounicode", () =>
            $"Built-in CMap {name}: char_map={cmap.CharMap.Count} ranges={cmap.Ranges.Count}");
        return cmap;
    }

    /// <summary>
    /// Loads a named CMap, as referenced by a <c>usecmap</c> directive. Only UCS-2
    /// maps carry Unicode destinations, so other names are rejected outright.
    /// </summary>
    public static ToUnicodeCMap? LoadByName(string name)
    {
        if (!name.EndsWith("UCS2", StringComparison.Ordinal))
        {
            return null;
        }

        lock (CacheLock)
        {
            if (Cache.TryGetValue(name, out var cached))
            {
                return cached?.Clone();
            }
        }

        _loading ??= [];
        if (!_loading.Add(name))
        {
            // A usecmap cycle; treat this level as absent.
            return null;
        }

        ToUnicodeCMap? result;
        try
        {
            var data = ReadFile($"{name}.bcmap");
            result = data is null ? null : ParseBinary(data);
        }
        finally
        {
            _loading.Remove(name);
        }

        lock (CacheLock)
        {
            Cache[name] = result;
        }

        return result?.Clone();
    }

    /// <summary>Parses the compact binary CMap format the bundled files use.</summary>
    public static ToUnicodeCMap? ParseBinary(byte[] data)
    {
        var stream = new BinaryCMapStream(data);
        if (stream.ReadByte() is null)
        {
            return null;
        }

        var cmap = new ToUnicodeCMap();
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

                var dataSize = b & 0x0F;
                if (dataSize + 1 > 16)
                {
                    return null;
                }

                var subitems = (int)stream.ReadNumber();

                switch (type)
                {
                    case 4: // bfchar
                        for (var i = 0; i < subitems; i++)
                        {
                            var src = stream.ReadHexNumber(1);
                            var dst = stream.ReadHexBytes(dataSize + 1);
                            var srcCode = (ushort)HexToUInt32(src);
                            if (BytesToUnicodeString(dst) is { } text)
                            {
                                cmap.CharMap[srcCode] = text;
                            }
                        }

                        break;

                    case 5: // bfrange
                        for (var i = 0; i < subitems; i++)
                        {
                            var start = stream.ReadHexNumber(1);
                            var endDelta = stream.ReadHexNumber(1);
                            var end = (byte[])start.Clone();
                            AddHex(end, endDelta);
                            var dst = stream.ReadHexBytes(dataSize + 1);

                            var startCode = (ushort)HexToUInt32(start);
                            var endCode = (ushort)HexToUInt32(end);

                            if (BytesToUnicodeString(dst) is not { } text)
                            {
                                continue;
                            }

                            if (text.Length == 1)
                            {
                                cmap.Ranges.Add((startCode, endCode, text[0]));
                            }
                            else
                            {
                                // Multi-character destinations expand into direct entries.
                                var cid = startCode;
                                foreach (var ch in text)
                                {
                                    cmap.CharMap[cid] = ch.ToString();
                                    if (cid == endCode)
                                    {
                                        break;
                                    }

                                    cid = cid == ushort.MaxValue ? cid : (ushort)(cid + 1);
                                }
                            }
                        }

                        break;

                    default:
                        // Only bfchar and bfrange matter for UCS-2 maps; consume
                        // the payload of anything else so the stream stays aligned.
                        for (var i = 0; i < subitems; i++)
                        {
                            if (type > 3)
                            {
                                continue;
                            }

                            stream.ReadHexNumber(dataSize);
                            stream.ReadHexNumber(dataSize);
                            if (type >= 1)
                            {
                                stream.ReadNumber();
                            }
                        }

                        break;
                }
            }
        }
        catch (EndOfStreamException)
        {
            // A record that runs off the end means the stream is no longer
            // aligned, so every later offset is guesswork. The reference discards
            // the whole map rather than keeping a partial one, and a partial map
            // is worse than none: it decodes some CIDs into plausible-looking but
            // wrong characters instead of leaving the markers that let the
            // garbled-text detection fire.
            return null;
        }

        cmap.Ranges.Sort((a, b) => a.Start.CompareTo(b.Start));

        if (useCMap is not null)
        {
            var mapBase = LoadByName(useCMap);
            if (mapBase is not null)
            {
                cmap = ToUnicodeCMap.MergeCMaps(mapBase, cmap);
            }
            else
            {
                Log.Warn("tounicode", $"bcmap usecmap={useCMap} could not be loaded");
            }
        }

        return cmap;
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

    private static string? BytesToUnicodeString(byte[] bytes)
    {
        if (bytes.Length == 0)
        {
            return null;
        }

        if (bytes.Length % 2 != 0)
        {
            // An odd length cannot be UTF-16BE; read it as Latin-1.
            var latin = new StringBuilder(bytes.Length);
            foreach (var b in bytes)
            {
                latin.Append((char)b);
            }

            return latin.ToString();
        }

        var output = new StringBuilder(bytes.Length / 2);
        for (var i = 0; i + 1 < bytes.Length; i += 2)
        {
            var cp = (uint)((bytes[i] << 8) | bytes[i + 1]);
            if (ToUnicodeCMap.CodePointToString(cp) is { } text)
            {
                output.Append(text);
            }
        }

        return output.Length == 0 ? null : output.ToString();
    }
}

/// <summary>Cursor over the variable-length integer encoding the binary CMap format uses.</summary>
internal sealed class BinaryCMapStream(byte[] data)
{
    private int _position;

    public byte? ReadByte() => _position < data.Length ? data[_position++] : null;

    private byte ReadByteOrThrow() => ReadByte() ?? throw new EndOfStreamException("unexpected EOF in bcmap");

    /// <summary>Reads a base-128 big-endian integer, terminated by a byte with the high bit clear.</summary>
    public uint ReadNumber()
    {
        uint n = 0;
        while (true)
        {
            var b = ReadByteOrThrow();
            n = (n << 7) | (uint)(b & 0x7F);
            if ((b & 0x80) == 0)
            {
                return n;
            }
        }
    }

    /// <summary>Reads a base-128 integer and re-packs it into <paramref name="size"/>+1 bytes.</summary>
    public byte[] ReadHexNumber(int size)
    {
        var stack = new List<byte>();
        while (true)
        {
            var b = ReadByteOrThrow();
            stack.Add((byte)(b & 0x7F));
            if ((b & 0x80) == 0)
            {
                break;
            }
        }

        var output = new byte[size + 1];
        uint buffer = 0;
        uint bufferSize = 0;

        for (var i = size; i >= 0; i--)
        {
            while (bufferSize < 8 && stack.Count > 0)
            {
                var top = stack[^1];
                stack.RemoveAt(stack.Count - 1);
                buffer |= (uint)top << (int)bufferSize;
                bufferSize += 7;
            }

            output[i] = (byte)(buffer & 0xFF);
            buffer >>= 8;
            bufferSize = bufferSize >= 8 ? bufferSize - 8 : 0;
        }

        return output;
    }

    public byte[] ReadHexBytes(int length)
    {
        if (_position + length > data.Length)
        {
            throw new EndOfStreamException("unexpected EOF in bcmap");
        }

        var output = data.AsSpan(_position, length).ToArray();
        _position += length;
        return output;
    }

    public string ReadString()
    {
        var length = (int)ReadNumber();
        var buffer = new byte[length];
        for (var i = 0; i < length; i++)
        {
            buffer[i] = (byte)ReadNumber();
        }

        return Encoding.UTF8.GetString(buffer);
    }
}
