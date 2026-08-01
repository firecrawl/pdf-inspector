// Ported from reference/src/markdown/convert.rs
using System.Text;
using PdfInspector.Structure;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Markdown;

/// <summary>The core line-to-markdown conversion loop, with table and image interleaving.</summary>
internal static class Convert
{
    private const string Module = "markdown";

    /// <summary>Words whose presence at the end of a line marks it as wrapped prose.</summary>
    private static readonly string[] ContinuationWords =
    [
        "the", "a", "an", "and", "or", "of", "in", "to", "for", "with", "by", "on", "at",
        "from", "as", "is", "are", "was", "were", "be", "that", "this", "their", "its", "our",
        "your", "has", "have", "had", "not",
    ];

    /// <summary>Which kind of positioned block a page carries.</summary>
    private enum PositionedBlockKind
    {
        Table,
        Image,
    }

    /// <summary>A positioned block with the identity needed to order and dedupe it.</summary>
    private readonly record struct PositionedBlockRef(
        PositionedBlockKind Kind,
        int Index,
        PositionedMarkdown Block);

    /// <summary>
    /// Where a point falls in a chart page's logical stream: which vertical zone,
    /// then which column. Lower sorts earlier.
    /// </summary>
    private static (int Zone, int Column) ChartStreamPosition(
        float y,
        float x,
        bool claimedByChart,
        ChartProseOrder order)
    {
        var low = MathF.Min(order.Region.Y0, order.Region.Y1) - ChartRegions.ChartSeparatorPad;
        var high = MathF.Max(order.Region.Y0, order.Region.Y1) + ChartRegions.ChartSeparatorPad;
        var inChartZone = claimedByChart || (y >= low && y <= high);

        var zone = inChartZone ? 1 : y > high ? 0 : 2;
        var column = inChartZone || x < order.SplitX ? 0 : 1;
        return (zone, column);
    }

    /// <summary>True when a positioned block should be emitted before the given line.</summary>
    private static bool PositionedBlockPrecedesLine(PositionedMarkdown block, TextLine line)
    {
        if (block.ChartOrder is not { } order)
        {
            return block.Y > line.Y;
        }

        var lineX = line.Items.Count > 0 ? line.Items[0].X : 0.0f;
        var regionList = new[] { order.Region };
        var lineClaimedByChart = line.Items.Any(item => ChartRegions.ItemIsInChartRegion(item, regionList));

        var blockPosition = ChartStreamPosition(block.Y, block.X, false, order);
        var linePosition = ChartStreamPosition(line.Y, lineX, lineClaimedByChart, order);

        var cmp = ComparePositions(blockPosition, linePosition);
        return cmp < 0 || (cmp == 0 && block.Y > line.Y);
    }

    /// <summary>Lexicographic comparison of two stream positions.</summary>
    private static int ComparePositions((int Zone, int Column) a, (int Zone, int Column) b)
    {
        var cmp = a.Zone.CompareTo(b.Zone);
        return cmp != 0 ? cmp : a.Column.CompareTo(b.Column);
    }

    /// <summary>
    /// Orders two positioned blocks. On a chart page they sort by logical stream
    /// then physical position; on an ordinary page the legacy order stands —
    /// tables in detection order, then images in input order.
    /// </summary>
    private static int ComparePositionedBlocks(PositionedBlockRef a, PositionedBlockRef b)
    {
        if (a.Block.ChartOrder is { } aOrder && b.Block.ChartOrder is { } bOrder)
        {
            var aPosition = ChartStreamPosition(a.Block.Y, a.Block.X, false, aOrder);
            var bPosition = ChartStreamPosition(b.Block.Y, b.Block.X, false, bOrder);

            var cmp = ComparePositions(aPosition, bPosition);
            if (cmp != 0)
            {
                return cmp;
            }

            cmp = FloatTotalOrder.Instance.Compare(b.Block.Y, a.Block.Y);
            if (cmp != 0)
            {
                return cmp;
            }

            cmp = FloatTotalOrder.Instance.Compare(a.Block.X, b.Block.X);
            if (cmp != 0)
            {
                return cmp;
            }

            cmp = a.Kind.CompareTo(b.Kind);
            return cmp != 0 ? cmp : a.Index.CompareTo(b.Index);
        }

        var kindCmp = a.Kind.CompareTo(b.Kind);
        return kindCmp != 0 ? kindCmp : a.Index.CompareTo(b.Index);
    }

    /// <summary>Collects a page's tables and images into one ordered block list.</summary>
    private static List<PositionedBlockRef> PositionedBlocksForPage(
        uint page,
        IReadOnlyDictionary<uint, List<PositionedMarkdown>> pageTables,
        IReadOnlyDictionary<uint, List<PositionedMarkdown>> pageImages)
    {
        var blocks = new List<PositionedBlockRef>();

        if (pageTables.TryGetValue(page, out var tables))
        {
            for (var idx = 0; idx < tables.Count; idx++)
            {
                blocks.Add(new PositionedBlockRef(PositionedBlockKind.Table, idx, tables[idx]));
            }
        }

        if (pageImages.TryGetValue(page, out var images))
        {
            for (var idx = 0; idx < images.Count; idx++)
            {
                blocks.Add(new PositionedBlockRef(PositionedBlockKind.Image, idx, images[idx]));
            }
        }

        blocks.Sort(ComparePositionedBlocks);
        return blocks;
    }

