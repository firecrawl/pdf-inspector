// Ported from reference/src/markdown/preprocess.rs
using PdfInspector.Structure;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>
/// Line preprocessing: heading merging, drop-cap handling and repeated-line
/// removal.
/// </summary>
internal static class Preprocess
{
    /// <summary>
    /// A line is in the page margin when it is among the first or last few
    /// distinct Y positions on that page. This beats a percentage-based zone,
    /// which misses edge lines when a page is sparsely filled. Five accommodates
    /// multi-line headers and footers plus repeated form column headers — the
    /// five-row IRS form header, say — that sit just inside the page margin.
    /// </summary>
    private const int EdgeLineCount = 5;

    /// <summary>
    /// The heading level for a line, weighing structure-tree roles first and
    /// falling back to the font heuristic.
    /// </summary>
    private static int? EffectiveHeadingLevel(
        TextLine line,
        float baseSize,
        IReadOnlyList<float> headingTiers,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles)
    {
        if (structRoles is not null && structRoles.TryGetValue(line.Page, out var pageRoles))
        {
            foreach (var item in line.Items)
            {
                if (item.Mcid is not { } mcid || !pageRoles.TryGetValue(mcid, out var role))
                {
                    continue;
                }

                int? level = role.Role switch
                {
                    StructRole.Kind.H or StructRole.Kind.H1 => 1,
                    StructRole.Kind.H2 => 2,
                    StructRole.Kind.H3 => 3,
                    StructRole.Kind.H4 => 4,
                    StructRole.Kind.H5 => 5,
                    StructRole.Kind.H6 => 6,
                    _ => null,
                };

                if (level is not null)
                {
                    return level;
                }
            }
        }

        var font = line.Items.Count > 0 ? line.Items[0].FontSize : baseSize;
        return Analysis.DetectHeaderLevel(font, baseSize, headingTiers, Analysis.LineIsMostlyBold(line));
    }

    /// <summary>
    /// Merges consecutive heading lines at the same level into one line. A
    /// heading that wraps across text lines — "About Glenair, the Mission-Critical"
    /// then "Interconnect Company" — would otherwise become two separate markdown
    /// headings. Both font-size headings and structure-tree tagged headings count.
    /// </summary>
    public static List<TextLine> MergeHeadingLines(
        List<TextLine> lines,
        float baseSize,
        IReadOnlyList<float> headingTiers,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles)
    {
        if (lines.Count == 0)
        {
            return lines;
        }

        var result = new List<TextLine>(lines.Count);

        foreach (var line in lines)
        {
            var lineLevel = EffectiveHeadingLevel(line, baseSize, headingTiers, structRoles);
            var lineFont = line.Items.Count > 0 ? line.Items[0].FontSize : baseSize;

            var shouldMerge = false;
            if (result.Count > 0 && lineLevel is { } currLevel)
            {
                var prev = result[^1];
                var prevLevel = EffectiveHeadingLevel(prev, baseSize, headingTiers, structRoles);
                var samePage = prev.Page == line.Page;
                var sameLevel = prevLevel == currLevel;
                var yGap = prev.Y - line.Y;

                // A gap within about two font sizes is normal line-wrap spacing.
                var closeEnough = yGap > 0.0f && yGap < lineFont * 2.0f;

                // Real headings are short, so a long combined text means body-text
                // lines mis-tagged as headings.
                var prevWords = WordCount(prev.Text());
                var currWords = WordCount(line.Text());
                shouldMerge = samePage && sameLevel && closeEnough && prevWords + currWords <= 20;
            }

            // Bold headings at body font size never reach a tier, so a wrapped one
            // splits into two output headings ("…of wood pellets and cost" /
            // "structure in Japan"). Merge a fully bold line into the previous fully
            // bold line when it reads as a wrap continuation: it starts lowercase,
            // the Y gap is tiny, and the previous line has no terminal punctuation.
            // Deliberately narrow — bold list labels and bold sentences start with
            // markers or capitals and are unaffected.
            if (!shouldMerge && result.Count > 0)
            {
                var prev = result[^1];
                var prevTrim = prev.Text().TrimEnd();
                var currTrim = line.Text().Trim();
                var yGap = prev.Y - line.Y;

                // Both lines must be tier-less: a tiered or tagged bold heading
                // followed by bold body text must not absorb it.
                shouldMerge = lineLevel is null
                    && EffectiveHeadingLevel(prev, baseSize, headingTiers, structRoles) is null
                    && prev.Page == line.Page
                    && AllBold(prev)
                    && AllBold(line)
                    && yGap > 0.0f
                    && yGap < lineFont * 1.6f
                    && currTrim.Length > 0
                    && char.IsLower(currTrim[0])
                    && !(prevTrim.Length > 0 && prevTrim[^1] is '.' or ':' or ';' or '!' or '?')
                    && WordCount(prevTrim) + WordCount(currTrim) <= 20;
            }

            if (shouldMerge)
            {
                var prev = result[^1];

                // A space-bearing item keeps the merged text separated.
                if (line.Items.Count > 0)
                {
                    var spaceItem = line.Items[0].Clone();
                    spaceItem.Text = " " + spaceItem.Text.TrimStart();
                    prev.Items.Add(spaceItem);
                }

                for (var i = 1; i < line.Items.Count; i++)
                {
                    prev.Items.Add(line.Items[i]);
                }
            }
            else
            {
                result.Add(line);
            }
        }

        return result;
    }

