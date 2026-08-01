// Ported from reference/src/lib.rs
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.Types;

namespace PdfInspector.Regions;

/// <summary>Which detector produced a region's table candidate.</summary>
internal enum TableCandidateSource
{
    Rect,
    Line,
    Heuristic,
    Column,
    KeyValue,
}

/// <summary>A structural defect that disqualifies a candidate outright.</summary>
internal enum TableCandidateIssue
{
    LineRowUndercount,
    SparseWideUndercount,
    TextColumnUndercount,
    ProseGridFragment,
}

/// <summary>The row/column shape read back out of a rendered pipe table.</summary>
internal readonly record struct MarkdownTableShape(int Rows, int Cols, int RawCols);

/// <summary>One detector's rendered table, with the checks it passed or failed.</summary>
internal sealed record TableCandidate(
    string Markdown,
    TableCandidateSource Source,
    MarkdownTableShape Shape,
    TableCandidateIssue? Issue);

/// <summary>
/// The quality gates that decide whether a region's detected table is good
/// enough to serve, or whether the caller should fall back to OCR.
/// </summary>
internal static class TableCandidates
{
    /// <summary>
    /// True when the captured markdown represents only a small fraction of the
    /// text the page extractor actually saw inside the region — typically a
    /// header-only band, or a sparse fragment where the detector found valid
    /// grid structure but missed most of the data rows below.
    /// </summary>
    /// <remarks>
    /// Tuned at a 25% floor: a table that captured at least a quarter of the
    /// region's text is complete enough. The 200-character region floor keeps
    /// short legitimate tables (units, axis labels, single-row stat blocks)
    /// from being mis-flagged.
    /// </remarks>
    public static bool CapturedOnlyAFragment(string markdown, int regionTextChars)
    {
        if (regionTextChars <= 200)
        {
            return false;
        }

        var capturedTextChars = markdown.Count(c => c is not ('|' or '-' or '\n'));
        return capturedTextChars * 4 < regionTextChars;
    }

    /// <summary>
    /// True when the text the page extractor saw inside this region is far too
    /// little for the bbox area — a strong signal of a font-CMap failure, where
    /// the rendered image still carries the visible text but the extractor
    /// returns punctuation-only fragments.
    /// </summary>
    /// <remarks>
    /// <see cref="CapturedOnlyAFragment"/> cannot catch this on its own, since
    /// its numerator and denominator collapse together under a decode failure.
    /// Comparing against bbox area breaks that symmetry, because area is
    /// independent of extraction success. The 0.003 chars/sq pt threshold sits
    /// between observed clean extractions (≥0.005) and observed font-decode
    /// failures (≤0.0014); the character and area bounds keep the guard from
    /// misfiring on tiny stat blocks and whole-page bboxes.
    /// </remarks>
    public static bool RegionTextDensityTooLow(int regionTextChars, float regionArea)
    {
        if (regionTextChars < 20)
        {
            return false;
        }

        if (regionArea is < 30_000.0f or > 400_000.0f)
        {
            return false;
        }

        return regionTextChars / regionArea < 0.003f;
    }

