// Ported from reference/src/markdown/analysis.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>Font size statistics for a document.</summary>
internal sealed class FontStats
{
    /// <summary>The document's body font size.</summary>
    public required float MostCommonSize { get; init; }

    /// <summary>
    /// The font-size frequency distribution, keyed by tenths of a point, used
    /// for rarity-based heading detection.
    /// </summary>
    public required Dictionary<int, int> SizeCounts { get; init; }

    /// <summary>How many lines went into the distribution.</summary>
    public required int TotalLines { get; init; }
}

/// <summary>Font statistics, heading detection, and document structure analysis.</summary>
internal static class Analysis
{
    private const string Module = "markdown";

    /// <summary>
    /// How rare a font size is in the document, from 0.0 (most common) to 1.0
    /// (unique). This mirrors opendataloader's font-rarity boosting: heading fonts
    /// appear on far fewer lines than body text, so their percentile rank is high.
    /// </summary>
    public static float FontSizeRarity(float fontSize, FontStats stats)
    {
        if (stats.TotalLines == 0)
        {
            return 0.0f;
        }

        var key = (int)(fontSize * 10.0f);
        var count = stats.SizeCounts.GetValueOrDefault(key, 0);

        // A size used on one line in a hundred has a rarity near 0.99.
        return 1.0f - (count / (float)stats.TotalLines);
    }

    /// <summary>Computes font stats straight from items, before line grouping.</summary>
    public static FontStats CalculateFontStatsFromItems(IReadOnlyList<TextItem> items)
    {
        var sizeCounts = new Dictionary<int, int>();

        foreach (var item in items)
        {
            if (item.FontSize >= 9.0f)
            {
                var key = (int)(item.FontSize * 10.0f);
                sizeCounts[key] = sizeCounts.GetValueOrDefault(key, 0) + 1;
            }
        }

        return BuildStats(sizeCounts);
    }

    /// <summary>Computes font stats from grouped lines.</summary>
    public static FontStats CalculateFontStats(IReadOnlyList<TextLine> lines)
    {
        var sizeCounts = new Dictionary<int, int>();

        foreach (var line in lines)
        {
            // Counting once per line, from its first item, gives each line equal
            // weight, so small captions and footnotes cannot skew the base.
            if (line.Items.Count > 0 && line.Items[0].FontSize >= 9.0f)
            {
                var key = (int)(line.Items[0].FontSize * 10.0f);
                sizeCounts[key] = sizeCounts.GetValueOrDefault(key, 0) + 1;
            }
        }

        return BuildStats(sizeCounts);
    }

    /// <summary>Picks the most common size out of a distribution, smaller size winning ties.</summary>
    private static FontStats BuildStats(Dictionary<int, int> sizeCounts)
    {
        var totalLines = sizeCounts.Values.Sum();

        var mostCommonKey = int.MinValue;
        var bestCount = int.MinValue;
        foreach (var (size, count) in sizeCounts)
        {
            // Ties go to the smaller size, so output stays deterministic.
            if (count > bestCount || (count == bestCount && size < mostCommonKey))
            {
                bestCount = count;
                mostCommonKey = size;
            }
        }

        return new FontStats
        {
            MostCommonSize = sizeCounts.Count > 0 ? mostCommonKey / 10.0f : 12.0f,
            SizeCounts = sizeCounts,
            TotalLines = totalLines,
        };
    }

    /// <summary>
    /// The heading level for a bold-only line that missed the font-size
    /// threshold, common in academic papers where section headings are bold at
    /// body size. The result sits below the lowest font-size tier, or at H2 when
    /// there are no tiers — H1 is reserved for titles, which are usually larger.
    /// </summary>
    public static int BoldHeadingLevel(IReadOnlyList<float> headingTiers) =>
        Math.Clamp(headingTiers.Count + 1, 2, 6);

    /// <summary>
    /// True for a contents-style line carrying dot leaders ("Section Name .... 42").
    /// Such a line must never be joined with its neighbours into a paragraph.
    /// Both consecutive dots ("....") and spaced groups ("...   ...") count.
    /// </summary>
    public static bool HasDotLeaders(string text)
    {
        if (text.Contains("....", StringComparison.Ordinal))
        {
            return true;
        }

        // Two or more groups of three-plus dots is a leader run.
        var dotGroups = 0;
        var consecutiveDots = 0;
        foreach (var ch in text)
        {
            if (ch == '.')
            {
                consecutiveDots++;
            }
            else
            {
                if (consecutiveDots >= 3)
                {
                    dotGroups++;
                }

                consecutiveDots = 0;
            }
        }

        if (consecutiveDots >= 3)
        {
            dotGroups++;
        }

        return dotGroups >= 2;
    }

