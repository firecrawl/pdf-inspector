// Ported from reference/src/extractor/mod.rs
using System.Text;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>
/// Joins the fragments a content stream emits back into whole words and lines.
/// Show-text operators split arbitrarily — per glyph, per style run, per
/// kerning pair — so this pass regroups by baseline and merges neighbours that
/// share a font size and style and sit close enough horizontally.
/// </summary>
internal static class ItemMerging
{
    private const float YTolerance = 5.0f;

    // ── Width capping ────────────────────────────────────────────────────

    /// <summary>
    /// Caps an item's width for gap computation. Large word spacing, used for
    /// justification, extends the advance width of strings containing spaces far
    /// past the visible glyphs; that inflated width collapses inter-column gaps
    /// and would merge items from different table columns.
    /// </summary>
    private static float EffectiveMergeWidth(TextItem item)
    {
        if (item.Width <= 0.0f || item.FontSize <= 0.0f)
        {
            return item.Width;
        }

        // Word spacing only inflates strings that contain space characters.
        if (!item.Text.Contains(' '))
        {
            return item.Width;
        }

        // CJK characters are naturally about one em wide, so the cap does not apply.
        foreach (var ch in item.Text)
        {
            if (TextUtils.IsCjkChar(ch))
            {
                return item.Width;
            }
        }

        var charCount = TextUtils.CharCount(item.Text);
        if (charCount == 0)
        {
            return item.Width;
        }

        var average = item.Width / charCount;

        // Proportional text runs about 0.5em per character and monospace about
        // 0.6em, so 0.85em catches word-spacing inflation.
        if (average > item.FontSize * 0.85f)
        {
            return MathF.Min(charCount * item.FontSize * 0.6f, item.Width);
        }

        return item.Width;
    }

    private static bool IsStandaloneBulletText(string text) =>
        text.Trim() is "•" or "○" or "●" or "◦";

    private static char? FirstTextChar(string text) => TextUtils.FirstChar(text.TrimStart());

    private static bool IsShortAlphaFragment(string text)
    {
        var trimmed = text.Trim();
        var charCount = TextUtils.CharCount(trimmed);
        return charCount is >= 1 and <= 4 && trimmed.All(char.IsLetter);
    }

    private static bool HasPhraseContinuationShape(string text)
    {
        var trimmed = text.TrimStart();
        var limit = Math.Min(24, trimmed.Length);
        for (var i = 0; i < limit; i++)
        {
            if (char.IsWhiteSpace(trimmed[i]) || trimmed[i] == '-')
            {
                return true;
            }
        }

        return false;
    }

    // ── Overlapping stream order ─────────────────────────────────────────

