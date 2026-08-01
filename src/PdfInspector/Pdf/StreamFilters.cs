using System.IO.Compression;

namespace PdfInspector.Pdf;

/// <summary>Applies a stream's <c>/Filter</c> chain.</summary>
internal static class StreamFilters
{
    /// <summary>
    /// Filters whose output is compressed image data rather than PDF syntax.
    /// The extractor never reads their bytes — only their dictionaries — so the
    /// chain stops when one is reached and the stream is reported as undecodable.
    /// </summary>
    private static readonly HashSet<string> ImageFilters = new(StringComparer.Ordinal)
    {
        "DCTDecode", "DCT", "JPXDecode", "CCITTFaxDecode", "CCF", "JBIG2Decode",
    };

    public static bool IsImageFilter(string name) => ImageFilters.Contains(name);

    /// <summary>Names of the filters applied to a stream, outermost first.</summary>
    public static List<string> FilterNames(PdfDictionary dict)
    {
        var names = new List<string>();
        var filter = dict.Get("Filter") ?? dict.Get("F");

        switch (filter)
        {
            case PdfName name:
                names.Add(name.Value);
                break;
            case PdfArray array:
                foreach (var entry in array)
                {
                    if (entry.AsName() is { } n)
                    {
                        names.Add(n);
                    }
                }

                break;
        }

        return names;
    }

    private static List<PdfDictionary?> DecodeParms(PdfDictionary dict, int count)
    {
        var parms = new List<PdfDictionary?>(count);
        var raw = dict.Get("DecodeParms") ?? dict.Get("DP");

        switch (raw)
        {
            case PdfDictionary single:
                parms.Add(single);
                break;
            case PdfArray array:
                foreach (var entry in array)
                {
                    parms.Add(entry.AsDictionary());
                }

                break;
        }

        while (parms.Count < count)
        {
            parms.Add(null);
        }

        return parms;
    }

    public static byte[]? Decode(PdfStream stream)
    {
        var names = FilterNames(stream.Dictionary);
        if (names.Count == 0)
        {
            return stream.RawData;
        }

        var parms = DecodeParms(stream.Dictionary, names.Count);
        var data = stream.RawData;

        for (var i = 0; i < names.Count; i++)
        {
            if (IsImageFilter(names[i]))
            {
                return null;
            }

            var decoded = ApplyFilter(names[i], data, parms[i]);
            if (decoded is null)
            {
                return null;
            }

            data = decoded;
        }

        return data;
    }

    private static byte[]? ApplyFilter(string name, byte[] data, PdfDictionary? parms) => name switch
    {
        "FlateDecode" or "Fl" => ApplyPredictor(Inflate(data), parms),
        "LZWDecode" or "LZW" => ApplyPredictor(LzwDecode(data, EarlyChange(parms)), parms),
        "ASCIIHexDecode" or "AHx" => AsciiHexDecode(data),
        "ASCII85Decode" or "A85" => Ascii85Decode(data),
        "RunLengthDecode" or "RL" => RunLengthDecode(data),
        // Crypt with /Identity is a no-op; document-level decryption already ran.
        "Crypt" => data,
        _ => null,
    };

    private static int EarlyChange(PdfDictionary? parms)
    {
        var value = parms?.Get("EarlyChange")?.AsInteger();
        return value == 0 ? 0 : 1;
    }

    // ── Flate ────────────────────────────────────────────────────────────

    /// <summary>
    /// Inflates zlib or raw-deflate data. Truncated streams are common in damaged
    /// files, so whatever was decoded before the error is kept.
    /// </summary>
    public static byte[]? Inflate(byte[] data)
    {
        if (data.Length == 0)
        {
            return [];
        }

        // Some writers pad the front with whitespace before the zlib header.
        var start = 0;
        while (start < data.Length && PdfLexer.IsWhitespace(data[start]))
        {
            start++;
        }

        if (start >= data.Length)
        {
            return [];
        }

        var attempt = TryInflate(data, start, zlib: true);
        if (attempt is { Length: > 0 })
        {
            return attempt;
        }

        // Not a valid zlib header — try raw deflate, then skipping one byte
        // (a known corruption where the header's second byte is wrong).
        var raw = TryInflate(data, start, zlib: false);
        if (raw is { Length: > 0 })
        {
            return raw;
        }

        if (start + 1 < data.Length)
        {
            var shifted = TryInflate(data, start + 1, zlib: false);
            if (shifted is { Length: > 0 })
            {
                return shifted;
            }
        }

        return attempt ?? raw ?? [];
    }