    /// <summary>
    /// Finds structure-tree heading levels that are so widely tagged they clearly
    /// mark body text, and returns the levels to suppress. Some PDFs tag every
    /// numbered paragraph line as H2, producing hundreds of false headings; any
    /// level accounting for over 15% of tagged lines is treated as overused.
    /// </summary>
    private static HashSet<int> DetectOverusedStructHeadingLevels(
        IReadOnlyList<TextLine> lines,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles)
    {
        var overused = new HashSet<int>();
        if (structRoles is null)
        {
            return overused;
        }

        var levelCounts = new Dictionary<int, int>();
        var total = 0;

        foreach (var line in lines)
        {
            if (ResolveLineStructRole(line, structRoles) is not { } role)
            {
                continue;
            }

            total++;
            if (StructRoleHeadingLevel(role) is { } level)
            {
                levelCounts[level] = levelCounts.GetValueOrDefault(level, 0) + 1;
            }
        }

        if (total < 20)
        {
            return overused;
        }

        foreach (var (level, count) in levelCounts)
        {
            var ratio = count / (float)total;
            if (ratio > 0.15f)
            {
                Log.Debug(Module, () =>
                    $"struct heading H{level} overused: {count}/{total} lines ({ratio * 100.0f:F0}%), suppressing");
                overused.Add(level);
            }
        }

        return overused;
    }

    /// <summary>
    /// Finds "isolated" lines: short ones with a paragraph break both before and
    /// after. These are heading candidates even at body font size, as academic
    /// papers show with "Acknowledgements" or "B.3 Prompt Engineering".
    /// </summary>
    private static HashSet<int> FindIsolatedLines(
        IReadOnlyList<TextLine> lines,
        float baseSize,
        float paraThreshold)
    {
        var set = new HashSet<int>();

        for (var i = 0; i < lines.Count; i++)
        {
            var line = lines[i];
            var trimmed = line.Text().Trim();
            var words = trimmed.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
            if (words.Length is < 1 or > 6 || trimmed.Length <= 3)
            {
                continue;
            }

            var fontSize = line.Items.Count > 0 ? line.Items[0].FontSize : 0.0f;
            if (fontSize < baseSize * 0.95f)
            {
                continue;
            }

            if (Classify.IsListItem(trimmed) || Classify.IsCaptionLine(trimmed))
            {
                continue;
            }

            // Reject lines that read as wrapped paragraph text: they end with a
            // hyphen, a comma, or a preposition that carries into the next line.
            var lastChar = trimmed.Length > 0 ? trimmed[^1] : ' ';
            if (lastChar is '-' or ',' or ';')
            {
                continue;
            }

            if (ContinuationWords.Contains(words[^1].ToLowerInvariant()))
            {
                continue;
            }

            var breakBefore = i == 0
                || lines[i - 1].Page != line.Page
                || MathF.Abs(lines[i - 1].Y - line.Y) > paraThreshold;

            var breakAfter = i + 1 >= lines.Count
                || lines[i + 1].Page != line.Page
                || MathF.Abs(line.Y - lines[i + 1].Y) > paraThreshold;

            if (breakBefore && breakAfter)
            {
                set.Add(i);
            }
        }

        // Density guard: when too many of a page's lines look isolated, they are
        // paragraph lines in a multi-column layout, not headings. Real headings are
        // rare — at most about a fifth of a page's lines.
        var pageLineCounts = new Dictionary<uint, (int Total, int Isolated)>();
        for (var i = 0; i < lines.Count; i++)
        {
            pageLineCounts.TryGetValue(lines[i].Page, out var entry);
            pageLineCounts[lines[i].Page] = (entry.Total + 1, entry.Isolated + (set.Contains(i) ? 1 : 0));
        }

        foreach (var (page, counts) in pageLineCounts)
        {
            // The ratio only means something on a page dense enough for a
            // multi-column misfire. On a sparse page — a cover, a contents page with
            // a lone title — one isolated line is a quarter of the page and is
            // exactly what isolation exists to find.
            if (counts.Total >= 10 && counts.Isolated / (float)counts.Total > 0.25f)
            {
                set.RemoveWhere(i => lines[i].Page == page);
            }
        }

        return set;
    }

    /// <summary>
    /// True for a section-numbered heading prefix followed by a word — "9.5. ",
    /// "12.3.1. ". Two components are the minimum: a lone "1. " is an ordered list
    /// item, and this prefix bypasses the isolation checks entirely.
    /// </summary>
    internal static bool StartsWithSectionNumber(string text)
    {
        var rest = text.TrimStart();
        var groups = 0;

        while (true)
        {
            var digits = 0;
            while (digits < rest.Length && char.IsAsciiDigit(rest[digits]))
            {
                digits++;
            }

            if (digits is 0 or > 3)
            {
                break;
            }

            groups++;
            rest = rest[digits..];
            if (rest.StartsWith('.'))
            {
                rest = rest[1..];
            }
            else
            {
                break;
            }
        }

        if (groups < 2 || rest.Length == 0 || !char.IsWhiteSpace(rest[0]))
        {
            return false;
        }

        var afterSpace = rest.TrimStart();
        return afterSpace.Length > 0 && char.IsLetter(afterSpace[0]);
    }

