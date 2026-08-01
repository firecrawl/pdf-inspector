// Ported from reference/src/extractor/underline.rs
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>A stroked line segment retained for underline detection, with its device-space width.</summary>
internal sealed class UnderlineLine
{
    public float X1;
    public float Y1;
    public float X2;
    public float Y2;
    public float StrokeWidth;
    public uint Page;
}

/// <summary>
/// Geometric underline and strikeout detection.
///
/// PDFs have no underline font flag — underlines are drawn as separate
/// graphics: stroked horizontal lines or thin filled rectangles. This pass
/// correlates those graphics with text items after extraction: an item is
/// underlined when a horizontal rule sits just below its baseline and covers
/// most of its horizontal extent.
///
/// Repeated same-span rules are treated as table or form rulings rather than
/// underlines, which avoids marking every cell in a ruled table.
/// </summary>
internal static class Underline
{
    /// <summary>
    /// Maximum thickness for a line or filled rect to count as an underline rule
    /// rather than a border or decorative band.
    /// </summary>
    private const float MaxRuleThickness = 2.0f;

    /// <summary>Fraction of the item's width the rule must cover horizontally.</summary>
    private const float MinXOverlap = 0.6f;

    /// <summary>Same-span rules repeated at this many levels are usually rulings.</summary>
    private const int MinRepeatedRuleLevels = 3;

    /// <summary>Vertical tolerance for treating two rules as being on the same row edge.</summary>
    private const float RuleYDedupEps = 2.0f;

    private const float RuleSpanOverlapRatio = 0.8f;
    private const float RuleSpanWidthRatio = 1.5f;

    /// <summary>Several separated segments on one row are per-column table separators.</summary>
    private const int MinSegmentedRowRules = 3;
    private const int MinSegmentedRowGaps = 2;
    private const float SegmentedRowGapMin = 12.0f;

    /// <summary>
    /// A single rule under several widely separated items is a table
    /// header/body separator, not a sentence underline.
    /// </summary>
    private const int MinTabularRuleItems = 3;
    private const int MinTabularRuleGaps = 2;
    private const float TabularRuleGapEm = 2.0f;

    /// <summary>A horizontal rule candidate in page coordinates (y up).</summary>
    private sealed class Rule
    {
        public float X1;
        public float X2;
        public float Y;

        public float Width => X2 - X1;
    }

    private static List<Rule> RulesFromGraphics(
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<UnderlineLine> lines,
        uint page)
    {
        var rules = new List<Rule>();

        foreach (var l in lines)
        {
            if (l.Page != page)
            {
                continue;
            }

            // A horizontal stroked line, tolerating slight skew.
            if (l.StrokeWidth <= MaxRuleThickness && MathF.Abs(l.Y1 - l.Y2) <= MaxRuleThickness)
            {
                var (x1, x2) = l.X1 <= l.X2 ? (l.X1, l.X2) : (l.X2, l.X1);
                if (x2 - x1 > 1.0f)
                {
                    rules.Add(new Rule { X1 = x1, X2 = x2, Y = (l.Y1 + l.Y2) / 2.0f });
                }
            }
        }

        foreach (var r in rects)
        {
            if (r.Page != page)
            {
                continue;
            }

            // Extents are normalised first: `re` operands pass through the
            // current transform, so width and height can be negative under
            // flipped axes. Without this, negative-width rules are missed and
            // negative-height bands slip past the thickness check.
            var (x1, x2) = r.Width >= 0.0f ? (r.X, r.X + r.Width) : (r.X + r.Width, r.X);

            if (MathF.Abs(r.Height) <= MaxRuleThickness && x2 - x1 > 1.0f)
            {
                rules.Add(new Rule { X1 = x1, X2 = x2, Y = r.Y + (r.Height / 2.0f) });
            }
        }

        return rules;
    }