    private static byte[]? TryInflate(byte[] data, int offset, bool zlib)
    {
        var output = new MemoryStream();
        try
        {
            using var input = new MemoryStream(data, offset, data.Length - offset, writable: false);
            Stream decompressor = zlib
                ? new ZLibStream(input, CompressionMode.Decompress, leaveOpen: true)
                : new DeflateStream(input, CompressionMode.Decompress, leaveOpen: true);

            using (decompressor)
            {
                var buffer = new byte[64 * 1024];
                int read;
                while ((read = decompressor.Read(buffer, 0, buffer.Length)) > 0)
                {
                    output.Write(buffer, 0, read);
                }
            }
        }
        catch (InvalidDataException)
        {
            // Keep the prefix that decoded cleanly.
        }
        catch (NotSupportedException)
        {
            return null;
        }

        return output.ToArray();
    }

    // ── Predictors ───────────────────────────────────────────────────────

    /// <summary>Reverses PNG or TIFF predictors, which Flate and LZW streams may apply.</summary>
    private static byte[]? ApplyPredictor(byte[]? data, PdfDictionary? parms)
    {
        if (data is null || parms is null)
        {
            return data;
        }

        var predictor = (int)(parms.Get("Predictor")?.AsInteger() ?? 1);
        if (predictor <= 1)
        {
            return data;
        }

        var colors = (int)(parms.Get("Colors")?.AsInteger() ?? 1);
        var bpc = (int)(parms.Get("BitsPerComponent")?.AsInteger() ?? 8);
        var columns = (int)(parms.Get("Columns")?.AsInteger() ?? 1);

        if (colors <= 0 || bpc <= 0 || columns <= 0)
        {
            return data;
        }

        return predictor == 2
            ? TiffPredictor(data, colors, bpc, columns)
            : PngPredictor(data, colors, bpc, columns);
    }

    private static byte[] TiffPredictor(byte[] data, int colors, int bpc, int columns)
    {
        if (bpc != 8)
        {
            // Sub-byte TIFF prediction is vanishingly rare; pass the data through.
            return data;
        }

        var rowLength = columns * colors;
        if (rowLength <= 0)
        {
            return data;
        }

        for (var row = 0; row + rowLength <= data.Length; row += rowLength)
        {
            for (var i = colors; i < rowLength; i++)
            {
                data[row + i] = (byte)(data[row + i] + data[row + i - colors]);
            }
        }

        return data;
    }

    private static byte[] PngPredictor(byte[] data, int colors, int bpc, int columns)
    {
        var bpp = Math.Max(1, colors * bpc / 8);
        var rowLength = (columns * colors * bpc + 7) / 8;
        var output = new MemoryStream();
        var previous = new byte[rowLength];
        var current = new byte[rowLength];

        var offset = 0;
        while (offset + 1 <= data.Length)
        {
            var tag = data[offset++];
            var available = Math.Min(rowLength, data.Length - offset);
            if (available <= 0)
            {
                break;
            }

            Array.Clear(current);
            Array.Copy(data, offset, current, 0, available);
            offset += available;

            switch (tag)
            {
                case 0:
                    break;
                case 1:
                    for (var i = bpp; i < rowLength; i++)
                    {
                        current[i] = (byte)(current[i] + current[i - bpp]);
                    }

                    break;
                case 2:
                    for (var i = 0; i < rowLength; i++)
                    {
                        current[i] = (byte)(current[i] + previous[i]);
                    }

                    break;
                case 3:
                    for (var i = 0; i < rowLength; i++)
                    {
                        var left = i >= bpp ? current[i - bpp] : 0;
                        current[i] = (byte)(current[i] + ((left + previous[i]) >> 1));
                    }

                    break;
                case 4:
                    for (var i = 0; i < rowLength; i++)
                    {
                        var a = i >= bpp ? current[i - bpp] : 0;
                        var b = previous[i];
                        var c = i >= bpp ? previous[i - bpp] : 0;
                        current[i] = (byte)(current[i] + Paeth(a, b, c));
                    }

                    break;
                default:
                    // Unknown tag: treat the row as unfiltered.
                    break;
            }

            output.Write(current, 0, rowLength);
            (previous, current) = (current, previous);
        }

        return output.ToArray();
    }

    private static int Paeth(int a, int b, int c)
    {
        var p = a + b - c;
        var pa = Math.Abs(p - a);
        var pb = Math.Abs(p - b);
        var pc = Math.Abs(p - c);
        if (pa <= pb && pa <= pc)
        {
            return a;
        }

        return pb <= pc ? b : c;
    }

    // ── LZW ──────────────────────────────────────────────────────────────

