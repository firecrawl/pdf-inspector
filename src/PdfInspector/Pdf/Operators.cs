using System.Text;

namespace PdfInspector.Pdf;

/// <summary>
/// Maps operator tokens to shared string instances.
/// </summary>
/// <remarks>
/// A content stream is built from a fixed vocabulary of about eighty
/// operators repeated millions of times, so allocating a fresh string for each
/// occurrence is pure waste. Handing back one instance per operator also makes
/// the extractor's dispatch cheaper: comparing interned references usually
/// settles on the first check rather than walking the characters.
/// </remarks>
internal static class Operators
{
    /// <summary>
    /// Every operator in the PDF specification's content-stream grammar
    /// (ISO 32000-1 table A.1), plus the inline-image markers.
    /// </summary>
    private static readonly string[] Known =
    [
        // Graphics state
        "q", "Q", "cm", "w", "J", "j", "M", "d", "ri", "i", "gs",

        // Path construction and painting
        "m", "l", "c", "v", "y", "h", "re",
        "S", "s", "f", "F", "f*", "B", "B*", "b", "b*", "n",

        // Clipping
        "W", "W*",

        // Text
        "BT", "ET", "Tc", "Tw", "Tz", "TL", "Tf", "Tr", "Ts",
        "Td", "TD", "Tm", "T*", "Tj", "TJ", "'", "\"",

        // Type 3 fonts
        "d0", "d1",

        // Colour
        "CS", "cs", "SC", "SCN", "sc", "scn", "G", "g", "RG", "rg", "K", "k",

        // Shading, XObjects and images
        "sh", "Do", "BI", "ID", "EI",

        // Marked content
        "MP", "DP", "BMC", "BDC", "EMC",

        // Compatibility
        "BX", "EX",
    ];

    /// <summary>
    /// Known operators bucketed by length, so a lookup compares only against
    /// candidates that could possibly match.
    /// </summary>
    private static readonly string[][] ByLength = BuildBuckets();

    private static string[][] BuildBuckets()
    {
        var longest = Known.Max(op => op.Length);
        var buckets = new List<string>[longest + 1];
        for (var i = 0; i < buckets.Length; i++)
        {
            buckets[i] = [];
        }

        foreach (var op in Known)
        {
            buckets[op.Length].Add(op);
        }

        return [.. buckets.Select(b => b.ToArray())];
    }

    /// <summary>
    /// Returns the shared instance for a known operator, or a fresh string for
    /// anything outside the specification's vocabulary.
    /// </summary>
    public static string Intern(ReadOnlySpan<byte> token)
    {
        if (token.Length < ByLength.Length)
        {
            foreach (var candidate in ByLength[token.Length])
            {
                if (Matches(token, candidate))
                {
                    return candidate;
                }
            }
        }

        // Producers do emit operators that are not in the specification. Those
        // still have to round-trip, they just do not get a shared instance.
        return Encoding.ASCII.GetString(token);
    }

    private static bool Matches(ReadOnlySpan<byte> token, string candidate)
    {
        for (var i = 0; i < candidate.Length; i++)
        {
            if (token[i] != (byte)candidate[i])
            {
                return false;
            }
        }

        return true;
    }
}