    /// <summary>Picks the candidate to serve, or null when every one is compromised.</summary>
    public static TableCandidate? SelectTableCandidate(IReadOnlyList<TableCandidate> candidates)
    {
        if (candidates.Count == 0)
        {
            return null;
        }

        var first = candidates[0];

        // A line grid that visibly collapses several captured text baselines
        // into too few rows is structurally undercounted. Prefer a clearly
        // wider clean heuristic if one exists; otherwise force OCR rather than
        // serving a tidy-looking fragment.
        if (first.Issue == TableCandidateIssue.LineRowUndercount)
        {
            return candidates.FirstOrDefault(candidate =>
                candidate.Source is TableCandidateSource.Heuristic
                    or TableCandidateSource.Column
                    or TableCandidateSource.KeyValue
                && candidate.Issue is null
                && candidate.Shape.Cols * 10 >= first.Shape.Cols * 13);
        }

        var accepted = candidates.FirstOrDefault(candidate => candidate.Issue is null);
        if (accepted is null)
        {
            return null;
        }

        // Keep vector-first behaviour unless the text heuristic is also clean
        // and has substantially more structure. That catches vector grids that
        // pass the text-quality checks while missing implicit rows or sparse
        // columns, without swapping on small shape noise.
        if (accepted.Source is TableCandidateSource.Rect or TableCandidateSource.Line)
        {
            var acceptedShape = accepted.Shape;
            var heuristic = candidates.FirstOrDefault(candidate =>
                candidate.Source is TableCandidateSource.Heuristic
                    or TableCandidateSource.Column
                    or TableCandidateSource.KeyValue
                && candidate.Issue is null
                && HeuristicSubstantiallyBetter(candidate.Shape, acceptedShape));
            if (heuristic is not null)
            {
                accepted = heuristic;
            }
        }

        if (accepted.Source == TableCandidateSource.Heuristic)
        {
            var acceptedShape = accepted.Shape;
            var layoutCandidate = candidates.FirstOrDefault(candidate =>
                candidate.Source is TableCandidateSource.Column or TableCandidateSource.KeyValue
                && candidate.Issue is null
                && candidate.Shape.Cols >= acceptedShape.Cols
                && candidate.Shape.Rows > acceptedShape.Rows);
            if (layoutCandidate is not null)
            {
                accepted = layoutCandidate;
            }
        }

        return accepted;
    }

    private static bool HeuristicSubstantiallyBetter(MarkdownTableShape heuristic, MarkdownTableShape accepted) =>
        (accepted.Rows > 0 && heuristic.Rows * 2 >= accepted.Rows * 3)
        || (accepted.Cols > 0 && heuristic.Cols * 10 >= accepted.Cols * 13);

    /// <summary>Reads a rendered pipe table's row and column counts back out.</summary>
    public static MarkdownTableShape MarkdownTableShapeOf(string markdown)
    {
        var rows = 0;
        var cols = 0;
        var rawCols = 0;

        foreach (var cells in MarkdownPipeRows(markdown))
        {
            rows++;
            rawCols = Math.Max(rawCols, cells.Count);
            cols = Math.Max(cols, cells.Count(cell => cell.Trim().Length > 0));
        }

        return new MarkdownTableShape(rows, cols, rawCols);
    }

    /// <summary>The inner cells of every pipe row, skipping the separator.</summary>
    public static List<List<string>> MarkdownPipeRows(string markdown)
    {
        var result = new List<List<string>>();

        foreach (var line in markdown.Split('\n'))
        {
            var trimmed = line.Trim();
            if (!trimmed.StartsWith('|') || !trimmed.EndsWith('|') || IsMarkdownSeparatorRow(trimmed))
            {
                continue;
            }

            var parts = trimmed.Split('|');
            if (parts.Length < 3)
            {
                continue;
            }

            result.Add([.. parts[1..^1]]);
        }

        return result;
    }

    private static bool IsMarkdownSeparatorRow(string line)
    {
        var sawDash = false;
        foreach (var ch in line)
        {
            switch (ch)
            {
                case '-':
                    sawDash = true;
                    break;
                case '|' or ':' or ' ':
                    break;
                default:
                    return false;
            }
        }

        return sawDash;
    }

    /// <summary>
    /// True when a line-detected grid folds several captured text baselines
    /// into far too few rows.
    /// </summary>
    public static bool LineTableCollapsesTextRows(
        Table table,
        IReadOnlyList<TextItem> items,
        MarkdownTableShape shape)
    {
        var tableRows = Math.Max(
            shape.Rows,
            table.Cells.Count(row => row.Any(cell => cell.Trim().Length > 0)));
        var tableCols = Math.Max(shape.RawCols, shape.Cols);
        if (tableRows is < 2 or > 4 || tableCols < 3)
        {
            return false;
        }

        var capturedItems = table.ItemIndices
            .Where(idx => idx >= 0 && idx < items.Count)
            .Select(idx => items[idx])
            .Where(item => item.Text.Trim().Length > 0)
            .ToList();
        var capturedChars = capturedItems.Sum(item => TextUtils.CharCount(item.Text));
        if (capturedChars <= 200)
        {
            return false;
        }

        var implicitRows = YClusterCount(capturedItems);
        return implicitRows >= 4 && tableRows * 2 <= implicitRows;
    }

