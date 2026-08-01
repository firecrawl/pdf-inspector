// Ported from reference/src/markdown/heading.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>
/// Deterministic document-sequence heading classification. The line-local
/// classifier in <see cref="Convert"/> is deliberately conservative, which means
/// it misses headings whose font is smaller than the document body — sidebars
/// being the common case. This pass looks for repeated visual styles and
/// coherent numbering across the whole line sequence, and promotes a line only
/// when another heading-like line supplies independent support. A one-off bold
/// label does not become a heading merely because it is rare.
/// </summary>
internal static class Heading
{
    private const string Module = "markdown";
    private const float XBucketPoints = 24.0f;
    private const float BaselinePeerTolerance = 2.0f;
    private const float MinFontRatio = 0.72f;
    private const int MaxSequenceDensityPercent = 20;

    /// <summary>Words that cannot end a complete sidebar label.</summary>
    private static readonly string[] ContinuationWords =
    [
        "a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to",
        "with",
    ];

    /// <summary>A line's repeatable look: face, approximate indent, and emphasis.</summary>
    private readonly record struct VisualStyle(string Font, int XBucket, bool Bold);

    /// <summary>Which numbering scheme a heading prefix uses.</summary>
    private enum NumberingKind
    {
        Decimal,
        Roman,
    }

    /// <summary>A parsed numbering prefix: its scheme, its depth, and its parts.</summary>
    private readonly record struct Numbering(NumberingKind Kind, int Depth, List<uint> Parts);

    /// <summary>A line that could be promoted, with the evidence gathered about it.</summary>
    private sealed class Candidate
    {
        public required int LineIndex { get; init; }

        public required float FontSize { get; init; }

        public required VisualStyle Style { get; init; }

        public Numbering? Numbering { get; init; }
    }

    /// <summary>The line's font by character mass, smaller name winning ties.</summary>
    private static string? DominantFont(TextLine line)
    {
        var weights = new Dictionary<string, int>(StringComparer.Ordinal);
        foreach (var item in line.Items)
        {
            var weight = Math.Max(item.Text.Trim().Length, 1);
            weights[item.Font] = weights.GetValueOrDefault(item.Font, 0) + weight;
        }

        string? best = null;
        var bestWeight = int.MinValue;
        foreach (var (font, weight) in weights)
        {
            if (weight > bestWeight
                || (weight == bestWeight && best is not null && string.CompareOrdinal(font, best) < 0))
            {
                bestWeight = weight;
                best = font;
            }
        }

        return best;
    }

    /// <summary>
    /// The character-weighted font size of a line's visible title portion.
    /// Section numbers are sometimes emitted as a separate, smaller item, or even
    /// as superscript, so using the first item's size would reject the whole
    /// heading even where the title itself carries the document's heading size.
    /// Character weighting lets the longer title win while keeping the exact size
    /// for ordinary one-item lines.
    /// </summary>
    private static float? DominantFontSize(TextLine line)
    {
        var weights = new Dictionary<int, int>();
        foreach (var item in line.Items)
        {
            var bucket = (int)MathF.Round(item.FontSize * 10.0f, MidpointRounding.AwayFromZero);
            var weight = Math.Max(item.Text.Trim().Length, 1);
            weights[bucket] = weights.GetValueOrDefault(bucket, 0) + weight;
        }

        int? best = null;
        var bestWeight = int.MinValue;
        foreach (var (bucket, weight) in weights)
        {
            // Ties go to the larger size.
            if (weight > bestWeight || (weight == bestWeight && best is { } b && bucket > b))
            {
                bestWeight = weight;
                best = bucket;
            }
        }

        return best is { } found ? found / 10.0f : null;
    }

    /// <summary>The document's body face: the most character mass among unbold items.</summary>
    private static string? DocumentBodyFont(IReadOnlyList<TextLine> lines)
    {
        var weights = new Dictionary<string, int>(StringComparer.Ordinal);
        foreach (var line in lines)
        {
            foreach (var item in line.Items)
            {
                if (item.IsBold)
                {
                    continue;
                }

                weights[item.Font] = weights.GetValueOrDefault(item.Font, 0) + item.Text.Trim().Length;
            }
        }

        string? best = null;
        var bestWeight = int.MinValue;
        foreach (var (font, weight) in weights)
        {
            if (weight > bestWeight
                || (weight == bestWeight && best is not null && string.CompareOrdinal(font, best) < 0))
            {
                bestWeight = weight;
                best = font;
            }
        }

        return best;
    }