    /// <summary>Counts whitespace-separated words.</summary>
    private static int WordCount(string text) =>
        text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;

    /// <summary>True when every item on the line is bold.</summary>
    private static bool AllBold(TextLine line) => line.Items.Count > 0 && line.Items.All(i => i.IsBold);

    /// <summary>
    /// Merges drop caps into the line they belong to. A drop cap is a single
    /// oversized letter opening a paragraph; PDF coordinate sorting can place it
    /// AFTER the line it belongs to.
    /// </summary>
    public static List<TextLine> MergeDropCaps(List<TextLine> lines, float baseSize)
    {
        var result = new List<TextLine>(lines.Count);

        foreach (var line in lines)
        {
            var trimmed = line.Text().Trim();

            // A drop cap is one character, or one plus a space, at least 2.5× the
            // base font, and uppercase.
            var isDropCap = trimmed.Length <= 2
                && (line.Items.Count > 0 ? line.Items[0].FontSize : 0.0f) >= baseSize * 2.5f
                && trimmed.Length > 0
                && char.IsUpper(trimmed[0]);

            if (!isDropCap)
            {
                result.Add(line);
                continue;
            }

            var dropChar = trimmed[0];

            // Find the first line that starts lowercase and opens a paragraph —
            // preceded by a heading or by a line that does not start lowercase.
            int? targetIdx = null;
            for (var idx = 0; idx < result.Count; idx++)
            {
                var prevLine = result[idx];
                if (prevLine.Page != line.Page)
                {
                    continue;
                }

                var prevTrimmed = prevLine.Text().Trim();
                if (prevTrimmed.Length == 0 || !char.IsLower(prevTrimmed[0]))
                {
                    continue;
                }

                bool isParaStart;
                if (idx == 0)
                {
                    isParaStart = true;
                }
                else
                {
                    var beforeTrimmed = result[idx - 1].Text().Trim();
                    isParaStart = beforeTrimmed.Length > 0 && !char.IsLower(beforeTrimmed[0]);
                }

                if (isParaStart)
                {
                    targetIdx = idx;
                    break;
                }
            }

            if (targetIdx is { } target && result[target].Items.Count > 0)
            {
                // Clone before rewriting: the item is shared with the caller's
                // item list, which table detection still indexes into.
                var firstItem = result[target].Items[0].Clone();
                firstItem.Text = dropChar + firstItem.Text.Trim();
                result[target].Items[0] = firstItem;
            }

            // The drop-cap line itself is dropped.
        }

        return result;
    }