    private static int YClusterCount(IReadOnlyList<TextItem> items)
    {
        if (items.Count == 0)
        {
            return 0;
        }

        var ys = items.Select(item => item.Y).ToList();
        ys.Sort(FloatTotalOrder.Instance);

        var clusters = 1;
        var center = ys[0];
        var count = 1;
        for (var i = 1; i < ys.Count; i++)
        {
            var y = ys[i];
            if (MathF.Abs(y - center) > 3.0f)
            {
                clusters++;
                center = y;
                count = 1;
            }
            else
            {
                center = (center * count + y) / (count + 1);
                count++;
            }
        }

        return clusters;
    }

    /// <summary>True when at least two long, near-vertical rules are present.</summary>
    public static bool HasVerticalRules(IReadOnlyList<PdfLine> lines)
    {
        var angleTolerance = MathF.Tan(2.0f * MathF.PI / 180.0f);
        return lines.Count(line =>
        {
            var dx = MathF.Abs(line.X2 - line.X1);
            var dy = MathF.Abs(line.Y2 - line.Y1);
            var length = MathF.Sqrt((dx * dx) + (dy * dy));
            return length >= 20.0f && dy > 0.01f && dx / dy <= angleTolerance;
        }) >= 2;
    }

    /// <summary>
    /// True when a wide table has a single unnamed early column and most data
    /// rows leave the leading columns blank — the shape of a detector that
    /// dropped the real leading columns.
    /// </summary>
    public static bool WideTableSparsePrefixUndercount(string markdown)
    {
        var rows = MarkdownPipeRows(markdown);
        if (rows.Count < 4)
        {
            return false;
        }

        var header = rows[0];
        var rawCols = header.Count;
        if (rawCols < 8)
        {
            return false;
        }

        var emptyHeaders = header
            .Select((cell, idx) => (cell, idx))
            .Where(pair => pair.cell.Trim().Length == 0)
            .Select(pair => pair.idx)
            .ToList();
        if (emptyHeaders.Count != 1)
        {
            return false;
        }

        var emptyHeaderIdx = emptyHeaders[0];
        if (emptyHeaderIdx == 0 || emptyHeaderIdx >= rawCols / 2)
        {
            return false;
        }

        var prefixEnd = Math.Min(Math.Max(rawCols / 2, emptyHeaderIdx + 1), rawCols);
        if (prefixEnd <= 2)
        {
            return false;
        }

        var dataRows = 0;
        var sparsePrefixRows = 0;
        foreach (var row in rows.Skip(1))
        {
            if (row.All(cell => cell.Trim().Length == 0))
            {
                continue;
            }

            dataRows++;
            var emptyPrefixCells = row
                .Skip(1)
                .Take(Math.Max(prefixEnd - 1, 0))
                .Count(cell => cell.Trim().Length == 0);
            if (emptyPrefixCells >= 2)
            {
                sparsePrefixRows++;
            }
        }

        return dataRows >= 3 && sparsePrefixRows * 2 >= dataRows;
    }

