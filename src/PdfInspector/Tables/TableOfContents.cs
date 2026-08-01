// Ported from reference/src/tables/detect_heuristic.rs and mod.rs
using System.Text;

using PdfInspector.Text;

namespace PdfInspector.Tables;

/// <summary>
/// Recognises a table of contents among detected tables. A contents page shares
/// row and column structure with a data table, so it reaches this pipeline, but
/// it renders as a flat list rather than markdown table syntax.
/// </summary>
internal static class TableOfContents
{
    public static bool IsTableOfContents(List<List<string>> cells) =>
        IsDotLeaderToc(cells) || IsTabularToc(cells) || IsPageNumberToc(cells);

    // ── Roman numerals ───────────────────────────────────────────────────

    private static readonly (uint Value, string Symbol)[] RomanTable =
    [
        (100, "c"), (90, "xc"), (50, "l"), (40, "xl"),
        (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
    ];

    /// <summary>The canonical lowercase roman numeral for a value, over the i–c range.</summary>
    public static string ToRomanLower(uint n)
    {
        var output = new StringBuilder();

        foreach (var (value, symbol) in RomanTable)
        {
            while (n >= value)
            {
                output.Append(symbol);
                n -= value;
            }
        }

        return output.ToString();
    }

    /// <summary>
    /// Parses a canonical roman numeral to its value, rejecting non-canonical
    /// strings so ordinary words made of those letters — "civil", "mix", "ill" —
    /// are not mistaken for numbers. Shared with the formatter so the two agree.
    /// </summary>
    public static uint? CanonicalRomanValue(string token)
    {
        var lower = token.Trim().ToLowerInvariant();

        if (lower.Length == 0 || TextUtils.ByteLength(lower) > 8 || !lower.All(c => "ivxlc".Contains(c, StringComparison.Ordinal)))
        {
            return null;
        }

        var total = 0;
        var prev = 0;

        for (var i = lower.Length - 1; i >= 0; i--)
        {
            var v = lower[i] switch
            {
                'i' => 1,
                'v' => 5,
                'x' => 10,
                'l' => 50,
                'c' => 100,
                _ => 0,
            };

            if (v == 0)
            {
                return null;
            }

            if (v < prev)
            {
                total -= v;
            }
            else
            {
                total += v;
                prev = v;
            }
        }

        if (total <= 0)
        {
            return null;
        }

        var value = (uint)total;
        return ToRomanLower(value) == lower ? value : null;
    }

    /// <summary>
    /// Parses a page-number token: a short arabic integer, or a canonical roman
    /// numeral as front matter uses.
    /// </summary>
    private static uint? PageNumberValue(string token)
    {
        var t = token.Trim();
        if (t.Length == 0)
        {
            return null;
        }

        if (t.All(char.IsAsciiDigit) && TextUtils.ByteLength(t) <= 4)
        {
            return uint.TryParse(t, out var value) ? value : null;
        }

        return CanonicalRomanValue(t);
    }

    // ── Page-number-column contents ──────────────────────────────────────

    /// <summary>
    /// A title-based contents page with no dot leaders and no section numbers.
    /// The signature is a text first column and a last column of page numbers
    /// whose values mostly ascend — that monotonic run is what separates real
    /// contents from an incidental two-column numeric table.
    /// </summary>
    public static bool IsPageNumberToc(List<List<string>> cells)
    {
        var numCols = cells.Count > 0 ? cells[0].Count : 0;

        // Contents are a narrow list of title and page, optionally with a leader
        // column. Wider grids are data tables.
        if (numCols is < 2 or > 3 || cells.Count < 5)
        {
            return false;
        }

        var last = numCols - 1;

        // A contents page has no header row: its first row is already an entry,
        // so its last cell is a page number. A data table's first row is a
        // column header — non-numeric, or an empty units cell — which is the
        // tell. The actual first row is checked, not the first non-empty one,
        // so a blank header cell still rejects.
        var firstLast = cells[0].Count > last ? cells[0][last].Trim() : string.Empty;
        if (PageNumberValue(firstLast) is null)
        {
            return false;
        }

        var filled = 0u;
        var pageValues = new List<uint>();

        foreach (var row in cells)
        {
            var cell = row.Count > last ? row[last].Trim() : string.Empty;
            if (cell.Length == 0)
            {
                continue;
            }

            filled++;
            if (PageNumberValue(cell) is { } value)
            {
                pageValues.Add(value);
            }
        }

        if (filled < 4 || pageValues.Count < 0.7f * filled)
        {
            return false;
        }

        // A text first column rejects numeric-versus-numeric grids.
        var textFirst = cells.Count(row => row.Count > 0 && row[0].Any(char.IsLetter));
        if (textFirst < 0.6f * cells.Count)
        {
            return false;
        }

        if (pageValues.Count < 2)
        {
            return false;
        }

        // Front-matter to body resets and noise are tolerated.
        var nonDecreasing = 0;
        for (var i = 0; i + 1 < pageValues.Count; i++)
        {
            if (pageValues[i + 1] >= pageValues[i])
            {
                nonDecreasing++;
            }
        }

        if (nonDecreasing < 0.7f * (pageValues.Count - 1))
        {
            return false;
        }

        // Real page numbers span the document: entries skip, so their range
        // exceeds the entry count. A rank, id, or ordinal column is instead a
        // perfectly dense consecutive run.
        var min = pageValues.Min();
        var max = pageValues.Max();
        var span = max - min;

        if (span > pageValues.Count)
        {
            return true;
        }

        var denseConsecutive = span + 1 == pageValues.Count
            && pageValues.Distinct().Count() == pageValues.Count;

        if (!denseConsecutive)
        {
            // A narrow range with a gap or repeat still reads as contents.
            return true;
        }

        // A dense counter is contents only when the titles read like headings
        // rather than the short single-word labels of a leaderboard.
        var totalWords = 0;
        var titledRows = 0;

        foreach (var row in cells)
        {
            if (row.Count == 0 || !row[0].Any(char.IsLetter))
            {
                continue;
            }

            foreach (var token in row[0].SplitWhitespace())
            {
                if (ContainsLetter(token))
                {
                    totalWords++;
                }
            }

            titledRows++;
        }

        return titledRows > 0 && (float)totalWords / titledRows >= 1.8f;
    }

    // ── Dot-leader contents ──────────────────────────────────────────────

    /// <summary>Any "Chapter 1 ........ 42" layout with explicit leader dots.</summary>
    public static bool IsDotLeaderToc(List<List<string>> cells) =>
        HasStructuralDotLeader(cells) || IsInlineLeaderIndex(cells);

    /// <summary>
    /// Rows with a dedicated dots-only cell flanked by a label and a number, the
    /// narrow contents layout. These render well as a flat list.
    /// </summary>
    private static bool HasStructuralDotLeader(List<List<string>> cells)
    {
        if (cells.Count == 0)
        {
            return false;
        }

        var structuralRows = cells.Count(RowHasDotLeader);
        return (float)structuralRows / cells.Count >= 0.3f;
    }

    /// <summary>
    /// A wide index layout where each cell holds a whole "label ... page"
    /// fragment, because the column detector kept multi-column indices as single
    /// cells. These render poorly either way, so they are rejected at detection
    /// time and fall back to the page's normal text flow.
    /// </summary>
    public static bool IsInlineLeaderIndex(List<List<string>> cells)
    {
        var inlineCells = 0;
        var totalNonEmpty = 0;

        foreach (var row in cells)
        {
            foreach (var cell in row)
            {
                var trimmed = cell.Trim();
                if (trimmed.Length == 0)
                {
                    continue;
                }

                totalNonEmpty++;
                if (CellIsInlineLeader(trimmed))
                {
                    inlineCells++;
                }
            }
        }

        return totalNonEmpty >= 4 && (float)inlineCells / totalNonEmpty >= 0.25f;
    }

    /// <summary>
    /// A row carrying a dot leader, in either layout: a dedicated dots-only cell
    /// with a label to its left and a page number to its right, or a "title ..."
    /// cell whose leader is glued to the title.
    /// </summary>
    private static bool RowHasDotLeader(List<string> row)
    {
        var hasPageNumber = row.Any(RowCellIsPageNumber);

        for (var ci = 0; ci < row.Count; ci++)
        {
            var trimmed = row[ci].Trim();

            var dotCount = trimmed.Count(c => c == '.');
            var isMostlyDots = dotCount >= 3
                && dotCount > TextUtils.ByteLength(trimmed) / 2
                && trimmed.All(c => c == '.' || char.IsWhiteSpace(c));

            if (isMostlyDots)
            {
                var hasLabelLeft = row.Take(ci).Any(c =>
                {
                    var t = c.Trim();
                    return t.Length > 0 && t.Any(char.IsLetter);
                });

                if (hasLabelLeft && hasPageNumber)
                {
                    return true;
                }

                continue;
            }

            if (hasPageNumber && CellHasTrailingLeader(trimmed))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>
    /// A cell ending in a run of dots preceded by a space and alphabetic text.
    /// Alphabetic rather than alphanumeric, so a data-table row label such as
    /// "1973 ... " does not register as a title.
    /// </summary>
    private static bool CellHasTrailingLeader(string cell)
    {
        var trimmed = cell.TrimEnd();
        if (!trimmed.EndsWith('.'))
        {
            return false;
        }

        var withoutDots = trimmed.TrimEnd('.');
        if (TextUtils.ByteLength(trimmed) - TextUtils.ByteLength(withoutDots) < 3)
        {
            return false;
        }

        // The space before the dot run rules out "etc..." and "Mr...".
        return withoutDots.EndsWith(' ') && withoutDots.Trim().Any(char.IsLetter);
    }

    /// <summary>
    /// A page-number-shaped cell: one short integer, a comma-space separated list
    /// of them, or a dashed section-page identifier. Decimal cells,
    /// thousands-separated values, and other statistical data are rejected.
    /// </summary>
    private static bool RowCellIsPageNumber(string cell)
    {
        var t = cell.Trim();
        if (t.Length == 0)
        {
            return false;
        }

        if (LooksLikeSectionPageId(t))
        {
            return true;
        }

        // The comma-space separator distinguishes a real page list from a
        // thousands-separated number such as "189,164".
        var parts = t.Split(", ");
        return parts.All(p => p.Length > 0 && TextUtils.ByteLength(p) <= 4 && p.All(char.IsAsciiDigit));
    }

    /// <summary>
    /// A cell shaped like an index leader fragment: "text ... number", or a bare
    /// "... number" where the label landed in another column. Both count only
    /// when followed by purely numeric content.
    /// </summary>
    private static bool CellIsInlineLeader(string cell)
    {
        cell = cell.Trim();

        var idx = cell.IndexOf("...", StringComparison.Ordinal);
        if (idx < 0)
        {
            return false;
        }

        var before = cell[..idx];
        var after = cell[(idx + 3)..].TrimStart('.');

        // A space or the cell start before the dots, and a space or digit after,
        // blocks an intra-word ellipsis.
        var beforeOk = before.Length == 0 || before.EndsWith(' ');
        var afterOk = after.StartsWith(' ') || after.Length == 0;

        if (!beforeOk || !afterOk)
        {
            return false;
        }

        var afterTrim = after.Trim();
        if (afterTrim.Length == 0)
        {
            return false;
        }

        var tailNumeric = afterTrim.All(c => char.IsAsciiDigit(c) || c is ',' or ' ' or '.' or '-' or '$')
            && afterTrim.Any(char.IsAsciiDigit);

        if (!tailNumeric)
        {
            return false;
        }

        // Either a label precedes the leader, or the leader is bare — both are
        // legitimate index fragments.
        return before.Any(char.IsLetter) || before.Trim().Length == 0;
    }

    // ── Dot-less tabular contents ────────────────────────────────────────

    /// <summary>
    /// Contents from a tagged PDF, emitted as rows whose first column starts
    /// with a dotted section number and whose last column is page numbers. These
    /// carry no leader dots but still render best as a flat list.
    /// </summary>
    public static bool IsTabularToc(List<List<string>> cells)
    {
        if (cells.Count == 0)
        {
            return false;
        }

        var numCols = cells[0].Count;
        if (numCols < 2 || cells.Count < 4)
        {
            return false;
        }

        var sectionRows = cells.Count(row =>
        {
            var first = row.FirstOrDefault(c => c.Trim().Length > 0);
            return first is not null && StartsWithSectionNumber(first.Trim());
        });

        var lastCol = numCols - 1;
        var lastFilled = 0u;
        var lastPageNum = 0u;

        foreach (var row in cells)
        {
            var cell = row.Count > lastCol ? row[lastCol].Trim() : string.Empty;
            if (cell.Length == 0)
            {
                continue;
            }

            lastFilled++;

            var isPageNums = true;
            foreach (var token in cell.SplitWhitespace())
            {
                if (token.ContainsAnyExceptInRange('0', '9'))
                {
                    isPageNums = false;
                    break;
                }
            }

            if (isPageNums)
            {
                lastPageNum++;
            }
        }

        var sectionRatio = (float)sectionRows / cells.Count;
        var pageNumLastRatio = lastFilled > 0 ? (float)lastPageNum / lastFilled : 0.0f;

        return sectionRatio >= 0.6f && lastFilled >= 3 && pageNumLastRatio >= 0.7f;
    }

    /// <summary>True when the span holds at least one letter.</summary>
    private static bool ContainsLetter(ReadOnlySpan<char> text)
    {
        foreach (var c in text)
        {
            if (char.IsLetter(c))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>A dashed section-page identifier as technical manuals use: "5-21", "A-1", "TC-2".</summary>
    private static bool LooksLikeSectionPageId(string s) =>
        s.All(c => char.IsAsciiDigit(c) || char.IsAsciiLetterUpper(c) || c == '-')
        && s.Any(char.IsAsciiDigit);

    /// <summary>
    /// True when the leading token is a dotted section number — "1.2", "4.3.1.2"
    /// — integer components joined by dots. At least one dot is required, since
    /// a bare number is too ambiguous.
    /// </summary>
    private static bool StartsWithSectionNumber(string s)
    {
        var firstWord = s.AsSpan().FirstWord();
        if (firstWord.IsEmpty)
        {
            return false;
        }

        var first = firstWord.TrimEnd('.').ToString();
        var parts = first.Split('.');

        if (parts.Length is < 2 or > 6)
        {
            return false;
        }

        return parts.All(p => p.Length > 0 && TextUtils.ByteLength(p) <= 3 && p.All(char.IsAsciiDigit));
    }
}