    /// <summary>Trims and collapses internal whitespace runs, for comparison.</summary>
    private static string NormalizeWhitespace(string s) =>
        string.Join(' ', s.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));

    /// <summary>
    /// Normalises text for frequency comparison: collapses whitespace and strips
    /// leading and trailing digit runs, which are page numbers. "Chapter 3 — Page 5"
    /// and "Chapter 3 — Page 6" both normalise to "Chapter 3 — Page".
    /// </summary>
    private static string NormalizeForComparison(string s)
    {
        var ws = NormalizeWhitespace(s);
        var start = 0;
        while (start < ws.Length && char.IsAsciiDigit(ws[start]))
        {
            start++;
        }

        var trimmed = ws[start..].TrimStart();
        var end = trimmed.Length;
        while (end > 0 && char.IsAsciiDigit(trimmed[end - 1]))
        {
            end--;
        }

        return trimmed[..end].TrimEnd();
    }

    /// <summary>True when the line reads as a list item or heading, which must survive stripping.</summary>
    private static bool IsStructuralLine(string text)
    {
        var t = text.TrimStart();
        return t.StartsWith('#')
            || t.StartsWith("- ", StringComparison.Ordinal)
            || t.StartsWith("* ", StringComparison.Ordinal)
            || t.StartsWith("• ", StringComparison.Ordinal)
            || (t.Length > 0
                && char.IsAsciiDigit(t[0])
                && (t.Contains(". ", StringComparison.Ordinal) || t.Contains(") ", StringComparison.Ordinal)));
    }

    /// <summary>True when the line is one character repeated — "----------", "======".</summary>
    private static bool IsDecorativeSeparator(string text) =>
        text.Length > 0 && text.All(c => c == text[0]);

    /// <summary>
    /// Strips lines that repeat on many distinct pages — running headers and
    /// footers.
    /// </summary>
    /// <remarks>
    /// A line qualifies when its normalised text appears on at least
    /// max(3, 30% of pages) distinct pages, runs to 10 characters or more, does
    /// not look structural, sits consistently among the top or bottom distinct Y
    /// positions with low variance across pages, and is not a decorative
    /// separator. The variance check is what separates true headers and footers
    /// from table content that happens to land near a page margin.
    /// <para>
    /// Lines sharing a Y position on a page form a "Y-band". When any member of a
    /// band is stripped, its siblings go too — that handles split column headers
    /// whose individual fragments never meet the frequency threshold on their own.
    /// </para>
    /// <para>
    /// Page numbers are stripped from the text before comparison, so
    /// "Chapter 3 — Page 5" and "Chapter 3 — Page 6" compare equal. The first
    /// occurrence, on the lowest page number, is always kept so document titles
    /// and column headers still appear once.
    /// </para>
    /// </remarks>
    public static List<TextLine> StripRepeatedLines(List<TextLine> lines, uint pageCount)
    {
        if (lines.Count == 0 || pageCount < 3)
        {
            return lines;
        }

        var pageYRange = new Dictionary<uint, (float Lo, float Hi)>();
        foreach (var line in lines)
        {
            if (pageYRange.TryGetValue(line.Page, out var entry))
            {
                pageYRange[line.Page] = (MathF.Min(entry.Lo, line.Y), MathF.Max(entry.Hi, line.Y));
            }
            else
            {
                pageYRange[line.Page] = (line.Y, line.Y);
            }
        }

        // Sorted distinct Y values per page let a line's rank from the edge be checked.
        var pageSortedYs = new Dictionary<uint, List<float>>();
        foreach (var line in lines)
        {
            if (!pageSortedYs.TryGetValue(line.Page, out var ys))
            {
                ys = [];
                pageSortedYs[line.Page] = ys;
            }

            ys.Add(line.Y);
        }

        foreach (var ys in pageSortedYs.Values)
        {
            ys.Sort(FloatTotalOrder.Instance);
            var write = 0;
            for (var read = 0; read < ys.Count; read++)
            {
                if (write == 0 || ys[read] != ys[write - 1])
                {
                    ys[write++] = ys[read];
                }
            }

            if (write < ys.Count)
            {
                ys.RemoveRange(write, ys.Count - write);
            }
        }

        bool IsYAtEdge(float y, uint page)
        {
            if (!pageSortedYs.TryGetValue(page, out var ys))
            {
                return false;
            }

            if (ys.Count <= EdgeLineCount * 2)
            {
                // A page with very few lines has everything near an edge.
                return true;
            }

            var pos = ys.FindIndex(py => MathF.Abs(py - y) < 0.1f);
            return pos >= 0 && (pos < EdgeLineCount || pos >= ys.Count - EdgeLineCount);
        }

        // Average page span, for normalising Y variance.
        var avgSpan = 1.0f;
        if (pageYRange.Count > 0)
        {
            var total = pageYRange.Values.Sum(r => r.Hi - r.Lo);
            avgSpan = MathF.Max(total / pageYRange.Count, 1.0f);
        }

        // Y-bands group line indices by page and quantised Y, within about 0.1pt.
        var yBands = new Dictionary<(uint Page, int YBucket), List<int>>();
        for (var idx = 0; idx < lines.Count; idx++)
        {
            var key = (lines[idx].Page, (int)MathF.Round(lines[idx].Y * 10.0f, MidpointRounding.AwayFromZero));
            if (!yBands.TryGetValue(key, out var list))
            {
                list = [];
                yBands[key] = list;
            }

            list.Add(idx);
        }

        var freq = new Dictionary<string, HashSet<uint>>();
        var yPositions = new Dictionary<string, List<float>>();
        foreach (var line in lines)
        {
            if (!IsYAtEdge(line.Y, line.Page))
            {
                continue;
            }

            var normalized = NormalizeForComparison(line.Text());
            if (normalized.Length < 10 || IsDecorativeSeparator(normalized))
            {
                continue;
            }

            Add(freq, normalized, line.Page);
            AddPosition(yPositions, normalized, line.Y);
        }

        // Coalesced row text catches split column headers whose fragments never
        // meet the frequency threshold on their own but whose combined row does.
        var bandFreq = new Dictionary<string, HashSet<uint>>();
        var bandYPositions = new Dictionary<string, List<float>>();
        foreach (var ((page, _), indices) in yBands)
        {
            if (indices.Count < 2)
            {
                continue;
            }

            var bandY = lines[indices[0]].Y;
            if (!IsYAtEdge(bandY, page))
            {
                continue;
            }

            var normalized = NormalizeForComparison(CoalesceBand(lines, indices));
            if (normalized.Length < 10 || IsDecorativeSeparator(normalized))
            {
                continue;
            }

            Add(bandFreq, normalized, page);
            AddPosition(bandYPositions, normalized, bandY);
        }

        var threshold = Math.Max(3u, pageCount * 30 / 100);

        // Headers and footers land at the same position on every page while table
        // content drifts, so require a normalised standard deviation under 5% of
        // the average page span.
        bool HasConsistentY(string text, Dictionary<string, List<float>> positions)
        {
            if (!positions.TryGetValue(text, out var pos) || pos.Count < 2)
            {
                // A single occurrence is allowed.
                return true;
            }

            var n = pos.Count;
            var mean = pos.Sum() / n;
            var variance = pos.Sum(y => (y - mean) * (y - mean)) / n;
            return MathF.Sqrt(variance) / avgSpan < 0.05f;
        }

        var candidates = freq
            .Where(kv => kv.Value.Count >= threshold
                && !IsStructuralLine(kv.Key)
                && HasConsistentY(kv.Key, yPositions))
            .Select(kv => kv.Key)
            .ToHashSet();

        var bandCandidates = bandFreq
            .Where(kv => kv.Value.Count >= threshold
                && !IsStructuralLine(kv.Key)
                && HasConsistentY(kv.Key, bandYPositions))
            .Select(kv => kv.Key)
            .ToHashSet();

        if (candidates.Count == 0 && bandCandidates.Count == 0)
        {
            return lines;
        }

        var removalSet = new HashSet<int>();

        // Track which page first shows each candidate, so that occurrence survives.
        var firstPageIndividual = new Dictionary<string, uint>();
        for (var idx = 0; idx < lines.Count; idx++)
        {
            var line = lines[idx];
            if (!IsYAtEdge(line.Y, line.Page))
            {
                continue;
            }

            var normalized = NormalizeForComparison(line.Text());
            if (!candidates.Contains(normalized))
            {
                continue;
            }

            if (!firstPageIndividual.TryGetValue(normalized, out var first))
            {
                first = line.Page;
                firstPageIndividual[normalized] = first;
            }

            if (line.Page > first)
            {
                removalSet.Add(idx);
            }
        }

        // The band candidates need their own first-page pass, since band iteration
        // order is not page order.
        var firstPageBand = new Dictionary<string, uint>();
        foreach (var ((page, _), indices) in yBands)
        {
            if (indices.Count < 2 || !IsYAtEdge(lines[indices[0]].Y, page))
            {
                continue;
            }

            var normalized = NormalizeForComparison(CoalesceBand(lines, indices));
            if (!bandCandidates.Contains(normalized))
            {
                continue;
            }

            if (!firstPageBand.TryGetValue(normalized, out var first) || page < first)
            {
                firstPageBand[normalized] = page;
            }
        }

        foreach (var ((page, _), indices) in yBands)
        {
            if (indices.Count < 2 || !IsYAtEdge(lines[indices[0]].Y, page))
            {
                continue;
            }

            var normalized = NormalizeForComparison(CoalesceBand(lines, indices));
            if (!bandCandidates.Contains(normalized))
            {
                continue;
            }

            if (page > firstPageBand.GetValueOrDefault(normalized, 0u))
            {
                foreach (var idx in indices)
                {
                    removalSet.Add(idx);
                }
            }
        }

        // Y-band sibling propagation: once any member is removed, the whole band
        // goes, provided the band sits at an edge position.
        foreach (var ((page, _), indices) in yBands)
        {
            if (!IsYAtEdge(lines[indices[0]].Y, page))
            {
                continue;
            }

            if (indices.Any(removalSet.Contains))
            {
                foreach (var idx in indices)
                {
                    removalSet.Add(idx);
                }
            }
        }

        return removalSet.Count == 0
            ? lines
            : lines.Where((_, idx) => !removalSet.Contains(idx)).ToList();
    }

    /// <summary>Joins a Y-band's lines, in index order, into one comparison string.</summary>
    private static string CoalesceBand(List<TextLine> lines, List<int> indices) =>
        string.Join(' ', indices.OrderBy(i => i).Select(i => lines[i].Text()));

    /// <summary>Records that a normalised text appears on a page.</summary>
    private static void Add(Dictionary<string, HashSet<uint>> map, string key, uint page)
    {
        if (!map.TryGetValue(key, out var pages))
        {
            pages = [];
            map[key] = pages;
        }

        pages.Add(page);
    }

    /// <summary>Records a Y position for a normalised text.</summary>
    private static void AddPosition(Dictionary<string, List<float>> map, string key, float y)
    {
        if (!map.TryGetValue(key, out var positions))
        {
            positions = [];
            map[key] = positions;
        }

        positions.Add(y);
    }
}
