// Ported from reference/src/tables/format.rs
using System.Text;
using PdfInspector.Text;

namespace PdfInspector.Tables;

/// <summary>Table-to-markdown formatting and cell cleanup.</summary>
internal static class TableFormat
{
    /// <summary>Renders a detected table as markdown.</summary>
    public static string TableToMarkdown(Table table)
    {
        if (table.Cells.Count == 0 || table.Cells[0].Count == 0)
        {
            return string.Empty;
        }

        // A contents page renders poorly as a markdown table — emit a flat per-row
        // text list instead, so page numbers stay beside their section titles
        // rather than drifting into a separate column. Formatting works from the
        // raw cells because continuation-row merging in CleanTableCells collapses
        // separate entries ("6.2 Contamination" plus "6.2.1 SWE-bench") into one
        // line wherever a sub-entry leaves column 0 empty.
        if (table.Kind == TableKind.Toc)
        {
            return FormatTocAsList(table.Cells, []);
        }

        var (cleanedCells, footnotes) = CleanTableCells(table.Cells);
        if (cleanedCells.Count == 0)
        {
            return string.Empty;
        }

        var numCols = cleanedCells[0].Count;
        var output = new StringBuilder();

        // Compact format: no padding, minimal separators. Optimised for token
        // efficiency, since AI agents are the primary consumer, not human eyes.
        for (var rowIdx = 0; rowIdx < cleanedCells.Count; rowIdx++)
        {
            output.Append('|');
            foreach (var cell in cleanedCells[rowIdx])
            {
                output.Append(cell).Append('|');
            }

            output.Append('\n');

            if (rowIdx == 0)
            {
                output.Append('|');
                for (var i = 0; i < numCols; i++)
                {
                    output.Append("---|");
                }

                output.Append('\n');
            }
        }

        if (footnotes.Count > 0)
        {
            output.Append('\n');
            foreach (var footnote in footnotes)
            {
                output.Append(footnote).Append('\n');
            }
        }

        return output.ToString();
    }

    /// <summary>
    /// Renders a table of contents as a flat per-row text block. Each row becomes
    /// one line: non-empty cells joined with spaces, with the last cell —
    /// typically a page number — separated by a tab, so the page numbers stay
    /// aligned with their titles instead of being pulled into a separate column by
    /// the column-aware reader.
    /// </summary>
    private static string FormatTocAsList(List<List<string>> cells, IReadOnlyList<string> footnotes)
    {
        var output = new StringBuilder();

        foreach (var row in cells)
        {
            var trimmed = row.Select(c => c.Trim()).ToList();
            var lastIdx = trimmed.FindLastIndex(c => c.Length > 0);
            if (lastIdx < 0)
            {
                continue;
            }

            var lastCell = trimmed[lastIdx];
            var lastIsPage = IsPageNumberCell(lastCell);

            List<string> titleCells;
            string? trailing;
            if (lastIsPage && lastIdx > 0)
            {
                titleCells = trimmed[..lastIdx];
                trailing = lastCell;
            }
            else
            {
                titleCells = trimmed[..(lastIdx + 1)];
                trailing = null;
            }

            // In a detected contents layout a "...." cell is a leader separator,
            // not part of the entry name.
            var title = string.Join(' ', titleCells.Where(c => c.Length > 0 && !IsDotsOnly(c)));

            if (title.Length == 0 && trailing is null)
            {
                continue;
            }

            if (title.Length > 0)
            {
                output.Append(title);
            }

            if (trailing is not null)
            {
                if (title.Length > 0)
                {
                    output.Append('\t');
                }

                output.Append(trailing);
            }

            output.Append('\n');
        }

        if (footnotes.Count > 0)
        {
            output.Append('\n');
            foreach (var footnote in footnotes)
            {
                output.Append(footnote).Append('\n');
            }
        }

        return output.ToString();
    }

