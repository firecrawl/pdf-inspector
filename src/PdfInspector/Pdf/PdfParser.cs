namespace PdfInspector.Pdf;

/// <summary>
/// Parses PDF objects out of a byte buffer. Deliberately permissive: real-world
/// files routinely violate the specification, and the reference implementation
/// this port follows recovers from those files rather than rejecting them.
/// </summary>
internal sealed class PdfParser(byte[] data)
{
    /// <summary>Bounds runaway recursion on self-referential or corrupt structures.</summary>
    private const int MaxDepth = 256;

    private readonly PdfLexer _lexer = new(data);

    public byte[] Data { get; } = data;

    public int Position
    {
        get => _lexer.Position;
        set => _lexer.Position = value;
    }

    /// <summary>
    /// Resolves stream lengths given as indirect references. Set by the document
    /// once the cross-reference table is available.
    /// </summary>
    public Func<PdfObjectId, PdfObject?>? ResolveLength { get; set; }

    public PdfObject? ParseObject() => ParseObject(0);

    private PdfObject? ParseObject(int depth)
    {
        if (depth > MaxDepth)
        {
            return null;
        }

        _lexer.SkipWhitespace();
        if (_lexer.AtEnd)
        {
            return null;
        }

        var b = _lexer.Peek();
        switch (b)
        {
            case (byte)'/':
                _lexer.Position++;
                return new PdfName(_lexer.ReadNameBody());

            case (byte)'(':
                _lexer.Position++;
                return new PdfString(_lexer.ReadLiteralStringBody());

            case (byte)'<':
                if (_lexer.PeekAt(1) == (byte)'<')
                {
                    _lexer.Position += 2;
                    return ParseDictionaryOrStream(depth);
                }

                _lexer.Position++;
                return new PdfString(_lexer.ReadHexStringBody(), PdfStringFormat.Hexadecimal);

            case (byte)'[':
                _lexer.Position++;
                return ParseArray(depth);

            case (byte)']':
            case (byte)'>':
            case (byte)')':
            case (byte)'}':
                // Stray closing delimiter — caller handles termination.
                return null;

            case (byte)'{':
                // PostScript procedure (function dictionaries); treated as an array.
                _lexer.Position++;
                return ParseProcedure(depth);
        }

        var start = _lexer.Position;
        var token = _lexer.ReadToken();
        if (token.Length == 0)
        {
            // Unrecognised delimiter; step over it so callers make progress.
            _lexer.Position = start + 1;
            return null;
        }

        switch (token)
        {
            case "true":
                return PdfBoolean.True;
            case "false":
                return PdfBoolean.False;
            case "null":
                return PdfObject.Null;
        }

        if (PdfLexer.LooksNumeric(token))
        {
            var number = PdfLexer.ParseNumber(token);
            if (number is PdfInteger integer && integer.Value >= 0)
            {
                // Could be "<num> <gen> R"; look ahead without committing.
                var save = _lexer.Position;
                _lexer.SkipWhitespace();
                var genToken = _lexer.ReadToken();
                if (genToken.Length > 0 && PdfLexer.LooksNumeric(genToken) &&
                    PdfLexer.ParseNumber(genToken) is PdfInteger gen && gen.Value is >= 0 and <= ushort.MaxValue)
                {
                    _lexer.SkipWhitespace();
                    if (_lexer.ReadToken() == "R")
                    {
                        return new PdfReference(new PdfObjectId((int)integer.Value, (ushort)gen.Value));
                    }
                }

                _lexer.Position = save;
                return number;
            }

            return number;
        }

        // Unknown keyword: skip it and let the caller continue.
        return null;
    }

    private PdfArray ParseArray(int depth)
    {
        var array = new PdfArray();
        var guard = 0;

        while (true)
        {
            _lexer.SkipWhitespace();
            if (_lexer.AtEnd)
            {
                break;
            }

            if (_lexer.Peek() == (byte)']')
            {
                _lexer.Position++;
                break;
            }

            // A dictionary or stream terminator inside an array means the array
            // was never closed; stop rather than consuming the rest of the file.
            if (_lexer.Peek() == (byte)'>' && _lexer.PeekAt(1) == (byte)'>')
            {
                break;
            }

            var before = _lexer.Position;
            var item = ParseObject(depth + 1);
            if (item is not null)
            {
                array.Add(item);
            }

            if (_lexer.Position == before)
            {
                _lexer.Position++;
            }

            if (++guard > 500_000)
            {
                break;
            }
        }

        return array;
    }

    private PdfArray ParseProcedure(int depth)
    {
        var array = new PdfArray();
        var guard = 0;

        while (true)
        {
            _lexer.SkipWhitespace();
            if (_lexer.AtEnd)
            {
                break;
            }

            if (_lexer.Peek() == (byte)'}')
            {
                _lexer.Position++;
                break;
            }

            var before = _lexer.Position;
            var item = ParseObject(depth + 1);
            if (item is not null)
            {
                array.Add(item);
            }

            if (_lexer.Position == before)
            {
                _lexer.Position++;
            }

            if (++guard > 200_000)
            {
                break;
            }
        }

        return array;
    }

