using System.Globalization;
using System.Text;

namespace PdfInspector.Pdf;

/// <summary>
/// A byte-level cursor over PDF syntax. Shared by the file parser and the
/// content-stream parser, which use the same tokenisation rules.
/// </summary>
internal sealed class PdfLexer(byte[] data, int position = 0)
{
    public byte[] Data { get; } = data;

    public int Position { get; set; } = position;

    public int Length => Data.Length;

    public bool AtEnd => Position >= Data.Length;

    public static bool IsWhitespace(byte b) =>
        b is 0x00 or 0x09 or 0x0A or 0x0C or 0x0D or 0x20;

    public static bool IsDelimiter(byte b) =>
        b is (byte)'(' or (byte)')' or (byte)'<' or (byte)'>' or (byte)'[' or (byte)']'
            or (byte)'{' or (byte)'}' or (byte)'/' or (byte)'%';

    public static bool IsRegular(byte b) => !IsWhitespace(b) && !IsDelimiter(b);

    public byte Peek() => Position < Data.Length ? Data[Position] : (byte)0;

    public byte PeekAt(int offset) =>
        Position + offset < Data.Length ? Data[Position + offset] : (byte)0;

    /// <summary>Skips whitespace and <c>%</c> comments, which are equivalent to whitespace.</summary>
    public void SkipWhitespace()
    {
        while (Position < Data.Length)
        {
            var b = Data[Position];
            if (IsWhitespace(b))
            {
                Position++;
            }
            else if (b == (byte)'%')
            {
                while (Position < Data.Length && Data[Position] != (byte)'\n' && Data[Position] != (byte)'\r')
                {
                    Position++;
                }
            }
            else
            {
                return;
            }
        }
    }

    /// <summary>Reads a run of regular characters — a keyword, number, or operator.</summary>
    public string ReadToken()
    {
        var span = ReadTokenSpan();
        return span.IsEmpty ? string.Empty : Encoding.ASCII.GetString(span);
    }

    /// <summary>
    /// Reads a token without allocating, as a window over the source bytes.
    /// </summary>
    /// <remarks>
    /// The content-stream decoder runs this millions of times on a large
    /// document — once per number, operator and keyword. Materialising a string
    /// for each was the single largest allocation source in extraction, and
    /// almost all of those strings were numbers thrown away immediately after
    /// parsing. Callers that genuinely need a string call <see cref="ReadToken"/>.
    /// </remarks>
    public ReadOnlySpan<byte> ReadTokenSpan()
    {
        var start = Position;
        var data = Data;
        var i = Position;
        while (i < data.Length && IsRegular(data[i]))
        {
            i++;
        }

        Position = i;
        return data.AsSpan(start, i - start);
    }

    /// <summary>True when the bytes at the cursor match <paramref name="keyword"/>.</summary>
    public bool Matches(string keyword)
    {
        if (Position + keyword.Length > Data.Length)
        {
            return false;
        }

        for (var i = 0; i < keyword.Length; i++)
        {
            if (Data[Position + i] != (byte)keyword[i])
            {
                return false;
            }
        }

        return true;
    }

    public bool TryConsume(string keyword)
    {
        if (!Matches(keyword))
        {
            return false;
        }

        Position += keyword.Length;
        return true;
    }

    /// <summary>Reads a name object's body, expanding <c>#xx</c> escapes. The leading slash must already be consumed.</summary>
    public string ReadNameBody()
    {
        var sb = new StringBuilder();
        while (Position < Data.Length && IsRegular(Data[Position]))
        {
            var b = Data[Position];
            if (b == (byte)'#' && Position + 2 < Data.Length &&
                TryHex(Data[Position + 1], out var hi) && TryHex(Data[Position + 2], out var lo))
            {
                sb.Append((char)((hi << 4) | lo));
                Position += 3;
            }
            else
            {
                sb.Append((char)b);
                Position++;
            }
        }

        return sb.ToString();
    }

    /// <summary>Reads a literal string, handling nesting, escapes, and octal codes. The opening paren must already be consumed.</summary>
    public byte[] ReadLiteralStringBody()
    {
        var buffer = new List<byte>();
        var depth = 1;

        while (Position < Data.Length)
        {
            var b = Data[Position++];
            switch (b)
            {
                case (byte)'\\':
                    if (Position >= Data.Length)
                    {
                        break;
                    }

                    var esc = Data[Position++];
                    switch (esc)
                    {
                        case (byte)'n': buffer.Add((byte)'\n'); break;
                        case (byte)'r': buffer.Add((byte)'\r'); break;
                        case (byte)'t': buffer.Add((byte)'\t'); break;
                        case (byte)'b': buffer.Add((byte)'\b'); break;
                        case (byte)'f': buffer.Add((byte)'\f'); break;
                        case (byte)'(': buffer.Add((byte)'('); break;
                        case (byte)')': buffer.Add((byte)')'); break;
                        case (byte)'\\': buffer.Add((byte)'\\'); break;
                        case (byte)'\r':
                            // Line continuation; a following LF is part of the same break.
                            if (Position < Data.Length && Data[Position] == (byte)'\n')
                            {
                                Position++;
                            }

                            break;
                        case (byte)'\n':
                            break;
                        case >= (byte)'0' and <= (byte)'7':
                            var value = esc - (byte)'0';
                            for (var i = 0; i < 2 && Position < Data.Length; i++)
                            {
                                var d = Data[Position];
                                if (d is < (byte)'0' or > (byte)'7')
                                {
                                    break;
                                }

                                value = (value << 3) | (d - (byte)'0');
                                Position++;
                            }

                            buffer.Add((byte)(value & 0xFF));
                            break;
                        default:
                            // Undefined escapes drop the backslash and keep the character.
                            buffer.Add(esc);
                            break;
                    }

                    break;

                case (byte)'(':
                    depth++;
                    buffer.Add(b);
                    break;

                case (byte)')':
                    depth--;
                    if (depth == 0)
                    {
                        return [.. buffer];
                    }

                    buffer.Add(b);
                    break;

                default:
                    buffer.Add(b);
                    break;
            }
        }

        return [.. buffer];
    }