    public static byte[] LzwDecode(byte[] data, int earlyChange)
    {
        const int ClearCode = 256;
        const int EodCode = 257;

        var output = new MemoryStream();
        var table = new List<byte[]>(4096);

        void ResetTable()
        {
            table.Clear();
            for (var i = 0; i < 256; i++)
            {
                table.Add([(byte)i]);
            }

            table.Add([]); // 256: clear
            table.Add([]); // 257: end of data
        }

        ResetTable();

        var codeWidth = 9;
        var bitBuffer = 0;
        var bitCount = 0;
        byte[]? previous = null;

        foreach (var octet in data)
        {
            bitBuffer = (bitBuffer << 8) | octet;
            bitCount += 8;

            while (bitCount >= codeWidth)
            {
                var code = (bitBuffer >> (bitCount - codeWidth)) & ((1 << codeWidth) - 1);
                bitCount -= codeWidth;

                if (code == EodCode)
                {
                    return output.ToArray();
                }

                if (code == ClearCode)
                {
                    ResetTable();
                    codeWidth = 9;
                    previous = null;
                    continue;
                }

                byte[] entry;
                if (code < table.Count)
                {
                    entry = table[code];
                }
                else if (previous is not null)
                {
                    entry = [.. previous, previous[0]];
                }
                else
                {
                    // Code outside the table with no prior entry — stream is corrupt.
                    return output.ToArray();
                }

                output.Write(entry, 0, entry.Length);

                if (previous is not null && table.Count < 4096)
                {
                    table.Add([.. previous, entry[0]]);
                }

                previous = entry;

                var limit = table.Count + earlyChange;
                codeWidth = limit switch
                {
                    >= 2048 => 12,
                    >= 1024 => 11,
                    >= 512 => 10,
                    _ => 9,
                };
            }
        }

        return output.ToArray();
    }

    // ── ASCII filters ────────────────────────────────────────────────────

    public static byte[] AsciiHexDecode(byte[] data)
    {
        var output = new List<byte>(data.Length / 2);
        int? pending = null;

        foreach (var b in data)
        {
            if (b == (byte)'>')
            {
                break;
            }

            if (!PdfLexer.TryHex(b, out var digit))
            {
                continue;
            }

            if (pending is null)
            {
                pending = digit;
            }
            else
            {
                output.Add((byte)((pending.Value << 4) | digit));
                pending = null;
            }
        }

        if (pending is not null)
        {
            output.Add((byte)(pending.Value << 4));
        }

        return [.. output];
    }

    public static byte[] Ascii85Decode(byte[] data)
    {
        var output = new MemoryStream();
        var group = new int[5];
        var count = 0;
        var index = 0;

        // An optional "<~" prefix introduces the data.
        if (data.Length >= 2 && data[0] == (byte)'<' && data[1] == (byte)'~')
        {
            index = 2;
        }

        for (; index < data.Length; index++)
        {
            var b = data[index];

            if (b == (byte)'~')
            {
                break;
            }

            if (PdfLexer.IsWhitespace(b))
            {
                continue;
            }

            if (b == (byte)'z' && count == 0)
            {
                output.Write([0, 0, 0, 0], 0, 4);
                continue;
            }

            if (b is < (byte)'!' or > (byte)'u')
            {
                continue;
            }

            group[count++] = b - (byte)'!';
            if (count != 5)
            {
                continue;
            }

            var value = 0u;
            for (var i = 0; i < 5; i++)
            {
                value = (value * 85) + (uint)group[i];
            }

            output.Write([(byte)(value >> 24), (byte)(value >> 16), (byte)(value >> 8), (byte)value], 0, 4);
            count = 0;
        }

        if (count > 0)
        {
            // A partial group encodes count-1 bytes; pad with the maximum digit.
            var partial = count;
            for (var i = count; i < 5; i++)
            {
                group[i] = 84;
            }

            var value = 0u;
            for (var i = 0; i < 5; i++)
            {
                value = (value * 85) + (uint)group[i];
            }

            Span<byte> bytes = [(byte)(value >> 24), (byte)(value >> 16), (byte)(value >> 8), (byte)value];
            output.Write(bytes[..(partial - 1)]);
        }

        return output.ToArray();
    }

    public static byte[] RunLengthDecode(byte[] data)
    {
        var output = new MemoryStream();
        var index = 0;

        while (index < data.Length)
        {
            var length = data[index++];
            if (length == 128)
            {
                break;
            }

            if (length < 128)
            {
                var count = length + 1;
                var available = Math.Min(count, data.Length - index);
                if (available <= 0)
                {
                    break;
                }

                output.Write(data, index, available);
                index += available;
            }
            else
            {
                if (index >= data.Length)
                {
                    break;
                }

                var repeated = data[index++];
                var count = 257 - length;
                for (var i = 0; i < count; i++)
                {
                    output.WriteByte(repeated);
                }
            }
        }

        return output.ToArray();
    }
}