    /// <summary>The document's body indent bucket, taken from the unbold lines.</summary>
    private static int? DocumentBodyXBucket(IReadOnlyList<TextLine> lines)
    {
        var weights = new Dictionary<int, int>();
        foreach (var line in lines)
        {
            if (line.Items.Count == 0 || Analysis.LineIsMostlyBold(line))
            {
                continue;
            }

            var bucket = (int)MathF.Round(line.Items[0].X / XBucketPoints, MidpointRounding.AwayFromZero);
            weights[bucket] = weights.GetValueOrDefault(bucket, 0) + line.Text().Trim().Length;
        }

        int? best = null;
        var bestWeight = int.MinValue;
        foreach (var (bucket, weight) in weights)
        {
            // Ties go to the leftmost bucket.
            if (weight > bestWeight || (weight == bestWeight && best is { } b && bucket < b))
            {
                bestWeight = weight;
                best = bucket;
            }
        }

        return best;
    }

    /// <summary>The line's visual style, or null when it holds no items.</summary>
    private static VisualStyle? VisualStyleOf(TextLine line)
    {
        if (line.Items.Count == 0 || DominantFont(line) is not { } font)
        {
            return null;
        }

        return new VisualStyle(
            font,
            (int)MathF.Round(line.Items[0].X / XBucketPoints, MidpointRounding.AwayFromZero),
            Analysis.LineIsMostlyBold(line));
    }

    /// <summary>The value of an uppercase roman numeral, or null when the token is not one.</summary>
    private static uint? RomanValue(string token)
    {
        if (token.Length == 0 || TextUtils.ByteLength(token) > 8)
        {
            return null;
        }

        var values = new int[token.Length];
        for (var i = 0; i < token.Length; i++)
        {
            values[i] = token[i] switch
            {
                'I' => 1,
                'V' => 5,
                'X' => 10,
                'L' => 50,
                'C' => 100,
                _ => 0,
            };

            if (values[i] == 0)
            {
                return null;
            }
        }

        var total = 0;
        for (var i = 0; i < values.Length; i++)
        {
            total += i + 1 < values.Length && values[i] < values[i + 1] ? -values[i] : values[i];
        }

        return total > 0 ? (uint)total : null;
    }

    /// <summary>Parses a leading section number such as "3.", "4.2)" or "IV.".</summary>
    private static Numbering? ParseNumbering(string text)
    {
        var first = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).FirstOrDefault();
        if (first is null || first.Length == 0 || first[^1] is not ('.' or ')' or ':'))
        {
            return null;
        }

        var token = first.TrimEnd('.', ')', ':');
        if (token.Length == 0)
        {
            return null;
        }

        var decimalParts = new List<uint>();
        var decimalOk = true;
        foreach (var part in token.Split('.'))
        {
            if (part.Length == 0 || TextUtils.ByteLength(part) > 3 || !part.All(char.IsAsciiDigit) || !uint.TryParse(part, out var value))
            {
                decimalOk = false;
                break;
            }

            decimalParts.Add(value);
        }

        if (decimalOk && decimalParts.Count > 0)
        {
            return new Numbering(NumberingKind.Decimal, decimalParts.Count, decimalParts);
        }