    /// <summary>
    /// True when clustering the items' x-positions surfaces materially more
    /// columns than the rendered table has.
    /// </summary>
    public static bool TextClusterColumnUndercount(IReadOnlyList<TextItem> items, MarkdownTableShape shape)
    {
        var tableCols = Math.Max(shape.RawCols, shape.Cols);
        if (tableCols < 2 || items.Count < tableCols * 2)
        {
            return false;
        }

        // Count "significant" x-clusters — those holding at least a quarter of
        // the dominant cluster's items. That filters out within-cell variation
        // (wrapped continuations, bullet starts, indents), which produces many
        // small clusters that do not correspond to real columns.
        var clusterCounts = XClusterItemCounts(items);
        if (clusterCounts.Count == 0)
        {
            return false;
        }

        var dominant = clusterCounts.Max();
        var minClusterSize = Math.Max(dominant / 4, 2);
        var significantClusters = clusterCounts.Count(n => n >= minClusterSize);

        // Two regimes. A wide table undercount (an 11-column table dropped to
        // 9) needs at least two extra columns and 1.2× the rendered count. A
        // narrow undercount (4 columns dropped to 2) needs twice the rendered
        // count and at least 3 real columns, which catches narrow numeric
        // columns collapsed into their neighbours.
        var wideUndercount = tableCols >= 6
            && significantClusters >= tableCols + 2
            && significantClusters * 10 >= tableCols * 12;
        var narrowUndercount = significantClusters >= 3 && significantClusters >= tableCols * 2;
        return wideUndercount || narrowUndercount;
    }

    /// <summary>
    /// Clusters non-empty items' x-positions with an 8pt tolerance and returns
    /// each cluster's population, so the caller can weight out single-item
    /// outliers.
    /// </summary>
    private static List<int> XClusterItemCounts(IReadOnlyList<TextItem> items)
    {
        var xs = items
            .Where(item => item.Text.Trim().Length > 0)
            .Select(item => item.X)
            .ToList();
        if (xs.Count == 0)
        {
            return [];
        }

        xs.Sort(FloatTotalOrder.Instance);

        var counts = new List<int>();
        var center = xs[0];
        var count = 1;
        for (var i = 1; i < xs.Count; i++)
        {
            var x = xs[i];
            if (MathF.Abs(x - center) > 8.0f)
            {
                counts.Add(count);
                center = x;
                count = 1;
            }
            else
            {
                center = (center * count + x) / (count + 1);
                count++;
            }
        }

        counts.Add(count);
        return counts;
    }

    /// <summary>
    /// True when a narrow grid is overwhelmingly long prose with no column of
    /// compact identifiers — a paragraph block mistaken for a table.
    /// </summary>
    public static bool ProseGridFragmentNeedsOcr(string markdown)
    {
        var rows = MarkdownPipeRows(markdown);
        if (rows.Count < 2)
        {
            return false;
        }

        var rawCols = rows[0].Count;
        if (rawCols is < 2 or > 4)
        {
            return false;
        }

        var seenByCol = new int[rawCols];
        var compactByCol = new int[rawCols];
        var longProse = 0;
        var total = 0;

        foreach (var row in rows.Skip(1))
        {
            for (var col = 0; col < Math.Min(rawCols, row.Count); col++)
            {
                var trimmed = row[col].Trim();
                if (trimmed.Length == 0)
                {
                    continue;
                }

                total++;
                seenByCol[col]++;
                if (CompactIdentifierCell(trimmed))
                {
                    compactByCol[col]++;
                }

                if (LongProseCell(trimmed))
                {
                    longProse++;
                }
            }
        }

        var minTotal = rawCols == 2 ? 2 : rawCols * 2;
        if (total < minTotal || longProse * 3 < total * 2)
        {
            return false;
        }

        for (var col = 0; col < rawCols; col++)
        {
            if (seenByCol[col] >= 3 && compactByCol[col] * 2 >= seenByCol[col])
            {
                return false;
            }
        }

        return true;
    }

    private static bool CompactIdentifierCell(string cell)
    {
        var trimmed = cell.Trim();
        if (trimmed.Length == 0)
        {
            return false;
        }

        var words = WordCount(trimmed);
        if (words <= 3 && TextUtils.CharCount(trimmed) <= 40)
        {
            return true;
        }

        var chars = trimmed.Count(c => !char.IsWhiteSpace(c));
        if (chars is 0 or > 48)
        {
            return false;
        }

        var compactMarks = trimmed.Count(c =>
            char.IsAsciiDigit(c) || c is '.' or ',' or ':' or ';' or '/' or '-' or '(' or ')' or '[' or ']');
        return compactMarks * 2 >= chars;
    }