    /// <summary>
    /// True for a contents entry: a line ending in a page number preceded by a
    /// dot-leader group ("Measurement Lab worksheet ... 3").
    /// <see cref="HasDotLeaders"/> misses single-group leaders, but a trailing
    /// "&lt;dots&gt; &lt;number&gt;" is a strong signal on its own, and such lines
    /// must never be promoted to headings.
    /// </summary>
    public static bool IsTocEntryLine(string text)
    {
        var trimmed = text.TrimEnd();
        var digits = 0;
        while (digits < trimmed.Length && char.IsAsciiDigit(trimmed[trimmed.Length - 1 - digits]))
        {
            digits++;
        }

        if (digits is 0 or > 4)
        {
            return false;
        }

        var beforeNumber = trimmed[..(trimmed.Length - digits)].TrimEnd();
        var dots = 0;
        while (dots < beforeNumber.Length && beforeNumber[beforeNumber.Length - 1 - dots] == '.')
        {
            dots++;
        }

        return dots >= 3;
    }

    /// <summary>
    /// True for a heading that announces a contents page. Lines after it on the
    /// same page are contents entries — section titles that look exactly like
    /// headings but must not be promoted.
    /// </summary>
    public static bool IsTocMarkerHeading(string text)
    {
        var t = text.Trim().TrimEnd(':').Trim().ToLowerInvariant();
        return t is "contents" or "table of contents";
    }

    /// <summary>
    /// True for lines that look structurally like headings but are display-math
    /// fragments: an equation ending in its number ("S = kB ln W, (2)") or an
    /// equation lead-in ("Rearranging Equation (8) gives:").
    /// </summary>
    /// <remarks>
    /// Both carry an "(N)" equation reference, but a trailing "(N)" alone is not
    /// enough — real headings end with parenthesised numbers too ("Nicaea (325)",
    /// appendix numbering) — so the suffix form additionally needs math evidence:
    /// an "=" somewhere in the line, or a comma immediately before the number.
    /// Both appear in every display equation and in no name-plus-number heading. A
    /// bare trailing colon is not a fragment signal either, since real headings
    /// frequently end with one ("Procedure:", "Steps for Using the Microscope:").
    /// </remarks>
    public static bool IsHeadingFragment(string text)
    {
        var t = text.TrimEnd();

        // A lowercase-initial one- or two-word "heading" is a mid-sentence fragment
        // beside display math ("or inversely", "and therefore"); real headings that
        // short start uppercase. Measured as spurious headings on academic
        // documents (fire-pdf ENG-5029, opendataloader MHS).
        var words = t.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        if (words.Length <= 2)
        {
            foreach (var c in t)
            {
                if (char.IsLetter(c))
                {
                    if (char.IsLower(c))
                    {
                        return true;
                    }

                    break;
                }
            }
        }

        // Split on spaces from the right, matching the reference's rsplit(' ').
        var parts = t.Split(' ');
        var last = parts.Length > 0 ? parts[^1] : string.Empty;
        if (IsEquationNumber(last))
        {
            // Page-of-total running headers: "LIVSMEDELSVERKET PM 2 (10)".
            if (parts.Length >= 2
                && uint.TryParse(parts[^2], out var page)
                && uint.TryParse(last.TrimStart('(').TrimEnd(')'), out var total)
                && page <= total)
            {
                return true;
            }

            var punctBefore = parts.Length >= 2
                && (parts[^2].EndsWith(',') || parts[^2].EndsWith(':'));
            var hasMathOp = t.Any(c => c is '=' or '<' or '>' or '≤' or '≥' or '≪' or '≫' or '≈'
                or '≠' or '±' or '∑' or '∫' or '√' or '∝');
            if (punctBefore || hasMathOp)
            {
                return true;
            }
        }

        // A lead-in ends with a colon and references an equation number inline.
        return t.EndsWith(':') && words.Any(IsEquationNumber);
    }

    /// <summary>True for a parenthesised one- to three-digit equation number.</summary>
    private static bool IsEquationNumber(string s)
    {
        if (!s.StartsWith('(') || !s.EndsWith(')') || s.Length < 2)
        {
            return false;
        }

        var inner = s[1..^1];
        return inner.Length is > 0 and <= 3 && inner.All(char.IsAsciiDigit);
    }

    /// <summary>
    /// The Y-gap threshold that marks a paragraph break. Rather than a fixed
    /// multiple of the base size, which fails on double-spaced documents, this
    /// takes the document's median line spacing and multiplies that: a gap
    /// noticeably larger than typical is a paragraph break. When typical spacing
    /// cannot be measured it falls back to 1.8× the base size.
    /// </summary>
    public static float ComputeParagraphThreshold(IReadOnlyList<TextLine> lines, float baseSize)
    {
        var fallback = baseSize * 1.8f;

        var gaps = new List<float>();
        (uint Page, float Y)? prev = null;

        foreach (var line in lines)
        {
            if (prev is { } p && line.Page == p.Page)
            {
                var gap = p.Y - line.Y;

                // Only positive gaps within a reasonable range; huge ones come from
                // page headers and footers.
                if (gap > 0.0f && gap < baseSize * 10.0f)
                {
                    gaps.Add(gap);
                }
            }

            prev = (line.Page, line.Y);
        }

        if (gaps.Count < 5)
        {
            return fallback;
        }

        gaps.Sort(FloatTotalOrder.Instance);
        var median = gaps[gaps.Count / 2];
        var threshold = MathF.Max(median * 1.3f, baseSize * 1.5f);

        Log.Debug(Module, () =>
            $"paragraph_threshold: base_size={baseSize:F1} median_gap={median:F1} threshold={threshold:F1} " +
            $"({gaps.Count} gaps sampled)");

        return threshold;
    }

