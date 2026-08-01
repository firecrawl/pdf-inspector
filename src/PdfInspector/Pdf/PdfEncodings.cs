using System.Text;

namespace PdfInspector.Pdf;

/// <summary>PDFDocEncoding, used for text strings that carry no byte-order mark.</summary>
internal static class PdfEncodings
{
    /// <summary>
    /// PDFDocEncoding differs from Latin-1 only in 0x18–0x1F and 0x80–0x9F; every
    /// other byte maps to the same code point. This table covers the divergent range.
    /// </summary>
    private static readonly char[] HighRange =
    [
        /* 0x18 */ '˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜',
    ];

    private static readonly char[] Range80 =
    [
        /* 0x80 */ '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄',
        /* 0x88 */ '‹', '›', '−', '‰', '„', '“', '”', '‘',
        /* 0x90 */ '’', '‚', '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š',
        /* 0x98 */ 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '�',
    ];

    public static string PdfDocEncodingToString(ReadOnlySpan<byte> bytes)
    {
        var sb = new StringBuilder(bytes.Length);
        foreach (var b in bytes)
        {
            sb.Append(Decode(b));
        }

        return sb.ToString();
    }

    private static char Decode(byte b) => b switch
    {
        >= 0x18 and <= 0x1F => HighRange[b - 0x18],
        >= 0x80 and <= 0x9F => Range80[b - 0x80],
        0xA0 => '€',
        _ => (char)b,
    };
}
