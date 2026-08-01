namespace PdfInspector.Pdf;

/// <summary>A single content-stream operator with the operands that preceded it.</summary>
public sealed class PdfOperation(string @operator, List<PdfObject> operands)
{
    public string Operator { get; } = @operator;

    public List<PdfObject> Operands { get; } = operands;

    public override string ToString() => string.Join(" ", Operands) + " " + Operator;
}

/// <summary>Decodes a content stream into its operator sequence.</summary>
public static class ContentStream
{
    /// <summary>
    /// Parses content-stream bytes. Malformed operands are skipped rather than
    /// aborting the stream, because a single bad token should not cost a page
    /// its remaining text.
    /// </summary>
    public static List<PdfOperation> Decode(byte[] data)
    {
        // A content-stream operation averages a couple of dozen bytes, so this
        // lands within a doubling of the final count and skips the growth
        // sequence a text-heavy page would otherwise walk.
        var operations = new List<PdfOperation>(Math.Min((data.Length / 24) + 8, 1 << 21));
        var parser = new PdfParser(data);
        var lexer = new PdfLexer(data);

        // Operands accumulate here and are copied out at exact size when the
        // operator arrives. A fresh list per operation grew 0 -> 4 -> 8 for
        // every one of them, which was the single largest allocation site in
        // the extractor.
        var operands = new List<PdfObject>();

        while (true)
        {
            lexer.SkipWhitespace();
            if (lexer.AtEnd)
            {
                break;
            }

            var b = lexer.Peek();

            // Operand: delegate to the object parser.
            if (b is (byte)'/' or (byte)'(' or (byte)'<' or (byte)'[' or (byte)'{')
            {
                parser.Position = lexer.Position;
                var value = parser.ParseObject();
                if (parser.Position <= lexer.Position)
                {
                    lexer.Position++;
                    continue;
                }

                lexer.Position = parser.Position;
                if (value is not null)
                {
                    operands.Add(value);
                }

                continue;
            }

            if (b is (byte)']' or (byte)')' or (byte)'>' or (byte)'}')
            {
                // Stray delimiter left over from a malformed operand.
                lexer.Position++;
                continue;
            }

            var token = lexer.ReadTokenSpan();
            if (token.Length == 0)
            {
                lexer.Position++;
                continue;
            }

            if (PdfLexer.LooksNumeric(token))
            {
                var number = PdfLexer.ParseNumber(token);
                if (number is not null)
                {
                    operands.Add(number);
                }

                continue;
            }

            if (token.SequenceEqual("true"u8))
            {
                operands.Add(PdfBoolean.True);
                continue;
            }

            if (token.SequenceEqual("false"u8))
            {
                operands.Add(PdfBoolean.False);
                continue;
            }

            if (token.SequenceEqual("null"u8))
            {
                operands.Add(PdfObject.Null);
                continue;
            }

            if (token.SequenceEqual("BI"u8))
            {
                // Inline image: the binary payload between ID and EI is not
                // PDF syntax, so it must be skipped as raw bytes.
                lexer.Position = SkipInlineImage(data, lexer.Position);
                operations.Add(new PdfOperation(Operators.Intern("BI"u8), []));
                operands.Clear();
                continue;
            }

            operations.Add(new PdfOperation(
                Operators.Intern(token),
                operands.Count == 0 ? [] : [.. operands]));
            operands.Clear();
        }

        return operations;
    }

    /// <summary>
    /// Advances past an inline image's dictionary and data, returning the offset
    /// just after the terminating <c>EI</c>.
    /// </summary>
    private static int SkipInlineImage(byte[] data, int position)
    {
        // Find the ID keyword that introduces the binary data.
        var lexer = new PdfLexer(data, position);
        var parser = new PdfParser(data);

        while (!lexer.AtEnd)
        {
            lexer.SkipWhitespace();
            if (lexer.AtEnd)
            {
                return data.Length;
            }

            var b = lexer.Peek();
            if (b is (byte)'/' or (byte)'(' or (byte)'<' or (byte)'[')
            {
                parser.Position = lexer.Position;
                parser.ParseObject();
                lexer.Position = parser.Position > lexer.Position ? parser.Position : lexer.Position + 1;
                continue;
            }

            var token = lexer.ReadToken();
            if (token.Length == 0)
            {
                lexer.Position++;
                continue;
            }

            if (token == "ID")
            {
                break;
            }

            if (token == "EI")
            {
                return lexer.Position;
            }
        }

        // Exactly one whitespace byte separates ID from the data.
        var start = lexer.Position;
        if (start < data.Length && PdfLexer.IsWhitespace(data[start]))
        {
            start++;
        }

        // Scan for an EI that is delimited on both sides — the payload can
        // contain the bytes "EI" by coincidence.
        for (var i = start; i + 1 < data.Length; i++)
        {
            if (data[i] != (byte)'E' || data[i + 1] != (byte)'I')
            {
                continue;
            }

            var precededByWhitespace = i > start && PdfLexer.IsWhitespace(data[i - 1]);
            var followedByBreak = i + 2 >= data.Length || !PdfLexer.IsRegular(data[i + 2]);

            if (precededByWhitespace && followedByBreak)
            {
                return i + 2;
            }
        }

        return data.Length;
    }
}