    /// <summary>
    /// Discovers the document's distinct heading font-size tiers, largest first,
    /// so tier 0 is H1, tier 1 is H2 and so on. Sizes within 0.5pt cluster into
    /// one tier, and the list caps at four.
    /// </summary>
    public static List<float> ComputeHeadingTiers(IReadOnlyList<TextLine> lines, float baseSize)
    {
        var headingSizes = new List<float>();

        foreach (var line in lines)
        {
            if (line.Items.Count == 0 || line.Items[0].FontSize / baseSize < 1.2f)
            {
                continue;
            }

            // Digit-only lines — page numbers, issue numbers — must not define a
            // tier: a large bold folio claims tier 0 and blocks the bold-size
            // fallback for the document's real same-size headings.
            var t = line.Text().Trim();
            if (t.Length > 0 && !t.Any(char.IsLetter))
            {
                continue;
            }

            headingSizes.Add(line.Items[0].FontSize);
        }

        headingSizes.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        var tiers = new List<float>();
        foreach (var size in headingSizes)
        {
            if (!tiers.Any(t => MathF.Abs(t - size) < 0.5f))
            {
                tiers.Add(size);
            }
        }

        // Books often set section headings barely above body size — 11pt bold over
        // 10pt text. When nothing clears the 1.2× gate, fall back to bold lines
        // modestly larger than body, so those documents still get an H1 instead of
        // every bold heading defaulting to H2.
        if (tiers.Count == 0)
        {
            var boldSizes = lines
                .Where(line =>
                {
                    var t = line.Text().Trim();
                    return t.Length > 0 && t.Any(char.IsLetter);
                })
                .Where(line => line.Items.Count > 0)
                .Select(line => line.Items[0])
                .Where(it => it.IsBold && it.FontSize / baseSize >= 1.05f)
                .Select(it => it.FontSize)
                .OrderByDescending(s => s, FloatTotalOrder.Instance)
                .ToList();

            foreach (var size in boldSizes)
            {
                if (!tiers.Any(t => MathF.Abs(t - size) < 0.5f))
                {
                    tiers.Add(size);
                }
            }
        }

        if (tiers.Count > 4)
        {
            tiers.RemoveRange(4, tiers.Count - 4);
        }

        return tiers;
    }

    /// <summary>
    /// Boldness judged by character mass, so a heading whose section-number prefix
    /// is unbold ("4. " plus a bold title) still counts as bold.
    /// </summary>
    public static bool LineIsMostlyBold(TextLine line)
    {
        var bold = 0;
        var total = 0;
        foreach (var it in line.Items)
        {
            var n = TextUtils.CharCount(it.Text.Trim());
            total += n;
            if (it.IsBold)
            {
                bold += n;
            }
        }

        return total > 0 && bold * 2 >= total;
    }

    /// <summary>
    /// Maps a font size to a heading level using the document's tiers — tier 0 to
    /// H1, tier 1 to H2 and so on — falling back to ratio thresholds when no tiers
    /// were discovered.
    /// </summary>
    public static int? DetectHeaderLevel(float fontSize, float baseSize, IReadOnlyList<float> headingTiers, bool isBold)
    {
        var ratio = fontSize / baseSize;

        // Below the 1.2× gate, down to 1.05×, a tier match is trusted only for bold
        // lines: sub-gate tiers come from the bold fallback, and honouring them for
        // non-bold text at the same size would promote captions.
        if (ratio is >= 1.05f and < 1.2f && isBold && headingTiers.Count > 0)
        {
            for (var i = 0; i < headingTiers.Count; i++)
            {
                if (MathF.Abs(fontSize - headingTiers[i]) < 0.5f)
                {
                    return i + 1;
                }
            }
        }

        if (ratio < 1.2f)
        {
            return null;
        }

        if (headingTiers.Count > 0)
        {
            for (var i = 0; i < headingTiers.Count; i++)
            {
                if (MathF.Abs(fontSize - headingTiers[i]) < 0.5f)
                {
                    return i + 1;
                }
            }

            // No tier matched, but the ratio is large: assign a level after the last
            // tier. A small ratio with no match is not a heading at all.
            return ratio >= 1.5f ? Math.Min(headingTiers.Count + 1, 4) : null;
        }

        // No tiers discovered, so fall back to ratio thresholds.
        if (ratio >= 2.0f)
        {
            return 1;
        }

        if (ratio >= 1.5f)
        {
            return 2;
        }

        return ratio >= 1.25f ? 3 : 4;
    }
}
