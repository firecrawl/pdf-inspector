// Ported from reference/src/tables/detect_lines.rs
using System.Text;
using PdfInspector.Extractor;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>A table inferred from text anchors, with the band of page it claims.</summary>
internal sealed class TextAnchorTable
{
    public required Table Table { get; init; }

    public required float XLeft { get; init; }

    public required float XRight { get; init; }

    public required float YBottom { get; init; }

    public required float YTop { get; init; }

    /// <summary>True when a drawn line falls inside this table's band.</summary>
    public bool OverlapsLine(PdfLine line)
    {
        var lineXMin = MathF.Min(line.X1, line.X2);
        var lineXMax = MathF.Max(line.X1, line.X2);
        var lineYMin = MathF.Min(line.Y1, line.Y2);
        var lineYMax = MathF.Max(line.Y1, line.Y2);

        return lineXMax >= XLeft - LineDetector.RuleJoinGap
            && lineXMin <= XRight + LineDetector.RuleJoinGap
            && lineYMax >= YBottom - LineDetector.RuleYTolerance
            && lineYMin <= YTop + LineDetector.RuleYTolerance;
    }
}

/// <summary>One text row inside a ruled band, with its baseline.</summary>
internal sealed class AnchoredRow
{
    public required float Y { get; init; }

    public required List<(int Index, TextItem Item)> Items { get; init; }
}

/// <summary>
/// Table hypotheses inferred from text alignment inside a ruled band. Booktabs
/// and response-form tables draw horizontal rules only, so their columns must
/// come from where the text sits rather than from drawn dividers.
/// </summary>
internal static class TextAnchorTables
{
    private const string Module = "tables";

    private static float JoinGap => LineDetector.RuleJoinGap;

    private static float YTolerance => LineDetector.RuleYTolerance;

    private static float RowTolerance => LineDetector.TextRowTolerance;

    private static float SpanTolerance => LineDetector.RuleSpanTolerance;

    /// <summary>
    /// Merges touching path segments into logical rules. Forms often stroke one
    /// segment per cell at the same height; treating those as separate rules
    /// would manufacture column edges out of path endpoints.
    /// </summary>
    internal static List<HorizontalRule> MergeHorizontalSegments(List<HorizontalRule> horizontals)
    {
        var sorted = horizontals
            .OrderByDescending(r => r.Y, FloatTotalOrder.Instance)
            .ThenBy(r => r.XMin, FloatTotalOrder.Instance)
            .ToList();

        var yGroups = new List<List<HorizontalRule>>();
        foreach (var rule in sorted)
        {
            if (yGroups.Count > 0 && MathF.Abs(yGroups[^1][0].Y - rule.Y) <= YTolerance)
            {
                yGroups[^1].Add(rule);
            }
            else
            {
                yGroups.Add([rule]);
            }
        }

        var merged = new List<HorizontalRule>();
        foreach (var group in yGroups)
        {
            group.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.XMin, b.XMin));

            var y = group.Sum(r => r.Y) / group.Count;
            var current = new HorizontalRule(y, group[0].XMin, group[0].XMax);

            foreach (var rule in group.Skip(1))
            {
                if (rule.XMin <= current.XMax + JoinGap)
                {
                    current = current with { XMax = MathF.Max(current.XMax, rule.XMax) };
                }
                else
                {
                    merged.Add(current);
                    current = new HorizontalRule(y, rule.XMin, rule.XMax);
                }
            }