    private static List<Rule> DiscardRepeatedRulingRules(
        List<Rule> rules,
        IReadOnlyList<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<UnderlineLine> lines,
        uint page)
    {
        if (rules.Count < MinRepeatedRuleLevels)
        {
            return rules;
        }

        // A rule snugly owned by one text line is an underline even when
        // span-similar rules repeat down the page: documents that underline many
        // full-width lines look exactly like table rulings to the repetition
        // check. Table rulings fail snugness — row separators extend past their
        // cells' text, or have no text on the baseline above — and multi-column
        // matches are culled by the tabular filter afterwards. Same-row segmented
        // rules are always rulings, since each segment snugly owns its column
        // label, so snugness must not override that check.
        return [.. rules.Where(rule =>
            !IsSegmentedRowRulingRule(rule, rules)
            && ((HasSnugTextOwner(rule, items) && !HasFlankingVerticals(rule, rects, lines, page))
                || !IsRepeatedRulingRule(rule, rules)))];
    }

    /// <summary>
    /// True when a rule is flanked by vertical strokes at its ends, or sits
    /// inside a grid of drawn boxes — either marks a table or box border rather
    /// than an underline.
    /// </summary>
    private static bool HasFlankingVerticals(
        Rule rule,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<UnderlineLine> lines,
        uint page)
    {
        // A drawn rect containing the rule vetoes rescue only with grid
        // evidence: another rect abutting it vertically, as cell rows tile.
        // Height alone cannot separate a table cell from a decorative callout
        // panel — genuine underlines live inside isolated filled panels, and
        // multiline table cells can be arbitrarily tall.
        var pageRects = new List<(float XLo, float XHi, float YLo, float YHi)>();
        foreach (var r in rects)
        {
            if (r.Page != page || MathF.Abs(r.Height) <= 6.0f)
            {
                continue;
            }

            var (xLo, xHi) = r.Width >= 0.0f ? (r.X, r.X + r.Width) : (r.X + r.Width, r.X);
            var (yLo, yHi) = r.Height >= 0.0f ? (r.Y, r.Y + r.Height) : (r.Y + r.Height, r.Y);
            pageRects.Add((xLo, xHi, yLo, yHi));
        }

        foreach (var (xLo, xHi, yLo, yHi) in pageRects)
        {
            var contains = xLo <= rule.X1 + 2.0f
                && xHi >= rule.X2 - 2.0f
                && yLo <= rule.Y + 2.0f
                && yHi >= rule.Y - 2.0f;

            if (!contains)
            {
                continue;
            }

            foreach (var (nxLo, nxHi, nyLo, nyHi) in pageRects)
            {
                var xOverlap = MathF.Min(nxHi, xHi) - MathF.Max(nxLo, xLo);
                if (xOverlap <= 10.0f)
                {
                    continue;
                }

                if (MathF.Abs(nyLo - yHi) <= 3.0f || MathF.Abs(yLo - nyHi) <= 3.0f)
                {
                    return true;
                }
            }
        }

        foreach (var l in lines)
        {
            if (l.Page != page || MathF.Abs(l.X1 - l.X2) > 2.0f)
            {
                continue;
            }

            var x = (l.X1 + l.X2) / 2.0f;
            var nearEnd = MathF.Abs(x - rule.X1) <= 6.0f || MathF.Abs(x - rule.X2) <= 6.0f;
            if (!nearEnd)
            {
                continue;
            }

            var (yLo, yHi) = l.Y1 <= l.Y2 ? (l.Y1, l.Y2) : (l.Y2, l.Y1);
            if (yLo <= rule.Y + 2.0f && yHi >= rule.Y - 2.0f)
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>
    /// True when the text on the rule's baseline row both contains the rule
    /// horizontally and covers most of it. Underlines are drawn to the width of
    /// the text they decorate, but that text may be split into several runs, so
    /// ownership is judged against their union. Table and form rulings overshoot
    /// their row's text and fail either containment or coverage.
    /// </summary>
    private static bool HasSnugTextOwner(Rule rule, IReadOnlyList<TextItem> items)
    {
        var matched = items.Where(item => IsUnderlineCandidate(item) && RuleMatchesItem(rule, item)).ToList();
        if (matched.Count == 0)
        {
            return false;
        }

        var x1 = matched.Min(i => i.X);
        var x2 = matched.Max(i => i.X + i.Width);
        var maxFontSize = matched.Max(i => i.FontSize);
        var pad = MathF.Max(maxFontSize * 0.75f, 4.0f);

        if (rule.X1 < x1 - pad || rule.X2 > x2 + pad)
        {
            return false;
        }

        var covered = matched.Sum(i => i.Width);
        if (covered < rule.Width * 0.6f)
        {
            return false;
        }

        // A table row also unions to the rule's span, but its cells sit apart.
        // An underlined text line is contiguous runs with word-sized gaps; any
        // column-sized hole means this is a row ruling.
        matched.Sort((a, b) => Text.FloatTotalOrder.Instance.Compare(a.X, b.X));
        for (var i = 0; i + 1 < matched.Count; i++)
        {
            var gap = matched[i + 1].X - (matched[i].X + matched[i].Width);
            if (gap > MathF.Max(maxFontSize * 2.0f, 12.0f))
            {
                return false;
            }
        }

        return true;
    }

    private static bool IsRepeatedRulingRule(Rule rule, List<Rule> rules)
    {
        var yLevels = rules.Where(other => HasSimilarSpan(rule, other)).Select(other => other.Y).ToList();
        yLevels.Sort(Text.FloatTotalOrder.Instance);

        // Collapse levels that sit within the dedup tolerance of the previous one.
        var distinct = 0;
        float? last = null;
        foreach (var y in yLevels)
        {
            if (last is null || MathF.Abs(y - last.Value) > RuleYDedupEps)
            {
                distinct++;
                last = y;
            }
        }

        return distinct >= MinRepeatedRuleLevels;
    }

    private static bool IsSegmentedRowRulingRule(Rule rule, List<Rule> rules)
    {
        var rowRules = rules.Where(other => MathF.Abs(other.Y - rule.Y) <= RuleYDedupEps).ToList();
        if (rowRules.Count < MinSegmentedRowRules)
        {
            return false;
        }

        rowRules.Sort((a, b) => Text.FloatTotalOrder.Instance.Compare(a.X1, b.X1));

        var largeGaps = 0;
        for (var i = 0; i + 1 < rowRules.Count; i++)
        {
            if (rowRules[i + 1].X1 - rowRules[i].X2 > SegmentedRowGapMin)
            {
                largeGaps++;
            }
        }

        return largeGaps >= MinSegmentedRowGaps;
    }

    private static bool HasSimilarSpan(Rule a, Rule b)
    {
        var aWidth = a.Width;
        var bWidth = b.Width;
        if (aWidth <= 1.0f || bWidth <= 1.0f)
        {
            return false;
        }

        var widthRatio = MathF.Max(aWidth, bWidth) / MathF.Min(aWidth, bWidth);
        if (widthRatio > RuleSpanWidthRatio)
        {
            return false;
        }

        var overlap = MathF.Min(a.X2, b.X2) - MathF.Max(a.X1, b.X1);
        return overlap >= MathF.Min(aWidth, bWidth) * RuleSpanOverlapRatio;
    }

    private static HashSet<int> TabularRowSeparatorRuleIndices(List<Rule> rules, IReadOnlyList<TextItem> items)
    {
        var tabularRules = new HashSet<int>();

        for (var ruleIdx = 0; ruleIdx < rules.Count; ruleIdx++)
        {
            var rule = rules[ruleIdx];
            var matched = items.Where(item => IsUnderlineCandidate(item) && RuleMatchesItem(rule, item)).ToList();

            if (matched.Count < MinTabularRuleItems)
            {
                continue;
            }

            matched.Sort((a, b) => Text.FloatTotalOrder.Instance.Compare(a.X, b.X));

            var largeGaps = 0;
            for (var i = 0; i + 1 < matched.Count; i++)
            {
                var left = matched[i];
                var right = matched[i + 1];
                var gap = right.X - (left.X + left.Width);
                var fontSize = MathF.Max(MathF.Max(left.FontSize, right.FontSize), 1.0f);
                if (gap > fontSize * TabularRuleGapEm)
                {
                    largeGaps++;
                }
            }

            if (largeGaps >= MinTabularRuleGaps)
            {
                tabularRules.Add(ruleIdx);
            }
        }

        return tabularRules;
    }

    private static bool IsUnderlineCandidate(TextItem item) =>
        item.Kind == ItemKind.Text && item.Text.Trim().Length > 0 && item.Width > 0.0f;

    private static bool RuleMatchesItem(Rule rule, TextItem item)
    {
        // Underlines sit at or slightly below the baseline: Latin fonts draw them
        // at roughly 5–15% of the em below, while CJK layouts put them under the
        // full em box, up to about 0.67em down. Allow 0.72em (minimum 3pt) below
        // and 1pt above for rounding.
        var below = MathF.Max(item.FontSize * 0.72f, 3.0f);
        var yMin = item.Y - below;
        var yMax = item.Y + 1.0f;

        if (rule.Y < yMin || rule.Y > yMax)
        {
            return false;
        }

        var ix1 = item.X;
        var ix2 = item.X + item.Width;
        var minOverlap = item.Width * MinXOverlap;
        var overlap = MathF.Min(rule.X2, ix2) - MathF.Max(rule.X1, ix1);

        return overlap >= minOverlap;
    }

    /// <summary>
    /// A rule crossing the glyphs. Strikethroughs sit at roughly 20–35% of the em
    /// above the baseline, about half the x-height; the band stays well inside
    /// the glyph body so baseline underlines and overlines never qualify.
    /// </summary>
    private static bool RuleStrikesItem(Rule rule, TextItem item)
    {
        var yMin = item.Y + (item.FontSize * 0.12f);
        var yMax = item.Y + (item.FontSize * 0.55f);

        if (rule.Y < yMin || rule.Y > yMax)
        {
            return false;
        }

        var ix1 = item.X;
        var ix2 = item.X + item.Width;
        var minOverlap = item.Width * MinXOverlap;
        var overlap = MathF.Min(rule.X2, ix2) - MathF.Max(rule.X1, ix1);

        return overlap >= minOverlap;
    }

    /// <summary>
    /// Marks items that have a horizontal rule just below their baseline as
    /// underlined, and items whose glyphs a rule crosses at mid x-height as
    /// struck out. All inputs are one page's extraction output in PDF
    /// coordinates, where an item's Y is its text baseline.
    /// </summary>
    public static void MarkUnderlinedItems(
        List<TextItem> items,
        IReadOnlyList<PdfRect> rects,
        IReadOnlyList<UnderlineLine> lines,
        uint page)
    {
        var rules = DiscardRepeatedRulingRules(
            RulesFromGraphics(rects, lines, page), items, rects, lines, page);

        if (rules.Count == 0)
        {
            return;
        }

        var tabularRules = TabularRowSeparatorRuleIndices(rules, items);

        // Math fraction bars are short horizontal lines with the numerator just
        // above and the denominator just below — underline geometry seen from
        // above, but no real underline has text hanging directly beneath it at
        // fraction distance. Only narrow rules qualify: a genuine underline under
        // a short label has its next text line a full line-pitch away.
        var fractionRules = new HashSet<int>();
        for (var i = 0; i < rules.Count; i++)
        {
            var rule = rules[i];
            if (rule.Width > 60.0f)
            {
                continue;
            }

            foreach (var item in items)
            {
                if (!IsUnderlineCandidate(item))
                {
                    continue;
                }

                // A denominator hugs the bar — fraction typesetting leaves about
                // 0.1–0.2em — and is bar-sized. Both bounds matter: a short last
                // line of a paragraph at normal leading sits further below, and a
                // full next text line is far wider than the rule.
                var dy = rule.Y - (item.Y + item.Height);
                var overlap = MathF.Min(rule.X2, item.X + item.Width) - MathF.Max(rule.X1, item.X);

                if (dy > 0.0f
                    && dy <= item.FontSize * 0.3f
                    && overlap > rule.Width * 0.5f
                    && item.Width <= rule.Width * 1.5f)
                {
                    fractionRules.Add(i);
                    break;
                }
            }
        }

        foreach (var item in items)
        {
            if (!IsUnderlineCandidate(item))
            {
                continue;
            }

            for (var ruleIdx = 0; ruleIdx < rules.Count; ruleIdx++)
            {
                if (tabularRules.Contains(ruleIdx))
                {
                    continue;
                }

                var rule = rules[ruleIdx];

                // The fraction guard gates underline marking only: a rule that
                // reads as a fraction bar from below can still legitimately
                // strike through the line above it.
                if (!fractionRules.Contains(ruleIdx) && RuleMatchesItem(rule, item))
                {
                    item.IsUnderline = true;
                }

                if (RuleStrikesItem(rule, item))
                {
                    item.IsStrikeout = true;
                }

                if (item.IsUnderline && item.IsStrikeout)
                {
                    break;
                }
            }
        }
    }
}