        return RomanValue(token) is { } roman ? new Numbering(NumberingKind.Roman, 1, [roman]) : null;
    }

    /// <summary>
    /// True when a word after the first also carries dotted decimal numbering.
    /// Two numbered tokens on one line is a data row, not a section heading.
    /// </summary>
    private static bool HasAdditionalDecimalNumbering(string text)
    {
        foreach (var word in text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Skip(1))
        {
            // Strip surrounding punctuation, keeping only the digits and dots.
            var start = 0;
            while (start < word.Length && !char.IsAsciiDigit(word[start]) && word[start] != '.')
            {
                start++;
            }

            var end = word.Length;
            while (end > start && !char.IsAsciiDigit(word[end - 1]) && word[end - 1] != '.')
            {
                end--;
            }

            var parts = word[start..end].Split('.').Where(p => p.Length > 0).ToList();
            if (parts.Count >= 2 && parts.All(p => p.All(char.IsAsciiDigit)))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>True when one numbering is a strict prefix of the other, so they nest.</summary>
    private static bool NumberingFormsHierarchy(List<uint> left, List<uint> right) =>
        left.Count != right.Count
        && (StartsWith(left, right) || StartsWith(right, left));

    /// <summary>True when <paramref name="whole"/> opens with <paramref name="prefix"/>.</summary>
    private static bool StartsWith(List<uint> whole, List<uint> prefix)
    {
        if (prefix.Count > whole.Count)
        {
            return false;
        }

        for (var i = 0; i < prefix.Count; i++)
        {
            if (whole[i] != prefix[i])
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>
    /// A compact parent/child numbering run is more likely an ordered list than a
    /// document section hierarchy. Genuine section headings normally have body
    /// content between levels, so requiring at least two intervening lines keeps
    /// nested "1." / "1.1." list items in the list formatter.
    /// </summary>
    private static bool NumberingHasSectionSeparation(Candidate left, Candidate right, IReadOnlyList<TextLine> lines) =>
        lines[left.LineIndex].Page != lines[right.LineIndex].Page
        || Math.Abs(left.LineIndex - right.LineIndex) >= 3;

    /// <summary>
    /// True when the line has a same-baseline neighbour at a displaced x, or an
    /// internal gap that wide. Fixed-size sidebar labels need stronger evidence
    /// than typography alone: table headers and parallel-column fragments repeat
    /// the same small bold font at a displaced x, and their peer text may survive
    /// as a separate line or already be grouped into the same line.
    /// </summary>
    private static bool HasDisplacedBaselinePeer(IReadOnlyList<TextLine> lines, int lineIdx)
    {
        var line = lines[lineIdx];
        if (line.Items.Count == 0)
        {
            return false;
        }

        var x = line.Items[0].X;

        var items = line.Items.OrderBy(i => i.X, Text.FloatTotalOrder.Instance).ToList();
        for (var i = 0; i + 1 < items.Count; i++)
        {
            var leftEdge = items[i].X + MathF.Max(items[i].Width, 0.0f);
            if (items[i + 1].X - leftEdge >= XBucketPoints)
            {
                return true;
            }
        }

        for (var otherIdx = 0; otherIdx < lines.Count; otherIdx++)
        {
            var other = lines[otherIdx];
            if (otherIdx != lineIdx
                && other.Page == line.Page
                && MathF.Abs(other.Y - line.Y) <= BaselinePeerTolerance
                && other.Items.Count > 0
                && MathF.Abs(other.Items[0].X - x) >= XBucketPoints)
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>True when the text stands alone as a label rather than wrapping or navigating.</summary>
    private static bool CompleteSidebarLabel(string text)
    {
        var trimmed = text.Trim();
        if (trimmed.EndsWith('-'))
        {
            return false;
        }

        var words = trimmed.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        var lastWord = words.Length > 0 ? words[^1].ToLowerInvariant() : string.Empty;
        if (ContinuationWords.Contains(lastWord))
        {
            return false;
        }

        // Margin references such as "G 02" are navigation codes, not headings.
        return !(words.Length == 2
            && words[0].Length == 1
            && words[0].All(char.IsLetter)
            && words[1].All(char.IsAsciiDigit));
    }

    /// <summary>True when the text has the shape of a title rather than prose or a caption.</summary>
    private static bool TitleLike(string text, bool numbered, bool bold)
    {
        var trimmed = text.Trim();
        var wordCount = trimmed.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;
        if (wordCount is < 1 or > 12
            || TextUtils.ByteLength(trimmed) is < 4 or > 140
            || !trimmed.Any(char.IsLetter)
            || trimmed[^1] is '.' or ',' or ';')
        {
            return false;
        }

        if (Classify.StartsWithBulletMarker(trimmed)
            || Classify.IsCaptionLine(trimmed)
            || Analysis.IsTocEntryLine(trimmed)
            || Analysis.IsHeadingFragment(trimmed))
        {
            return false;
        }

        // A visible numbered prefix is let through even though the generic list
        // recogniser also matches it; the sequence checks below are what separate a
        // section run from an ordinary ordered list.
        if (!numbered && Classify.IsListItem(trimmed))
        {
            return false;
        }

        var alphaWords = trimmed
            .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
            .Where(word => word.Any(char.IsLetter))
            .ToList();
        var capitalized = alphaWords.Count(word =>
        {
            foreach (var c in word)
            {
                if (char.IsLetter(c))
                {
                    return char.IsUpper(c);
                }
            }

            return false;
        });

        return numbered || bold || capitalized * 2 >= Math.Max(alphaWords.Count, 1);
    }

    /// <summary>The heading level a promoted candidate takes.</summary>
    private static int SequenceLevel(Candidate candidate, float baseSize, IReadOnlyList<float> headingTiers)
    {
        if (candidate.Numbering is { } numbering)
        {
            return Math.Clamp(numbering.Depth, 1, 6);
        }

        return Analysis.DetectHeaderLevel(candidate.FontSize, baseSize, headingTiers, candidate.Style.Bold)
            ?? Analysis.BoldHeadingLevel(headingTiers);
    }

    /// <summary>
    /// Returns the heading levels supported by a repeated visual or numbering
    /// sequence, keyed by line index.
    /// </summary>
    /// <param name="excludedLines">
    /// Wrapped bold paragraphs, chart labels, and lines with an explicit
    /// non-heading structure role. These cannot support another line's candidacy,
    /// which is what stops table headers and list labels manufacturing a false
    /// sequence.
    /// </param>
    public static Dictionary<int, int> ClassifyHeadingSequences(
        IReadOnlyList<TextLine> lines,
        float baseSize,
        IReadOnlyList<float> headingTiers,
        IReadOnlySet<int> isolatedLines,
        IReadOnlySet<int> excludedLines)
    {
        var bodyFont = DocumentBodyFont(lines);
        var bodyXBucket = DocumentBodyXBucket(lines);
        var candidates = new List<Candidate>();

        for (var lineIdx = 0; lineIdx < lines.Count; lineIdx++)
        {
            var line = lines[lineIdx];
            if (excludedLines.Contains(lineIdx))
            {
                var excluded = lineIdx;
                Log.Trace(Module, () => $"heading sequence excludes line {excluded}: {line.Text()}");
                continue;
            }

            if (line.Items.Count == 0 || DominantFontSize(line) is not { } fontSize)
            {
                continue;
            }

            if (fontSize < baseSize * MinFontRatio)
            {
                continue;
            }

            var text = line.Text().Trim();
            var numbering = ParseNumbering(text);
            if (numbering is not null && HasAdditionalDecimalNumbering(text))
            {
                continue;
            }

            if (VisualStyleOf(line) is not { } style || !TitleLike(text, numbering is not null, style.Bold))
            {
                continue;
            }

            var candidate = new Candidate
            {
                LineIndex = lineIdx,
                FontSize = fontSize,
                Style = style,
                Numbering = numbering,
            };

            var idx = lineIdx;
            Log.Trace(Module, () =>
                $"heading sequence candidate {idx}: page={line.Page} style={style} size={fontSize:F1} " +
                $"isolated={isolatedLines.Contains(idx)} text={text}");
            candidates.Add(candidate);
        }

        Log.Debug(Module, () =>
            $"heading sequence: {lines.Count} lines, {candidates.Count} candidates, {excludedLines.Count} excluded");

        var decisions = new Dictionary<int, int>();
        var eligibleLineCount = lines
            .Where((line, idx) => !excludedLines.Contains(idx) && line.Items.Count > 0)
            .Count();
        var sparseCandidatePopulation =
            candidates.Count * 100 <= eligibleLineCount * MaxSequenceDensityPercent;

        // Repeated visual styles share a font, an emphasis and an approximate
        // indent. Repetition alone is weak evidence, because captions, author lists
        // and table rows repeat too, so promote only a genuinely displaced smaller
        // sidebar style, or a style containing a coherent numbered run.
        var visualGroups = new Dictionary<VisualStyle, List<Candidate>>();
        foreach (var candidate in candidates)
        {
            if (!visualGroups.TryGetValue(candidate.Style, out var group))
            {
                group = [];
                visualGroups[candidate.Style] = group;
            }

            group.Add(candidate);
        }

        var maxSequenceLines = Math.Max(eligibleLineCount * MaxSequenceDensityPercent / 100, 4);

        foreach (var group in visualGroups.Values)
        {
            if (group.Count < 2 || group.Count > maxSequenceLines)
            {
                continue;
            }

            var style = group[0].Style;
            var distinctBoldFace = style.Bold && (bodyFont is null || style.Font != bodyFont);

            var supportedNumbered = group
                .Where(c => c.Numbering is not null
                    && (c.FontSize >= baseSize * 1.05f
                        || (c.Style.Bold && (bodyFont is null || c.Style.Font != bodyFont))))
                .ToList();

            var hierarchicalLines = new HashSet<int>();
            for (var idx = 0; idx < supportedNumbered.Count; idx++)
            {
                var left = supportedNumbered[idx];
                var leftNumbering = left.Numbering!.Value;
                for (var j = idx + 1; j < supportedNumbered.Count; j++)
                {
                    var right = supportedNumbered[j];
                    var rightNumbering = right.Numbering!.Value;
                    if (leftNumbering.Kind == rightNumbering.Kind
                        && NumberingFormsHierarchy(leftNumbering.Parts, rightNumbering.Parts)
                        && NumberingHasSectionSeparation(left, right, lines))
                    {
                        hierarchicalLines.Add(left.LineIndex);
                        hierarchicalLines.Add(right.LineIndex);
                    }
                }
            }

            var sizeMin = float.PositiveInfinity;
            var sizeMax = float.NegativeInfinity;
            foreach (var c in group)
            {
                sizeMin = MathF.Min(sizeMin, c.FontSize);
                sizeMax = MathF.Max(sizeMax, c.FontSize);
            }

            var distinctSidebarLabels = group
                .Select(c => lines[c.LineIndex].Text().Trim().ToLowerInvariant())
                .ToHashSet(StringComparer.Ordinal);

            var fixedSizeSidebarEvidence = sizeMax - sizeMin < 0.4f
                && distinctSidebarLabels.Count >= 2
                && group.All(c =>
                    CompleteSidebarLabel(lines[c.LineIndex].Text())
                    && !HasDisplacedBaselinePeer(lines, c.LineIndex));

            var displacedSidebar = distinctBoldFace
                && sparseCandidatePopulation
                && (sizeMax - sizeMin >= 0.4f || fixedSizeSidebarEvidence)
                && group.All(c => lines[c.LineIndex].Page == lines[group[0].LineIndex].Page)
                && AllSpacedApart(group)
                && bodyXBucket is { } bodyX && Math.Abs(style.XBucket - bodyX) >= 4
                && group.All(c => c.FontSize < baseSize * 0.95f);

            if (hierarchicalLines.Count == 0 && !displacedSidebar)
            {
                continue;
            }

            foreach (var candidate in group)
            {
                var viaHierarchy = hierarchicalLines.Contains(candidate.LineIndex);
                if (!displacedSidebar && !viaHierarchy)
                {
                    continue;
                }

                var evidence = viaHierarchy ? "numbering hierarchy" : "sidebar style";
                Log.Debug(Module, () =>
                    $"heading sequence promotes line {candidate.LineIndex} via {evidence} " +
                    $"{lines[candidate.LineIndex].Text()}");
                decisions[candidate.LineIndex] = SequenceLevel(candidate, baseSize, headingTiers);
            }
        }

        return decisions;
    }

    /// <summary>True when consecutive group members sit at least four lines apart.</summary>
    private static bool AllSpacedApart(List<Candidate> group)
    {
        for (var i = 0; i + 1 < group.Count; i++)
        {
            if (Math.Max(group[i + 1].LineIndex - group[i].LineIndex, 0) < 4)
            {
                return false;
            }
        }

        return true;
    }
}
