// Ported from reference/src/tables/mod.rs
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Region-scoped two-column key/value table construction from text baselines.
/// This deliberately lives outside the full-page heuristic detector: layout
/// callers have already supplied a table-shaped bbox, and some real table
/// regions are plain product or spec forms with only two visual columns. The
/// main column fallback starts at four columns to avoid newspaper and prose
/// false positives, so this path keeps tighter key/value-specific guards
/// instead.
/// </summary>
internal static class KeyValueTables
{
    private const string Module = "tables";

    /// <summary>A page item paired with its index in the caller's list.</summary>
    private sealed class RowItem
    {
        public required int Index { get; init; }

        public required TextItem Item { get; init; }
    }

    /// <summary>Items sharing a baseline, left to right.</summary>
    private sealed class VisualRow
    {
        public float Y { get; set; }

        public List<RowItem> Items { get; } = [];
    }

    /// <summary>A visual row split into its key and value halves.</summary>
    private sealed class KeyValueRow
    {
        public float Y { get; init; }

        public string Left { get; set; } = string.Empty;

        public string Right { get; set; } = string.Empty;

        public List<int> ItemIndices { get; init; } = [];
    }

    /// <summary>Row-shape counts the single-pair allowance weighs.</summary>
    private readonly record struct KeyValueSinglePairStats(
        int PairedRows,
        int SectionRows,
        int RawLeftOnlyRows,
        int RawRightOnlyRows);

