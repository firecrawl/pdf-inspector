// Ported from reference/tests/integration_tests.rs
using System.Globalization;
using System.Text;
using PdfInspector.Types;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>Covers image-XObject bounding boxes and the Type0/CID decode guard.</summary>
public sealed class ImageAndCidTests
{
    /// <summary>
    /// A one-page PDF holding a single image XObject placed at a known matrix.
    /// The <c>Do</c> operator applies the matrix to the unit square, so an
    /// axis-aligned image of size w×h at (x, y) uses [w, 0, 0, h, x, y].
    /// </summary>
    private static byte[] MakePdfWithImage(float[] imageCtm)
    {
        var pdf = new List<byte>(Encoding.ASCII.GetBytes("%PDF-1.4\n"));
        var offsets = new List<int> { 0 };

        void AddObject(int id, string body)
        {
            offsets.Add(pdf.Count);
            pdf.AddRange(Encoding.ASCII.GetBytes($"{id} 0 obj\n"));
            pdf.AddRange(Encoding.ASCII.GetBytes(body));
            pdf.AddRange(Encoding.ASCII.GetBytes("\nendobj\n"));
        }

        void AddStreamObject(int id, string dict, byte[] streamBytes)
        {
            offsets.Add(pdf.Count);
            pdf.AddRange(Encoding.ASCII.GetBytes($"{id} 0 obj\n"));
            pdf.AddRange(Encoding.ASCII.GetBytes($"<< {dict} /Length {streamBytes.Length} >>\nstream\n"));
            pdf.AddRange(streamBytes);
            pdf.AddRange(Encoding.ASCII.GetBytes("\nendstream\nendobj\n"));
        }

        AddObject(1, "<< /Type /Catalog /Pages 2 0 R >>");
        AddObject(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        AddObject(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            + "/Resources << /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >> /Contents 4 0 R >>");

        // A little text keeps the page from classifying as image-only, which
        // would route it down a different path. Then the matrix is applied
        // inside a saved graphics state and the image invoked.
        var matrix = string.Join(' ', imageCtm.Select(v => v.ToString("0.####", CultureInfo.InvariantCulture)));
        var content = $"BT /F1 12 Tf 100 700 Td (Hi) Tj ET\nq {matrix} cm /Im0 Do Q";
        AddStreamObject(4, string.Empty, Encoding.ASCII.GetBytes(content));
        AddObject(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

        // A 1×1 mid-grey image. Its contents do not matter: the extractor reads
        // only the XObject's subtype and the matrix in force at the invocation.
        AddStreamObject(
            6,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 "
            + "/ColorSpace /DeviceGray /BitsPerComponent 8",
            [128]);

        var xrefStart = pdf.Count;
        pdf.AddRange(Encoding.ASCII.GetBytes($"xref\n0 {offsets.Count}\n"));
        pdf.AddRange(Encoding.ASCII.GetBytes("0000000000 65535 f \n"));
        foreach (var offset in offsets.Skip(1))
        {
            pdf.AddRange(Encoding.ASCII.GetBytes($"{offset:D10} 00000 n \n"));
        }

        pdf.AddRange(Encoding.ASCII.GetBytes(
            $"trailer\n<< /Size {offsets.Count} /Root 1 0 R >>\nstartxref\n{xrefStart}\n%%EOF"));

        return [.. pdf];
    }

    [Fact]
    public void AnImageXObjectEmitsItsBoundingBox()
    {
        // A 200×100 image at (50, 600) in user space, whose origin is bottom-left.
        var pdf = MakePdfWithImage([200.0f, 0.0f, 0.0f, 100.0f, 50.0f, 600.0f]);
        var items = PdfProcessor.ExtractTextWithPositionsMem(pdf);

        var image = Assert.Single(items, i => i.Kind == ItemKind.Image);
        Assert.True(MathF.Abs(image.X - 50.0f) < 0.01f, $"x={image.X}");
        Assert.True(MathF.Abs(image.Y - 600.0f) < 0.01f, $"y={image.Y}");
        Assert.True(MathF.Abs(image.Width - 200.0f) < 0.01f, $"width={image.Width}");
        Assert.True(MathF.Abs(image.Height - 100.0f) < 0.01f, $"height={image.Height}");
        Assert.Equal(1u, image.Page);

        // The text field carries the form the markdown emitter already parses.
        Assert.Equal("[Image: Im0]", image.Text);
    }

    [Fact]
    public void ARotatedImageMatrixYieldsTheEnclosingBox()
    {
        // A 90° rotation: the unit square rotates counter-clockwise about the
        // origin and then translates to (200, 300). For a 100×100 image the
        // corners land at (200,300), (200,400), (100,400) and (100,300), so the
        // enclosing box is x=100..200 by y=300..400.
        var pdf = MakePdfWithImage([0.0f, 100.0f, -100.0f, 0.0f, 200.0f, 300.0f]);
        var items = PdfProcessor.ExtractTextWithPositionsMem(pdf);

        var image = Assert.Single(items, i => i.Kind == ItemKind.Image);
        Assert.True(MathF.Abs(image.X - 100.0f) < 0.01f, $"x={image.X}");
        Assert.True(MathF.Abs(image.Y - 300.0f) < 0.01f, $"y={image.Y}");
        Assert.True(MathF.Abs(image.Width - 100.0f) < 0.01f, $"width={image.Width}");
        Assert.True(MathF.Abs(image.Height - 100.0f) < 0.01f, $"height={image.Height}");
    }

    [Fact]
    public void ImageEmissionDoesNotChangeTheDefaultMarkdown()
    {
        // Images are off by default, so emitting them must not start producing
        // placeholders for callers who never asked for them.
        var pdf = MakePdfWithImage([200.0f, 0.0f, 0.0f, 100.0f, 50.0f, 600.0f]);
        var result = PdfProcessor.ExtractPagesMarkdownMem(pdf);

        var page = Assert.Single(result.Pages);
        Assert.DoesNotContain("Image:", page.Markdown, StringComparison.Ordinal);
    }

    /// <summary>
    /// A Type0 font with Identity-H encoding and a ToUnicode stream that is not
    /// a CMap at all. The two-byte CID 0xCDD9 must not decode as the Latin-1
    /// pair "ÍÙ" — the production scrape symptom — and must instead produce a
    /// replacement character per CID so the page routes to OCR.
    /// </summary>
    [Fact]
    public void ABrokenType0ToUnicodeEmitsReplacementCharactersNotMojibake()
    {
        var pdf = MakeType0BrokenToUnicodePdf();

        var items = PdfProcessor.ExtractTextWithPositionsMem(pdf);
        var combined = string.Concat(items.Select(i => i.Text));

        Assert.DoesNotContain('Í', combined);
        Assert.DoesNotContain('Ù', combined);
        Assert.Contains('�', combined);

        Assert.Contains(1u, PdfProcessor.ProcessPdfMem(pdf).PagesNeedingOcr);
    }

    /// <summary>
    /// Builds the Type0 fixture: an Identity-H font whose descendant marks it
    /// as a CID font, and whose ToUnicode stream is deliberately junk so CMap
    /// parsing fails while the reference from the font dictionary remains.
    /// </summary>
    private static byte[] MakeType0BrokenToUnicodePdf()
    {
        var pdf = new List<byte>(Encoding.ASCII.GetBytes("%PDF-1.5\n"));
        var offsets = new List<int> { 0 };

        void AddObject(int id, string body)
        {
            offsets.Add(pdf.Count);
            pdf.AddRange(Encoding.ASCII.GetBytes($"{id} 0 obj\n"));
            pdf.AddRange(Encoding.ASCII.GetBytes(body));
            pdf.AddRange(Encoding.ASCII.GetBytes("\nendobj\n"));
        }

        void AddStreamObject(int id, string dict, byte[] streamBytes)
        {
            offsets.Add(pdf.Count);
            pdf.AddRange(Encoding.ASCII.GetBytes($"{id} 0 obj\n"));
            pdf.AddRange(Encoding.ASCII.GetBytes($"<< {dict} /Length {streamBytes.Length} >>\nstream\n"));
            pdf.AddRange(streamBytes);
            pdf.AddRange(Encoding.ASCII.GetBytes("\nendstream\nendobj\n"));
        }

        AddObject(1, "<< /Type /Catalog /Pages 2 0 R >>");
        AddObject(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        AddObject(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
            + "/Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>");

        // A hex string of two 2-byte CIDs, each with a high byte.
        const string content = "BT /F0 12 Tf 50 100 Td <CDD9CDD9> Tj ET";
        AddStreamObject(4, string.Empty, Encoding.ASCII.GetBytes(content));

        AddObject(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /AAAAAA+SyntheticCID /Encoding /Identity-H "
            + "/DescendantFonts [6 0 R] /ToUnicode 8 0 R >>");
        AddObject(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /AAAAAA+SyntheticCID "
            + "/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> "
            + "/FontDescriptor 7 0 R /DW 1000 >>");
        AddObject(
            7,
            "<< /Type /FontDescriptor /FontName /AAAAAA+SyntheticCID /Flags 4 "
            + "/FontBBox [-100 -100 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 "
            + "/CapHeight 700 /StemV 80 >>");

        // Junk rather than a CMap, so parsing fails while the reference stands.
        AddStreamObject(8, string.Empty, Encoding.ASCII.GetBytes("this is not a valid CMap stream"));

        var xrefStart = pdf.Count;
        pdf.AddRange(Encoding.ASCII.GetBytes($"xref\n0 {offsets.Count}\n"));
        pdf.AddRange(Encoding.ASCII.GetBytes("0000000000 65535 f \n"));
        foreach (var offset in offsets.Skip(1))
        {
            pdf.AddRange(Encoding.ASCII.GetBytes($"{offset:D10} 00000 n \n"));
        }

        pdf.AddRange(Encoding.ASCII.GetBytes(
            $"trailer\n<< /Size {offsets.Count} /Root 1 0 R >>\nstartxref\n{xrefStart}\n%%EOF"));

        return [.. pdf];
    }
}
