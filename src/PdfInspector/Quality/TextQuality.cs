// Ported from reference/src/text_quality.rs
using System.Text;
using PdfInspector.Types;

namespace PdfInspector.Quality;

/// <summary>The outcome of a whole-document text-quality analysis.</summary>
public sealed class TextQualityReport
{
    public List<uint> PagesNeedingOcr { get; init; } = [];

    public bool HasEncodingIssues { get; init; }

    public SortedDictionary<uint, List<string>> ReasonsByPage { get; init; } = [];
}

/// <summary>
/// Decides when an extracted text layer is too broken to serve and a page
/// should fall back to OCR.
///
/// Extraction can produce plausible-looking bytes that are actually garbage —
/// failed CID→Unicode mappings, broken ToUnicode CMaps, mojibake. The detectors
/// come in two layers over the same primitives: markdown-level checks that run
/// on a page's final string, and item-level checks that accumulate per-page
/// evidence so a localized garbled span on an otherwise clean page is caught
/// without any single span having to condemn the page.
/// </summary>
internal static class TextQuality
{
    /// <summary>OCR reason emitted when the text layer appears garbled.</summary>
    public const string ReasonSuspectedGarbledText = "suspected_garbled_text";

    // ── Markdown-level detection ─────────────────────────────────────────

    /// <summary>Detects broken font encodings in a page's extracted markdown.</summary>
    public static bool DetectEncodingIssues(string markdown)
    {
        // Any replacement character means a decode failed.
        if (markdown.Contains('�'))
        {
            return true;
        }

        if (HasDollarAsSpacePattern(markdown))
        {
            return true;
        }

        var stats = new CipherGarbleStats();
        stats.AddText(markdown);
        return stats.LooksGarbled();
    }

    /// <summary>
    /// Detects the <c>Word$Word$Word</c> pattern a broken ToUnicode CMap
    /// produces, where <c>$</c> stands in for the space character. Triggers when
    /// most dollars sit between letters, or when there are simply too many such
    /// occurrences for ordinary financial text.
    /// </summary>
    private static bool HasDollarAsSpacePattern(string markdown)
    {
        var totalDollars = markdown.Count(c => c == '$');
        if (totalDollars <= 10)
        {
            return false;
        }

        var letterDollarLetter = 0;
        for (var i = 1; i + 1 < markdown.Length; i++)
        {
            if (markdown[i] == '$' &&
                char.IsAsciiLetter(markdown[i - 1]) &&
                char.IsAsciiLetter(markdown[i + 1]))
            {
                letterDollarLetter++;
            }
        }

        return letterDollarLetter > 20 || letterDollarLetter * 2 > totalDollars;
    }

    /// <summary>
    /// True when extracted text is predominantly non-alphanumeric. Broken
    /// encodings produce output like <c>----1-.-.-.___ --.-. .._ I_---.</c>;
    /// real text in any language is more than half alphanumeric.
    /// </summary>
    public static bool IsGarbageText(string markdown)
    {
        var alphanum = 0;
        var nonAlphanum = 0;

        var chars = markdown.ToCharArray();
        var i = 0;
        while (i < chars.Length)
        {
            var ch = chars[i];
            var runEnd = i + 1;
            while (runEnd < chars.Length && chars[runEnd] == ch)
            {
                runEnd++;
            }

            // Dot leaders and underscore rules are layout, not text.
            var isDecorativeLeader = ch is '.' or '_' or '·' && runEnd - i >= 3;
            if (!isDecorativeLeader)
            {
                for (var j = i; j < runEnd; j++)
                {
                    var runCh = chars[j];
                    if (char.IsWhiteSpace(runCh))
                    {
                        continue;
                    }

                    // Markdown syntax this pipeline adds is not from the PDF.
                    if (runCh is '#' or '*' or '|' or '-' or '\n')
                    {
                        continue;
                    }

                    if (char.IsLetterOrDigit(runCh))
                    {
                        alphanum++;
                    }
                    else
                    {
                        nonAlphanum++;
                    }
                }
            }

            i = runEnd;
        }

        var total = alphanum + nonAlphanum;
        return total >= 50 && alphanum * 2 < total;
    }