    /// <summary>
    /// Merges a two- or three-line group of consecutive all-bold body-size lines
    /// into one line when the group is isolated and short enough to be a heading.
    /// Left split, the internal line gap breaks each line's isolation and neither
    /// classifies as a heading, so the whole group merges into the following body
    /// paragraph. Longer or wordier bold runs are wrapped bold paragraphs, left for
    /// <see cref="FindWrappedBoldParagraphLines"/> to suppress.
    /// </summary>
    private static List<TextLine> MergeWrappedBoldHeadingGroups(
        List<TextLine> lines,
        float baseSize,
        float paraThreshold)
    {
        var output = new List<TextLine>(lines.Count);
        var i = 0;

        while (i < lines.Count)
        {
            if (!IsBodySizeAllBoldLine(lines[i], baseSize))
            {
                output.Add(lines[i]);
                i++;
                continue;
            }

            var start = i;
            var end = i;
            var wordCount = WordCount(lines[i].Text());

            while (end + 1 < lines.Count
                && IsBodySizeAllBoldLine(lines[end + 1], baseSize)
                && IsWrappedSameStyleLine(lines[end], lines[end + 1], paraThreshold))
            {
                end++;
                wordCount += WordCount(lines[end].Text());
            }

            var lineCount = end - start + 1;

            // Column-local isolation: on an interleaved multi-column page the
            // neighbouring lines in the vector may belong to the other column, so
            // judge the break by x-overlapping lines only.
            var gx0 = float.PositiveInfinity;
            var gx1 = float.NegativeInfinity;
            for (var idx = start; idx <= end; idx++)
            {
                foreach (var item in lines[idx].Items)
                {
                    gx0 = MathF.Min(gx0, item.X);
                    gx1 = MathF.Max(gx1, item.X + item.Width);
                }
            }

            bool OverlapsX(TextLine l)
            {
                var lx0 = float.PositiveInfinity;
                var lx1 = float.NegativeInfinity;
                foreach (var item in l.Items)
                {
                    lx0 = MathF.Min(lx0, item.X);
                    lx1 = MathF.Max(lx1, item.X + item.Width);
                }

                return lx0 < gx1 && lx1 > gx0;
            }

            var page = lines[start].Page;
            var breakBefore = !lines.Any(l =>
                l.Page == page
                && l.Y > lines[start].Y
                && l.Y - lines[start].Y <= paraThreshold
                && OverlapsX(l));
            var breakAfter = !lines.Any(l =>
                l.Page == page
                && l.Y < lines[end].Y
                && lines[end].Y - l.Y <= paraThreshold
                && OverlapsX(l));

            var numbered = StartsWithSectionNumber(lines[start].Text());

            if (lineCount is >= 2 and <= 3 && wordCount <= 15 && ((breakBefore && breakAfter) || numbered))
            {
                var merged = new TextLine
                {
                    Items = [.. lines[start].Items],
                    Y = lines[start].Y,
                    Page = lines[start].Page,
                    AdaptiveThreshold = lines[start].AdaptiveThreshold,
                };

                for (var idx = start + 1; idx <= end; idx++)
                {
                    merged.Items.AddRange(lines[idx].Items);
                }

                output.Add(merged);
            }
            else
            {
                for (var idx = start; idx <= end; idx++)
                {
                    output.Add(lines[idx]);
                }
            }

            i = end + 1;
        }

        return output;
    }

    /// <summary>
    /// Finds body-size all-bold runs too long to be headings. Some academic PDFs
    /// set an all-bold abstract straight after the author block; a line-local bold
    /// heading heuristic sees each wrapped visual line as standalone once the first
    /// is misclassified, producing a stack of headings.
    /// </summary>
    private static HashSet<int> FindWrappedBoldParagraphLines(
        IReadOnlyList<TextLine> lines,
        float baseSize,
        float paraThreshold)
    {
        var set = new HashSet<int>();
        var i = 0;

        while (i < lines.Count)
        {
            if (!IsBodySizeAllBoldLine(lines[i], baseSize))
            {
                i++;
                continue;
            }

            var start = i;
            var end = i;
            var wordCount = WordCount(lines[i].Text());

            while (end + 1 < lines.Count
                && IsBodySizeAllBoldLine(lines[end + 1], baseSize)
                && IsWrappedSameStyleLine(lines[end], lines[end + 1], paraThreshold))
            {
                end++;
                wordCount += WordCount(lines[end].Text());
            }

            if (end - start + 1 >= 3 && wordCount > 20)
            {
                for (var idx = start; idx <= end; idx++)
                {
                    set.Add(idx);
                }
            }

            i = end + 1;
        }

        return set;
    }

    /// <summary>True when every item on the line is bold and sits at body size.</summary>
    private static bool IsBodySizeAllBoldLine(TextLine line, float baseSize)
    {
        if (line.Items.Count == 0)
        {
            return false;
        }

        var first = line.Items[0];
        return first.FontSize >= baseSize * 0.95f
            && first.FontSize < baseSize * 1.2f
            && line.Items.All(item => item.IsBold && MathF.Abs(item.FontSize - first.FontSize) < 0.5f);
    }

    /// <summary>True when the next line continues the previous one at wrap spacing and indent.</summary>
    private static bool IsWrappedSameStyleLine(TextLine prev, TextLine next, float paraThreshold)
    {
        if (prev.Page != next.Page)
        {
            return false;
        }

        var yGap = prev.Y - next.Y;
        if (yGap <= 0.0f || yGap > paraThreshold)
        {
            return false;
        }

        var prevX = prev.Items.Count > 0 ? prev.Items[0].X : 0.0f;
        var nextX = next.Items.Count > 0 ? next.Items[0].X : 0.0f;
        return MathF.Abs(prevX - nextX) <= 40.0f;
    }

    /// <summary>
    /// The dominant structure role for a line, from its items' MCIDs. Container
    /// roles are skipped — they carry no meaning for markdown generation.
    /// </summary>
    private static StructRole? ResolveLineStructRole(
        TextLine line,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>> structRoles)
    {
        if (!structRoles.TryGetValue(line.Page, out var pageRoles))
        {
            return null;
        }

        foreach (var item in line.Items)
        {
            if (item.Mcid is not { } mcid || !pageRoles.TryGetValue(mcid, out var role))
            {
                continue;
            }

            switch (role.Role)
            {
                case StructRole.Kind.Document:
                case StructRole.Kind.Part:
                case StructRole.Kind.Art:
                case StructRole.Kind.Sect:
                case StructRole.Kind.Div:
                case StructRole.Kind.NonStruct:
                case StructRole.Kind.Span:
                case StructRole.Kind.Private:
                    continue;

                default:
                    return role;
            }
        }

        return null;
    }

    /// <summary>The markdown heading level a structure role maps to, if any.</summary>
    private static int? StructRoleHeadingLevel(StructRole role) => role.Role switch
    {
        // A generic heading becomes H1.
        StructRole.Kind.H or StructRole.Kind.H1 => 1,
        StructRole.Kind.H2 => 2,
        StructRole.Kind.H3 => 3,
        StructRole.Kind.H4 => 4,
        StructRole.Kind.H5 => 5,
        StructRole.Kind.H6 => 6,
        _ => null,
    };