    /// <summary>Builds a two-column key/value table from the region's text baselines.</summary>
    public static Table? TryBuildKeyValueTableFromRows(IReadOnlyList<TextItem> items, uint page)
    {
        var pageItems = new List<RowItem>();
        for (var idx = 0; idx < items.Count; idx++)
        {
            var item = items[idx];
            if (item.Page == page && item.Text.Trim().Length > 0)
            {
                pageItems.Add(new RowItem { Index = idx, Item = item });
            }
        }

        if (pageItems.Count < 2)
        {
            return null;
        }

        var medianFontSize = MathF.Max(
            MedianF32(pageItems.Select(ri => ri.Item.FontSize).ToList()) ?? 10.0f,
            1.0f);
        var yTol = Math.Clamp(medianFontSize * 0.75f, 4.0f, 9.0f);
        var rows = GroupKeyValueVisualRows(pageItems, yTol);
        if (rows.Count == 0 || rows.Count > 80)
        {
            return null;
        }

        if (InferKeyValueSplitX(rows, medianFontSize) is not { } splitX)
        {
            return null;
        }

        var kvRows = new List<KeyValueRow>();
        var leftStarts = new List<float>();
        var rightStarts = new List<float>();

        foreach (var row in rows)
        {
            var leftItems = row.Items.Where(i => i.Item.X < splitX).ToList();
            var rightItems = row.Items.Where(i => i.Item.X >= splitX).ToList();

            var left = JoinRowItemText(leftItems);
            var right = JoinRowItemText(rightItems);
            if (left.Length == 0 && right.Length == 0)
            {
                continue;
            }

            var itemIndices = row.Items.Select(ri => ri.Index).Distinct().OrderBy(i => i).ToList();

            if (left.Length > 0 && right.Length > 0)
            {
                if (leftItems.Count > 0)
                {
                    leftStarts.Add(leftItems[0].Item.X);
                }

                if (rightItems.Count > 0)
                {
                    rightStarts.Add(rightItems[0].Item.X);
                }
            }

            kvRows.Add(new KeyValueRow
            {
                Y = row.Y,
                Left = left,
                Right = right,
                ItemIndices = itemIndices,
            });
        }

        if (kvRows.Count == 0)
        {
            return null;
        }

        var rawLeftOnlyRows = kvRows.Count(r => r.Left.Length > 0 && r.Right.Length == 0);
        var rawRightOnlyRows = kvRows.Count(r => r.Left.Length == 0 && r.Right.Length > 0);
        var edgarTagRows = KeyValueRowsLookLikeEdgarTags(kvRows);
        if (edgarTagRows)
        {
            kvRows.RemoveAll(r => r.Right.Length == 0 && IsEdgarTableBoundaryCell(r.Left));
        }

        var headerInferred = !edgarTagRows && KeyValueFirstPairIsHeader(kvRows);
        kvRows = NormalizeKeyValueRows(kvRows, headerInferred);

        var pairedRows = kvRows.Count(r => r.Left.Length > 0 && r.Right.Length > 0);
        var sectionRows = kvRows.Count(r => r.Left.Length > 0 && r.Right.Length == 0);
        var danglingRightRows = kvRows.Count(r => r.Left.Length == 0 && r.Right.Length > 0);
        var leftLabelLike = kvRows
            .Where(r => r.Left.Length > 0 && r.Right.Length > 0)
            .Count(r => LooksLikeKeyValueLabel(r.Left));

        if (pairedRows < 1 || danglingRightRows > 0)
        {
            return null;
        }

        var leftX = MedianF32(leftStarts)
            ?? rows.SelectMany(r => r.Items.Select(ri => ri.Item.X)).Aggregate(float.PositiveInfinity, MathF.Min);
        var rightX = MedianF32(rightStarts) ?? splitX;
        if (!float.IsFinite(leftX) || !float.IsFinite(rightX) || rightX - leftX < 40.0f)
        {
            return null;
        }

        var singlePairAllowed = KeyValueSinglePairAllowed(
            new KeyValueSinglePairStats(pairedRows, sectionRows, rawLeftOnlyRows, rawRightOnlyRows),
            kvRows,
            headerInferred,
            leftX,
            rightX);
        if ((kvRows.Count < 2 || pairedRows < 2) && !singlePairAllowed)
        {
            return null;
        }

        var dataPairs = headerInferred ? Math.Max(pairedRows - 1, 0) : pairedRows;
        if (dataPairs < 1)
        {
            return null;
        }

        if (sectionRows > (pairedRows * 2) + 2 && !singlePairAllowed)
        {
            return null;
        }

        var labelRowsForScore = headerInferred ? Math.Max(pairedRows - 1, 0) : pairedRows;
        var labelLikeForScore = headerInferred && kvRows.Count > 0
            ? Math.Max(leftLabelLike - 1, 0)
            : leftLabelLike;
        if (!headerInferred
            && !edgarTagRows
            && labelRowsForScore >= 2
            && labelLikeForScore * 2 < labelRowsForScore)
        {
            return null;
        }

        // A right side with many distinct x clusters is a matrix, not a value
        // column; so is a compact marker column repeated down the page.
        var rightClusterCount = SignificantSideXClusters(rows, splitX, leftSide: false);
        var markerRows = MarkerMatrixValueRows(kvRows);
        if (!singlePairAllowed
            && !edgarTagRows
            && ((rightClusterCount >= 5 && pairedRows >= 3)
                || (rightClusterCount >= 3 && markerRows >= 3 && markerRows * 2 >= pairedRows)))
        {
            return null;
        }

        if (!edgarTagRows && KeyValueRowsLookLikeProse(kvRows, headerInferred))
        {
            return null;
        }

        var tableRows = new List<float>();
        var cells = new List<List<string>>();
        var itemIndicesOut = new List<int>();

        var startIdx = 0;
        if (headerInferred)
        {
            var header = kvRows[0];
            tableRows.Add(header.Y);
            cells.Add([header.Left, header.Right]);
            itemIndicesOut.AddRange(header.ItemIndices);
            startIdx = 1;
        }
        else
        {
            tableRows.Add(kvRows.Count > 0 ? kvRows[0].Y + yTol : 0.0f);
            cells.Add(["Field", "Value"]);
        }

        foreach (var row in kvRows.Skip(startIdx))
        {
            if (row.Left.Length > 0 && row.Right.Length > 0)
            {
                tableRows.Add(row.Y);
                cells.Add([row.Left, row.Right]);
                itemIndicesOut.AddRange(row.ItemIndices);
            }
            else if (row.Left.Length > 0)
            {
                tableRows.Add(row.Y);
                cells.Add(["Section", row.Left]);
                itemIndicesOut.AddRange(row.ItemIndices);
            }
            else if (row.Right.Length > 0 && cells.Count > 0 && cells[^1].Count > 1)
            {
                var last = cells[^1];
                last[1] = last[1].Trim().Length > 0 ? last[1] + " " + row.Right : last[1] + row.Right;
                itemIndicesOut.AddRange(row.ItemIndices);
            }
        }

        if (cells.Count < 2)
        {
            return null;
        }

        var finalIndices = itemIndicesOut.Distinct().OrderBy(i => i).ToList();

        Log.Debug(Module, () =>
            $"key-value table: {cells.Count} rows, pairs={pairedRows}, sections={sectionRows}, split_x={splitX:F1}");

        return Table.Create([leftX, rightX], tableRows, cells, finalIndices);
    }