    private static bool LongProseCell(string cell)
    {
        var trimmed = cell.Trim();
        if (CompactIdentifierCell(trimmed))
        {
            return false;
        }

        var words = WordCount(trimmed);
        var alpha = trimmed.Count(char.IsLetter);
        return words >= 4 && alpha >= 12;
    }

    private static int WordCount(string text) => text
        .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
        .Count(word => word.Any(char.IsLetterOrDigit));

    private static bool StartsWithNumberedTableLabel(string cell)
    {
        var trimmed = cell.TrimStart();
        var digitCount = trimmed.TakeWhile(char.IsAsciiDigit).Count();
        return digitCount is > 0 and <= 3
            && digitCount < trimmed.Length
            && trimmed[digitCount] is '.' or ')' or '-' or ':';
    }

    private static bool StartsWithUppercaseAlpha(string cell)
    {
        foreach (var ch in cell)
        {
            if (char.IsLetter(ch))
            {
                return char.IsUpper(ch);
            }
        }

        return false;
    }

    private static bool CompactTitleLikeCell(string cell)
    {
        var trimmed = cell.Trim();
        var byteLength = TextUtils.ByteLength(trimmed);
        if (byteLength is < 3 or > 80 || !StartsWithUppercaseAlpha(trimmed))
        {
            return false;
        }

        var words = trimmed
            .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
            .Count(word => word.Any(char.IsLetter));
        return words is >= 1 and <= 6;
    }

    /// <summary>
    /// True when a numbered hierarchical table relies on row spans markdown
    /// cannot express, so wrapped content risks being attributed to the wrong
    /// row.
    /// </summary>
    private static bool NumberedRowspanHierarchyNeedsOcr(string markdown)
    {
        var rows = MarkdownPipeRows(markdown);
        if (rows.Count < 6)
        {
            return false;
        }

        var nCols = rows[0].Count;
        if (nCols is < 3 or > 6)
        {
            return false;
        }

        var dataRows = rows.Skip(1).ToList();
        var numberedGroupRows = dataRows.Count(row =>
            row.Count > 0 && StartsWithNumberedTableLabel(row[0]));
        var blankFirstSubrows = dataRows.Count(row =>
            row.Count > 0 && row[0].Trim().Length == 0
            && row.Count > 1 && CompactTitleLikeCell(row[1]));

        return numberedGroupRows >= 2 && blankFirstSubrows >= 2;
    }