    /// <summary>
    /// True when the cell reads as a page number: plain digit tokens ("42",
    /// "86 86"), canonical roman numerals for front matter ("vii", "ix", "xii"),
    /// or the dashed section-page IDs technical manuals use ("5-21", "A-1",
    /// "B--3", "TC-2").
    /// </summary>
    private static bool IsPageNumberCell(string cell)
    {
        var tokens = cell.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        if (tokens.Length == 0)
        {
            return false;
        }

        return tokens.All(t =>
        {
            if (t.Length == 0 || TextUtils.ByteLength(t) > 8)
            {
                return false;
            }

            if (t.All(char.IsAsciiDigit))
            {
                return TextUtils.ByteLength(t) <= 4;
            }

            if (TableOfContents.CanonicalRomanValue(t) is not null)
            {
                return true;
            }

            // Section-page form: uppercase letters, digits and dashes, with at
            // least one digit present.
            return t.All(c => char.IsAsciiDigit(c) || char.IsAsciiLetterUpper(c) || c == '-')
                && t.Any(char.IsAsciiDigit);
        });
    }

    /// <summary>True when the cell is nothing but leader dots, three or more, plus whitespace.</summary>
    private static bool IsDotsOnly(string cell)
    {
        var t = cell.Trim();
        var dots = t.Count(c => c == '.');
        return dots >= 3 && t.All(c => c == '.' || char.IsWhiteSpace(c));
    }

    /// <summary>True when the cell's first alphanumeric character is uppercase.</summary>
    private static bool StartsWithUppercaseWord(string cell)
    {
        foreach (var c in cell)
        {
            if (char.IsLetterOrDigit(c))
            {
                return char.IsUpper(c);
            }
        }

        return false;
    }

    /// <summary>True when the cell's first letter is uppercase.</summary>
    private static bool StartsWithUppercaseAlpha(string cell)
    {
        foreach (var c in cell)
        {
            if (char.IsLetter(c))
            {
                return char.IsUpper(c);
            }
        }

        return false;
    }

    /// <summary>True when the cell's first letter is lowercase.</summary>
    private static bool StartsWithLowercaseAlpha(string cell)
    {
        foreach (var c in cell)
        {
            if (char.IsLetter(c))
            {
                return char.IsLower(c);
            }
        }

        return false;
    }

    /// <summary>True for a leading "1.", "2)", "3-" or "4:" style label.</summary>
    private static bool StartsWithNumberedLabel(string cell)
    {
        var trimmed = cell.TrimStart();
        var digitCount = 0;
        while (digitCount < trimmed.Length && char.IsAsciiDigit(trimmed[digitCount]))
        {
            digitCount++;
        }

        return digitCount is > 0 and <= 3
            && digitCount < trimmed.Length
            && trimmed[digitCount] is '.' or ')' or '-' or ':';
    }