    /// <summary>
    /// Folds continuation rows into the pair above them: a value-only row extends
    /// the previous value, and a label-only row extends the previous label when it
    /// reads as a wrap rather than a new entry.
    /// </summary>
    private static List<KeyValueRow> NormalizeKeyValueRows(List<KeyValueRow> rows, bool headerInferred)
    {
        var normalized = new List<KeyValueRow>(rows.Count);

        foreach (var row in rows)
        {
            if (row.Left.Length == 0 && row.Right.Length == 0)
            {
                continue;
            }

            if (row.Left.Length == 0 && row.Right.Length > 0)
            {
                if (normalized.Count > 0 && normalized[^1].Right.Length > 0)
                {
                    var last = normalized[^1];
                    last.Right = AppendKeyValueText(last.Right, row.Right);
                    last.ItemIndices.AddRange(row.ItemIndices);
                    continue;
                }

                normalized.Add(row);
                continue;
            }

            if (row.Left.Length > 0 && row.Right.Length == 0 && normalized.Count > 0)
            {
                var last = normalized[^1];
                var lastIsHeader = headerInferred && normalized.Count == 1;
                if (!lastIsHeader
                    && last.Left.Length > 0
                    && last.Right.Length > 0
                    && KeyValueLeftContinuationAllowed(last.Left, row.Left))
                {
                    last.Left = AppendKeyValueText(last.Left, row.Left);
                    last.ItemIndices.AddRange(row.ItemIndices);
                    continue;
                }
            }

            normalized.Add(row);
        }

        return normalized;
    }

    /// <summary>Appends text to a cell, inserting a space only where one is needed.</summary>
    private static string AppendKeyValueText(string target, string addition)
    {
        var trimmed = addition.Trim();
        if (trimmed.Length == 0)
        {
            return target;
        }

        return target.Trim().Length > 0 ? target + " " + trimmed : target + trimmed;
    }

    /// <summary>
    /// True when a label-only row continues the label above it rather than
    /// starting a new entry: the previous label breaks mid-phrase, or the
    /// continuation opens lowercase, or it is simply too long to be a new label.
    /// </summary>
    private static bool KeyValueLeftContinuationAllowed(string previousLeft, string continuation)
    {
        var trimmed = continuation.Trim();
        if (trimmed.Length == 0 || LooksLikeKeyValueSectionLabel(trimmed))
        {
            return false;
        }

        var previous = previousLeft.TrimEnd();
        var continuationChars = TextUtils.CharCount(trimmed);
        var continuationWords = WordCountSimple(trimmed);
        return (previous.Length > 0 && previous[^1] is '-' or '/' or ',' or ';' or ':')
            || FirstAlphaIsLowercase(trimmed)
            || continuationChars > 28
            || continuationWords > 4;
    }