    /// <summary>Reads a hex string body. The opening angle bracket must already be consumed.</summary>
    public byte[] ReadHexStringBody()
    {
        var buffer = new List<byte>();
        int? pending = null;

        while (Position < Data.Length)
        {
            var b = Data[Position++];
            if (b == (byte)'>')
            {
                break;
            }

            if (!TryHex(b, out var digit))
            {
                continue;
            }

            if (pending is null)
            {
                pending = digit;
            }
            else
            {
                buffer.Add((byte)((pending.Value << 4) | digit));
                pending = null;
            }
        }

        // An odd number of digits is padded with a trailing zero.
        if (pending is not null)
        {
            buffer.Add((byte)(pending.Value << 4));
        }

        return [.. buffer];
    }

    public static bool TryHex(byte b, out int value)
    {
        switch (b)
        {
            case >= (byte)'0' and <= (byte)'9':
                value = b - (byte)'0';
                return true;
            case >= (byte)'a' and <= (byte)'f':
                value = b - (byte)'a' + 10;
                return true;
            case >= (byte)'A' and <= (byte)'F':
                value = b - (byte)'A' + 10;
                return true;
            default:
                value = 0;
                return false;
        }
    }

    /// <summary>
    /// Parses a PDF numeric token. Tolerates the malformed forms real files
    /// contain: leading <c>+</c>, bare <c>.</c>, repeated signs, and trailing junk.
    /// </summary>
    public static PdfObject? ParseNumber(string token)
    {
        Span<byte> bytes = token.Length <= 64 ? stackalloc byte[64] : new byte[token.Length];
        var count = Encoding.ASCII.GetBytes(token, bytes);
        return ParseNumber(bytes[..count]);
    }

    /// <summary>
    /// Parses a PDF numeric token from raw bytes, without allocating.
    /// </summary>
    /// <remarks>
    /// The cleaned digits are gathered into a stack buffer and handed to the
    /// framework's own parser rather than accumulated arithmetically. Rolling a
    /// mantissa by hand would be faster still, but it would not round
    /// identically to <c>double.Parse</c> in every case, and a single ulp of
    /// drift in a coordinate is enough to move a table's row band off the text
    /// baseline it should coincide with — see the note on float handling in
    /// CLAUDE.md. Same parser, same bits, no allocation.
    /// </remarks>
    public static PdfObject? ParseNumber(ReadOnlySpan<byte> token)
    {
        if (token.Length == 0)
        {
            return null;
        }

        // A PDF number longer than this is malformed; the surplus is junk that
        // the cleaning loop below would discard anyway.
        const int MaxDigits = 64;
        Span<char> clean = stackalloc char[MaxDigits];
        var length = 0;
        var isReal = false;

        foreach (var b in token)
        {
            var c = (char)b;
            if (c is '-' or '+')
            {
                // Only a leading sign is meaningful; interior signs terminate the number.
                if (length == 0)
                {
                    if (c == '-')
                    {
                        clean[length++] = '-';
                    }

                    continue;
                }

                break;
            }

            if (c == '.')
            {
                if (isReal)
                {
                    break;
                }

                isReal = true;
                if (length < MaxDigits)
                {
                    clean[length++] = '.';
                }

                continue;
            }

            if (c is >= '0' and <= '9')
            {
                if (length < MaxDigits)
                {
                    clean[length++] = c;
                }

                continue;
            }

            break;
        }

        var text = clean[..length];
        if (length == 0 || text.SequenceEqual("-") || text.SequenceEqual(".") || text.SequenceEqual("-."))
        {
            return isReal ? new PdfReal(0) : null;
        }

        if (isReal)
        {
            return double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var real)
                ? new PdfReal(real)
                : new PdfReal(0);
        }

        if (long.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var integer))
        {
            return PdfInteger.Create(integer);
        }

        // Out-of-range integers still carry usable magnitude for heuristics.
        return double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var fallback)
            ? new PdfReal(fallback)
            : null;
    }

    /// <summary>True when the token could begin a number.</summary>
    public static bool LooksNumeric(string token) =>
        token.Length > 0 && (token[0] is >= '0' and <= '9' or '-' or '+' or '.');

    /// <summary>True when the token could begin a number.</summary>
    public static bool LooksNumeric(ReadOnlySpan<byte> token) =>
        token.Length > 0 && (token[0] is >= (byte)'0' and <= (byte)'9'
            or (byte)'-' or (byte)'+' or (byte)'.');
}