    /// <summary>
    /// Merges continuation tables that span page breaks. When consecutive pages
    /// each hold exactly one table with the same column count, and every page is
    /// table-only, they are one table: the later headers and separators are
    /// stripped and their data rows appended to the first page's table.
    /// </summary>
    public static void MergeContinuationTables(
        Dictionary<uint, List<PositionedMarkdown>> pageTables,
        IReadOnlySet<uint> tableOnlyPages)
    {
        var sortedPages = pageTables.Keys.OrderBy(p => p).ToList();
        if (sortedPages.Count < 2)
        {
            return;
        }

        var i = 0;
        while (i < sortedPages.Count)
        {
            var firstPage = sortedPages[i];

            if (!pageTables.TryGetValue(firstPage, out var firstTables)
                || firstTables.Count != 1
                || !tableOnlyPages.Contains(firstPage))
            {
                i++;
                continue;
            }

            var firstColCount = CountTableColumns(firstTables[0].Markdown);
            if (firstColCount == 0)
            {
                i++;
                continue;
            }

            var continuationPages = new List<uint>();
            var j = i + 1;
            while (j < sortedPages.Count)
            {
                var nextPage = sortedPages[j];
                var prevPage = continuationPages.Count == 0 ? firstPage : continuationPages[^1];
                if (nextPage != prevPage + 1 || !tableOnlyPages.Contains(nextPage))
                {
                    break;
                }

                if (!pageTables.TryGetValue(nextPage, out var nextTables) || nextTables.Count != 1)
                {
                    break;
                }

                if (CountTableColumns(nextTables[0].Markdown) != firstColCount)
                {
                    break;
                }

                continuationPages.Add(nextPage);
                j++;
            }

            if (continuationPages.Count == 0)
            {
                i++;
                continue;
            }

            var extraRows = new StringBuilder();
            foreach (var contPage in continuationPages)
            {
                if (!pageTables.TryGetValue(contPage, out var tables))
                {
                    continue;
                }

                // Skip the header and separator rows, keeping the data.
                var tableLines = tables[0].Markdown.Split('\n');
                var count = tableLines.Length > 0 && tableLines[^1].Length == 0
                    ? tableLines.Length - 1
                    : tableLines.Length;
                for (var lineIdx = 2; lineIdx < count; lineIdx++)
                {
                    extraRows.Append(tableLines[lineIdx]).Append('\n');
                }
            }

            firstTables[0].Markdown += extraRows.ToString();

            foreach (var contPage in continuationPages)
            {
                pageTables.Remove(contPage);
            }

            i = j;
        }
    }

    /// <summary>Counts a markdown table's columns from the pipes in its separator row.</summary>
    private static int CountTableColumns(string tableMd)
    {
        var lines = tableMd.Split('\n');
        if (lines.Length < 2 || !lines[1].Contains("---", StringComparison.Ordinal))
        {
            return 0;
        }

        var pipes = lines[1].Count(c => c == '|');
        return pipes >= 2 ? pipes - 1 : 0;
    }

    /// <summary>Mutable state threaded through the conversion loop.</summary>
    private sealed class OutputState
    {
        public StringBuilder Output { get; } = new();

        public bool InParagraph { get; set; }
    }

    /// <summary>Emits any of a page's tables and images that have not been placed yet.</summary>
    private static void FlushPageTablesAndImages(
        uint page,
        IReadOnlyDictionary<uint, List<PositionedBlockRef>> pageBlocks,
        HashSet<(uint Page, int Index)> insertedTables,
        HashSet<(uint Page, int Index)> insertedImages,
        OutputState state)
    {
        if (!pageBlocks.TryGetValue(page, out var blocks))
        {
            return;
        }

        foreach (var (kind, idx, block) in blocks)
        {
            var inserted = kind == PositionedBlockKind.Table ? insertedTables : insertedImages;
            if (inserted.Contains((page, idx)))
            {
                continue;
            }

            if (state.InParagraph)
            {
                state.Output.Append("\n\n");
                state.InParagraph = false;
            }

            state.Output.Append('\n').Append(block.Markdown).Append('\n');
            inserted.Add((page, idx));
        }
    }

    /// <summary>Counts whitespace-separated words.</summary>
    private static int WordCount(string text) => text.CountWords();