    /// <summary>
    /// True when a line's content stream deliberately backtracks to overlay
    /// fragments, so sorting by x would scramble the intended reading order.
    /// </summary>
    private static bool ShouldPreserveOverlappingStreamOrder(List<TextItem> group)
    {
        if (group.Count < 3)
        {
            return false;
        }

        var first = group.FirstOrDefault(item => item.Text.Trim().Length > 0);
        if (first is null)
        {
            return false;
        }

        if (group.All(item => item.Mcid is null))
        {
            return false;
        }

        var nonEmptyCount = 0;
        var nonSpaceChars = 0;
        var mathSymbolChars = 0;
        var maxFontSize = first.FontSize;

        foreach (var item in group)
        {
            if (item.Text.Trim().Length > 0)
            {
                nonEmptyCount++;
            }

            if (MathF.Abs(item.FontSize - first.FontSize) > first.FontSize * 0.25f)
            {
                return false;
            }

            maxFontSize = MathF.Max(maxFontSize, item.FontSize);

            foreach (var ch in item.Text)
            {
                if (char.IsWhiteSpace(ch))
                {
                    continue;
                }

                nonSpaceChars++;
                if (ch is '*' or 'ˆ' or '^' or '=' or '+' or '_' or '[' or ']' or '{' or '}' or '|' or '<' or '>')
                {
                    mathSymbolChars++;
                }
            }
        }

        if (nonEmptyCount < 2)
        {
            return false;
        }

        // A symbol-heavy line is mathematics, whose overlays are positioning
        // rather than reading order.
        if (nonSpaceChars > 0 && mathSymbolChars * 4 > nonSpaceChars)
        {
            return false;
        }

        var sortedByX = group.OrderBy(i => i.X, FloatTotalOrder.Instance).ToList();
        var clusterStart = sortedByX[0].X;
        var clusterEnd = clusterStart + EffectiveMergeWidth(sortedByX[0]);

        for (var i = 1; i < sortedByX.Count; i++)
        {
            var gap = sortedByX[i].X - clusterEnd;
            if (gap > maxFontSize * 2.5f)
            {
                return false;
            }

            clusterEnd = MathF.Max(clusterEnd, sortedByX[i].X + EffectiveMergeWidth(sortedByX[i]));
        }

        if (clusterEnd - clusterStart > maxFontSize * 36.0f)
        {
            return false;
        }

        for (var index = 0; index + 1 < group.Count; index++)
        {
            var previous = group[index];
            var next = group[index + 1];
            var fontSize = MathF.Max(previous.FontSize, next.FontSize);
            var backtrackThreshold = fontSize * 0.25f;
            var previousStart = previous.X;
            var nextStart = next.X;
            var nextEnd = next.X + EffectiveMergeWidth(next);

            if (nextStart >= previousStart - backtrackThreshold ||
                nextEnd <= previousStart + backtrackThreshold)
            {
                continue;
            }

            var hasNearPrefix = false;
            for (var back = index; back >= 0 && back > index - 4; back--)
            {
                var item = group[back];
                if (IsShortAlphaFragment(item.Text)
                    && item.X >= nextStart - (fontSize * 0.5f)
                    && item.X <= nextStart + (fontSize * 4.0f))
                {
                    hasNearPrefix = true;
                    break;
                }
            }

            var startsLowercase = FirstTextChar(next.Text) is { } fc && char.IsLower(fc);
            var phraseContinuation = HasPhraseContinuationShape(next.Text);

            var hasNearBullet = false;
            for (var bulletIndex = 0; bulletIndex <= index; bulletIndex++)
            {
                var item = group[bulletIndex];
                if (!IsStandaloneBulletText(item.Text) || nextStart > item.X + (fontSize * 3.0f))
                {
                    continue;
                }

                if (bulletIndex >= index)
                {
                    break;
                }

                for (var after = index; after > bulletIndex; after--)
                {
                    if (group[after].Text.Trim().Length == 0)
                    {
                        continue;
                    }

                    hasNearBullet = TextUtils.CharCount(group[after].Text.Trim()) <= 8
                        && HasPhraseContinuationShape(next.Text);
                    break;
                }

                break;
            }

            if ((hasNearPrefix && startsLowercase && phraseContinuation) || hasNearBullet)
            {
                return true;
            }
        }

        return false;
    }

    // ── Tracked runs ─────────────────────────────────────────────────────

    /// <summary>
    /// Han and Kana write without inter-word spaces. Hangul does space between
    /// words and deliberately stays out of this set, so a Korean tracked run
    /// keeps normal word-boundary handling.
    /// </summary>
    private static bool IsSpacelessCjk(char c) =>
        c is >= '\u3000' and <= '\u303F'   // CJK Symbols and Punctuation
            or >= '\u3040' and <= '\u309F' // Hiragana
            or >= '\u30A0' and <= '\u30FF' // Katakana
            or >= '\u4E00' and <= '\u9FFF' // CJK Unified Ideographs
            or >= '\uF900' and <= '\uFAFF' // CJK Compatibility Ideographs
            or >= '\uFF00' and <= '\uFFEF'; // Halfwidth and Fullwidth Forms

