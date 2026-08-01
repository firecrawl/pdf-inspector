using PdfInspector.Pdf;
using Xunit;

namespace PdfInspector.Tests;

public sealed class PdfLexerTests
{
    [Theory]
    [InlineData("0", 0L)]
    [InlineData("42", 42L)]
    [InlineData("-17", -17L)]
    [InlineData("+5", 5L)]
    public void ParsesIntegers(string token, long expected)
    {
        Assert.Equal(expected, PdfLexer.ParseNumber(token)!.AsInteger());
    }

    [Theory]
    [InlineData("3.14", 3.14)]
    [InlineData("-.5", -0.5)]
    [InlineData(".25", 0.25)]
    // Malformed forms real files contain: a trailing sign or second dot ends the number.
    [InlineData("1.2.3", 1.2)]
    [InlineData("4-5", 4.0)]
    public void ParsesReals(string token, double expected)
    {
        Assert.Equal(expected, PdfLexer.ParseNumber(token)!.AsNumber()!.Value, 5);
    }

    [Fact]
    public void ReadsLiteralStringEscapesAndNesting()
    {
        var data = "(a\\(b\\)c (nested) \\101 \\n)"u8.ToArray();
        var lexer = new PdfLexer(data, 1);
        var body = lexer.ReadLiteralStringBody();

        Assert.Equal("a(b)c (nested) A \n", System.Text.Encoding.ASCII.GetString(body));
    }

    [Fact]
    public void PadsOddLengthHexString()
    {
        var data = "<48656C6C6F7>"u8.ToArray();
        var lexer = new PdfLexer(data, 1);

        // The trailing digit is padded with a zero, per the specification.
        Assert.Equal([0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x70], lexer.ReadHexStringBody());
    }

    [Fact]
    public void ExpandsNameEscapes()
    {
        var data = "/A#20B"u8.ToArray();
        var lexer = new PdfLexer(data, 1);

        Assert.Equal("A B", lexer.ReadNameBody());
    }
}

public sealed class PdfParserTests
{
    private static PdfObject Parse(string source) =>
        new PdfParser(System.Text.Encoding.ASCII.GetBytes(source)).ParseObject()!;

    [Fact]
    public void ParsesNestedDictionary()
    {
        var dict = Parse("<< /Type /Page /MediaBox [0 0 612 792] /Count 3 >>").AsDictionary()!;

        Assert.Equal("Page", dict.Get("Type")!.AsName());
        Assert.Equal(3, dict.Get("Count")!.AsInteger());

        var box = dict.Get("MediaBox")!.AsArray()!;
        Assert.Equal(4, box.Count);
        Assert.Equal(612, box[2].AsInteger());
    }

    [Fact]
    public void DistinguishesReferencesFromAdjacentIntegers()
    {
        var array = Parse("[1 0 R 2 3]").AsArray()!;

        Assert.Equal(3, array.Count);
        Assert.Equal(new PdfObjectId(1, 0), array[0].AsReference());
        Assert.Equal(2, array[1].AsInteger());
        Assert.Equal(3, array[2].AsInteger());
    }

    [Fact]
    public void RecoversStreamWithWrongDeclaredLength()
    {
        // A Length that overshoots the real data must not swallow the terminator.
        const string source = "<< /Length 999 >>\nstream\nHELLO\nendstream";
        var stream = Parse(source).AsStream()!;

        Assert.Equal("HELLO", System.Text.Encoding.ASCII.GetString(stream.RawData));
    }

    [Fact]
    public void ParsesIndirectObjectHeader()
    {
        var data = System.Text.Encoding.ASCII.GetBytes("12 0 obj\n(hi)\nendobj");
        var parsed = new PdfParser(data).ParseIndirectObjectAt(0)!;

        Assert.Equal(new PdfObjectId(12, 0), parsed.Value.Id);
        Assert.Equal("hi", ((PdfString)parsed.Value.Value).AsText());
    }
}

public sealed class StreamFilterTests
{
    [Fact]
    public void DecodesAsciiHex()
    {
        Assert.Equal("Hi"u8.ToArray(), StreamFilters.AsciiHexDecode("48 69 >"u8.ToArray()));
    }

    [Fact]
    public void DecodesAscii85WithPartialGroup()
    {
        // Two full five-character groups plus a four-character partial group,
        // which encodes three bytes.
        var decoded = StreamFilters.Ascii85Decode("87cURD]j7BEbo7~>"u8.ToArray());
        Assert.Equal("Hello world", System.Text.Encoding.ASCII.GetString(decoded));
    }

    [Fact]
    public void DecodesRunLengthLiteralAndRepeat()
    {
        // 2 -> three literal bytes; 254 -> repeat the next byte three times; 128 ends.
        byte[] encoded = [2, (byte)'a', (byte)'b', (byte)'c', 254, (byte)'z', 128];

        Assert.Equal("abczzz", System.Text.Encoding.ASCII.GetString(StreamFilters.RunLengthDecode(encoded)));
    }

    [Fact]
    public void InflatesZlibData()
    {
        var original = System.Text.Encoding.ASCII.GetBytes(new string('x', 500) + "tail");

        using var buffer = new MemoryStream();
        using (var deflate = new System.IO.Compression.ZLibStream(buffer, System.IO.Compression.CompressionLevel.Optimal, leaveOpen: true))
        {
            deflate.Write(original);
        }

        Assert.Equal(original, StreamFilters.Inflate(buffer.ToArray()));
    }
}

public sealed class ContentStreamTests
{
    [Fact]
    public void DecodesOperatorsAndOperands()
    {
        var ops = ContentStream.Decode("BT /F1 12 Tf 100 700 Td (Hello) Tj ET"u8.ToArray());

        Assert.Equal(["BT", "Tf", "Td", "Tj", "ET"], ops.Select(o => o.Operator));
        Assert.Equal("F1", ops[1].Operands[0].AsName());
        Assert.Equal(12, ops[1].Operands[1].AsInteger());
    }

    [Fact]
    public void SkipsInlineImageBinaryPayload()
    {
        // The payload contains bytes that look like operators; only the delimited
        // EI terminates the image.
        var data = "q BI /W 2 /H 2 ID \x01Q ET\x02 EI Q"u8.ToArray();
        var ops = ContentStream.Decode(data);

        Assert.Equal(["q", "BI", "Q"], ops.Select(o => o.Operator));
    }

    [Fact]
    public void StripsCommentsOutsideStrings()
    {
        var stripped = PdfInspector.Extractor.ContentStreamExtractor.StripPdfComments(
            "(a % b) % comment\nQ"u8.ToArray());

        Assert.Equal("(a % b)  \nQ", System.Text.Encoding.ASCII.GetString(stripped));
    }
}