    /// <summary>
    /// Converts text lines to markdown, inserting tables and images at the right Y
    /// positions.
    /// </summary>
    public static string ToMarkdownFromLinesWithTablesAndImages(
        List<TextLine> lines,
        MarkdownOptions options,
        Dictionary<uint, List<PositionedMarkdown>> pageTables,
        Dictionary<uint, List<PositionedMarkdown>> pageImages,
        IReadOnlyDictionary<uint, List<ChartRegion>> pageChartRegions,
        IReadOnlySet<uint> bandSplitPages,
        IReadOnlyDictionary<uint, Dictionary<long, StructRole>>? structRoles)
    {
        if (lines.Count == 0 && pageTables.Count == 0 && pageImages.Count == 0)
        {
            return string.Empty;
        }

        var fontStats = Analysis.CalculateFontStats(lines);
        var baseSize = options.BaseFontSize ?? fontStats.MostCommonSize;

        lines = Preprocess.MergeDropCaps(lines, baseSize);
        var headingTiers = Analysis.ComputeHeadingTiers(lines, baseSize);
        lines = Preprocess.MergeHeadingLines(lines, baseSize, headingTiers, structRoles);

        // The typical line spacing drives paragraph-break detection. In a
        // double-spaced document — legal and government PDFs especially — normal
        // spacing can reach 2.3× the base size, which a fixed 1.8× threshold would
        // read as a paragraph break on every line.
        var paraThreshold = Analysis.ComputeParagraphThreshold(lines, baseSize);

        lines = MergeWrappedBoldHeadingGroups(lines, baseSize, paraThreshold);

        var isolatedLines = FindIsolatedLines(lines, baseSize, paraThreshold);
        var wrappedBoldParagraphLines = FindWrappedBoldParagraphLines(lines, baseSize, paraThreshold);

        var sequenceExcludedLines = new HashSet<int>(wrappedBoldParagraphLines);
        for (var lineIdx = 0; lineIdx < lines.Count; lineIdx++)
        {
            var line = lines[lineIdx];
            if (pageChartRegions.TryGetValue(line.Page, out var regions)
                && line.Items.Any(item => ChartRegions.ItemIsInChartRegion(item, regions)))
            {
                sequenceExcludedLines.Add(lineIdx);
            }
        }

        if (structRoles is not null)
        {
            for (var lineIdx = 0; lineIdx < lines.Count; lineIdx++)
            {
                if (ResolveLineStructRole(lines[lineIdx], structRoles) is { } role
                    && role.IsNonHeadingContent)
                {
                    sequenceExcludedLines.Add(lineIdx);
                }
            }
        }

        var sequenceHeadingLevels = Heading.ClassifyHeadingSequences(
            lines, baseSize, headingTiers, isolatedLines, sequenceExcludedLines);

        var overusedHeadingLevels = DetectOverusedStructHeadingLevels(lines, structRoles);

        var state = new OutputState();
        var output = state.Output;
        var currentPage = 0u;
        var prevY = float.MaxValue;
        var prevX = 0.0f;
        var inList = false;
        float? lastListX = null;
        var inCodeBlock = false;
        var prevHadDotLeaders = false;
        var paragraphInWrappedBoldRun = false;
        uint? tocSuppressPage = null;
        var insertedTables = new HashSet<(uint, int)>();
        var insertedImages = new HashSet<(uint, int)>();

        var allContentPages = pageTables.Keys.Concat(pageImages.Keys).Distinct().OrderBy(p => p).ToList();

        // The unified table/image order is built once per page. It is only a
        // meaningful sort on chart/prose pages; ordinary pages keep their legacy
        // table-then-image order without repeating the work for every line.
        var pageBlocks = allContentPages.ToDictionary(
            page => page,
            page => PositionedBlocksForPage(page, pageTables, pageImages));

        for (var lineIdx = 0; lineIdx < lines.Count; lineIdx++)
        {
            var line = lines[lineIdx];

            if (line.Page != currentPage)
            {
                if (currentPage > 0)
                {
                    if (inCodeBlock)
                    {
                        output.Append("```\n");
                        inCodeBlock = false;
                    }

                    FlushPageTablesAndImages(currentPage, pageBlocks, insertedTables, insertedImages, state);
                    if (state.InParagraph)
                    {
                        output.Append("\n\n");
                        state.InParagraph = false;
                    }

                    output.Append("\n\n");
                }

                // Flush intermediate pages — image-only or table-only — that carry no
                // text lines of their own.
                foreach (var p in allContentPages)
                {
                    if (p <= currentPage)
                    {
                        continue;
                    }

                    if (p >= line.Page)
                    {
                        break;
                    }

                    FlushPageTablesAndImages(p, pageBlocks, insertedTables, insertedImages, state);
                    if (state.InParagraph)
                    {
                        output.Append("\n\n");
                        state.InParagraph = false;
                    }

                    output.Append("\n\n");
                }

                currentPage = line.Page;
                prevY = float.MaxValue;
                prevX = 0.0f;
                paragraphInWrappedBoldRun = false;

                if (options.IncludePageNumbers)
                {
                    output.Append($"<!-- Page {currentPage} -->\n\n");
                }
            }

            // Tables and images go in through one ordered stream. Chart/prose pages
            // sort by zone, column and physical Y; ordinary pages keep the legacy
            // table-then-image input order.
            if (pageBlocks.TryGetValue(currentPage, out var blocks))
            {
                foreach (var (kind, idx, block) in blocks)
                {
                    var inserted = kind == PositionedBlockKind.Table ? insertedTables : insertedImages;
                    if (inserted.Contains((currentPage, idx))
                        || !PositionedBlockPrecedesLine(block, line))
                    {
                        continue;
                    }

                    if (state.InParagraph)
                    {
                        output.Append("\n\n");
                        state.InParagraph = false;
                        paragraphInWrappedBoldRun = false;
                    }

                    output.Append('\n').Append(block.Markdown).Append('\n');
                    inserted.Add((currentPage, idx));
                }
            }

            // A paragraph breaks on a large forward Y gap, or on a large backward
            // jump where newspaper columns are emitted sequentially on one page.
            var yGap = prevY - line.Y;
            var lineX = line.Items.Count > 0 ? line.Items[0].X : 0.0f;
            var isParaBreak = MathF.Abs(yGap) > paraThreshold;

            // A page with a band-split side-by-side layout also breaks when X jumps
            // at the same Y level, so interleaved left and right band lines do not
            // merge into one paragraph.
            var isBandSwitch = bandSplitPages.Contains(line.Page)
                && MathF.Abs(yGap) <= paraThreshold
                && MathF.Abs(prevX - lineX) > 50.0f
                && prevY < float.MaxValue;

            var lineAllBold = line.Items.Count > 0 && line.Items.All(item => item.IsBold);
            var lineInWrappedBoldRun = wrappedBoldParagraphLines.Contains(lineIdx);
            var isBoldToRegularBreak = state.InParagraph
                && paragraphInWrappedBoldRun
                && !lineInWrappedBoldRun
                && !lineAllBold
                && yGap > baseSize * 1.2f
                && yGap <= paraThreshold;

            if ((isParaBreak || isBandSwitch || isBoldToRegularBreak) && state.InParagraph)
            {
                output.Append("\n\n");
                state.InParagraph = false;
                paragraphInWrappedBoldRun = false;
            }

            // The list does not end on a paragraph break; the continuation check
            // below decides whether the list continues.
            prevY = line.Y;
            prevX = lineX;

            var trimmed = line
                .TextWithFormatting(options.DetectBold, options.DetectItalic, options.DetectUnderline)
                .Trim();
            var plainText = line.Text();
            var plainTrimmed = plainText.Trim();

            if (trimmed.Length == 0)
            {
                continue;
            }

            var structRole = structRoles is null ? null : ResolveLineStructRole(line, structRoles);

            // Code lines accumulate into a block, from a structure role or a font.
            var isCodeLine = structRole?.Role == StructRole.Kind.Code
                || (options.DetectCode && line.Items.Any(i => Classify.IsMonospaceFont(i.Font)));

            if (inCodeBlock && !isCodeLine)
            {
                output.Append("```\n");
                inCodeBlock = false;
            }

            // Captions and source citations get their own line and a paragraph break.
            if (structRole?.Role == StructRole.Kind.Caption || Classify.IsCaptionLine(plainTrimmed))
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append(trimmed).Append("\n\n");
                continue;
            }

            // Structure-tree headings win, then the font-size heuristic. Structure
            // roles ADD headings — same-size text tagged H2 — but never suppress one
            // the font heuristic would find, since some tagged PDFs mark obvious
            // headings as P or Span.
            var structHeading = structRole is null ? null : StructRoleHeadingLevel(structRole);
            if (structHeading is { } sh && overusedHeadingLevels.Contains(sh))
            {
                structHeading = null;
            }

            // Protect wrapped list items: inside a list, a visually continuing line
            // at the same indent and wrap spacing must not be reclassified as a
            // heading. PDFs often bold the lead phrase of a list item across several
            // wrap lines, and an all-bold middle line would otherwise split one item
            // into a heading plus stray body text. Gating on the paragraph threshold
            // keeps genuine section headings after a numbered paragraph detectable.
            var looksLikeListContinuation = inList
                && lastListX is { } continuationListX
                && line.Items.Count > 0
                && line.Items[0].X >= continuationListX - 5.0f
                && line.Items[0].X <= continuationListX + 50.0f
                && yGap >= 0.0f
                && yGap <= paraThreshold
                && !Classify.IsListItem(plainTrimmed);

            // A line tagged with an explicit non-heading content role must never be
            // promoted by the visual heuristic: a tagged list item, quote or code
            // line can look exactly like a heading — short and isolated.
            var nonHeadingRole = structRole?.IsNonHeadingContent ?? false;

            int? heuristicHeading = null;
            if (options.DetectHeaders
                && !nonHeadingRole
                && !isCodeLine
                && !looksLikeListContinuation
                && TextUtils.ByteLength(plainTrimmed) > 3
                && WordCount(plainTrimmed) <= 15
                && !Classify.StartsWithBulletMarker(plainTrimmed)
                && !Analysis.IsTocEntryLine(plainTrimmed)
                && !Analysis.IsHeadingFragment(plainTrimmed)
                && tocSuppressPage != line.Page)
            {
                var lineFontSize = line.Items.Count > 0 ? line.Items[0].FontSize : baseSize;

                heuristicHeading = Analysis.DetectHeaderLevel(
                        lineFontSize, baseSize, headingTiers, Analysis.LineIsMostlyBold(line))
                    ?? RarityHeadingLevel(
                        line, lineIdx, plainTrimmed, lineFontSize, baseSize, headingTiers,
                        fontStats, isolatedLines, wrappedBoldParagraphLines, state.InParagraph)
                    ?? (sequenceHeadingLevels.TryGetValue(lineIdx, out var seqLevel) ? seqLevel : null);
            }

            if ((structHeading ?? heuristicHeading) is { } level)
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                // Headings take plain text — bold and italic inside "#" is redundant —
                // but underline survives, since "<u>" carries meaning "#" does not.
                var headingText = options.DetectUnderline
                    ? line.TextWithFormatting(false, false, true)
                    : plainText;

                output.Append(new string('#', level)).Append(' ').Append(headingText.Trim()).Append("\n\n");

                if (Analysis.IsTocMarkerHeading(plainTrimmed))
                {
                    tocSuppressPage = line.Page;
                }

                inList = false;
                continue;
            }

