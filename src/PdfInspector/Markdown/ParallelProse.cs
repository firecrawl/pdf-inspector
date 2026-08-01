// Ported from reference/src/markdown/mod.rs
using PdfInspector.Tables;
using PdfInspector.Text;

namespace PdfInspector.Markdown;

/// <summary>
/// The parallel-prose test that guards chart pages. A heuristic grid can project
/// two independent prose columns onto one table; this rejects only that shape,
/// which is deliberately narrower than disabling body-font detection for a whole
/// page. Numeric, compact, headed and otherwise table-shaped candidates stay
/// eligible.
/// </summary>
internal static class ParallelProse
{
    private const string Module = "markdown";

    /// <summary>
    /// True when adjacent physical rows form an unterminated, lowercase prose
    /// continuation in the same projected column.
    /// </summary>
    private static bool IsCrossRowProseContinuation(string previous, string current)
    {
        previous = previous.Trim();
        current = current.Trim();
        if (previous.Length == 0 || current.Length == 0)
        {
            return false;
        }

        var withoutClosers = previous.TrimEnd('"', '\'', '”', ')', ']');
        var previousIsOpen = withoutClosers.Length > 0
            && withoutClosers[^1] is not ('.' or '!' or '?' or ':' or ';');

        var currentStartsAsContinuation = false;
        foreach (var ch in current)
        {
            if (char.IsLetter(ch))
            {
                currentStartsAsContinuation = char.IsLower(ch);
                break;
            }
        }

        return previousIsOpen && currentStartsAsContinuation;
    }

    /// <summary>
    /// True for a section-numbered heading such as "3.2 Measurement Methods".
    /// One embedded in a candidate is strong evidence that a heuristic grid has
    /// captured page prose rather than a real table.
    /// </summary>
    private static bool LooksLikeNumberedSectionHeading(string text)
    {
        var trimmed = text.Trim();
        var spaceIdx = trimmed.IndexOfAny([' ', '\t', '\n', '\r', '\f', '\v']);
        if (spaceIdx <= 0)
        {
            return false;
        }

        var prefix = trimmed[..spaceIdx].TrimEnd('.');
        var groupCount = 0;
        foreach (var group in prefix.Split('.'))
        {
            if (group.Length is 0 or > 3 || !group.All(char.IsAsciiDigit))
            {
                return false;
            }

            groupCount++;
        }

        var title = trimmed[(spaceIdx + 1)..].Trim();
        if (groupCount is < 1 or > 4
            || title.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length < 3)
        {
            return false;
        }

        foreach (var ch in title)
        {
            if (char.IsLetter(ch))
            {
                return char.IsUpper(ch);
            }
        }

        return false;
    }

    /// <summary>True when a candidate's cells are overwhelmingly parallel prose fragments.</summary>
    public static bool IsParallelProseTable(Table table)
    {
        if (table.Kind != TableKind.Data || table.Columns.Count is < 2 or > 3 || table.Rows.Count < 3)
        {
            return false;
        }

        var nonEmpty = 0;
        var longProse = 0;
        var rowsWithParallelProse = 0;
        var occupiedRows = 0;

        var hasNumberedSectionHeading = table.Cells.SelectMany(row => row).Any(LooksLikeNumberedSectionHeading);

        var firstFilledRow = table.Cells.FirstOrDefault(row => row.Any(cell => cell.Trim().Length > 0));
        var hasCompactHeader = false;
        if (firstFilledRow is not null)
        {
            var filled = firstFilledRow.Where(cell => cell.Trim().Length > 0).ToList();
            hasCompactHeader = filled.Count >= 2
                && filled.All(cell =>
                    cell.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length <= 4
                    && TextUtils.CharCount(cell.Trim()) <= 28);
        }

        foreach (var row in table.Cells)
        {
            var rowLongProse = 0;
            var rowNonEmpty = 0;

            foreach (var cell in row)
            {
                var text = cell.Trim();
                if (text.Length == 0)
                {
                    continue;
                }

                nonEmpty++;
                rowNonEmpty++;

                var chars = text.Count(ch => !char.IsWhiteSpace(ch));
                var alphabetic = text.Count(char.IsLetter);
                var words = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;
                if (chars >= 28 && words >= 5 && alphabetic * 5 >= chars * 3)
                {
                    longProse++;
                    rowLongProse++;
                }
            }

            if (rowLongProse >= 2)
            {
                rowsWithParallelProse++;
            }

            if (rowNonEmpty > 0)
            {
                occupiedRows++;
            }
        }

        // A lowercase cell is not continuation evidence on its own: legitimate
        // headerless tables often use sentence fragments as row values. What matters
        // is a direct physical-row transition from an unterminated cell in the same
        // column — the shape produced when independent prose columns are accidentally
        // projected onto one grid.
        var continuationFragments = 0;
        var continuationColumns = new bool[table.Columns.Count];
        for (var r = 0; r + 1 < table.Cells.Count; r++)
        {
            for (var column = 0; column < continuationColumns.Length; column++)
            {
                var previous = column < table.Cells[r].Count ? table.Cells[r][column] : string.Empty;
                var current = column < table.Cells[r + 1].Count ? table.Cells[r + 1][column] : string.Empty;
                if (IsCrossRowProseContinuation(previous, current))
                {
                    continuationFragments++;
                    continuationColumns[column] = true;
                }
            }
        }

        var continuationColumnCount = continuationColumns.Count(v => v);

        var isParallel = !hasCompactHeader
            && nonEmpty >= 5

            // Independent prose columns break lines and paragraphs asynchronously, so
            // a fully populated grid is positive evidence for a real descriptive table
            // even when every value is a lowercase sentence fragment.
            && nonEmpty < table.Cells.Count * table.Columns.Count
            && longProse >= 4
            && longProse * 5 >= nonEmpty * 3

            // Row-spanning blanks are common in real headerless description tables, so
            // long text must run in parallel on at least half the occupied rows —
            // unless a section heading was swallowed into the grid, which is direct
            // evidence that the candidate is page prose.
            && ((rowsWithParallelProse >= 2 && rowsWithParallelProse * 2 >= occupiedRows)
                || (rowsWithParallelProse >= 1 && hasNumberedSectionHeading))
            && continuationFragments >= 3
            && continuationColumnCount >= 2;

        Log.Debug(Module, () =>
            $"chart table hypothesis: {table.Rows.Count}x{table.Columns.Count}, non_empty={nonEmpty}, " +
            $"long_prose={longProse}, parallel_rows={rowsWithParallelProse}/{occupiedRows}, " +
            $"section_heading={hasNumberedSectionHeading}, continuation_fragments={continuationFragments}, " +
            $"continuation_columns={continuationColumnCount}, reject={isParallel}");

        return isParallel;
    }
}