            merged.Add(current);
        }

        merged.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));
        return merged;
    }

    /// <summary>Groups rules that share their horizontal extent, which marks one table.</summary>
    internal static List<List<HorizontalRule>> GroupRulesBySpan(List<HorizontalRule> rules)
    {
        var groups = new List<List<HorizontalRule>>();

        foreach (var rule in rules)
        {
            var bestIndex = -1;
            var bestError = float.PositiveInfinity;

            for (var index = 0; index < groups.Count; index++)
            {
                var first = groups[index][0];
                if (MathF.Abs(first.XMin - rule.XMin) > SpanTolerance ||
                    MathF.Abs(first.XMax - rule.XMax) > SpanTolerance)
                {
                    continue;
                }

                var error = MathF.Abs(first.XMin - rule.XMin) + MathF.Abs(first.XMax - rule.XMax);
                if (error < bestError)
                {
                    bestError = error;
                    bestIndex = index;
                }
            }

            if (bestIndex >= 0)
            {
                groups[bestIndex].Add(rule);
            }
            else
            {
                groups.Add([rule]);
            }
        }

        foreach (var group in groups)
        {
            group.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));
        }

        return groups;
    }

    private static bool NumberedTableCaption(string text)
    {
        var lower = text.Trim().ToLowerInvariant();
        if (!lower.StartsWith("table ", StringComparison.Ordinal))
        {
            return false;
        }

        var token = lower[6..].Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).FirstOrDefault();
        if (token is null)
        {
            return false;
        }

        var digits = token.Trim(token.Where(c => !char.IsAsciiDigit(c)).Distinct().ToArray());
        return digits.Length > 0 && digits.All(char.IsAsciiDigit);
    }

    /// <summary>
    /// Splits equal-width rules into independent runs. Consecutive booktabs
    /// tables share their endpoints; a numbered caption between them is an
    /// explicit separator, and a large empty band is the fallback for
    /// captionless ones.
    /// </summary>
    internal static List<List<HorizontalRule>> SplitIndependentRuleRuns(
        List<HorizontalRule> rules,
        IReadOnlyList<TextItem> items,
        uint page)
    {
        if (rules.Count == 0)
        {
            return [];
        }

        var groups = new List<List<HorizontalRule>>();
        var current = new List<HorizontalRule> { rules[0] };

        for (var index = 0; index + 1 < rules.Count; index++)
        {
            var a = rules[index];
            var b = rules[index + 1];
            var yMin = MathF.Min(a.Y, b.Y);
            var yMax = MathF.Max(a.Y, b.Y);

            var hasCaption = items.Any(item =>
                item.Page == page && item.Y > yMin && item.Y < yMax && NumberedTableCaption(item.Text));

            // Both sides need at least two rules, and the empty interval must be
            // proportionally large, so a long table's regularly spaced rows are
            // not split apart.
            var canFormTwoRuns = index + 1 >= 2 && rules.Count - (index + 1) >= 2;
            var ruleGap = yMax - yMin;
            var hasEmptySeparator = false;

            if (canFormTwoRuns && ruleGap >= 36.0f)
            {
                var xMin = MathF.Min(a.XMin, b.XMin) - JoinGap;
                var xMax = MathF.Max(a.XMax, b.XMax) + JoinGap;

                var occupiedY = items
                    .Where(item => item.Page == page
                        && Columns.IsTextLayoutItem(item)
                        && item.Text.Trim().Length > 0
                        && item.Y > yMin
                        && item.Y < yMax
                        && item.X + MathF.Max(item.Width, 0.0f) >= xMin
                        && item.X <= xMax)
                    .Select(item => item.Y)
                    .ToList();

                occupiedY.Add(yMin);
                occupiedY.Add(yMax);
                occupiedY.Sort(FloatTotalOrder.Instance);

                var deduped = new List<float>();
                foreach (var y in occupiedY)
                {
                    if (deduped.Count == 0 || MathF.Abs(y - deduped[^1]) > RowTolerance)
                    {
                        deduped.Add(y);
                    }
                }

                var largestEmptyGap = 0.0f;
                for (var i = 0; i + 1 < deduped.Count; i++)
                {
                    largestEmptyGap = MathF.Max(largestEmptyGap, deduped[i + 1] - deduped[i]);
                }

                hasEmptySeparator = largestEmptyGap >= MathF.Max(36.0f, ruleGap * 0.45f);
            }

            if (hasCaption || hasEmptySeparator)
            {
                groups.Add(current);
                current = [b];
            }
            else
            {
                current.Add(b);
            }
        }

        groups.Add(current);
        return groups;
    }

    /// <summary>Collects the text rows that fall inside a run of rules.</summary>
    internal static List<AnchoredRow> CollectAnchoredRows(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> rules,
        uint page)
    {
        var yTop = rules.Max(r => r.Y);
        var yBottom = rules.Min(r => r.Y);
        var xMin = rules.Min(r => r.XMin);
        var xMax = rules.Max(r => r.XMax);

        var selected = new List<(int Index, TextItem Item)>();
        for (var i = 0; i < items.Count; i++)
        {
            var item = items[i];
            if (item.Page == page
                && Columns.IsTextLayoutItem(item)
                && item.Text.Trim().Length > 0
                && item.Y >= yBottom - YTolerance
                && item.Y <= yTop + YTolerance
                && item.X + MathF.Max(item.Width, 0.0f) >= xMin - JoinGap
                && item.X <= xMax + JoinGap)
            {
                selected.Add((i, item));
            }
        }

        selected.Sort((a, b) =>
        {
            var byY = FloatTotalOrder.Instance.Compare(b.Item.Y, a.Item.Y);
            return byY != 0 ? byY : FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X);
        });

        var rows = new List<AnchoredRow>();
        foreach (var entry in selected)
        {
            if (rows.Count > 0 && MathF.Abs(rows[^1].Y - entry.Item.Y) <= RowTolerance)
            {
                rows[^1].Items.Add(entry);
                continue;
            }

            rows.Add(new AnchoredRow { Y = entry.Item.Y, Items = [entry] });
        }

        foreach (var row in rows)
        {
            row.Items.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X));
        }

        return rows;
    }

    /// <summary>True when the rules are evenly spaced, which marks chart gridlines.</summary>
    private static bool RulesAreUniformGrid(List<HorizontalRule> rules)
    {
        if (rules.Count < 5)
        {
            return false;
        }

        var spacings = new List<float>();
        for (var i = 0; i + 1 < rules.Count; i++)
        {
            spacings.Add(MathF.Abs(rules[i].Y - rules[i + 1].Y));
        }

        var mean = spacings.Sum() / spacings.Count;
        if (mean <= 0.1f)
        {
            return false;
        }

        var variance = spacings.Sum(s => (s - mean) * (s - mean)) / spacings.Count;
        return MathF.Sqrt(variance) / mean < 0.02f;
    }

    /// <summary>
    /// The logical column starts of a row: the left edge of each run of items
    /// that are not separated by more than the join gap.
    /// </summary>
    private static List<float> LogicalRowAnchors(List<(int Index, TextItem Item)> row)
    {
        var spans = row
            .Select(e => (Left: e.Item.X, Right: e.Item.X + MathF.Max(e.Item.Width, 0.0f)))
            .OrderBy(s => s.Left, FloatTotalOrder.Instance)
            .ToList();

        var anchors = new List<float>();
        var currentRight = float.NegativeInfinity;

        foreach (var (left, right) in spans)
        {
            if (anchors.Count == 0 || left > currentRight + JoinGap)
            {
                anchors.Add(left);
                currentRight = right;
            }
            else
            {
                currentRight = MathF.Max(currentRight, right);
            }
        }

        return anchors;
    }

    private static int? NearestAnchorColumn(TextItem item, List<float> anchors)
    {
        if (anchors.Count == 0)
        {
            return null;
        }

        var best = 0;
        var bestDistance = MathF.Abs(anchors[0] - item.X);

        for (var i = 1; i < anchors.Count; i++)
        {
            var distance = MathF.Abs(anchors[i] - item.X);
            if (distance < bestDistance)
            {
                bestDistance = distance;
                best = i;
            }
        }

        return best;
    }

    private static int MatchedAnchorColumnCount(List<(int Index, TextItem Item)> row, List<float> anchors) =>
        row.Select(e => NearestAnchorColumn(e.Item, anchors))
            .Where(c => c is not null)
            .Distinct()
            .Count();

    // ── Text-anchor tables ───────────────────────────────────────────────

    /// <summary>
    /// Finds tables whose columns are inferred from where the text sits inside a
    /// sparsely ruled band.
    /// </summary>
    public static List<TextAnchorTable> DetectTextAnchorRuleTables(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> horizontals,
        List<VerticalRule> verticals,
        IReadOnlyList<PdfLine> pathLines,
        uint page)
    {
        var logicalRules = MergeHorizontalSegments(horizontals);
        var tables = new List<TextAnchorTable>();

        foreach (var spanGroup in GroupRulesBySpan(logicalRules))
        {
            foreach (var rules in SplitIndependentRuleRuns(spanGroup, items, page))
            {
                var yTop = rules.Max(r => r.Y);
                var yBottom = rules.Min(r => r.Y);
                var xLeft = rules.Min(r => r.XMin);
                var xRight = rules.Max(r => r.XMax);

                // Text-anchor inference is a sparse-geometry fallback; dense
                // line art in the same band means a chart or schematic, which
                // the physical-grid and chart detectors should own.
                var densePathRegion = pathLines
                    .Where(line => line.Page == page
                        && MathF.Max(line.X1, line.X2) >= xLeft - JoinGap
                        && MathF.Min(line.X1, line.X2) <= xRight + JoinGap
                        && MathF.Max(line.Y1, line.Y2) >= yBottom - YTolerance
                        && MathF.Min(line.Y1, line.Y2) <= yTop + YTolerance)
                    .Take(200)
                    .Count() >= 200;

                if (densePathRegion)
                {
                    continue;
                }

                var bandVerticals = verticals
                    .Where(v => v.X >= xLeft - JoinGap
                        && v.X <= xRight + JoinGap
                        && v.YMax >= yBottom - YTolerance
                        && v.YMin <= yTop + YTolerance)
                    .ToList();

                var spanningXs = bandVerticals
                    .Where(v => v.YMin <= yBottom + YTolerance && v.YMax >= yTop - YTolerance)
                    .Select(v => v.X)
                    .ToList();

                var bandXs = bandVerticals.Select(v => v.X).ToList();

                // Two coordinates can be the outer borders of an otherwise
                // borderless table; a physical grid needs an interior divider
                // spanning the band too. Many short marks are strong diagram
                // evidence even when no single one proves a cell grid.
                if (RectGrid.SnapEdges(spanningXs, 3.0f).Count >= 3 ||
                    RectGrid.SnapEdges(bandXs, 3.0f).Count >= 6)
                {
                    continue;
                }

                if (BuildTextAnchorTable(items, rules, page) is { } table)
                {
                    Log.Debug(Module, () =>
                        $"detect_lines p{page}: accepted text-anchor rule table " +
                        $"{table.Cells.Count}x{(table.Cells.Count > 0 ? table.Cells[0].Count : 0)} " +
                        $"from {rules.Count} rules");

                    tables.Add(new TextAnchorTable
                    {
                        Table = table,
                        XLeft = xLeft,
                        XRight = xRight,
                        YBottom = yBottom,
                        YTop = yTop,
                    });
                }
            }
        }

        tables.Sort((a, b) => FloatTotalOrder.Instance.Compare(
            b.Table.Rows.Count > 0 ? b.Table.Rows[0] : 0f,
            a.Table.Rows.Count > 0 ? a.Table.Rows[0] : 0f));

        return tables;
    }

    private static Table? BuildTextAnchorTable(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> rules,
        uint page)
    {
        if (rules.Count < 2 || RulesAreUniformGrid(rules))
        {
            return null;
        }

        var rows = CollectAnchoredRows(items, rules, page);
        if (rows.Count < 2)
        {
            return null;
        }

        var anchors = new List<float>();
        foreach (var (_, item) in rows[0].Items)
        {
            if (anchors.Count == 0 || MathF.Abs(item.X - anchors[^1]) > JoinGap)
            {
                anchors.Add(item.X);
            }
        }

        if (anchors.Count == 1)
        {
            return BuildStackedTokenTable(rows, rules);
        }

        if (anchors.Count is < 2 or > 25 || anchors[^1] - anchors[0] < 30.0f)
        {
            return null;
        }

        // Header anchors must describe every column: a numeric data row is weak
        // evidence for a header, and a body stub left of the first anchor proves
        // the inferred grid omitted a column.
        var numericHeaderCells = rows[0].Items.Count(e =>
        {
            var text = e.Item.Text.Trim();
            return text.Any(char.IsAsciiDigit) && !text.Any(char.IsLetter);
        });

        if (rows[0].Items.All(e => !e.Item.Text.Any(char.IsLetter))
            || numericHeaderCells * 2 > rows[0].Items.Count
            || rows.Skip(1).SelectMany(r => r.Items).Any(e => e.Item.X < anchors[0] - JoinGap))
        {
            return null;
        }

        if (rules.Count == 2)
        {
            // A bounded response form can have only top and bottom rules: the
            // header names both columns, while each prompt row fills the leading
            // column and leaves the response column blank.
            var responseForm = rows.Count >= 5
                && anchors.Count <= 4
                && rows.Skip(1).All(r =>
                    r.Items.Count > 0
                    && r.Items.Count < anchors.Count
                    && r.Items.All(e =>
                        e.Item.Text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length <= 4
                        && MathF.Abs(e.Item.X - anchors[0]) <= JoinGap));

            if (!responseForm)
            {
                return null;
            }
        }
        else if (anchors.Count == 2 && (rules.Count < 5 || rows.Count > rules.Count + 2))
        {
            // Two text columns bracketed by a few decorative rules cannot be
            // told from two-column prose by geometry alone. Only the
            // high-confidence cases are kept: response forms, the stacked-token
            // case, and densely ruled forms where the rule and row counts agree.
            return null;
        }

        if (anchors.Count > 2 && rules.Count > 3)
        {
            // Four or more full-width rules describe row structure rather than a
            // sparse band. First-row anchors alone may start below a real header
            // and would preempt a better hypothesis.
            return null;
        }

        var xMin = MathF.Min(rules.Min(r => r.XMin), anchors[0]);
        var xMax = MathF.Max(rules.Max(r => r.XMax), anchors[^1]);

        if (xMax - xMin < 50.0f)
        {
            return null;
        }

        var columns = new List<float> { xMin };
        for (var i = 0; i + 1 < anchors.Count; i++)
        {
            columns.Add((anchors[i] + anchors[i + 1]) / 2.0f);
        }

        columns.Add(xMax);

        var cells = BuildEmptyCells(rows.Count, anchors.Count);
        var itemIndices = new List<int>();
        var wideItems = 0;
        var measuredItems = 0;

        for (var rowIndex = 0; rowIndex < rows.Count; rowIndex++)
        {
            foreach (var (itemIndex, item) in rows[rowIndex].Items)
            {
                if (NearestAnchorColumn(item, anchors) is not { } column)
                {
                    return null;
                }

                var columnWidth = columns[column + 1] - columns[column];
                if (columnWidth > 0.0f)
                {
                    measuredItems++;
                    if (MathF.Max(item.Width, 0.0f) > columnWidth * 0.72f)
                    {
                        wideItems++;
                    }
                }

                AppendCell(cells, rowIndex, column, item.Text.Trim());
                itemIndices.Add(itemIndex);
            }
        }

        itemIndices = itemIndices.Distinct().Order().ToList();

        var occupiedRows = cells.Count(row => row.Any(cell => cell.Length > 0));
        var occupiedColumns = Enumerable.Range(0, anchors.Count)
            .Count(column => cells.Any(row => row[column].Length > 0));

        if (occupiedRows < 2 || occupiedColumns < 2)
        {
            return null;
        }

        // Sparse rules around a full multi-column text region expose paragraph
        // baselines whose starts repeat at the column margins. What is rejected
        // is sustained prose, not height: a long table of short labels and
        // values stays valid however many rows it has.
        var bodyCells = cells.Skip(1).SelectMany(r => r).Where(c => c.Length > 0).ToList();

        var proseLikeBodyCells = bodyCells.Count(cell =>
        {
            var alphaWords = cell
                .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
                .Count(word => word.Any(char.IsLetter));
            return alphaWords >= 3 && cell.Length >= 12;
        });

        var sustainedSparseProse = rules.Count <= 4
            && rows.Count > (rules.Count * 2) + 2
            && bodyCells.Count > 0
            && proseLikeBodyCells * 2 >= bodyCells.Count;

        if (sustainedSparseProse
            || (anchors.Count >= 3 && rows.Count >= 4 && measuredItems > 0 && wideItems * 3 >= measuredItems))
        {
            Log.Trace(Module, () =>
                $"detect_lines p{page}: rejected unbounded text-anchor candidate " +
                $"({rows.Count} rows, {wideItems} wide of {measuredItems} items)");
            return null;
        }

        // A few long decorative rules can bracket a whole prose region, whose
        // first baseline then looks like a header. Real ruled tables wrap their
        // labels, so the guard is deliberately loose: only an extreme cell, or a
        // sustained concentration of paragraph-sized ones, rejects.
        var nonEmptyCells = cells.SelectMany(r => r).Where(c => c.Length > 0).ToList();
        var longCells = nonEmptyCells.Count(cell => cell.Length > 100);

        if (nonEmptyCells.Any(cell => cell.Length > 240) ||
            (longCells >= 2 && longCells * 5 >= nonEmptyCells.Count))
        {
            Log.Trace(Module, () =>
                $"detect_lines p{page}: rejected prose-like text-anchor candidate " +
                $"({longCells} long of {nonEmptyCells.Count} cells)");
            return null;
        }

        return Table.Create(columns, rows.Select(r => r.Y).ToList(), cells, itemIndices);
    }

    /// <summary>
    /// The special case of a single-anchor band: a header over a stack of
    /// underscore or colon tokens, which is one label and one combined value.
    /// </summary>
    private static Table? BuildStackedTokenTable(List<AnchoredRow> rows, List<HorizontalRule> rules)
    {
        if (rules.Count != 3 || rows.Count < 5 || rows.Any(r => r.Items.Count != 1))
        {
            return null;
        }

        var anchorX = rows[0].Items[0].Item.X;
        if (rows.Any(r => MathF.Abs(r.Items[0].Item.X - anchorX) > JoinGap))
        {
            return null;
        }

        var body = rows.Skip(1).ToList();
        var tokenRows = body.Count(r =>
        {
            var text = r.Items[0].Item.Text.Trim();
            return text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length == 1
                && text.Any(c => c is '_' or ':');
        });

        if (tokenRows * 4 < body.Count * 3)
        {
            return null;
        }

        var header = rows[0].Items[0].Item.Text.Trim();
        var value = string.Join(" ", body.Select(r => r.Items[0].Item.Text.Trim()));

        var itemIndices = rows
            .SelectMany(r => r.Items.Select(e => e.Index))
            .Distinct()
            .Order()
            .ToList();

        var xMin = rules.Min(r => r.XMin);
        var xMax = rules.Max(r => r.XMax);
        var split = xMin + ((xMax - xMin) * 0.35f);

        return Table.Create(
            [xMin, split, xMax],
            [rows[0].Y],
            [[header, value]],
            itemIndices);
    }

    // ── Dense-row anchors ────────────────────────────────────────────────

    /// <summary>
    /// Builds a hypothesis from the densest text row inside a ruled band.
    /// Multi-level booktabs headers put spanning labels on the first baselines
    /// while a later data row exposes every real column, so keying on the first
    /// row alone would collapse the table.
    /// </summary>
    public static Table? BuildDenseRowAnchorTable(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> horizontals,
        List<VerticalRule> verticals,
        uint page)
    {
        var rules = MergeHorizontalSegments(horizontals);
        if (rules.Count < 4 || RulesAreUniformGrid(rules))
        {
            return null;
        }

        var distinctRuleYs = new List<float>();
        foreach (var y in rules.Select(r => r.Y).OrderByDescending(v => v, FloatTotalOrder.Instance))
        {
            if (distinctRuleYs.Count == 0 || MathF.Abs(y - distinctRuleYs[^1]) > YTolerance)
            {
                distinctRuleYs.Add(y);
            }
        }

        // A page of stacked charts contributes one dense numeric row each.
        // Separated bands must not combine into a synthetic page-wide table.
        var ruleGaps = new List<float>();
        for (var i = 0; i + 1 < distinctRuleYs.Count; i++)
        {
            ruleGaps.Add(distinctRuleYs[i] - distinctRuleYs[i + 1]);
        }

        ruleGaps.Sort(FloatTotalOrder.Instance);
        if (ruleGaps.Count > 0)
        {
            var medianGap = ruleGaps[ruleGaps.Count / 2];
            if (medianGap > 0.0f && ruleGaps[^1] > medianGap * 2.5f)
            {
                return null;
            }
        }

        var hasUniformRun = false;
        for (var i = 0; i + 3 < distinctRuleYs.Count && !hasUniformRun; i++)
        {
            float[] spacings =
            [
                distinctRuleYs[i] - distinctRuleYs[i + 1],
                distinctRuleYs[i + 1] - distinctRuleYs[i + 2],
                distinctRuleYs[i + 2] - distinctRuleYs[i + 3],
            ];

            var mean = spacings.Sum() / spacings.Length;
            if (mean <= 0.1f)
            {
                continue;
            }

            var variance = spacings.Sum(s => (s - mean) * (s - mean)) / spacings.Length;
            hasUniformRun = MathF.Sqrt(variance) / mean < 0.02f;
        }

        if (hasUniformRun)
        {
            return null;
        }

        var xMin = rules.Min(r => r.XMin);
        var xMax = rules.Max(r => r.XMax);
        var tableWidth = xMax - xMin;

        if (tableWidth < 100.0f || distinctRuleYs.Count == 0)
        {
            return null;
        }

        var yTop = distinctRuleYs[0];
        var yBottom = distinctRuleYs[^1];

        // Dense-row anchors are a horizontal-rule fallback: a pair of verticals
        // inside this band means the physical-grid hypotheses should own it.
        var bandVerticalXs = verticals
            .Where(v => v.X >= xMin - JoinGap
                && v.X <= xMax + JoinGap
                && v.YMax >= yBottom - YTolerance
                && v.YMin <= yTop + YTolerance)
            .Select(v => v.X)
            .ToList();

        if (RectGrid.SnapEdges(bandVerticalXs, 3.0f).Count >= 2)
        {
            return null;
        }

        if (rules.Count(r => r.Width >= tableWidth * 0.8f) < 2)
        {
            return null;
        }

        var rows = CollectAnchoredRows(items, rules, page);
        if (rows.Count is < 3 or > 30)
        {
            return null;
        }

        // Sparse decorations around prose expose many aligned text starts, but
        // the rule levels do not corroborate that row schema.
        if (rows.Count > (distinctRuleYs.Count * 2) + 2)
        {
            return null;
        }

        var anchors = rows
            .Select(r => LogicalRowAnchors(r.Items))
            .OrderByDescending(a => a.Count)
            .FirstOrDefault();

        if (anchors is null || anchors.Count is < 4 or > 25 ||
            anchors[^1] - anchors[0] < tableWidth * 0.6f)
        {
            return null;
        }

        // Two rows must independently expose nearly the whole schema; one busy
        // line inside a chart or form is not enough.
        var denseThreshold = (anchors.Count * 3 / 4) + ((anchors.Count * 3 % 4) > 0 ? 1 : 0);
        var denseRows = rows.Count(r => MatchedAnchorColumnCount(r.Items, anchors) >= denseThreshold);

        if (denseRows < 2)
        {
            return null;
        }

        var columns = new List<float> { MathF.Min(xMin, anchors[0]) };
        for (var i = 0; i + 1 < anchors.Count; i++)
        {
            columns.Add((anchors[i] + anchors[i + 1]) / 2.0f);
        }

        columns.Add(MathF.Max(xMax, anchors[^1]));

        var cells = BuildEmptyCells(rows.Count, anchors.Count);
        var itemIndices = new List<int>();

        for (var rowIndex = 0; rowIndex < rows.Count; rowIndex++)
        {
            foreach (var (itemIndex, item) in rows[rowIndex].Items)
            {
                if (NearestAnchorColumn(item, anchors) is not { } column)
                {
                    return null;
                }

                AppendCell(cells, rowIndex, column, item.Text.Trim());
                itemIndices.Add(itemIndex);
            }
        }

        itemIndices = itemIndices.Distinct().Order().ToList();

        var bodyRows = cells.Skip(1).ToList();
        var numericCells = bodyRows.SelectMany(r => r).Count(cell => cell.Any(char.IsAsciiDigit));
        var nonEmptyBodyCells = bodyRows.SelectMany(r => r).Count(cell => cell.Length > 0);

        if (numericCells < Math.Min(anchors.Count, 3) || numericCells * 4 < Math.Max(nonEmptyBodyCells, 1))
        {
            return null;
        }

        return Table.Create(columns, rows.Select(r => r.Y).ToList(), cells, itemIndices);
    }

    // ── Open-edge grids ──────────────────────────────────────────────────

    /// <summary>
    /// Recovers grids whose horizontal rules give the outer bounds while the
    /// vertical strokes draw only the internal dividers. A header may sit just
    /// above the top rule, as presentation tables commonly set it.
    /// </summary>
    public static List<Table> BuildOpenEdgeGridTables(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> horizontals,
        List<VerticalRule> verticals,
        uint page)
    {
        var logicalRules = MergeHorizontalSegments(horizontals);

        return [.. GroupRulesBySpan(logicalRules)
            .SelectMany(spanGroup => SplitIndependentRuleRuns(spanGroup, items, page))
            .Where(rules => rules.Count >= 3)
            .Select(rules => BuildOpenEdgeGridTableForRules(items, logicalRules, rules, verticals, page))
            .Where(table => table is not null)
            .Select(table => table!)];
    }

    private static Table? BuildOpenEdgeGridTableForRules(
        IReadOnlyList<TextItem> items,
        List<HorizontalRule> logicalRules,
        List<HorizontalRule> rules,
        List<VerticalRule> verticals,
        uint page)
    {
        var xMin = rules.Min(r => r.XMin);
        var xMax = rules.Max(r => r.XMax);
        var yTop = rules.Max(r => r.Y);
        var yBottom = rules.Min(r => r.Y);
        var width = xMax - xMin;
        var height = yTop - yBottom;

        if (width < 100.0f || height < 20.0f)
        {
            return null;
        }

        var scopedVerticalXs = verticals
            .Where(v => v.X > xMin + JoinGap
                && v.X < xMax - JoinGap
                && v.YMin <= yBottom + YTolerance
                && v.YMax >= yTop - YTolerance
                && v.Height >= height * 0.8f)
            .Select(v => v.X)
            .ToList();

        var interiorEdges = RectGrid.SnapEdges(scopedVerticalXs, 3.0f);
        if (interiorEdges.Count is < 1 or > 24)
        {
            return null;
        }

        var colEdges = new List<float>(interiorEdges.Count + 2) { xMin };
        colEdges.AddRange(interiorEdges);
        colEdges.Add(xMax);

        var rowEdges = RectGrid.SnapEdges(rules.Select(r => r.Y).ToList(), 3.0f);
        rowEdges.Sort((a, b) => FloatTotalOrder.Instance.Compare(b, a));

        if (rowEdges.Count < 3)
        {
            return null;
        }

        var (bodyCells, itemIndices) = RectGrid.AssignItemsToGrid(items, colEdges, rowEdges, page);
        var columnCount = colEdges.Count - 1;

        var occupiedBodyRows = bodyCells.Count(row => row.Any(cell => cell.Length > 0));
        var occupiedBodyColumns = Enumerable.Range(0, columnCount)
            .Count(column => bodyCells.Any(row => row[column].Length > 0));

        if (occupiedBodyRows < 2 || occupiedBodyColumns != columnCount)
        {
            return null;
        }

        List<HorizontalRule> headerBand =
        [
            new(yTop + 30.0f, xMin, xMax),
            new(yTop + YTolerance, xMin, xMax),
        ];

        var headerRows = CollectAnchoredRows(items, headerBand, page);
        if (headerRows.Count == 0)
        {
            return null;
        }

        var headerY = headerRows[0].Y;
        var headerCells = new List<string>(new string[columnCount].Select(_ => string.Empty));
        var headerIndices = new List<int>();

        foreach (var headerRow in headerRows)
        {
            foreach (var (itemIndex, item) in headerRow.Items)
            {
                var centerX = item.X + (item.Width / 2.0f);
                int? column = null;

                for (var index = 0; index < columnCount; index++)
                {
                    if (centerX >= colEdges[index] && centerX <= colEdges[index + 1])
                    {
                        column = index;
                        break;
                    }
                }

                if (column is not { } c)
                {
                    return null;
                }

                if (headerCells[c].Length > 0)
                {
                    headerCells[c] += " ";
                }

                headerCells[c] += item.Text.Trim();
                headerIndices.Add(itemIndex);
            }
        }

        var mixedRuleSpanInBand = logicalRules.Any(rule =>
            rule.Y >= yBottom - YTolerance
            && rule.Y <= yTop + YTolerance
            && rule.XMax >= xMin - JoinGap
            && rule.XMin <= xMax + JoinGap
            && !rules.Contains(rule));

        // The first column may be an unlabelled row-header stub or a normal
        // labelled column. A fully populated header is less distinctive, so it
        // is accepted only when every logical rule corroborates the same band;
        // mixed spans belong to the physical-grid detector.
        if (headerCells.Skip(1).Any(c => c.Length == 0)
            || (headerCells[0].Length > 0 && mixedRuleSpanInBand))
        {
            return null;
        }

        var cells = new List<List<string>>(bodyCells.Count + 1) { headerCells };
        cells.AddRange(bodyCells);

        itemIndices.AddRange(headerIndices);
        itemIndices = itemIndices.Distinct().Order().ToList();

        var rows = new List<float>(rowEdges.Count) { headerY };
        rows.AddRange(rowEdges[..^1]);

        return Table.Create(colEdges, rows, cells, itemIndices);
    }

    // ── Shared helpers ───────────────────────────────────────────────────

    private static List<List<string>> BuildEmptyCells(int rowCount, int columnCount)
    {
        var cells = new List<List<string>>(rowCount);
        for (var r = 0; r < rowCount; r++)
        {
            var row = new List<string>(columnCount);
            for (var c = 0; c < columnCount; c++)
            {
                row.Add(string.Empty);
            }

            cells.Add(row);
        }

        return cells;
    }

    private static void AppendCell(List<List<string>> cells, int row, int column, string text)
    {
        if (cells[row][column].Length > 0)
        {
            cells[row][column] += " ";
        }

        cells[row][column] += text;
    }
}
