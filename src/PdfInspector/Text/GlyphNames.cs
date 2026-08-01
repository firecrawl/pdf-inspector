// Ported from reference/src/glyph_names.rs
using System.Globalization;

namespace PdfInspector.Text;

/// <summary>Maps Adobe Glyph List (and related) glyph names to Unicode.</summary>
internal static partial class GlyphNames
{
    /// <summary>Resolves a glyph name to its character, or null when it is unknown.</summary>
    public static char? GlyphToChar(string name)
    {
        if (Table.TryGetValue(name, out var direct))
        {
            return direct;
        }

        // Per the Adobe Glyph List spec, a suffix after '.' selects a variant of
        // a base glyph: "zero.tf" → "zero", "a.ss01" → "a".
        var dot = name.IndexOf('.', StringComparison.Ordinal);
        if (dot > 0 && Table.TryGetValue(name[..dot], out var baseChar))
        {
            return baseChar;
        }

        if (name.StartsWith("uni", StringComparison.Ordinal) && name.Length >= 7 &&
            uint.TryParse(name.AsSpan(3, 4), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var uniCode))
        {
            // Windows Symbol fonts map ASCII into the F000 private-use block.
            if (uniCode is >= 0xF000 and <= 0xF0FF)
            {
                uniCode -= 0xF000;
            }

            return FromCodePoint(uniCode);
        }

        if (name.Length >= 5 && name[0] == 'u' &&
            uint.TryParse(name.AsSpan(1), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var uCode))
        {
            return FromCodePoint(uCode);
        }

        return null;
    }

    /// <summary>
    /// Narrows a code point to a UTF-16 code unit. Values outside the basic
    /// multilingual plane have no single-char representation and are rejected,
    /// matching the reference build's use of a scalar glyph type.
    /// </summary>
    private static char? FromCodePoint(uint code)
    {
        if (code > 0xFFFF)
        {
            return null;
        }

        // Unpaired surrogates are not scalar values.
        if (code is >= 0xD800 and <= 0xDFFF)
        {
            return null;
        }

        return (char)code;
    }
}