    /// <summary>
    /// True when a rendered pipe table shows structural signs that the detector
    /// missed or mangled rows or columns, so the caller should fall back to OCR.
    /// </summary>
    /// <remarks>
    /// Conservative by design: a false positive only means running OCR, which
    /// is the existing safe path. When <paramref name="layoutAssisted"/> is set
    /// the layout model already supplied the table's bbox, so the
    /// boundary-detection heuristics relax — the open question is no longer
    /// "is this a table?" but "can we extract it correctly?". The paragraph and
    /// duplicate-header checks stay either way, since those indicate genuine
    /// extraction problems however the region was found.
    /// </remarks>
    public static bool LooksLikePartialTableEx(string markdown, bool layoutAssisted)
    {
        var lines = markdown.Split('\n').Where(l => l.StartsWith('|')).ToList();
        if (lines.Count < 2)
        {
            return false;
        }

        var headerLine = lines[0];
        var separatorLine = lines.Count > 1 ? lines[1] : string.Empty;
        if (!separatorLine.All(c => c is '|' or '-' or ' '))
        {
            // No separator after the first line — not a well-formed pipe table.
            // The renderer always emits one when it returns content, so this
            // should not happen; if it does, fall through to OCR.
            return true;
        }

        var cells = headerLine.Split('|').Select(s => s.Trim()).ToList();

        // The first and last pieces are always empty, since the line starts and
        // ends with a pipe.
        if (cells.Count < 3)
        {
            return false;
        }

        var headerCells = cells[1..^1];
        var nCols = headerCells.Count;
        if (nCols < 2)
        {
            // Single-column tables are usually lists or key/value blocks, not
            // tables. Keep them; the multi-column checks below don't apply.
            return false;
        }

        if (layoutAssisted && NumberedRowspanHierarchyNeedsOcr(markdown))
        {
            return true;
        }

        // Failure mode 1: the header starts with a bare number, which suggests
        // the real header row above was missed. Skipped when layout-assisted:
        // the model's bbox includes the real header, so a numeric first cell
        // (a year, say) is legitimate.
        if (!layoutAssisted && headerCells.Count > 0)
        {
            var firstCell = headerCells[0].Trim();
            if (firstCell.Length > 0 && firstCell.All(char.IsAsciiDigit))
            {
                return true;
            }
        }

        // Failure mode 2: the header has empty cells in a multi-column table.
        // When layout-assisted, tolerate merged or spanning header gaps if the
        // body is dense: a layout bbox often starts at a visual table whose
        // header cannot be represented faithfully as a flat pipe table, while
        // the body rows are still complete enough to use.
        var headerEmptyIndices = headerCells
            .Select((cell, idx) => (cell, idx))
            .Where(pair => pair.cell.Length == 0)
            .Select(pair => pair.idx)
            .ToHashSet();
        var emptyCount = headerEmptyIndices.Count;
        if (layoutAssisted)
        {
            if (nCols >= 3 && emptyCount >= 2 && !LayoutAssistedEmptyHeaderHasDenseBody(markdown, nCols))
            {
                return true;
            }
        }
        else if (nCols >= 3 && emptyCount >= 1)
        {
            return true;
        }

        // Failure mode 3: the header has duplicate non-empty cells, which means
        // a multi-line header was collapsed wrongly.
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var cell in headerCells)
        {
            if (cell.Length > 0 && !seen.Add(cell))
            {
                return true;
            }
        }

        // Failure mode 4: the first data row has many empty cells in a
        // multi-column table. Real tables rarely open with a mostly blank row;
        // when this happens the heuristic usually split a multi-row header into
        // a single-row header plus a sparse data row.
        if (lines.Count > 2)
        {
            var dataCells = lines[2].Split('|').Select(s => s.Trim()).ToList();
            if (dataCells.Count >= 3)
            {
                var dataInner = dataCells[1..^1];
                var emptyData = dataInner.Count(c => c.Length == 0);

                // Layout assistance relaxes the ratio from 33% to 50%: the bbox
                // is more reliable, and real tables with one sparse first row
                // (totals, subtotals) are common.
                var threshold = layoutAssisted ? 2 : 3;
                if (nCols >= 3 && emptyData * threshold >= nCols)
                {
                    var sharesHeaderSpacer = layoutAssisted
                        && dataInner
                            .Select((cell, idx) => (cell, idx))
                            .Any(pair => pair.cell.Trim().Length == 0 && headerEmptyIndices.Contains(pair.idx))
                        && LayoutAssistedEmptyHeaderHasDenseBody(markdown, nCols);
                    var isSectionLabel = layoutAssisted
                        && LayoutAssistedSparseSectionRowIsOk(dataInner, markdown, nCols);
                    if (!sharesHeaderSpacer && !isSectionLabel)
                    {
                        return true;
                    }
                }
            }
        }

        // Failure mode 5: cells flow as a continuation paragraph — text
        // wrapping mistaken for column structure. When prose is mis-detected as
        // a multi-column table, cells in the same column tend to start with
        // lowercase letters or mid-sentence punctuation rather than capitals or
        // digits. Real tables almost never have most data cells start lowercase.
        var dataRows = lines
            .Skip(2)
            .Select(l =>
            {
                var parts = l.Split('|').Select(s => s.Trim()).ToList();
                return parts.Count >= 3 ? parts[1..^1] : [];
            })
            .Where(row => row.Count > 0)
            .ToList();