    /// <summary>
    /// Detects a tracked (letter-spaced) run of single-glyph items and derives
    /// its run-local space floor.
    ///
    /// Display type set with tracking renders one glyph per show operator; the
    /// merge loop's fixed thresholds then read every letter gap as a word
    /// boundary and emit "H O W" instead of "HOW". Within such a run the gaps
    /// carry the real signal: letter gaps cluster tightly just above the fixed
    /// threshold, and word gaps sit clearly higher.
    /// </summary>
    /// <returns>
    /// The last index of the run and the gap above which a space is inserted
    /// (infinity meaning the whole run is one word), or null for normal text.
    /// </returns>
    private static (int RunEnd, float Floor)? TrackedRunSpaceFloor(List<TextItem> group, int start)
    {
        const int MinGaps = 4;

        var first = group[start];
        if (TextUtils.CharCount(first.Text.Trim()) != 1)
        {
            return null;
        }

        var fs = first.FontSize;
        if (fs <= 0.0f)
        {
            return null;
        }

        // The run is walked under the same break conditions as the merge loop —
        // size band, style equality, mergeable gap — so the indices stay aligned.
        var gaps = new List<float>();
        var endX = first.X + EffectiveMergeWidth(first);
        var end = start;

        for (var i = start + 1; i < group.Count; i++)
        {
            var next = group[i];

            if (TextUtils.CharCount(next.Text.Trim()) != 1)
            {
                break;
            }

            if (MathF.Abs(next.FontSize - fs) > fs * 0.20f)
            {
                break;
            }

            if (next.IsBold != first.IsBold
                || next.IsItalic != first.IsItalic
                || next.IsUnderline != first.IsUnderline
                || next.IsStrikeout != first.IsStrikeout)
            {
                break;
            }

            var gap = next.X - endX;
            if (gap > fs * 0.5f || gap < -fs * 0.5f)
            {
                break;
            }

            gaps.Add(gap / fs);
            endX = next.X + EffectiveMergeWidth(next);
            end = i;
        }

        if (gaps.Count < 2)
        {
            return null;
        }

        var sorted = new List<float>(gaps);
        sorted.Sort(FloatTotalOrder.Instance);
        var median = sorted[sorted.Count / 2];

        // Typographic convention gate for both tiers: display tracking is an
        // all-caps convention, and Han/Kana never space between glyphs. Mixed-
        // or lowercase Latin runs keep their boundaries, because geometry alone
        // cannot distinguish spaced singles ("A b c d e") from a tracked
        // title-case word ("B u f f a l o").
        var runChars = new List<char>();
        for (var i = start; i <= end; i++)
        {
            runChars.AddRange(group[i].Text.Trim());
        }

        var spacelessCjk = runChars.All(c => IsSpacelessCjk(c) || !char.IsLetterOrDigit(c))
            && runChars.Any(IsSpacelessCjk);
        var allCaps = runChars.All(c => char.IsUpper(c) || TextUtils.IsCjkChar(c) || !char.IsLetter(c));

        if (!spacelessCjk && !allCaps)
        {
            return null;
        }

        if (gaps.Count >= MinGaps)
        {
            // The run's typical gap must clear the fixed space threshold, or the
            // merge loop would not have split it in the first place.
            if (median <= 0.075f)
            {
                return null;
            }
        }
        else
        {
            // Short runs demand a stricter shape — clearly wide, uniform, and
            // all-caps — because a genuine spaced sequence of single letters has
            // the same gap count.
            var uniform = sorted[^1] <= MathF.Max(sorted[0], 0.01f) * 1.4f;
            if (median < 0.09f || !uniform)
            {
                return null;
            }
        }

        // Han and Kana take no inter-glyph spaces at all, so a non-uniform gap
        // distribution must not manufacture word boundaries.
        if (spacelessCjk)
        {
            return (end, float.PositiveInfinity);
        }

        // Word gaps, where present, form a second mode above the letter-gap
        // cluster; the split goes at the largest relative jump. A unimodal
        // distribution means the run is a single word.
        var bestJump = 1.0f;
        var floor = float.PositiveInfinity;

        for (var i = 0; i + 1 < sorted.Count; i++)
        {
            var lo = MathF.Max(sorted[i], 0.01f);
            var hi = MathF.Max(sorted[i + 1], 0.01f);
            var jump = hi / lo;
            if (jump > bestJump)
            {
                bestJump = jump;
                floor = (lo + hi) / 2.0f;
            }
        }

        if (bestJump < 1.4f)
        {
            floor = float.PositiveInfinity;
        }

        return (end, floor * fs);
    }

    // ── Line merging ─────────────────────────────────────────────────────