    /// <summary>Groups items into baseline rows, ordered top to bottom.</summary>
    private static List<VisualRow> GroupKeyValueVisualRows(List<RowItem> items, float yTol)
    {
        items.Sort((a, b) =>
        {
            var cmp = FloatTotalOrder.Instance.Compare(b.Item.Y, a.Item.Y);
            return cmp != 0 ? cmp : FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X);
        });

        var rows = new List<VisualRow>();
        foreach (var rowItem in items)
        {
            var existing = rows.FirstOrDefault(row => MathF.Abs(row.Y - rowItem.Item.Y) <= yTol);
            if (existing is not null)
            {
                var len = existing.Items.Count;
                existing.Y = ((existing.Y * len) + rowItem.Item.Y) / (len + 1.0f);
                existing.Items.Add(rowItem);
                continue;
            }

            var row = new VisualRow { Y = rowItem.Item.Y };
            row.Items.Add(rowItem);
            rows.Add(row);
        }

        foreach (var row in rows)
        {
            row.Items.Sort((a, b) => FloatTotalOrder.Instance.Compare(a.Item.X, b.Item.X));
        }

        rows.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Y, a.Y));
        return rows;
    }

    /// <summary>
    /// Infers the x that separates keys from values: the median of each row's
    /// widest inter-item gap, once that gap is wide enough to be a real gutter.
    /// </summary>
    private static float? InferKeyValueSplitX(List<VisualRow> rows, float medianFontSize)
    {
        var minGap = MathF.Max(medianFontSize * 2.0f, 24.0f);
        var splits = new List<float>();

        foreach (var row in rows)
        {
            if (row.Items.Count < 2)
            {
                continue;
            }

            var bestGap = 0.0f;
            float? bestSplit = null;
            for (var i = 0; i + 1 < row.Items.Count; i++)
            {
                var left = row.Items[i].Item;
                var right = row.Items[i + 1].Item;
                var leftRight = left.X + MathF.Max(left.Width, 0.0f);
                var gap = right.X - leftRight;
                if (gap > bestGap)
                {
                    bestGap = gap;
                    bestSplit = leftRight + (gap / 2.0f);
                }
            }

            if (bestGap >= minGap && bestSplit is { } split)
            {
                splits.Add(split);
            }
        }

        if (splits.Count < 2)
        {
            // A single-pair region still counts when there is exactly one paired
            // visual row and nothing wider anywhere on the page.
            var pairedVisualRows = rows.Count(row => row.Items.Count >= 2);
            if (splits.Count == 1
                && pairedVisualRows == 1
                && (rows.Count == 1 || rows.All(row => row.Items.Count <= 2)))
            {
                return splits[0];
            }

            return null;
        }

        return MedianF32(splits);
    }

    /// <summary>Joins a side's item text into one normalised cell string.</summary>
    private static string JoinRowItemText(List<RowItem> items)
    {
        var parts = items.Select(i => i.Item.Text.Trim()).Where(t => t.Length > 0);
        return NormalizeCellText(string.Join(' ', parts));
    }

    /// <summary>Collapses whitespace runs to single spaces.</summary>
    private static string NormalizeCellText(string text) =>
        string.Join(' ', text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));

    /// <summary>True when the first pair reads as a header row rather than data.</summary>
    private static bool KeyValueFirstPairIsHeader(List<KeyValueRow> rows)
    {
        if (rows.Count == 0)
        {
            return false;
        }

        var first = rows[0];
        if (first.Left.Length == 0 || first.Right.Length == 0)
        {
            return false;
        }

        if (!LooksLikeKeyValueHeaderCell(first.Left) || !LooksLikeKeyValueHeaderCell(first.Right))
        {
            return false;
        }

        return rows.Skip(1).Any(row => row.Left.Length > 0 && row.Right.Length > 0);
    }

    /// <summary>True for a short, digit-free, unpunctuated phrase — a column label.</summary>
    private static bool LooksLikeKeyValueHeaderCell(string cell)
    {
        var trimmed = cell.Trim();
        if (trimmed.Length is < 2 or > 40)
        {
            return false;
        }

        if (WordCountSimple(trimmed) is < 1 or > 4)
        {
            return false;
        }

        var lower = trimmed.ToLowerInvariant();
        if (lower is "yes" or "no" or "true" or "false" or "none" or "n/a" or "na")
        {
            return false;
        }

        return trimmed.Any(char.IsLetter)
            && !trimmed.Any(char.IsAsciiDigit)
            && !(trimmed.Length > 0 && trimmed[^1] is '.' or ',' or ';' or ':');
    }

    /// <summary>True for a plausible key cell: a short unterminated phrase with letters.</summary>
    private static bool LooksLikeKeyValueLabel(string cell)
    {
        var trimmed = cell.Trim();
        if (trimmed.Length is < 2 or > 90)
        {
            return false;
        }

        var words = WordCountSimple(trimmed);
        if (words is 0 or > 10)
        {
            return false;
        }

        if (trimmed.Length > 0 && trimmed[^1] is '.' or ',' or ';')
        {
            return false;
        }

        return trimmed.Any(char.IsLetter);
    }

    /// <summary>
    /// True for the SGML-style tag rows an EDGAR filing emits — <c>&lt;S&gt;</c>
    /// beside <c>&lt;C&gt;</c> and friends. Those keep their pairing even though
    /// the ordinary label guards would reject them.
    /// </summary>
    private static bool KeyValueRowsLookLikeEdgarTags(List<KeyValueRow> rows)
    {
        var pairedRows = rows.Count(r => r.Left.Length > 0 && r.Right.Length > 0);
        if (pairedRows < 2)
        {
            return false;
        }

        var tagPairs = rows
            .Where(r => r.Left.Length > 0 && r.Right.Length > 0)
            .Count(r => IsEdgarTagCell(r.Left));
        var firstMarker = rows.Count > 0
            && string.Equals(rows[0].Left, "<S>", StringComparison.OrdinalIgnoreCase)
            && string.Equals(rows[0].Right, "<C>", StringComparison.OrdinalIgnoreCase);

        return tagPairs >= 3 || (firstMarker && tagPairs >= 2);
    }

    /// <summary>True when the cell is an angle-bracketed uppercase tag.</summary>
    private static bool IsEdgarTagCell(string cell)
    {
        var trimmed = cell.Trim();
        if (!trimmed.StartsWith('<') || !trimmed.EndsWith('>') || trimmed.Length < 2)
        {
            return false;
        }

        var inner = trimmed[1..^1];
        return inner.Length is > 0 and <= 48
            && inner.All(ch => char.IsAsciiLetterUpper(ch) || char.IsAsciiDigit(ch) || ch is '-' or '_');
    }

    /// <summary>True for the <c>&lt;TABLE&gt;</c> markers that bracket an EDGAR table.</summary>
    private static bool IsEdgarTableBoundaryCell(string cell)
    {
        var trimmed = cell.Trim();
        return string.Equals(trimmed, "<TABLE>", StringComparison.OrdinalIgnoreCase)
            || string.Equals(trimmed, "</TABLE>", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>
    /// Whether a lone key/value pair is enough to call the region a table. It
    /// takes a wide gutter plus either a label facing a compact scalar, or a label
    /// facing a value that wrapped across several rows.
    /// </summary>
    private static bool KeyValueSinglePairAllowed(
        KeyValueSinglePairStats stats,
        List<KeyValueRow> rows,
        bool headerInferred,
        float leftX,
        float rightX)
    {
        if (headerInferred || stats.PairedRows != 1 || stats.SectionRows != 0 || rows.Count != 1)
        {
            return false;
        }

        if (rightX - leftX < 60.0f)
        {
            return false;
        }

        var row = rows.FirstOrDefault(r => r.Left.Length > 0 && r.Right.Length > 0);
        if (row is null)
        {
            return false;
        }

        var leftChars = TextUtils.CharCount(row.Left);
        var rightChars = TextUtils.CharCount(row.Right);
        if (leftChars is < 2 or > 120 || rightChars == 0)
        {
            return false;
        }

        if (KeyValueCellLooksLikeSentence(row.Left))
        {
            return false;
        }

        if (stats.RawLeftOnlyRows == 0
            && stats.RawRightOnlyRows >= 2
            && leftChars <= 70
            && rightChars <= 1_500
            && LooksLikeKeyValueLabel(row.Left))
        {
            return true;
        }

        if (rightChars > 80)
        {
            return false;
        }

        if (KeyValueCellLooksLikeSentence(row.Right) && !CompactKeyValueScalar(row.Right))
        {
            return false;
        }

        return (LooksLikeKeyValueLabel(row.Left) || leftChars <= 90) && CompactKeyValueScalar(row.Right);
    }

    /// <summary>True for a short value cell: a number, a yes/no word, or a few words.</summary>
    private static bool CompactKeyValueScalar(string cell)
    {
        var trimmed = cell.Trim();
        var chars = TextUtils.CharCount(trimmed);
        var words = WordCountSimple(trimmed);
        if (trimmed.Length == 0 || chars > 60 || words > 6 || trimmed[^1] is '.' or '!' or '?')
        {
            return false;
        }

        var lower = trimmed.ToLowerInvariant();
        return trimmed.Any(char.IsAsciiDigit)
            || lower is "yes" or "no" or "true" or "false" or "none" or "n/a" or "na"
            || words <= 4;
    }

    /// <summary>True for a short capitalised heading that opens a group of pairs.</summary>
    private static bool LooksLikeKeyValueSectionLabel(string cell)
    {
        var trimmed = cell.Trim();
        var chars = TextUtils.CharCount(trimmed);
        var words = WordCountSimple(trimmed);
        if (words is < 1 or > 5 || chars is < 2 or > 48)
        {
            return false;
        }

        if ((trimmed.Length > 0 && trimmed[^1] is '.' or ',' or ';' or ':') || FirstAlphaIsLowercase(trimmed))
        {
            return false;
        }

        if (trimmed.Any(ch => ch is '.' or ',' or ';' or '(' or ')' or '[' or ']'))
        {
            return false;
        }

        return trimmed.Any(char.IsLetter);
    }

    /// <summary>True when the cell's first letter is lowercase.</summary>
    private static bool FirstAlphaIsLowercase(string cell)
    {
        foreach (var ch in cell)
        {
            if (char.IsLetter(ch))
            {
                return char.IsLower(ch);
            }
        }

        return false;
    }

    /// <summary>True when the cell is long or terminated enough to read as prose.</summary>
    private static bool KeyValueCellLooksLikeSentence(string cell)
    {
        var trimmed = cell.Trim();
        var chars = TextUtils.CharCount(trimmed);
        return chars > 90
            || WordCountSimple(trimmed) > 12
            || (chars > 42 && trimmed.Length > 0 && trimmed[^1] is '.' or '!' or '?');
    }

    /// <summary>
    /// True when the rows read as running prose rather than key/value pairs. Two
    /// columns of wrapped paragraph text produce the same surface shape as a spec
    /// form, so cell length and sentence structure have to tell them apart.
    /// </summary>
    private static bool KeyValueRowsLookLikeProse(List<KeyValueRow> rows, bool headerInferred)
    {
        var leftCells = 0;
        var leftProseCells = 0;
        var leftLabelLike = 0;
        var totalLeftChars = 0;
        var pairedRows = 0;
        var pairedSentenceRows = 0;
        var soloProseRows = 0;

        foreach (var row in rows.Skip(headerInferred ? 1 : 0))
        {
            if (row.Left.Length > 0 && row.Right.Length > 0)
            {
                pairedRows++;
                var left = row.Left.Trim();
                var right = row.Right.Trim();
                var leftProse = KeyValueCellLooksLikeSentence(left);
                var rightProse = KeyValueCellLooksLikeSentence(right);
                leftCells++;
                totalLeftChars += TextUtils.CharCount(left);
                if (LooksLikeKeyValueLabel(left))
                {
                    leftLabelLike++;
                }

                if (leftProse)
                {
                    leftProseCells++;
                }

                if (leftProse && rightProse)
                {
                    pairedSentenceRows++;
                }
            }
            else
            {
                var solo = (row.Left.Length == 0 ? row.Right : row.Left).Trim();
                var soloChars = TextUtils.CharCount(solo);
                if (soloChars > 70
                    || WordCountSimple(solo) > 9
                    || (soloChars > 35 && solo.Length > 0 && solo[^1] is '.' or '!' or '?'))
                {
                    soloProseRows++;
                }
            }
        }

        if (pairedRows < 1 || leftCells == 0)
        {
            return true;
        }

        if (soloProseRows >= 3)
        {
            return true;
        }

        if (pairedRows >= 2 && pairedSentenceRows * 2 >= pairedRows)
        {
            return true;
        }

        if (!headerInferred && leftProseCells * 2 >= leftCells)
        {
            return true;
        }

        var avgLeftChars = totalLeftChars / (float)leftCells;
        return !headerInferred && avgLeftChars > 70.0f && leftLabelLike * 2 < leftCells;
    }

    /// <summary>Counts rows whose value cell is a bare marker or number.</summary>
    private static int MarkerMatrixValueRows(List<KeyValueRow> rows) =>
        rows.Count(row => row.Left.Length > 0 && CompactMarkerValue(row.Right));

    /// <summary>True for a letter-free cell built from digits or bullet glyphs.</summary>
    private static bool CompactMarkerValue(string cell)
    {
        var trimmed = cell.Trim();
        if (trimmed.Length == 0 || TextUtils.CharCount(trimmed) > 80)
        {
            return false;
        }

        if (trimmed.Any(char.IsLetter))
        {
            return false;
        }

        return trimmed.Any(ch => char.IsAsciiDigit(ch) || ch is '•' or '●' or '·');
    }

    /// <summary>
    /// Counts the x clusters on one side of the split that hold two or more items.
    /// Several such clusters on the value side mean a matrix, not a value column.
    /// </summary>
    private static int SignificantSideXClusters(List<VisualRow> rows, float splitX, bool leftSide)
    {
        var xs = new List<float>();
        foreach (var row in rows)
        {
            foreach (var item in row.Items)
            {
                if ((item.Item.X < splitX) == leftSide)
                {
                    xs.Add(item.Item.X);
                }
            }
        }

        xs.Sort(FloatTotalOrder.Instance);

        var counts = new List<int>();
        float? center = null;
        var count = 0;
        foreach (var x in xs)
        {
            if (center is { } current && MathF.Abs(x - current) <= 8.0f)
            {
                center = ((current * count) + x) / (count + 1.0f);
                count++;
            }
            else if (center is not null)
            {
                counts.Add(count);
                center = x;
                count = 1;
            }
            else
            {
                center = x;
                count = 1;
            }
        }

        if (count > 0)
        {
            counts.Add(count);
        }

        return counts.Count(c => c >= 2);
    }

    /// <summary>Counts whitespace-separated words containing at least one alphanumeric.</summary>
    private static int WordCountSimple(string cell) => cell
        .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
        .Count(word => word.Any(char.IsLetterOrDigit));

    /// <summary>The median of the finite values, or null when none are finite.</summary>
    private static float? MedianF32(List<float> values)
    {
        var finite = values.Where(float.IsFinite).OrderBy(v => v, FloatTotalOrder.Instance).ToList();
        return finite.Count == 0 ? null : finite[finite.Count / 2];
    }
}
