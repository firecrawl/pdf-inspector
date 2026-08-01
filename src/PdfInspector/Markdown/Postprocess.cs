// Ported from reference/src/markdown/postprocess.rs
using System.Text;
using System.Text.RegularExpressions;

using PdfInspector.Text;

namespace PdfInspector.Markdown;

/// <summary>Markdown cleanup and post-processing.</summary>
internal static partial class Postprocess
{
    /// <summary>Runs of four or more dots, as a table of contents uses for leaders.</summary>
    [GeneratedRegex(@"\.{4,}")]
    private static partial Regex DotLeaderRegex { get; }

    /// <summary>
    /// A spaced hyphen between two letters, which should be a compound word.
    /// Latin-1 accented letters are listed explicitly, matching the reference.
    /// </summary>
    [GeneratedRegex("([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ]) - ([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ])")]
    private static partial Regex SpacedHyphenRegex { get; }

    /// <summary>An http(s) URL, stopping before trailing punctuation and brackets.</summary>
    [GeneratedRegex(@"https?://[^\s<>\)\]]+[^\s<>\)\]\.\,;]")]
    private static partial Regex UrlRegex { get; }

    /// <summary>Cleans up markdown output.</summary>
    public static string CleanMarkdown(string text, MarkdownOptions options)
    {
        if (options.Profile == MarkdownProfile.Compact)
        {
            // Collapsing dot leaders saves tokens but rewrites source text, so it is
            // reserved for the explicit compact profile.
            text = CollapseDotLeaders(text);
        }

        // Hyphenation comes first, before any other rewriting.
        if (options.FixHyphenation)
        {
            text = FixHyphenation(text);
        }

        if (options.RemovePageNumbers)
        {
            text = RemovePageNumbers(text);
        }

        if (options.FormatUrls)
        {
            text = FormatUrls(text);
        }

        // OCR text layers and some PDF producers emit a trailing space on each text
        // item, which combines with gap-based space insertion to produce double
        // spaces — "Vice  President" instead of "Vice President".
        text = CollapseConsecutiveSpaces(text);
        text = RemoveSpacesBeforeClosingBrackets(text);
        text = RemoveSpacesBeforeSentencePunctuation(text);

        while (text.Contains("\n\n\n", StringComparison.Ordinal))
        {
            text = text.Replace("\n\n\n", "\n\n", StringComparison.Ordinal);
        }

        return text.Trim() + "\n";
    }

    /// <summary>
    /// Collapses runs of two or more spaces to one within each line, preserving
    /// leading indentation and markdown table pipe alignment.
    /// </summary>
    private static string CollapseConsecutiveSpaces(string text)
    {
        var result = new StringBuilder(text.Length);

        foreach (var line in text.Split('\n'))
        {
            // The reference guards on the buffer rather than the loop index, so a
            // leading blank line collapses away. Kept as-is for output parity.
            if (result.Length > 0)
            {
                result.Append('\n');
            }

            var trimmed = line.TrimStart();
            result.Append(line, 0, line.Length - trimmed.Length);

            var prevSpace = false;
            foreach (var ch in trimmed)
            {
                if (ch == ' ')
                {
                    if (!prevSpace)
                    {
                        result.Append(' ');
                    }

                    prevSpace = true;
                }
                else
                {
                    prevSpace = false;
                    result.Append(ch);
                }
            }
        }

        return result.ToString();
    }

    /// <summary>
    /// Removes a space before a closing square bracket. Unit markers and markdown
    /// links occasionally pick up a gap-inserted space before "]" — "[kg/m3 ]" —
    /// which is cosmetic padding.
    /// </summary>
    internal static string RemoveSpacesBeforeClosingBrackets(string text)
    {
        var result = new StringBuilder(text.Length);
        foreach (var ch in text)
        {
            if (ch == ']' && result.Length > 0 && result[^1] == ' ')
            {
                result.Length--;
            }

            result.Append(ch);
        }

        return result.ToString();
    }

    /// <summary>
    /// Removes a stray space before sentence punctuation ("word ." to "word.").
    /// Style-boundary item splits — bold, italic and underline runs — can strand a
    /// trailing period or comma in its own fragment, and several assembly paths
    /// join fragments with spaces. This only fires when the punctuation ends the
    /// token, so decimals and dot leaders are untouched.
    /// </summary>
    internal static string RemoveSpacesBeforeSentencePunctuation(string text)
    {
        var result = new StringBuilder(text.Length);
        for (var i = 0; i < text.Length; i++)
        {
            var ch = text[i];
            if (ch is '.' or ',' or ';' && result.Length > 0 && result[^1] == ' ')
            {
                char? next = i + 1 < text.Length ? text[i + 1] : null;

                // A pipe counts as a token end, so table cells get the same fix.
                var tokenEnds = next is not { } n || char.IsWhiteSpace(n) || n == '|';

                // Runs of dots — ellipses and leaders — are never touched.
                var inDotRun = ch == '.' && next == '.';
                if (tokenEnds && !inDotRun)
                {
                    result.Length--;
                }
            }

            result.Append(ch);
        }

        return result.ToString();
    }