        if (nCols >= 2 && dataRows.Count >= 4)
        {
            var continuation = 0;
            var total = 0;
            foreach (var cell in dataRows.SelectMany(row => row))
            {
                var trimmed = cell.Trim();
                if (trimmed.Length == 0)
                {
                    continue;
                }

                total++;
                var first = trimmed[0];
                if (char.IsLower(first)
                    || first is ',' or '.' or ';' or ')' or '"' or '\'' or '”' or '’')
                {
                    continuation++;
                }
            }

            if (total > 0 && continuation * 5 >= total * 3)
            {
                return true;
            }
        }

        return false;
    }

    private static bool LayoutAssistedEmptyHeaderHasDenseBody(string markdown, int nCols)
    {
        var rows = MarkdownPipeRows(markdown);
        var dataRows = rows
            .Skip(1)
            .Where(row => row.Any(cell => cell.Trim().Length > 0))
            .ToList();
        if (dataRows.Count < 2 || nCols < 3)
        {
            return false;
        }

        var totalCells = dataRows.Count * nCols;
        var filledCells = 0;
        var rowsWithMultipleCells = 0;
        var maxFilledInRow = 0;
        foreach (var row in dataRows)
        {
            var filled = row.Count(cell => cell.Trim().Length > 0);
            filledCells += filled;
            maxFilledInRow = Math.Max(maxFilledInRow, filled);
            if (filled >= 2)
            {
                rowsWithMultipleCells++;
            }
        }

        // Dense enough to be a useful extraction despite lossy merged headers.
        // The row-count gate avoids accepting a single tidy row under a broken
        // header, and the density gate keeps sparse fragments on the OCR path.
        return rowsWithMultipleCells * 2 >= dataRows.Count
            && maxFilledInRow >= Math.Min(nCols, 3)
            && filledCells * 100 >= totalCells * 45;
    }

    private static bool LayoutAssistedSparseSectionRowIsOk(IReadOnlyList<string> row, string markdown, int nCols)
    {
        var labels = row.Select(cell => cell.Trim()).Where(cell => cell.Length > 0).ToList();
        if (labels.Count != 1)
        {
            return false;
        }

        var label = labels[0];
        if (TextUtils.ByteLength(label) > 40 || !label.Any(char.IsLetter))
        {
            return false;
        }

        if (label.EndsWith('.') || label.EndsWith('!') || label.EndsWith('?') || label.EndsWith(':'))
        {
            return false;
        }

        return LayoutAssistedEmptyHeaderHasDenseBody(markdown, nCols);
    }

    /// <summary>True when a rendered table's body is filled in densely enough to trust.</summary>
    public static bool MarkdownTableBodyIsDense(string markdown)
    {
        var rows = MarkdownPipeRows(markdown);
        var dataRows = rows
            .Skip(1)
            .Where(row => row.Any(cell => cell.Trim().Length > 0))
            .ToList();
        if (dataRows.Count < 3)
        {
            return false;
        }

        var cols = rows.Count > 0 ? rows.Max(row => row.Count) : 0;
        if (cols < 3)
        {
            return false;
        }

        var filledCells = 0;
        var rowsWithMultipleCells = 0;
        foreach (var row in dataRows)
        {
            var filled = row.Count(cell => cell.Trim().Length > 0);
            filledCells += filled;
            if (filled >= Math.Min(cols, 3))
            {
                rowsWithMultipleCells++;
            }
        }

        var totalCells = dataRows.Count * cols;
        return rowsWithMultipleCells * 2 >= dataRows.Count && filledCells * 100 >= totalCells * 45;
    }

    /// <summary>The strict validation, for callers with no layout assistance.</summary>
    public static bool LooksLikePartialTable(string markdown) => LooksLikePartialTableEx(markdown, false);
}