    /// <summary>True for a leading dotted section number such as "3.1" or "4.2.1".</summary>
    private static bool StartsWithHierarchicalNumberedLabel(string cell)
    {
        var first = cell.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).FirstOrDefault() ?? string.Empty;
        var token = first.TrimEnd('.', ')', ':', '-');
        var levels = token.Split('.');
        return levels.Length is >= 2 and <= 4
            && levels.All(level => level.Length > 0 && TextUtils.ByteLength(level) <= 3 && level.All(char.IsAsciiDigit));
    }

    /// <summary>Counts whitespace-separated words that contain at least one letter.</summary>
    private static int AlphaWordCount(string cell) => cell
        .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
        .Count(word => word.Any(char.IsLetter));

    /// <summary>True for a short capitalised or numbered label, as a hierarchy sub-row starts with.</summary>
    private static bool LooksLikeCompactEntryLabel(string cell)
    {
        var trimmed = cell.Trim();
        if (TextUtils.ByteLength(trimmed) is < 3 or > 80)
        {
            return false;
        }

        if (!StartsWithUppercaseAlpha(trimmed) && !StartsWithNumberedLabel(trimmed))
        {
            return false;
        }

        if (trimmed.Length > 0 && trimmed[^1] is '.' or ',' or ';' or ':')
        {
            return false;
        }

        return AlphaWordCount(trimmed) is >= 1 and <= 6;
    }

    /// <summary>True for a short digit-free capitalised phrase, as a section band row carries.</summary>
    private static bool LooksLikePlainSectionLabel(string cell)
    {
        var trimmed = cell.Trim();
        if (TextUtils.ByteLength(trimmed) is < 4 or > 40)
        {
            return false;
        }

        if ((trimmed.Length > 0 && trimmed[^1] is '.' or ',' or ';' or ':') || trimmed.Any(char.IsAsciiDigit))
        {
            return false;
        }

        if (TextUtils.ByteLength(trimmed) <= 4 && !trimmed.Any(char.IsLower))
        {
            return false;
        }

        return trimmed.All(ch => char.IsLetter(ch) || char.IsWhiteSpace(ch) || ch is '&' or '/' or '-')
            && StartsWithUppercaseAlpha(trimmed)
            && AlphaWordCount(trimmed) is >= 1 and <= 4;
    }

    /// <summary>True when the cell trails off mid-phrase, so the next row continues it.</summary>
    private static bool EndsLikeIncompletePhrase(string cell)
    {
        var lower = cell.TrimEnd().ToLowerInvariant();
        return lower.EndsWith(" and", StringComparison.Ordinal)
            || lower.EndsWith(" or", StringComparison.Ordinal)
            || lower.EndsWith(',')
            || lower.EndsWith('-')
            || lower.EndsWith('/');
    }

    /// <summary>
    /// Cleans up table cells: merges continuation rows, lifts footnotes out, and
    /// drops empty rows.
    /// </summary>
    private static (List<List<string>> Cleaned, List<string> Footnotes) CleanTableCells(List<List<string>> cells)
    {
        var cleaned = new List<List<string>>();
        var footnotes = new List<string>();

        foreach (var row in cells)
        {
            if (row.All(c => c.Trim().Length == 0))
            {
                continue;
            }

            var firstCell = row.Count > 0 ? row[0].Trim() : string.Empty;
            if (IsFootnoteRow(firstCell))
            {
                footnotes.Add(string.Join(' ', row.Select(c => c.Trim()).Where(c => c.Length > 0)));
                continue;
            }

            var numCols = row.Count;
            var filledCells = row.Count(c => c.Trim().Length > 0);

            // A continuation row leaves the first column empty and carries content
            // elsewhere. A row with just one short non-empty cell beyond the first
            // is more likely a section sub-header ("JAN", "FEB") than overflow text,
            // so it stays put. A row with content in many columns is a real data row
            // whose first-column cell spans, not text overflow.
            var nonFirstCells = row.Skip(1).Select(c => c.Trim()).Where(c => c.Length > 0).ToList();
            var isShortSubheader = nonFirstCells.Count == 1 && TextUtils.ByteLength(nonFirstCells[0]) <= 5;

            // Rows of several short values — numeric data in a lookup table — are
            // data rows with a spanning first column, not overflow from the row
            // above. Continuation rows carry longer descriptive text; data rows
            // carry short numbers.
            var avgCellLen = nonFirstCells.Count == 0
                ? 0.0f
                : nonFirstCells.Sum(TextUtils.ByteLength) / (float)nonFirstCells.Count;
            var numericCells = nonFirstCells.Count(c =>
                c.All(ch => char.IsAsciiDigit(ch) || ch is '.' or '-' or ',' or ' '));
            var looksLikeDataRow = nonFirstCells.Count >= 2
                && avgCellLen <= 10.0f
                && numericCells > nonFirstCells.Count / 2;

            var uppercaseLeadingCells = nonFirstCells.Count(StartsWithUppercaseWord);
            var firstNonEmptyCol = row.FindIndex(c => c.Trim().Length > 0);
            var firstNonEmptyCell = firstNonEmptyCol >= 0 ? row[firstNonEmptyCol].Trim() : string.Empty;
            var titleLikeLaterCells = firstNonEmptyCol < 0
                ? 0
                : row.Skip(firstNonEmptyCol + 1)
                    .Select(c => c.Trim())
                    .Count(c => c.Length > 0 && StartsWithUppercaseAlpha(c));

            var prevFirstCell = cleaned.Count > 0 && cleaned[^1].Count > 0 ? cleaned[^1][0].Trim() : string.Empty;
            var prevFirstCellEmpty = cleaned.Count > 0 && cleaned[^1].Count > 0 && prevFirstCell.Length == 0;
            var headerFilled = cleaned.Count > 0 ? cleaned[0].Count(c => c.Trim().Length > 0) : numCols;

            var looksLikeSpanningFirstColumnRow = firstCell.Length == 0
                && row.Count >= 4
                && nonFirstCells.Count == Math.Max(row.Count - 1, 0)
                && uppercaseLeadingCells >= Math.Max(nonFirstCells.Count - 1, 0);

            // Hierarchical tables often span the first column: sub-rows leave column
            // 0 blank, then open a compact title-like label in column 1. Wrapped
            // continuations in the fixtures start mid-sentence or lowercase
            // ("continued text here", "with 3.5%...") or carry lowercase fragments
            // in their later cells, so those stay mergeable.
            var looksLikeHierarchicalSubrow = firstCell.Length == 0
                && firstNonEmptyCol == 1
                && LooksLikeCompactEntryLabel(firstNonEmptyCell)
                && ((row.Count == 2 && StartsWithHierarchicalNumberedLabel(firstNonEmptyCell))
                    || (row.Count >= 3 && nonFirstCells.Count >= 2 && titleLikeLaterCells > 0)
                    || (nonFirstCells.Count == 1
                        && row.Count >= 3
                        && prevFirstCellEmpty
                        && AlphaWordCount(firstNonEmptyCell) >= 2));

            var looksLikeNewFirstColumnEntry = firstCell.Length > 0
                && (StartsWithNumberedLabel(firstCell) || StartsWithUppercaseAlpha(firstCell))
                && filledCells >= 2
                && nonFirstCells.Any(LooksLikeCompactEntryLabel);

            var looksLikeSectionLabelRow = firstCell.Length > 0
                && filledCells == 1
                && headerFilled >= 3
                && LooksLikePlainSectionLabel(firstCell);

            // The classic continuation: first cell empty, content in the others.
            var isClassicContinuation = firstCell.Length == 0
                && nonFirstCells.Count > 0
                && !isShortSubheader
                && !looksLikeDataRow
                && !looksLikeSpanningFirstColumnRow
                && !looksLikeHierarchicalSubrow
                && cleaned.Count > 1;

            // The wrapped-cell continuation: the row has fewer filled cells than the
            // header, suggesting overflow from the previous row's cells. It only
            // triggers when the previous row has meaningfully more filled cells.
            var prevFilled = cleaned.Count > 0 ? cleaned[^1].Count(c => c.Trim().Length > 0) : 0;

            // Wide tables (5+ columns) need the row down to half the header's filled
            // cells; narrow tables (2–4) just need fewer than the header. That keeps
            // normal data rows in wide tables intact while continuation merging still
            // works in narrow ones.
            var maxFilledForMerge = headerFilled >= 5 ? headerFilled / 2 : Math.Max(headerFilled - 1, 0);
            var continuesWrappedFirstColumnLabel = firstCell.Length > 0
                && StartsWithLowercaseAlpha(firstCell)
                && EndsLikeIncompletePhrase(prevFirstCell);

            var isWrappedContinuation = cleaned.Count > 1
                && filledCells <= maxFilledForMerge
                && (prevFilled > filledCells
                    || (continuesWrappedFirstColumnLabel && prevFilled >= filledCells))
                && !looksLikeDataRow
                && !looksLikeSpanningFirstColumnRow
                && !looksLikeHierarchicalSubrow
                && !looksLikeNewFirstColumnEntry
                && !looksLikeSectionLabelRow
                && !isShortSubheader;

            if (isClassicContinuation || isWrappedContinuation)
            {
                if (cleaned.Count > 0)
                {
                    var prevRow = cleaned[^1];
                    for (var colIdx = 0; colIdx < row.Count && colIdx < prevRow.Count; colIdx++)
                    {
                        var cellText = row[colIdx].Trim();
                        if (cellText.Length == 0)
                        {
                            continue;
                        }

                        prevRow[colIdx] = prevRow[colIdx].Length > 0
                            ? prevRow[colIdx] + " " + cellText
                            : cellText;
                    }
                }
            }
            else
            {
                cleaned.Add(row.Select(c => c.Trim()).ToList());
            }
        }

        return (cleaned, footnotes);
    }

    /// <summary>True when a cell's value marks the row as a footnote.</summary>
    private static bool IsFootnoteRow(string text)
    {
        var trimmed = text.Trim();

        // "(1)", "(2)" and so on.
        if (trimmed.StartsWith('(') && trimmed.Length >= 2)
        {
            var inside = trimmed[1..];
            var closeIdx = inside.IndexOf(')', StringComparison.Ordinal);
            if (closeIdx >= 0 && inside[..closeIdx].All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        // "1)", "2)" and so on.
        if (trimmed.Length >= 2)
        {
            var parenIdx = trimmed.IndexOf(')', StringComparison.Ordinal);
            if (parenIdx > 0 && trimmed[..parenIdx].All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        var lower = trimmed.ToLowerInvariant();
        return lower.StartsWith("note:", StringComparison.Ordinal)
            || lower.StartsWith("notes:", StringComparison.Ordinal);
    }
}