    /// <summary>
    /// Detects garbage from a failed CID→Unicode mapping on Identity-H fonts,
    /// where raw bytes land in the C1 control range or the private-use area,
    /// mixed with stray Latin Extended characters.
    /// </summary>
    public static bool IsCidGarbage(string text)
    {
        if (IsGarbageText(text))
        {
            return true;
        }

        var total = 0;
        var c1Control = 0;
        var highLatin = 0;

        foreach (var ch in text)
        {
            if (char.IsWhiteSpace(ch))
            {
                continue;
            }

            total++;

            if (ch == '·')
            {
                continue;
            }

            // C1 controls almost never appear in real text.
            if (ch is >= '\u0080' and <= '\u009F')
            {
                c1Control++;
            }

            // High Latin-1 is legitimate in Western European text, but paired
            // with ASCII in CID passthrough it signals mojibake from CID values
            // being read as Latin-1.
            if (ch is >= '\u00A0' and <= '\u00FF')
            {
                highLatin++;
            }
        }

        if (total < 5)
        {
            return false;
        }

        if (c1Control >= 2 && c1Control * 20 >= total)
        {
            return true;
        }

        // The length floor keeps short math tokens such as "2×()×" from routing
        // a clean page to OCR.
        var asciiLetters = text.Count(char.IsAsciiLetter);
        return total >= 20 && highLatin * 5 >= total * 2 && asciiLetters * 3 < total;
    }

    // ── Item-level detection ─────────────────────────────────────────────

    private enum SpanIssue
    {
        Replacement,
        Strong,
    }

    /// <summary>Per-page evidence accumulated across a document's text items.</summary>
    private sealed class PageEvidence
    {
        public int Chars;
        public int ReplacementChars;
        public int ReplacementSpans;
        public int LongestReplacementRun;
        public CipherGarbleStats CipherGarble = new();
    }

    /// <summary>Analyses every text item and reports which pages need OCR.</summary>
    public static TextQualityReport AnalyzeTextQuality(IReadOnlyList<TextItem> items)
    {
        var reasonsByPage = new SortedDictionary<uint, List<string>>();
        var evidenceByPage = new SortedDictionary<uint, PageEvidence>();

        foreach (var item in items)
        {
            if (item.Kind != ItemKind.Text)
            {
                continue;
            }

            if (!evidenceByPage.TryGetValue(item.Page, out var evidence))
            {
                evidence = new PageEvidence();
                evidenceByPage[item.Page] = evidence;
            }

            evidence.Chars += item.Text.Count(ch => !char.IsWhiteSpace(ch));
            evidence.CipherGarble.AddText(item.Text);

            switch (SpanDecodingIssueKind(item.Text))
            {
                case SpanIssue.Strong:
                    AddReason(reasonsByPage, item.Page, ReasonSuspectedGarbledText);
                    break;

                case SpanIssue.Replacement:
                {
                    var (replacement, longestRun) = ReplacementTextStats(item.Text);
                    evidence.ReplacementChars += replacement;
                    evidence.ReplacementSpans += 1;
                    evidence.LongestReplacementRun = Math.Max(evidence.LongestReplacementRun, longestRun);
                    break;
                }
            }
        }

        foreach (var (page, evidence) in evidenceByPage)
        {
            if (reasonsByPage.ContainsKey(page))
            {
                continue;
            }

            if (PageReplacementEvidenceNeedsOcr(evidence) || evidence.CipherGarble.LooksGarbled())
            {
                AddReason(reasonsByPage, page, ReasonSuspectedGarbledText);
            }
        }

        var pagesNeedingOcr = reasonsByPage.Keys.ToList();
        return new TextQualityReport
        {
            HasEncodingIssues = pagesNeedingOcr.Count > 0,
            PagesNeedingOcr = pagesNeedingOcr,
            ReasonsByPage = reasonsByPage,
        };
    }