            // A structure-tree list item. LBody is a continuation, not a new item, so
            // only LI counts. Some tagged PDFs use a flat style where every wrapped
            // line in an item gets its own MCID tagged under LI; inside a list, a
            // line with no visible bullet marker falls through to the continuation
            // logic below instead.
            if (structRole?.Role == StructRole.Kind.Li && !Classify.IsListItem(plainTrimmed) && !inList)
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append("- ").Append(trimmed).Append('\n');
                inList = true;
                lastListX = line.Items.Count > 0 ? line.Items[0].X : null;
                continue;
            }

            if (options.DetectLists && Classify.IsListItem(plainTrimmed))
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append(Classify.FormatListItem(trimmed)).Append('\n');
                inList = true;
                lastListX = line.Items.Count > 0 ? line.Items[0].X : null;
                continue;
            }

            if (inList)
            {
                // A continuation sits at or past the list text position, within a few
                // line heights, and is not itself a new list item.
                var isContinuation = lastListX is { } listX
                    && line.Items.Count > 0
                    && line.Items[0].X >= listX - 5.0f
                    && line.Items[0].X <= listX + 50.0f
                    && yGap < baseSize * 7.0f
                    && !Classify.IsListItem(plainTrimmed)
                    && !Analysis.HasDotLeaders(plainTrimmed);

                if (isContinuation)
                {
                    if (output.Length > 0 && output[^1] == '\n')
                    {
                        output.Length--;
                        output.Append(' ');
                    }

                    output.Append(trimmed).Append('\n');
                    continue;
                }

                inList = false;
                lastListX = null;
            }

            if (structRole?.Role == StructRole.Kind.BlockQuote)
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append("> ").Append(trimmed).Append('\n');
                continue;
            }

            if (isCodeLine)
            {
                if (state.InParagraph)
                {
                    output.Append("\n\n");
                    state.InParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                if (!inCodeBlock)
                {
                    output.Append("```\n");
                    inCodeBlock = true;
                }

                output.Append(plainTrimmed).Append('\n');
                continue;
            }

            // Regular text joins the paragraph with a space, or a newline when dot
            // leaders are involved on either side.
            var curDotLeaders = Analysis.HasDotLeaders(plainTrimmed);
            if (state.InParagraph)
            {
                output.Append(curDotLeaders || prevHadDotLeaders ? '\n' : ' ');
            }

            output.Append(trimmed);
            paragraphInWrappedBoldRun = state.InParagraph
                ? paragraphInWrappedBoldRun || lineInWrappedBoldRun
                : lineInWrappedBoldRun;
            state.InParagraph = true;
            prevHadDotLeaders = curDotLeaders;
        }

        if (inCodeBlock)
        {
            output.Append("```\n");
        }

        // Flush the current page and every later one, covering table-only pages
        // after the last text line and trailing image-only pages.
        FlushPageTablesAndImages(currentPage, pageBlocks, insertedTables, insertedImages, state);
        foreach (var p in allContentPages)
        {
            if (p > currentPage)
            {
                FlushPageTablesAndImages(p, pageBlocks, insertedTables, insertedImages, state);
            }
        }

        if (state.InParagraph)
        {
            output.Append('\n');
        }

        return Postprocess.CleanMarkdown(output.ToString(), options);
    }

    /// <summary>
    /// Rarity-based heading detection, after opendataloader's scoring with
    /// lookahead context: rarity weighs half, boldness and standalone placement the
    /// rest, and paragraph isolation adds its own boost.
    /// </summary>
    private static int? RarityHeadingLevel(
        TextLine line,
        int lineIdx,
        string plainTrimmed,
        float lineFontSize,
        float baseSize,
        IReadOnlyList<float> headingTiers,
        FontStats fontStats,
        IReadOnlySet<int> isolatedLines,
        IReadOnlySet<int> wrappedBoldParagraphLines,
        bool inParagraph)
    {
        // Only lines at or above body font size are considered.
        if (lineFontSize < baseSize * 0.95f)
        {
            return null;
        }

        var wordCount = WordCount(plainTrimmed);
        if (wordCount is < 1 or > 15)
        {
            return null;
        }

        if (wrappedBoldParagraphLines.Contains(lineIdx))
        {
            return null;
        }

        var rarity = Analysis.FontSizeRarity(lineFontSize, fontStats);
        var allBold = line.Items.Count > 0 && line.Items.All(i => i.IsBold);
        var standalone = !inParagraph;
        var isolated = isolatedLines.Contains(lineIdx);

        var score = (rarity * 0.5f)
            + (allBold ? 0.3f : 0.0f)
            + (standalone ? 0.2f : 0.0f)
            + (isolated ? 0.3f : 0.0f);

        // Standalone placement plus at least one strong signal is required. A line
        // that is neither bold nor isolated needs very high rarity, so ordinary body
        // text in a multi-column layout — where column switches break paragraph
        // continuity and minor size variation inflates rarity — stays body text.
        var hasStrongSignal = allBold || isolated || (rarity >= 0.97f && wordCount <= 8);

        // Single-word headings ("IMPLEMENTATION", "CONTENTS", "Replace") are common.
        // An all-bold single word qualifies when standalone — a break before, or the
        // page top — since headings hug their section's first paragraph and
        // requiring a break after as well missed most of them. A mixed bold lead-in
        // ("Note: ...") is excluded by the all-bold test.
        var enoughWords = wordCount >= 2 || (allBold && TextUtils.ByteLength(plainTrimmed) >= 4);
        var numberedBold = allBold && StartsWithSectionNumber(plainTrimmed);

        return numberedBold || (score >= 0.5f && standalone && enoughWords && hasStrongSignal)
            ? Analysis.BoldHeadingLevel(headingTiers)
            : null;
    }

    /// <summary>Converts text lines to markdown, with no tables or images to interleave.</summary>
    public static string ToMarkdownFromLines(List<TextLine> lines, MarkdownOptions options)
    {
        if (lines.Count == 0)
        {
            return string.Empty;
        }

        var fontStats = Analysis.CalculateFontStats(lines);
        var baseSize = options.BaseFontSize ?? fontStats.MostCommonSize;

        lines = Preprocess.MergeDropCaps(lines, baseSize);
        var headingTiers = Analysis.ComputeHeadingTiers(lines, baseSize);
        lines = Preprocess.MergeHeadingLines(lines, baseSize, headingTiers, null);
        var paraThreshold = Analysis.ComputeParagraphThreshold(lines, baseSize);

        var isolatedLines = FindIsolatedLines(lines, baseSize, paraThreshold);
        var wrappedBoldParagraphLines = FindWrappedBoldParagraphLines(lines, baseSize, paraThreshold);
        var sequenceHeadingLevels = Heading.ClassifyHeadingSequences(
            lines, baseSize, headingTiers, isolatedLines, wrappedBoldParagraphLines);

        var output = new StringBuilder();
        var currentPage = 0u;
        var prevY = float.MaxValue;
        var inList = false;
        var inParagraph = false;
        float? lastListX = null;
        var prevHadDotLeaders = false;
        var paragraphInWrappedBoldRun = false;
        uint? tocSuppressPage = null;

        for (var lineIdx = 0; lineIdx < lines.Count; lineIdx++)
        {
            var line = lines[lineIdx];

            if (line.Page != currentPage)
            {
                if (currentPage > 0)
                {
                    if (inParagraph)
                    {
                        output.Append("\n\n");
                        inParagraph = false;
                    }

                    output.Append("\n\n");
                }

                currentPage = line.Page;
                prevY = float.MaxValue;
                inList = false;
                lastListX = null;
                prevHadDotLeaders = false;
                paragraphInWrappedBoldRun = false;

                if (options.IncludePageNumbers)
                {
                    output.Append($"<!-- Page {currentPage} -->\n\n");
                }
            }

            var yGap = prevY - line.Y;
            var isParaBreak = MathF.Abs(yGap) > paraThreshold;
            var lineAllBold = line.Items.Count > 0 && line.Items.All(item => item.IsBold);
            var lineInWrappedBoldRun = wrappedBoldParagraphLines.Contains(lineIdx);
            var isBoldToRegularBreak = inParagraph
                && paragraphInWrappedBoldRun
                && !lineInWrappedBoldRun
                && !lineAllBold
                && yGap > baseSize * 1.2f
                && yGap <= paraThreshold;

            if ((isParaBreak || isBoldToRegularBreak) && inParagraph)
            {
                output.Append("\n\n");
                inParagraph = false;
                paragraphInWrappedBoldRun = false;
            }

            prevY = line.Y;

            var trimmed = line
                .TextWithFormatting(options.DetectBold, options.DetectItalic, options.DetectUnderline)
                .Trim();
            var plainText = line.Text();
            var plainTrimmed = plainText.Trim();

            if (trimmed.Length == 0)
            {
                continue;
            }

            if (Classify.IsCaptionLine(plainTrimmed))
            {
                if (inParagraph)
                {
                    output.Append("\n\n");
                    inParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append(trimmed).Append("\n\n");
                continue;
            }

            if (options.DetectHeaders
                && TextUtils.ByteLength(plainTrimmed) > 3
                && WordCount(plainTrimmed) <= 15
                && !Analysis.IsTocEntryLine(plainTrimmed)
                && !Analysis.IsHeadingFragment(plainTrimmed)
                && tocSuppressPage != line.Page
                && !(options.DetectCode && line.Items.Any(i => Classify.IsMonospaceFont(i.Font))))
            {
                var lineFontSize = line.Items.Count > 0 ? line.Items[0].FontSize : baseSize;

                var headerLevel = Analysis.DetectHeaderLevel(
                        lineFontSize, baseSize, headingTiers, Analysis.LineIsMostlyBold(line))
                    ?? PlainRarityHeadingLevel(
                        line, lineIdx, plainTrimmed, lineFontSize, baseSize, headingTiers,
                        fontStats, isolatedLines, wrappedBoldParagraphLines, inParagraph)
                    ?? (sequenceHeadingLevels.TryGetValue(lineIdx, out var seqLevel) ? seqLevel : null);

                if (headerLevel is { } level)
                {
                    if (inParagraph)
                    {
                        output.Append("\n\n");
                        inParagraph = false;
                        paragraphInWrappedBoldRun = false;
                    }

                    var headingText = options.DetectUnderline
                        ? line.TextWithFormatting(false, false, true)
                        : plainText;

                    output.Append(new string('#', level)).Append(' ').Append(headingText.Trim()).Append("\n\n");

                    if (Analysis.IsTocMarkerHeading(plainTrimmed))
                    {
                        tocSuppressPage = line.Page;
                    }

                    inList = false;
                    continue;
                }
            }

            if (options.DetectLists && Classify.IsListItem(plainTrimmed))
            {
                if (inParagraph)
                {
                    output.Append("\n\n");
                    inParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append(Classify.FormatListItem(trimmed)).Append('\n');
                inList = true;
                lastListX = line.Items.Count > 0 ? line.Items[0].X : null;
                continue;
            }

            if (inList)
            {
                var isContinuation = lastListX is { } listX
                    && line.Items.Count > 0
                    && line.Items[0].X >= listX - 5.0f
                    && line.Items[0].X <= listX + 50.0f
                    && yGap < baseSize * 7.0f
                    && !Classify.IsListItem(plainTrimmed)
                    && !Analysis.HasDotLeaders(plainTrimmed);

                if (isContinuation)
                {
                    if (output.Length > 0 && output[^1] == '\n')
                    {
                        output.Length--;
                        output.Append(' ');
                    }

                    output.Append(trimmed).Append('\n');
                    continue;
                }

                inList = false;
                lastListX = null;
            }

            if (options.DetectCode && line.Items.Any(i => Classify.IsMonospaceFont(i.Font)))
            {
                if (inParagraph)
                {
                    output.Append("\n\n");
                    inParagraph = false;
                    paragraphInWrappedBoldRun = false;
                }

                output.Append("```\n").Append(plainTrimmed).Append("\n```\n");
                continue;
            }

            var curDotLeaders = Analysis.HasDotLeaders(plainTrimmed);
            if (inParagraph)
            {
                output.Append(curDotLeaders || prevHadDotLeaders ? '\n' : ' ');
            }

            output.Append(trimmed);
            paragraphInWrappedBoldRun = inParagraph
                ? paragraphInWrappedBoldRun || lineInWrappedBoldRun
                : lineInWrappedBoldRun;
            inParagraph = true;
            prevHadDotLeaders = curDotLeaders;
        }

        if (inParagraph)
        {
            output.Append('\n');
        }

        return Postprocess.CleanMarkdown(output.ToString(), options);
    }

    /// <summary>
    /// The rarity scoring used by the plain line converter. It differs from the
    /// table-aware path: there is no strong-signal requirement, and a single-word
    /// heading must also be isolated.
    /// </summary>
    private static int? PlainRarityHeadingLevel(
        TextLine line,
        int lineIdx,
        string plainTrimmed,
        float lineFontSize,
        float baseSize,
        IReadOnlyList<float> headingTiers,
        FontStats fontStats,
        IReadOnlySet<int> isolatedLines,
        IReadOnlySet<int> wrappedBoldParagraphLines,
        bool inParagraph)
    {
        if (lineFontSize < baseSize * 0.95f)
        {
            return null;
        }

        var wordCount = WordCount(plainTrimmed);
        if (wordCount is < 1 or > 15 || wrappedBoldParagraphLines.Contains(lineIdx))
        {
            return null;
        }

        var rarity = Analysis.FontSizeRarity(lineFontSize, fontStats);
        var allBold = line.Items.Count > 0 && line.Items.All(i => i.IsBold);
        var standalone = !inParagraph;
        var isolated = isolatedLines.Contains(lineIdx);

        var score = (rarity * 0.5f)
            + (allBold ? 0.3f : 0.0f)
            + (standalone ? 0.2f : 0.0f)
            + (isolated ? 0.3f : 0.0f);

        var enoughWords = wordCount >= 2 || (allBold && isolated && TextUtils.ByteLength(plainTrimmed) >= 4);

        return score >= 0.5f && standalone && enoughWords ? Analysis.BoldHeadingLevel(headingTiers) : null;
    }
}