    private PdfObject ParseDictionaryOrStream(int depth)
    {
        var dict = new PdfDictionary();
        var guard = 0;

        while (true)
        {
            _lexer.SkipWhitespace();
            if (_lexer.AtEnd)
            {
                break;
            }

            if (_lexer.Peek() == (byte)'>' && _lexer.PeekAt(1) == (byte)'>')
            {
                _lexer.Position += 2;
                break;
            }

            if (_lexer.Peek() != (byte)'/')
            {
                // Not a key — skip a token to make progress, or bail at a delimiter
                // that clearly belongs to an enclosing object.
                if (_lexer.Peek() is (byte)']' or (byte)')')
                {
                    _lexer.Position++;
                    continue;
                }

                var before = _lexer.Position;
                ParseObject(depth + 1);
                if (_lexer.Position == before)
                {
                    _lexer.Position++;
                }

                if (++guard > 500_000)
                {
                    break;
                }

                continue;
            }

            _lexer.Position++;
            var key = _lexer.ReadNameBody();
            var value = ParseObject(depth + 1);
            if (value is not null && key.Length > 0)
            {
                dict[key] = value;
            }

            if (++guard > 500_000)
            {
                break;
            }
        }

        // A `stream` keyword after the dictionary makes this a stream object.
        var save = _lexer.Position;
        _lexer.SkipWhitespace();
        if (!_lexer.TryConsume("stream"))
        {
            _lexer.Position = save;
            return dict;
        }

        // The keyword is followed by CRLF or LF (some writers emit only CR).
        if (_lexer.Peek() == (byte)'\r')
        {
            _lexer.Position++;
        }

        if (_lexer.Peek() == (byte)'\n')
        {
            _lexer.Position++;
        }

        var dataStart = _lexer.Position;
        var length = ResolveStreamLength(dict);

        int dataEnd;
        if (length is >= 0 && dataStart + length.Value <= Data.Length && EndstreamFollows(dataStart + length.Value))
        {
            dataEnd = dataStart + length.Value;
        }
        else
        {
            // Length is missing, indirect-and-unresolvable, or simply wrong.
            // Scanning for the terminator is what recovers these files.
            dataEnd = FindEndstream(dataStart);
        }

        var raw = new byte[Math.Max(0, dataEnd - dataStart)];
        Array.Copy(Data, dataStart, raw, 0, raw.Length);

        _lexer.Position = dataEnd;
        _lexer.SkipWhitespace();
        _lexer.TryConsume("endstream");

        return new PdfStream(dict, raw);
    }

    private int? ResolveStreamLength(PdfDictionary dict)
    {
        var lengthObj = dict.Get("Length");
        if (lengthObj is PdfReference reference)
        {
            lengthObj = ResolveLength?.Invoke(reference.Id);
        }

        var value = lengthObj?.AsInteger();
        if (value is null or < 0 or > int.MaxValue)
        {
            return null;
        }

        return (int)value.Value;
    }

    /// <summary>Confirms that <c>endstream</c> appears at <paramref name="offset"/>, allowing intervening whitespace.</summary>
    private bool EndstreamFollows(int offset)
    {
        var probe = offset;
        var limit = Math.Min(Data.Length, offset + 4);
        while (probe < limit && PdfLexer.IsWhitespace(Data[probe]))
        {
            probe++;
        }

        return MatchesAt(probe, "endstream");
    }

    private bool MatchesAt(int offset, string keyword)
    {
        if (offset < 0 || offset + keyword.Length > Data.Length)
        {
            return false;
        }

        for (var i = 0; i < keyword.Length; i++)
        {
            if (Data[offset + i] != (byte)keyword[i])
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>
    /// Scans forward for the <c>endstream</c> keyword, trimming the single EOL
    /// that writers insert before it (that byte is not part of the stream data).
    /// </summary>
    private int FindEndstream(int start)
    {
        var index = IndexOf(Data, "endstream"u8, start);
        if (index < 0)
        {
            return Data.Length;
        }

        var end = index;
        if (end > start && Data[end - 1] == (byte)'\n')
        {
            end--;
        }

        if (end > start && Data[end - 1] == (byte)'\r')
        {
            end--;
        }

        return end;
    }

    internal static int IndexOf(byte[] haystack, ReadOnlySpan<byte> needle, int start)
    {
        if (start < 0)
        {
            start = 0;
        }

        if (needle.Length == 0 || start >= haystack.Length)
        {
            return -1;
        }

        var index = haystack.AsSpan(start).IndexOf(needle);
        return index < 0 ? -1 : index + start;
    }

    internal static int LastIndexOf(byte[] haystack, ReadOnlySpan<byte> needle, int end)
    {
        if (needle.Length == 0)
        {
            return -1;
        }

        var limit = Math.Min(end, haystack.Length);
        if (limit < needle.Length)
        {
            return -1;
        }

        var index = haystack.AsSpan(0, limit).LastIndexOf(needle);
        return index;
    }

    /// <summary>
    /// Parses an indirect object at <paramref name="offset"/>, expecting the
    /// <c>N G obj</c> header. Returns the parsed body and the id actually found.
    /// </summary>
    public (PdfObjectId Id, PdfObject Value)? ParseIndirectObjectAt(int offset)
    {
        if (offset < 0 || offset >= Data.Length)
        {
            return null;
        }

        _lexer.Position = offset;
        _lexer.SkipWhitespace();

        var numberToken = _lexer.ReadToken();
        if (numberToken.Length == 0 || PdfLexer.ParseNumber(numberToken) is not PdfInteger number)
        {
            return null;
        }

        _lexer.SkipWhitespace();
        var genToken = _lexer.ReadToken();
        if (genToken.Length == 0 || PdfLexer.ParseNumber(genToken) is not PdfInteger generation)
        {
            return null;
        }

        _lexer.SkipWhitespace();
        if (!_lexer.TryConsume("obj"))
        {
            return null;
        }

        var value = ParseObject(0) ?? PdfObject.Null;
        var gen = generation.Value is >= 0 and <= ushort.MaxValue ? (ushort)generation.Value : (ushort)0;
        return (new PdfObjectId((int)number.Value, gen), value);
    }
}