    /// <summary>
    /// Collapses dot leaders into " ... ", so a contents line reads
    /// "Introduction ... 1" rather than carrying forty dots.
    /// </summary>
    internal static string CollapseDotLeaders(string text) => DotLeaderRegex.Replace(text, " ... ");

    /// <summary>
    /// Rejoins compound words split by a spaced hyphen, so "Limoeiro do Nort e"
    /// style breaks close up. List items, which open with "- ", are unaffected
    /// because a letter must precede the hyphen.
    /// </summary>
    internal static string FixHyphenation(string text) => SpacedHyphenRegex.Replace(text, "$1-$2");

    /// <summary>Removes standalone page numbers — lines that are just a short number.</summary>
    private static string RemovePageNumbers(string text)
    {
        var split = text.Split('\n');

        // Match Rust's line iterator: a single trailing newline yields no final
        // empty line, and each line loses a trailing carriage return.
        var count = split.Length > 0 && split[^1].Length == 0 ? split.Length - 1 : split.Length;
        var lines = new string[count];
        for (var i = 0; i < count; i++)
        {
            lines[i] = split[i].TrimEnd('\r');
        }

        var result = new List<string>(count);
        for (var i = 0; i < count; i++)
        {
            var trimmed = lines[i].Trim();

            if (IsPageNumberLine(trimmed))
            {
                var prevIsBreak = i > 0 && lines[i - 1].Trim() == "---";
                var nextIsBreak = i + 1 < count && lines[i + 1].Trim() == "---";
                var prevIsEmpty = i > 0 && lines[i - 1].Trim().Length == 0;
                var nextIsEmpty = i + 1 < count && lines[i + 1].Trim().Length == 0;

                // Isolated: surrounded by blank lines or page breaks.
                var isIsolated = (prevIsBreak || prevIsEmpty || i == 0)
                    && (nextIsBreak || nextIsEmpty || i + 1 == count);

                // Numbers directly before a page break go too.
                var beforeBreak = i + 1 < count
                    && (lines[i + 1].Trim() == "---"
                        || (i + 2 < count && lines[i + 1].Trim().Length == 0 && lines[i + 2].Trim() == "---"));

                if (isIsolated || beforeBreak)
                {
                    continue;
                }
            }

            result.Add(lines[i]);
        }

        return string.Join('\n', result);
    }

    /// <summary>True when a line reads as a page number.</summary>
    internal static bool IsPageNumberLine(string trimmed)
    {
        if (trimmed.Length == 0)
        {
            return false;
        }

        // A bare one- to four-digit number.
        if (trimmed.All(char.IsAsciiDigit) && TextUtils.ByteLength(trimmed) <= 4)
        {
            return true;
        }

        // "Page X of Y", "Page X", or the "Page   of" placeholder.
        var lower = trimmed.ToLowerInvariant();
        if (lower.StartsWith("page", StringComparison.Ordinal))
        {
            var rest = lower[4..].Trim();
            if (rest == "of" || rest.StartsWith("of ", StringComparison.Ordinal))
            {
                return true;
            }

            if (rest.Length > 0 && char.IsAsciiDigit(rest[0]))
            {
                return true;
            }

            if (rest.Length == 0
                || rest.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
                    .All(w => w == "of" || w.All(char.IsAsciiDigit)))
            {
                return true;
            }
        }

        // "X of Y", where both sides are numbers.
        var ofIdx = trimmed.IndexOf(" of ", StringComparison.Ordinal);
        if (ofIdx >= 0)
        {
            var before = trimmed[..ofIdx].Trim();
            var after = trimmed[(ofIdx + 4)..].Trim();
            if (before.Length > 0 && after.Length > 0
                && before.All(char.IsAsciiDigit) && after.All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        // A centred "- X -" page number.
        if (TextUtils.ByteLength(trimmed) >= 3 && trimmed.StartsWith('-') && trimmed.EndsWith('-'))
        {
            var inner = trimmed[1..^1].Trim();
            if (inner.Length > 0 && inner.All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>Converts bare URLs into markdown links, leaving already-linked ones alone.</summary>
    private static string FormatUrls(string text)
    {
        var result = new StringBuilder(text.Length);
        var lastEnd = 0;

        foreach (Match match in UrlRegex.Matches(text))
        {
            var start = match.Index;
            var url = match.Value;

            // A URL already inside a markdown link is preceded by "](".
            var checkStart = Math.Max(start - 2, 0);
            var before = text[checkStart..start];
            var alreadyLinked = before.EndsWith("](", StringComparison.Ordinal);

            // A URL inside link text sits between an unclosed "[" and its "]".
            var prefix = text[..start];
            var insideLinkText = prefix.Count(c => c == '[') > prefix.Count(c => c == ']');

            if (alreadyLinked || insideLinkText)
            {
                result.Append(text, lastEnd, match.Index + match.Length - lastEnd);
            }
            else
            {
                result.Append(text, lastEnd, start - lastEnd);
                result.Append('[').Append(url).Append("](").Append(url).Append(')');
            }

            lastEnd = match.Index + match.Length;
        }

        if (lastEnd < text.Length)
        {
            result.Append(text, lastEnd, text.Length - lastEnd);
        }

        return result.ToString();
    }
}