    private static void AddReason(SortedDictionary<uint, List<string>> reasons, uint page, string reason)
    {
        if (!reasons.TryGetValue(page, out var list))
        {
            list = [];
            reasons[page] = list;
        }

        if (!list.Contains(reason, StringComparer.Ordinal))
        {
            list.Add(reason);
        }
    }

    /// <summary>True when any text item in a region shows a decoding problem.</summary>
    public static bool RegionItemsHaveDecodingIssue(IReadOnlyList<TextItem> items) =>
        items.Any(item => item.Kind == ItemKind.Text && SpanDecodingIssueKind(item.Text) is not null);

    private static SpanIssue? SpanDecodingIssueKind(string text)
    {
        text = text.Trim();
        if (text.Length == 0)
        {
            return null;
        }

        if (HasDollarAsSpacePattern(text)
            || HasPrivateUseTextRun(text)
            || IsCidGarbage(text)
            || HasCidControlToken(text))
        {
            return SpanIssue.Strong;
        }

        return HasReplacementTextRun(text) ? SpanIssue.Replacement : null;
    }

    private static (int Replacement, int LongestRun) ReplacementTextStats(string text)
    {
        var replacement = 0;
        var currentRun = 0;
        var longestRun = 0;

        foreach (var ch in text)
        {
            if (ch == '�')
            {
                replacement++;
                currentRun++;
                longestRun = Math.Max(longestRun, currentRun);
            }
            else
            {
                currentRun = 0;
            }
        }

        return (replacement, longestRun);
    }

    private static bool PageReplacementEvidenceNeedsOcr(PageEvidence evidence)
    {
        if (evidence.ReplacementChars == 0 || evidence.Chars == 0)
        {
            return false;
        }

        // When the whole page is a short broken text layer, even a brief
        // replacement run is enough. On text-heavy pages, density is required
        // so that math formulas do not force full-page OCR.
        if (evidence.Chars <= 80 && evidence.LongestReplacementRun >= 2)
        {
            return true;
        }

        var densityBps = evidence.ReplacementChars * 10_000 / evidence.Chars;
        var enoughBadText = evidence.ReplacementChars >= 12 && densityBps >= 500;
        var repeatedBadSpans = evidence.ReplacementSpans >= 3 && densityBps >= 250;
        var longBadRun = evidence.LongestReplacementRun >= 8 && densityBps >= 250;

        return enoughBadText || repeatedBadSpans || longBadRun;
    }

    private static bool HasReplacementTextRun(string text)
    {
        var (replacement, longestRun) = ReplacementTextStats(text);
        return longestRun >= 2 || replacement >= 3;
    }

    private static bool HasPrivateUseTextRun(string text)
    {
        var total = 0;
        var privateUse = 0;
        var currentRun = 0;
        var longestRun = 0;

        foreach (var rune in text.EnumerateRunes())
        {
            if (Rune.IsWhiteSpace(rune))
            {
                currentRun = 0;
                continue;
            }

            total++;
            if (IsPrivateUseCodePoint(rune.Value))
            {
                privateUse++;
                currentRun++;
                longestRun = Math.Max(longestRun, currentRun);
            }
            else
            {
                currentRun = 0;
            }
        }

        if (privateUse == 0)
        {
            return false;
        }

        return longestRun >= 3 || (total >= 5 && privateUse >= 2 && privateUse * 2 >= total);
    }

    private static bool HasCidControlToken(string text) =>
        text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Any(TokenHasCidControl);

    private static bool TokenHasCidControl(string token)
    {
        var total = 0;
        var c1Control = 0;

        foreach (var ch in token)
        {
            total++;
            if (ch is >= '\u0080' and <= '\u009F')
            {
                c1Control++;
            }
        }

        return total >= 5 && c1Control >= 2 && c1Control * 20 >= total;
    }

    /// <summary>True for private-use code points in the basic and supplementary planes.</summary>
    private static bool IsPrivateUseCodePoint(int codePoint) => codePoint
        is >= 0xE000 and <= 0xF8FF
        or >= 0xF0000 and <= 0xFFFFD
        or >= 0x100000 and <= 0x10FFFD;
}
