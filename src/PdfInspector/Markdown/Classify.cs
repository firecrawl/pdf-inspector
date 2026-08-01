// Ported from reference/src/markdown/classify.rs
using PdfInspector.Text;

namespace PdfInspector.Markdown;

/// <summary>Line classification: captions, lists, and code detection.</summary>
internal static class Classify
{
    /// <summary>Caption prefixes that always match, since an identifier always follows.</summary>
    private static readonly string[] AlwaysPrefixes =
    [
        "Figura ", "Fig. ", "Fig ", "Tabela ", "Source:", "Fonte:", "Source ", "Fonte ",
        "Note:", "Nota:", "Chart ", "Gráfico ", "Graph ", "Diagram ", "Image ", "Imagem ",
        "Photo ", "Foto ",
    ];

    /// <summary>Font-name fragments that mark a monospace face.</summary>
    private static readonly string[] MonospacePatterns =
    [
        "courier", "consolas", "monaco", "menlo", "mono", "fixed", "terminal", "typewriter",
        "source code", "fira code", "jetbrains", "inconsolata", "dejavu sans mono",
        "liberation mono",
    ];

    /// <summary>Prefixes and operators that mark a line as source code.</summary>
    private static readonly string[] CodePatterns =
    [
        // Language keywords.
        "import ", "export ", "from ", "const ", "let ", "var ", "function ", "class ", "def ",
        "pub fn ", "fn ", "async fn ", "impl ",

        // Syntax patterns.
        "=> ", "-> ", ":: ", ":= ",
    ];

    /// <summary>True when the text is a figure or table caption, or a source citation.</summary>
    public static bool IsCaptionLine(string text)
    {
        var trimmed = text.Trim();

        foreach (var prefix in AlwaysPrefixes)
        {
            if (trimmed.StartsWith(prefix, StringComparison.Ordinal))
            {
                return true;
            }
        }

        // "Figure" and "Table" need a digit or reference after them, so captions
        // ("Table 1", "Figure 3.2") stay distinct from headings ("Table of Contents").
        foreach (var prefix in new[] { "Figure ", "Table " })
        {
            if (trimmed.StartsWith(prefix, StringComparison.Ordinal)
                && StartsWithReference(trimmed[prefix.Length..]))
            {
                return true;
            }
        }

        var lower = trimmed.ToLowerInvariant();
        foreach (var prefix in new[] { "figure ", "table " })
        {
            if (lower.StartsWith(prefix, StringComparison.Ordinal)
                && StartsWithReference(lower[prefix.Length..]))
            {
                return true;
            }
        }

        return lower.StartsWith("source:", StringComparison.Ordinal);
    }

    /// <summary>True when the remainder opens with a digit, a paren or a hash.</summary>
    private static bool StartsWithReference(string rest)
    {
        var t = rest.TrimStart();
        return t.Length > 0 && (char.IsAsciiDigit(t[0]) || t[0] is '(' or '#');
    }

    /// <summary>
    /// True when the text opens with an unambiguous bullet marker. This is
    /// narrower than <see cref="IsListItem"/>: it excludes numbered and lettered
    /// patterns such as "1." or "a)", which legitimately open section headings in
    /// many documents. The heading classifier uses it to reject bullet lines
    /// without also demoting numbered headings.
    /// </summary>
    public static bool StartsWithBulletMarker(string text)
    {
        var trimmed = text.TrimStart();
        return trimmed.StartsWith("• ", StringComparison.Ordinal)
            || trimmed.StartsWith("● ", StringComparison.Ordinal)
            || trimmed.StartsWith("○ ", StringComparison.Ordinal)
            || trimmed.StartsWith("◦ ", StringComparison.Ordinal)
            || trimmed.StartsWith("- ", StringComparison.Ordinal)
            || trimmed.StartsWith("* ", StringComparison.Ordinal);
    }

    /// <summary>True when the text reads as a list item.</summary>
    public static bool IsListItem(string text)
    {
        var trimmed = text.TrimStart();

        if (trimmed.StartsWith("• ", StringComparison.Ordinal)
            || trimmed.StartsWith("- ", StringComparison.Ordinal)
            || trimmed.StartsWith("* ", StringComparison.Ordinal)
            || trimmed.StartsWith("○ ", StringComparison.Ordinal)
            || trimmed.StartsWith("● ", StringComparison.Ordinal)
            || trimmed.StartsWith("◦ ", StringComparison.Ordinal))
        {
            return true;
        }

        // Numbered patterns: "1.", "1)", "10.".
        var firstChars = new string(trimmed.Take(5).ToArray());
        if (firstChars.Any(char.IsAsciiDigit))
        {
            var idx = firstChars.IndexOfAny(['.', ')']);
            if (idx >= 0 && firstChars[..idx].All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        // Lettered patterns: "a.", "a)", "(a)".
        if (trimmed.Length >= 2)
        {
            if (char.IsAsciiLetter(trimmed[0]) && trimmed[1] is '.' or ')')
            {
                return true;
            }

            if (trimmed[0] == '(' && trimmed.Length >= 3 && trimmed[2] == ')')
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>Rewrites a list item into markdown form.</summary>
    public static string FormatListItem(string text)
    {
        var trimmed = text.TrimStart();

        foreach (var bullet in new[] { '•', '○', '●', '◦' })
        {
            if (trimmed.Length > 0 && trimmed[0] == bullet)
            {
                return "- " + trimmed[1..].TrimStart();
            }

            // A bullet inside a leading style run — "**● Label:** rest" or
            // "<u>● Label</u>" — happens because the run wraps both the marker and
            // the label, which carry the same style in the PDF. The marker has to
            // move outside the wrapper so markdown still sees a list item.
            foreach (var wrapper in new[] { "**", "*", "<u>" })
            {
                if (trimmed.StartsWith(wrapper, StringComparison.Ordinal))
                {
                    var afterOpen = trimmed[wrapper.Length..];
                    if (afterOpen.Length > 0 && afterOpen[0] == bullet)
                    {
                        return "- " + wrapper + afterOpen[1..].TrimStart();
                    }
                }
            }
        }

        // A dash or asterisk marker, and numbered lists, already read as markdown.
        return trimmed;
    }

    /// <summary>True when the text reads as source code.</summary>
    public static bool IsCodeLike(string text)
    {
        var trimmed = text.Trim();

        foreach (var pattern in CodePatterns)
        {
            if (trimmed.StartsWith(pattern, StringComparison.Ordinal))
            {
                return true;
            }
        }

        var specialChars = trimmed.Count(c => c is '{' or '}' or '(' or ')' or '[' or ']' or ';' or '=' or '<' or '>');
        if (specialChars >= 3 && TextUtils.ByteLength(trimmed) < 200)
        {
            return true;
        }

        return trimmed.EndsWith(';') || trimmed.EndsWith('{') || trimmed.EndsWith('}');
    }

    /// <summary>True when the font name marks a monospace face.</summary>
    public static bool IsMonospaceFont(string fontName)
    {
        var lower = fontName.ToLowerInvariant();
        return MonospacePatterns.Any(p => lower.Contains(p, StringComparison.Ordinal));
    }
}