    /// <summary>Merges adjacent items on the same line into single items.</summary>
    public static List<TextItem> MergeTextItems(List<TextItem> items)
    {
        if (items.Count == 0)
        {
            return items;
        }

        var lineGroups = GroupByBaseline(items);
        var ordered = new List<(uint Page, float Y, List<TextItem> Group, bool PreserveStreamOrder)>();

        foreach (var (page, y, group) in lineGroups)
        {
            var rtl = TextUtils.IsRtlText(group.Select(i => i.Text));
            var preserveStreamOrder = !rtl && ShouldPreserveOverlappingStreamOrder(group);

            if (rtl)
            {
                group.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.X, a.X));
            }
            else if (!preserveStreamOrder)
            {
                group.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.X, b.X));
            }

            ordered.Add((page, y, group, preserveStreamOrder));
        }

        // Page order, then top of page first.
        ordered.Sort((a, b) =>
        {
            var byPage = a.Page.CompareTo(b.Page);
            return byPage != 0 ? byPage : FloatTotalOrder.Instance.Compare(b.Y, a.Y);
        });

        var merged = new List<TextItem>();

        foreach (var (_, _, group, preserveStreamOrder) in ordered)
        {
            var i = 0;
            while (i < group.Count)
            {
                var first = group[i];
                var text = new StringBuilder(first.Text);
                var endX = first.X + EffectiveMergeWidth(first);

                // A tracked run's local space floor overrides the fixed
                // thresholds for that run's junctions.
                var tracked = preserveStreamOrder ? null : TrackedRunSpaceFloor(group, i);

                var j = i + 1;
                while (j < group.Count)
                {
                    var next = group[j];

                    // Font sizes must be within 20%.
                    if (MathF.Abs(next.FontSize - first.FontSize) > first.FontSize * 0.20f)
                    {
                        break;
                    }

                    // Never merge across style boundaries: the merged item carries
                    // the first item's flags, so absorbing a styled run into a
                    // plain neighbour would silently erase styling the markdown
                    // emitter needs, and OR-ing underline instead would stretch
                    // the tag over neighbouring plain text.
                    if (next.IsBold != first.IsBold
                        || next.IsItalic != first.IsItalic
                        || next.IsUnderline != first.IsUnderline
                        || next.IsStrikeout != first.IsStrikeout)
                    {
                        break;
                    }

                    var gap = next.X - endX;
                    var xGapMax = preserveStreamOrder && IsStandaloneBulletText(text.ToString())
                        ? first.FontSize * 1.2f
                        : first.FontSize * 0.5f;

                    if (gap > xGapMax)
                    {
                        break;
                    }

                    if (gap < -first.FontSize * 0.5f && !preserveStreamOrder)
                    {
                        break;
                    }

                    var currentText = text.ToString();
                    var prevLast = TextUtils.LastChar(currentText.TrimEnd());
                    var nextFirst = TextUtils.FirstChar(next.Text.TrimStart());

                    float threshold;
                    if (nextFirst is '.' or ',' or ';' or ')' or ']' or '}')
                    {
                        // Never insert a space before joining punctuation.
                        threshold = first.FontSize * 0.25f;
                    }
                    else if (prevLast is { } pl && char.IsLower(pl) && nextFirst is { } nf && char.IsLower(nf))
                    {
                        // Lowercase to lowercase is likely mid-word; the wider
                        // threshold absorbs character-spacing adjustments that
                        // shift advance widths relative to positioning.
                        threshold = first.FontSize * 0.13f;
                    }
                    else
                    {
                        threshold = first.FontSize * 0.08f;
                    }

                    var needsBulletSpace = preserveStreamOrder
                        && IsStandaloneBulletText(currentText)
                        && next.Text.Trim().Length > 0;

                    var effectiveThreshold = tracked is { } t && j <= t.RunEnd ? t.Floor : threshold;

                    if (needsBulletSpace || gap > effectiveThreshold)
                    {
                        text.Append(' ');
                    }

                    text.Append(next.Text);

                    var nextEnd = next.X + EffectiveMergeWidth(next);
                    endX = preserveStreamOrder ? MathF.Max(endX, nextEnd) : nextEnd;
                    j++;
                }

                merged.Add(new TextItem
                {
                    Text = text.ToString(),
                    X = first.X,
                    Y = first.Y,
                    Width = endX - first.X,
                    Height = first.Height,
                    Font = first.Font,
                    FontSize = first.FontSize,
                    Page = first.Page,
                    IsBold = first.IsBold,
                    IsItalic = first.IsItalic,
                    IsUnderline = first.IsUnderline,
                    IsStrikeout = first.IsStrikeout,
                    Kind = first.Kind,
                    LinkUrl = first.LinkUrl,
                    Mcid = first.Mcid,
                });

                i = j;
            }
        }

        return merged;
    }

    /// <summary>
    /// Groups items into lines by page and baseline, in first-seen order, which
    /// is what the reference build's linear scan produces.
    /// </summary>
    private static List<(uint Page, float Y, List<TextItem> Group)> GroupByBaseline(List<TextItem> items)
    {
        var groups = new List<(uint Page, float Y, List<TextItem> Group)>();

        foreach (var item in items)
        {
            var placed = false;
            foreach (var (page, y, group) in groups)
            {
                if (page == item.Page && MathF.Abs(item.Y - y) < YTolerance)
                {
                    group.Add(item);
                    placed = true;
                    break;
                }
            }

            if (!placed)
            {
                groups.Add((item.Page, item.Y, [item]));
            }
        }

        return groups;
    }

    // ── Subscript merging ────────────────────────────────────────────────

    /// <summary>
    /// Absorbs subscript and superscript digits into the item they belong to, so
    /// downstream table detection and line grouping see whole tokens — "H2O"
    /// rather than "H", "2", "O".
    /// </summary>
    public static List<TextItem> MergeSubscriptItems(List<TextItem> items)
    {
        if (items.Count < 2)
        {
            return items;
        }

        var lineGroups = GroupByBaseline(items);
        var result = new List<TextItem>();

        foreach (var (_, _, group) in lineGroups)
        {
            group.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.X, b.X));

            var maxFontSize = group.Max(i => i.FontSize);
            if (maxFontSize < 1.0f)
            {
                result.AddRange(group);
                continue;
            }

            var subThreshold = maxFontSize * 0.75f;
            var merged = new List<TextItem>();

            foreach (var item in group)
            {
                // Only purely numeric text merges, which avoids false positives
                // on small bullets, ordinal indicators, and letter labels.
                var isCandidate = item.FontSize < subThreshold
                    && item.FontSize > 0.0f
                    && item.Text.Length <= 4
                    && item.Text.Length > 0
                    && item.Text.All(char.IsAsciiDigit);

                if (isCandidate && merged.Count > 0)
                {
                    var parent = merged[^1];

                    // The parent must be normal-sized, not itself a subscript,
                    // and end with a letter. That keeps chemical formulas and
                    // footnote references while refusing to merge into numbers
                    // such as "33" + "1" in "33 1/3%".
                    var endsWithLetter = TextUtils.LastChar(parent.Text) is { } lc && char.IsLetter(lc);

                    // Strikeout boundaries block the merge: a struck word must
                    // not extend its mark over a live footnote digit, and a
                    // struck digit must not lose its own. An underlined parent
                    // with an unmarked digit does merge — the drawn rule easily
                    // misses the tiny digit's overlap window, and refusing would
                    // cost the whole token.
                    var marksOk = parent.IsStrikeout == item.IsStrikeout
                        && (parent.IsUnderline == item.IsUnderline || parent.IsUnderline);

                    if (parent.FontSize >= subThreshold && endsWithLetter && marksOk)
                    {
                        var parentRight = parent.X + parent.Width;
                        var gap = item.X - parentRight;

                        if (gap < parent.FontSize * 0.2f && gap > -parent.FontSize * 0.3f)
                        {
                            // The script survives the merge as Unicode sub- or
                            // superscript digits, so the raised or lowered
                            // rendering is preserved in the extracted text.
                            // Compatibility normalisation folds these back to
                            // plain digits, so text matching is unaffected.
                            var raised = item.Y > parent.Y + (parent.FontSize * 0.1f);
                            parent.Text += MapScriptDigits(item.Text, raised);
                            parent.Width = item.X + item.Width - parent.X;
                            continue;
                        }
                    }
                }

                merged.Add(item);
            }

            result.AddRange(merged);
        }

        return result;
    }

    private static readonly char[] SuperscriptDigits = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    private static readonly char[] SubscriptDigits = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];

    /// <summary>
    /// Maps ASCII digits to their Unicode superscript or subscript forms.
    /// Callers guarantee digit-only input; anything else passes through.
    /// </summary>
    private static string MapScriptDigits(string text, bool raised)
    {
        var builder = new StringBuilder(text.Length);
        foreach (var c in text)
        {
            if (char.IsAsciiDigit(c))
            {
                builder.Append(raised ? SuperscriptDigits[c - '0'] : SubscriptDigits[c - '0']);
            }
            else
            {
                builder.Append(c);
            }
        }

        return builder.ToString();
    }
}
