using System.Globalization;
using System.Text;

namespace PdfInspector.Tests;

/// <summary>
/// Builds small uncompressed PDFs for the region and structure-recovery tests.
/// The Rust suite assembles these with lopdf's document writer; this is the
/// equivalent, emitting the same page geometry and content streams.
/// </summary>
internal static class SyntheticPdf
{
    /// <summary>
    /// Wraps a content stream in a one-page document with a Helvetica font
    /// resource named F1.
    /// </summary>
    public static byte[] SinglePage(string content, int width, int height)
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

        AddObject(1, "<< /Type /Catalog /Pages 2 0 R >>");
        AddObject(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        AddObject(
            3,
            $"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] "
            + "/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");
        AddObject(4, $"<< /Length {content.Length} >>\nstream\n{content}\nendstream");
        AddObject(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

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

    /// <summary>
    /// Three text rows spaced 16.8pt apart in two columns, in a 200×800 page.
    /// The tight row spacing is what makes generous structure-model cell boxes
    /// overlap their neighbours.
    /// </summary>
    public static byte[] DenseTable()
    {
        var content = new StringBuilder();
        content.Append("BT /F1 10 Tf 20 700 Td (Branch Name) Tj ");
        content.Append("100 0 Td (Deposits) Tj ");
        content.Append("-100 -16.8 Td (Oak Street) Tj ");
        content.Append("100 0 Td (100) Tj ");
        content.Append("-100 -16.8 Td (Boardwalk) Tj ");
        content.Append("100 0 Td (200) Tj ET");
        return SinglePage(content.ToString(), 200, 800);
    }

    /// <summary>
    /// A stroked 2×2 grid with a cell label in each box, in a 300×800 page.
    /// With <paramref name="twoTables"/> a second identical grid is drawn
    /// lower on the page, so region filtering has something to exclude.
    /// </summary>
    public static byte[] VectorGrid(bool twoTables)
    {
        var content = new StringBuilder();

        void PushGrid(int xLeft, int xMid, int xRight, int yTop, int yMid, int yBottom)
        {
            foreach (var y in new[] { yTop, yMid, yBottom })
            {
                content.Append(CultureInfo.InvariantCulture, $"{xLeft} {y} m {xRight} {y} l ");
            }

            foreach (var x in new[] { xLeft, xMid, xRight })
            {
                content.Append(CultureInfo.InvariantCulture, $"{x} {yBottom} m {x} {yTop} l ");
            }

            content.Append("S ");
        }

        void PushText(int x, int y, string text) =>
            content.Append(CultureInfo.InvariantCulture, $"1 0 0 1 {x} {y} Tm ({text}) Tj ");

        PushGrid(50, 130, 210, 740, 710, 670);
        if (twoTables)
        {
            PushGrid(50, 130, 210, 560, 530, 490);
        }

        content.Append("BT /F1 10 Tf ");
        PushText(70, 724, "A1");
        PushText(150, 724, "B1");
        PushText(70, 688, "A2");
        PushText(150, 688, "B2");
        if (twoTables)
        {
            PushText(70, 544, "C1");
            PushText(150, 544, "D1");
            PushText(70, 508, "C2");
            PushText(150, 508, "D2");
        }

        content.Append("ET");
        return SinglePage(content.ToString(), 300, 800);
    }

    /// <summary>A stroked grid of three rows by two columns, in a 300×800 page.</summary>
    public static byte[] VectorGridThreeRows()
    {
        var content = new StringBuilder();

        foreach (var y in new[] { 740, 710, 680, 650 })
        {
            content.Append(CultureInfo.InvariantCulture, $"50 {y} m 210 {y} l ");
        }

        foreach (var x in new[] { 50, 130, 210 })
        {
            content.Append(CultureInfo.InvariantCulture, $"{x} 650 m {x} 740 l ");
        }

        content.Append("S BT /F1 10 Tf ");
        foreach (var (x, y, text) in new (int X, int Y, string Text)[]
        {
            (70, 724, "Branch"), (150, 724, "Deposits"),
            (70, 694, "Oak"), (150, 694, "100"),
            (70, 664, "Boardwalk"), (150, 664, "200"),
        })
        {
            content.Append(CultureInfo.InvariantCulture, $"1 0 0 1 {x} {y} Tm ({text}) Tj ");
        }

        content.Append("ET");
        return SinglePage(content.ToString(), 300, 800);
    }

    /// <summary>
    /// A four-corner polygon <c>[x1,y1, x2,y1, x2,y2, x1,y2]</c> from an
    /// axis-aligned rect, matching the shape structure models emit for cells.
    /// </summary>
    public static float[] Polygon(float x1, float y1, float x2, float y2) =>
        [x1, y1, x2, y1, x2, y2, x1, y2];
}
