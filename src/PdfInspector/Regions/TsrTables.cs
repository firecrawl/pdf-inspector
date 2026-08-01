// Ported from reference/src/lib.rs
using PdfInspector.Extractor;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Regions;

/// <summary>
/// One cropped table region plus the raw structure-recovery output for it.
/// </summary>
/// <remarks>
/// The structure tokens and bboxes typically come from an external
/// table-structure recognition model — SLANet on PaddleOCR, say — run over a
/// rendered crop of the page. This library uses the structure only to lay the
/// cells out, and pulls the cell text from the native PDF; no OCR is involved.
/// </remarks>
public sealed class TsrTableInput
{
    /// <summary>The 0-indexed page the crop was taken from.</summary>
    public required uint Page { get; init; }

    /// <summary>
    /// The crop bbox on the page as [x1, y1, x2, y2] in PDF points with a
    /// top-left origin, matching the layout model's coordinate space.
    /// </summary>
    public required float[] CropPdfPtBBox { get; init; }

    /// <summary>The DPI the crop image was rendered at, used to convert cell bboxes back to points.</summary>
    public required float RenderDpi { get; init; }

    /// <summary>The raw structure tokens the model emitted, in document order.</summary>
    public List<string> StructureTokens { get; init; } = [];

    /// <summary>
    /// One bbox per cell, in document order and parallel to the cell open-tags
    /// in <see cref="StructureTokens"/>. Either 4 elements ([x1, y1, x2, y2])
    /// or an 8-element 4-corner polygon, in crop image-pixel space.
    /// </summary>
    public List<float[]> CellBBoxes { get; init; } = [];
}

/// <summary>Markdown for one extracted table, plus which path produced it.</summary>
public sealed class TableExtractionResult
{
    /// <summary>The rendered markdown table.</summary>
    public required string Markdown { get; init; }

    /// <summary>
    /// Null when the TSR-hybrid path produced the markdown directly; otherwise a
    /// short identifier for the quality issue that triggered a fallback. The
    /// string is stable enough to use as a metric label.
    /// </summary>
    public string? FallbackReason { get; init; }
}

/// <summary>
/// Region-based table extraction driven by externally supplied structure
/// recovery.
/// </summary>
public static class TsrTables
{
    /// <summary>
    /// Extracts structured cells using externally supplied structure recovery.
    /// </summary>
    /// <remarks>
    /// For each input this pairs every cell open-tag in the structure tokens
    /// with the next bbox in document order — tracking row and column with
    /// rowspan and colspan awareness — converts each bbox from crop pixels into
    /// page PDF points, then pulls the cell's text by overlap-testing PDF text
    /// items inside that bbox. Inputs whose page is out of range, or whose
    /// tokens parse to no cells, produce an empty list.
    /// </remarks>
    public static List<List<StructuredCell>> ExtractTablesWithStructureCellsMem(
        byte[] buffer,
        IReadOnlyList<TsrTableInput> inputs)
    {
        Validation.ValidatePdfBytes(buffer);
        var doc = PdfProcessor.LoadDocumentOrThrow(buffer, null);

        var neededPages = inputs.Select(t => t.Page + 1).ToHashSet();
        var cache = RegionPageCache.Build(doc, neededPages);

        var results = new List<List<StructuredCell>>(inputs.Count);

        foreach (var input in inputs)
        {
            var page1Idx = input.Page + 1;
            if (!cache.ItemsByPage.TryGetValue(page1Idx, out var items))
            {
                // An out-of-range page, or a page with no extractable text.
                results.Add([]);
                continue;
            }

            var pageH = cache.HeightOf(page1Idx);
            var adaptiveThreshold = cache.ThresholdOf(page1Idx);
            var coords = cache.CoordSpaceOf(page1Idx);
            float[] cropOrigin = [input.CropPdfPtBBox[0], input.CropPdfPtBBox[1]];

            var slots = StructuredCells.ParseStructure(input.StructureTokens);
            if (slots.Count == 0)
            {
                results.Add([]);
                continue;
            }

            var cells = new List<StructuredCell>(slots.Count);
            foreach (var slot in slots)
            {
                float[] pagePtBBox = [0.0f, 0.0f, 0.0f, 0.0f];
                if (slot.BBoxIndex < input.CellBBoxes.Count
                    && StructuredCells.PolygonToAabb(input.CellBBoxes[slot.BBoxIndex]) is { } aabbPx)
                {
                    pagePtBBox = StructuredCells.CellPxToPagePt(aabbPx, input.RenderDpi, cropOrigin);
                }

                cells.Add(new StructuredCell
                {
                    Row = slot.Row,
                    Col = slot.Col,
                    RowSpan = slot.RowSpan,
                    ColSpan = slot.ColSpan,
                    IsHeader = slot.IsHeader,
                    Text = string.Empty,
                    PagePtBBox = pagePtBBox,
                });
            }

            StructuredCells.NormalizeCellBands(cells);
            FillCellsFromItems(cells, items, pageH, coords, adaptiveThreshold);
            results.Add(cells);
        }

        return results;
    }

    /// <summary>
    /// Routes PDF text into the structure's cells, in two stages.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Stage 1 is exclusive per-token assignment. Each PDF text item is split
    /// into whitespace-separated tokens with estimated x positions; for each
    /// token the cells whose band-clamped bbox satisfies the strict membership
    /// rule are found, and the closest centre wins. Tokens that land nowhere are
    /// eligible for stage-2 orphan recovery.
    /// </para>
    /// <para>
    /// Per-token rather than per-item routing is what prevents the dense-grid
    /// collapse: a row rendered as one wide show-text operator produces a single
    /// item whose centre lies in only one cell, so per-item routing parks the
    /// whole row there and leaves the rest empty. Token-level exclusivity still
    /// prevents cell bleed when the model emits rows whose y-extents overlap.
    /// </para>
    /// <para>
    /// Stage 2 assigns leftover tokens to their nearest <em>empty</em> cell,
    /// which recovers header text positioned outside a band derived from data
    /// cells, and local row drift where a bbox sits slightly off its text. Only
    /// empty cells are eligible, so a cell filled by stage 1 is never
    /// overwritten or augmented.
    /// </para>
    /// </remarks>
    private static void FillCellsFromItems(
        List<StructuredCell> cells,
        IReadOnlyList<TextItem> items,
        float pageHeight,
        RegionCoordSpace coords,
        float adaptiveThreshold)
    {
        // Each cell's bounds and centre, precomputed so the work is not redone
        // per token.
        var cellMeta = new (RegionBounds Bounds, float Cx, float Cy)?[cells.Count];
        for (var i = 0; i < cells.Count; i++)
        {
            var bbox = cells[i].PagePtBBox;
            float x1 = bbox[0], y1 = bbox[1], x2 = bbox[2], y2 = bbox[3];
            if (x1 >= x2 || y1 >= y2)
            {
                continue;
            }

            var bounds = RegionGeometry.Bounds(x1, y1, x2, y2, pageHeight, coords);
            cellMeta[i] = (bounds, (bounds.XMin + bounds.XMax) * 0.5f, (bounds.YMin + bounds.YMax) * 0.5f);
        }

        var perCellItems = new List<TextItem>[cells.Count];
        for (var i = 0; i < cells.Count; i++)
        {
            perCellItems[i] = [];
        }

        // Tokens that did not land in any cell during stage 1. Token-grain
        // orphan candidates let stage 2 recover individual words that fell just
        // outside their cell's clamped band, without re-attributing tokens that
        // were already claimed.
        var orphanTokens = new List<TextItem>();

        foreach (var item in items)
        {
            foreach (var tokenItem in SplitItemIntoTokenSubitems(item))
            {
                var tokenW = TextUtils.EffectiveWidth(tokenItem);
                var tokenCx = tokenItem.X + (tokenW * 0.5f);
                var tokenCy = tokenItem.Y + (tokenItem.Height * 0.5f);
                var best = -1;
                var bestDist = 0.0f;

                for (var cellIdx = 0; cellIdx < cellMeta.Length; cellIdx++)
                {
                    if (cellMeta[cellIdx] is not { } meta
                        || !RegionGeometry.TsrContainsItem(tokenItem, meta.Bounds))
                    {
                        continue;
                    }

                    var dx = tokenCx - meta.Cx;
                    var dy = tokenCy - meta.Cy;
                    var distSq = (dx * dx) + (dy * dy);
                    if (best < 0 || distSq < bestDist)
                    {
                        best = cellIdx;
                        bestDist = distSq;
                    }
                }

                if (best >= 0)
                {
                    perCellItems[best].Add(tokenItem);
                }
                else
                {
                    orphanTokens.Add(tokenItem);
                }
            }
        }

        // Markdown cells must be one line, so line breaks from the line
        // grouping pass are collapsed.
        for (var cellIdx = 0; cellIdx < cells.Count; cellIdx++)
        {
            cells[cellIdx].Text = RegionGeometry
                .CollectTextFromMatchedItems(perCellItems[cellIdx], adaptiveThreshold)
                .Replace('\n', ' ')
                .Replace('\r', ' ');
        }

        AssignOrphanItems(orphanTokens, cells, pageHeight, coords);
    }

    /// <summary>
    /// Splits an item into one virtual sub-item per whitespace-separated token,
    /// with each token's x and width estimated from the item's effective width
    /// and the token's character offset.
    /// </summary>
    /// <remarks>
    /// The per-character estimate is <c>effectiveWidth / charCount</c>, uniform
    /// across the item. That is fine for routing, since only the cell each
    /// token's centre lands in matters, not its exact position. A single-token
    /// item collapses to a one-element list equivalent to the input, so this is
    /// a no-op in the common case.
    /// </remarks>
    private static List<TextItem> SplitItemIntoTokenSubitems(TextItem item)
    {
        var chars = item.Text.EnumerateRunes().ToList();
        var totalChars = chars.Count;
        if (totalChars == 0)
        {
            return [];
        }

        var itemW = TextUtils.EffectiveWidth(item);
        var charW = itemW / totalChars;

        var tokens = new List<TextItem>();
        var currentToken = new System.Text.StringBuilder();
        int? currentStartIdx = null;

        void PushToken(string text, int startIdx, int endIdx)
        {
            if (text.Length == 0)
            {
                return;
            }

            var sub = item.Clone();
            sub.Text = text;
            sub.X = item.X + (startIdx * charW);
            sub.Width = (endIdx - startIdx) * charW;
            tokens.Add(sub);
        }

        for (var idx = 0; idx < totalChars; idx++)
        {
            var rune = chars[idx];
            if (System.Text.Rune.IsWhiteSpace(rune))
            {
                if (currentStartIdx is { } startIdx)
                {
                    PushToken(currentToken.ToString(), startIdx, idx);
                    currentToken.Clear();
                    currentStartIdx = null;
                }
            }
            else
            {
                currentStartIdx ??= idx;
                currentToken.Append(rune);
            }
        }

        if (currentStartIdx is { } lastStart)
        {
            PushToken(currentToken.ToString(), lastStart, totalChars);
        }

        return tokens;
    }

    /// <summary>
    /// Plausibility caps for orphan assignment: the maximum x and y distance
    /// from a text item's centre to a candidate empty cell's bbox before the
    /// candidate is rejected.
    /// </summary>
    /// <remarks>
    /// The caps derive from cell geometry so they scale with the table: a dense
    /// small-row table gets a tight cap, a looser table more slack. The 5pt
    /// floors guard against a degenerate single-cell table collapsing the cap
    /// to zero, while staying tight enough not to cross into a neighbouring row
    /// or column.
    /// </remarks>
    private static (float CapX, float CapY) AssignmentCaps(IReadOnlyList<StructuredCell> cells)
    {
        var widths = new List<float>(cells.Count);
        var heights = new List<float>(cells.Count);
        foreach (var cell in cells)
        {
            var bbox = cell.PagePtBBox;
            var w = MathF.Abs(bbox[2] - bbox[0]);
            var h = MathF.Abs(bbox[3] - bbox[1]);
            if (w > 0.0f && h > 0.0f)
            {
                widths.Add(w);
                heights.Add(h);
            }
        }

        if (widths.Count == 0)
        {
            return (0.0f, 0.0f);
        }

        widths.Sort(FloatTotalOrder.Instance);
        heights.Sort(FloatTotalOrder.Instance);
        return (MathF.Max(widths[widths.Count / 2], 5.0f), MathF.Max(heights[heights.Count / 2], 5.0f));
    }

    /// <summary>
    /// Assigns each unclaimed token to its nearest empty cell within the caps,
    /// appending the text. Distance is point-to-rect: zero when the item centre
    /// is inside the bbox, otherwise the axis-aligned gap to the nearest edge.
    /// </summary>
    private static void AssignOrphanItems(
        IReadOnlyList<TextItem> items,
        List<StructuredCell> cells,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        if (cells.Count == 0)
        {
            return;
        }

        var (capX, capY) = AssignmentCaps(cells);
        if (capX <= 0.0f || capY <= 0.0f)
        {
            return;
        }

        // The y-tolerance for "same line as a previous orphan". A multi-token
        // name is several separate text items and should stack into the same
        // cell, but two orphans on different PDF rows targeting the same empty
        // cell must not merge — that produces run-on cells. Half a row of slack
        // is conservative.
        var yTolerance = MathF.Max(capY * 0.5f, 3.0f);

        // Each empty cell's bounds, precomputed so page coordinates are not
        // re-flipped for every orphan/candidate pair.
        var cellBounds = new RegionBounds?[cells.Count];
        for (var i = 0; i < cells.Count; i++)
        {
            if (cells[i].Text.Length > 0)
            {
                continue;
            }

            var bbox = cells[i].PagePtBBox;
            float x1 = bbox[0], y1 = bbox[1], x2 = bbox[2], y2 = bbox[3];
            if (x1 >= x2 || y1 >= y2)
            {
                continue;
            }

            cellBounds[i] = RegionGeometry.Bounds(x1, y1, x2, y2, pageHeight, coordSpace);
        }

        // The y-centre of the first orphan that landed in each cell, so later
        // orphans only stack when they are on the same line.
        var firstY = new Dictionary<int, float>();

        foreach (var item in items)
        {
            if (item.Text.Trim().Length == 0)
            {
                continue;
            }

            var itemW = TextUtils.EffectiveWidth(item);
            var cx = item.X + (itemW * 0.5f);
            var cy = item.Y + (item.Height * 0.5f);

            var best = -1;
            var bestDist = 0.0f;
            for (var ci = 0; ci < cellBounds.Length; ci++)
            {
                if (cellBounds[ci] is not { } bounds)
                {
                    continue;
                }

                // Once an orphan has landed here, only a same-line orphan may
                // join; a cross-line one looks elsewhere.
                if (firstY.TryGetValue(ci, out var y) && MathF.Abs(y - cy) > yTolerance)
                {
                    continue;
                }

                var dx = MathF.Max(MathF.Max(bounds.XMin - cx, 0.0f), cx - bounds.XMax);
                var dy = MathF.Max(MathF.Max(bounds.YMin - cy, 0.0f), cy - bounds.YMax);
                if (dx > capX || dy > capY)
                {
                    continue;
                }

                var distSq = (dx * dx) + (dy * dy);
                if (best < 0 || distSq < bestDist)
                {
                    best = ci;
                    bestDist = distSq;
                }
            }

            if (best >= 0)
            {
                var trimmed = item.Text.Trim();
                cells[best].Text = cells[best].Text.Length == 0
                    ? trimmed
                    : cells[best].Text + " " + trimmed;
                firstY.TryAdd(best, cy);
            }
        }
    }

    /// <summary>
    /// Extracts markdown tables using externally supplied structure recovery.
    /// </summary>
    /// <remarks>
    /// A convenience wrapper around
    /// <see cref="ExtractTablesWithStructureCellsMem"/> that renders each cell
    /// list to markdown. Inputs whose page is out of range, or whose tokens
    /// parse to no cells, produce an empty string.
    /// </remarks>
    public static List<string> ExtractTablesWithStructureMem(
        byte[] buffer,
        IReadOnlyList<TsrTableInput> inputs) =>
        [.. ExtractTablesWithStructureCellsMem(buffer, inputs)
            .Select(cells => cells.Count == 0 ? string.Empty : StructuredCells.CellsToMarkdown(cells))];

    /// <summary>
    /// The self-healing variant: runs the TSR-hybrid path, checks the cells for
    /// known structure-model pathologies, and falls back to the heuristic
    /// region extractor for any input where the TSR path looks compromised.
    /// </summary>
    /// <remarks>
    /// <para>
    /// On clean inputs this matches
    /// <see cref="ExtractTablesWithStructureMem"/>. On a multi-row-in-cell
    /// issue, over-stuffed rows are expanded in place first; only if that
    /// cannot produce a usable table does the heuristic markdown replace the
    /// TSR markdown.
    /// </para>
    /// <para>
    /// Three failure modes are guarded per input. When the heuristic returns
    /// nothing for a flagged region, the original TSR markdown is preserved and
    /// the reason gains a <c>_heuristic_empty</c> suffix, so a usable
    /// wrong-but-non-empty output is not replaced by literally nothing. When
    /// in-place recovery succeeds, the result is labelled
    /// <c>multi_row_in_cell_expanded</c> and the heuristic is not consulted. Any
    /// failure in detection or heuristic extraction is contained to its own
    /// input, which returns the raw TSR markdown with an <c>_error</c> label.
    /// </para>
    /// </remarks>
    public static List<TableExtractionResult> ExtractTablesWithStructureAutoMem(
        byte[] buffer,
        IReadOnlyList<TsrTableInput> inputs)
    {
        var tsrCells = ExtractTablesWithStructureCellsMem(buffer, inputs);
        var results = new List<TableExtractionResult>(inputs.Count);

        for (var i = 0; i < inputs.Count; i++)
        {
            var input = inputs[i];
            var cells = tsrCells[i];
            var tsrMarkdown = cells.Count == 0 ? string.Empty : StructuredCells.CellsToMarkdown(cells);

            TsrQualityIssue? issue;
            try
            {
                issue = DetectQualityIssue(buffer, input, cells);
            }
            catch (PdfException)
            {
                // Detection failed for this input — fall through with the raw
                // TSR markdown so the rest of the batch is unaffected, tagging
                // the reason for caller metrics.
                results.Add(new TableExtractionResult
                {
                    Markdown = tsrMarkdown,
                    FallbackReason = "detection_error",
                });
                continue;
            }

            if (issue is null)
            {
                results.Add(new TableExtractionResult { Markdown = tsrMarkdown });
                continue;
            }

            var reason = issue.Reason;
            if (issue.ExpandedCells is { } expandedCells)
            {
                var expandedMarkdown = StructuredCells.CellsToMarkdown(expandedCells);
                if (expandedMarkdown.Trim().Length > 0)
                {
                    results.Add(new TableExtractionResult
                    {
                        Markdown = expandedMarkdown,
                        FallbackReason = "multi_row_in_cell_expanded",
                    });
                    continue;
                }
            }

            // Fall back to the heuristic over the input's table region: the
            // crop's PDF-point bbox is the table region.
            string heuristicMarkdown;
            try
            {
                var pages = RegionExtraction.ExtractTablesInRegionsMem(
                    buffer, [(input.Page, new[] { input.CropPdfPtBBox })]);
                heuristicMarkdown = pages.Count > 0 && pages[0].Regions.Count > 0
                    ? pages[0].Regions[0].Text
                    : string.Empty;
            }
            catch (PdfException)
            {
                results.Add(new TableExtractionResult
                {
                    Markdown = tsrMarkdown,
                    FallbackReason = $"{reason}_heuristic_error",
                });
                continue;
            }

            results.Add(heuristicMarkdown.Trim().Length == 0
                ? new TableExtractionResult
                {
                    // The heuristic produced nothing useful — keep the TSR
                    // markdown rather than ship empty. The suffix lets callers
                    // count this case.
                    Markdown = tsrMarkdown,
                    FallbackReason = $"{reason}_heuristic_empty",
                }
                : new TableExtractionResult { Markdown = heuristicMarkdown, FallbackReason = reason });
        }

        return results;
    }

    /// <summary>A known structure-model pathology found in one input's cells.</summary>
    private sealed record TsrQualityIssue(string Reason, List<StructuredCell>? ExpandedCells);

    /// <summary>
    /// Detects quality issues in the TSR-hybrid output for a single input.
    /// </summary>
    /// <remarks>
    /// <para>
    /// <c>phantom_empty_row</c> — a row whose every cell is empty, with content
    /// both above and below. Structure models sometimes emit an extra row that
    /// matches no visible PDF row.
    /// </para>
    /// <para>
    /// <c>multi_row_in_cell</c> — a non-label rowspan-1 cell encloses PDF text
    /// that clusters into two distinct visual lines separated by a whitespace
    /// gap larger than the line height. Cells declared with a rowspan above 1
    /// are excluded, since those are expected to span lines. First-row and
    /// first-column wraps are ignored unless in-place row expansion has enough
    /// support to repair them, because those are often legitimate wrapped
    /// headers or row labels.
    /// </para>
    /// </remarks>
    private static TsrQualityIssue? DetectQualityIssue(
        byte[] buffer,
        TsrTableInput input,
        IReadOnlyList<StructuredCell> cells)
    {
        if (cells.Count == 0)
        {
            return null;
        }

        // A phantom row is cheap: it comes from the cell metadata alone.
        var maxRow = cells.Max(c => c.Row);
        if (maxRow >= 2)
        {
            var rowHasContent = new bool[maxRow + 1];
            foreach (var cell in cells)
            {
                if (cell.Text.Trim().Length > 0)
                {
                    rowHasContent[cell.Row] = true;
                }
            }

            for (var r = 1; r < maxRow; r++)
            {
                if (!rowHasContent[r] && rowHasContent[r - 1] && rowHasContent[r + 1])
                {
                    return new TsrQualityIssue("phantom_empty_row", null);
                }
            }
        }

        // Multi-row-in-cell needs the page's text items back: look for
        // rowspan-1 cells holding items that group into two or more visual
        // lines separated by a real whitespace gap. That is a tall model cell
        // catching text from two adjacent PDF rows it failed to separate.
        var doc = PdfProcessor.LoadDocumentOrThrow(buffer, null);
        var page1Idx = input.Page + 1;
        var page = doc.GetPage((int)page1Idx);
        if (page is null)
        {
            return null;
        }

        var pageH = RegionGeometry.GetPageHeight(doc, page) ?? 792.0f;
        var fontCMaps = FontCMaps.FromDocumentPagesFast(doc, [page1Idx]);
        var extraction = ContentStreamExtractor.ExtractPageTextItems(
            doc, page, page1Idx, fontCMaps, false, new FontStyleCache());
        var items = extraction.Items;
        var adaptiveThreshold = TextUtils.FixLetterspacedItems(items);
        var coords = extraction.CoordsRotated ? RegionCoordSpace.Rotated90Ccw : RegionCoordSpace.Standard;

        var expandedCells = TryExpandMultiRowCells(cells, items, pageH, coords, adaptiveThreshold);
        var firstRow = cells.Min(cell => cell.Row);
        var firstCol = cells.Min(cell => cell.Col);

        foreach (var cell in cells)
        {
            // Cells with a rowspan above 1 are intentionally multi-line.
            if (cell.RowSpan > 1 || cell.Text.Trim().Length == 0)
            {
                continue;
            }

            var cellItems = CollectItemsInCell(items, cell, pageH, coords);
            if (cellItems.Count < 2 || ClusterCellTextLines(cellItems).Count < 2)
            {
                continue;
            }

            if (expandedCells is not null)
            {
                return new TsrQualityIssue("multi_row_in_cell", expandedCells);
            }

            if (!IsWrappedLabelCell(cell, firstRow, firstCol))
            {
                return new TsrQualityIssue("multi_row_in_cell", null);
            }
        }

        return null;
    }

    private static bool IsWrappedLabelCell(StructuredCell cell, int firstRow, int firstCol) =>
        cell.IsHeader || cell.Row == firstRow || cell.Col == firstCol;

    /// <summary>A run of items on one visual baseline inside a TSR cell.</summary>
    private sealed class CellTextLine
    {
        public float CenterY { get; private set; }

        public float HalfHeight { get; private set; }

        public List<TextItem> Items { get; } = [];

        public CellTextLine(TextItem item)
        {
            CenterY = item.Y + (item.Height * 0.5f);
            HalfHeight = MathF.Max(item.Height * 0.5f, 2.5f);
            Items.Add(item);
        }

        public void Add(TextItem item)
        {
            var centerY = item.Y + (item.Height * 0.5f);
            var existing = (float)Items.Count;
            CenterY = ((CenterY * existing) + centerY) / (existing + 1.0f);
            HalfHeight = MathF.Max(HalfHeight, MathF.Max(item.Height * 0.5f, 2.5f));
            Items.Add(item);
        }

        public float BottomY() => Items.Aggregate(float.PositiveInfinity, (acc, item) => MathF.Min(acc, item.Y));
    }

    /// <summary>The visual bands one over-stuffed row expands into.</summary>
    private sealed record RowExpansion(List<float> Bands, float Tolerance);

    private static List<TextItem> CollectItemsInCell(
        IReadOnlyList<TextItem> items,
        StructuredCell cell,
        float pageHeight,
        RegionCoordSpace coordSpace)
    {
        var bbox = cell.PagePtBBox;
        float x1 = bbox[0], y1 = bbox[1], x2 = bbox[2], y2 = bbox[3];
        if (x1 >= x2 || y1 >= y2)
        {
            return [];
        }

        var bounds = RegionGeometry.Bounds(x1, y1, x2, y2, pageHeight, coordSpace);
        return [.. items
            .Where(item => item.Text.Trim().Length > 0 && RegionGeometry.TsrContainsItem(item, bounds))
            .Select(item => item.Clone())];
    }

    private static List<CellTextLine> ClusterCellTextLines(List<TextItem> items)
    {
        if (items.Count == 0)
        {
            return [];
        }

        items.Sort((a, b) =>
        {
            var ay = a.Y + (a.Height * 0.5f);
            var by = b.Y + (b.Height * 0.5f);
            var byCenter = FloatTotalOrder.Instance.Compare(by, ay);
            return byCenter != 0 ? byCenter : FloatTotalOrder.Instance.Compare(a.X, b.X);
        });

        var lines = new List<CellTextLine>();
        foreach (var item in items)
        {
            var itemTop = item.Y + item.Height;
            var itemHalfHeight = MathF.Max(item.Height * 0.5f, 2.5f);
            if (lines.Count > 0)
            {
                var last = lines[^1];
                var gap = last.BottomY() - itemTop;
                if (gap <= MathF.Max(last.HalfHeight, itemHalfHeight))
                {
                    last.Add(item);
                    continue;
                }
            }

            lines.Add(new CellTextLine(item));
        }

        return lines;
    }

    private static RowExpansion? BuildRowExpansion(
        IReadOnlyList<int> rowCells,
        IReadOnlyList<StructuredCell> cells,
        IReadOnlyList<List<CellTextLine>> cellLines)
    {
        if (rowCells.Count == 0)
        {
            return null;
        }

        if (rowCells.Any(idx => cells[idx].RowSpan > 1 || cells[idx].ColSpan > 1))
        {
            return null;
        }

        var multilineCells = rowCells.Count(idx => cellLines[idx].Count >= 2);
        if (rowCells.Count >= 2 && multilineCells < 2)
        {
            return null;
        }

        if (rowCells.Count == 1 && multilineCells == 0)
        {
            return null;
        }

        var centers = rowCells
            .SelectMany(idx => cellLines[idx].Select(line => (line.CenterY, line.HalfHeight)))
            .ToList();
        if (centers.Count < 2)
        {
            return null;
        }

        centers.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.CenterY, a.CenterY));

        var halfHeights = centers.Select(c => c.HalfHeight).ToList();
        halfHeights.Sort(FloatTotalOrder.Instance);
        var tolerance = MathF.Max(halfHeights[halfHeights.Count / 2] * 0.8f, 3.0f);

        var bands = new List<(float Center, int Count)>();
        foreach (var (center, _) in centers)
        {
            var found = false;
            for (var i = 0; i < bands.Count; i++)
            {
                if (MathF.Abs(bands[i].Center - center) <= tolerance)
                {
                    var (bandCenter, count) = bands[i];
                    bands[i] = (((bandCenter * count) + center) / (count + 1), count + 1);
                    found = true;
                    break;
                }
            }

            if (!found)
            {
                bands.Add((center, 1));
            }
        }

        // V1 targets the common one- and two-lost-row cases; a larger
        // compression stays on the heuristic fallback until there is evidence
        // to broaden it.
        if (bands.Count is < 2 or > 4)
        {
            return null;
        }

        var minSupport = rowCells.Count >= 2 ? 2 : 1;
        var supported = bands.All(band =>
            rowCells.Count(idx =>
                cellLines[idx].Any(line => MathF.Abs(line.CenterY - band.Center) <= tolerance)) >= minSupport);
        if (!supported)
        {
            return null;
        }

        bands.Sort((a, b) => FloatTotalOrder.Instance.Compare(b.Center, a.Center));
        return new RowExpansion([.. bands.Select(b => b.Center)], tolerance);
    }

    private static string TextForBand(
        IReadOnlyList<CellTextLine> lines,
        float band,
        float tolerance,
        float adaptiveThreshold)
    {
        var matched = lines
            .Where(line => MathF.Abs(line.CenterY - band) <= tolerance)
            .SelectMany(line => line.Items.Select(item => item.Clone()))
            .ToList();
        if (matched.Count == 0)
        {
            return string.Empty;
        }

        matched.Sort((a, b) =>
        {
            var byY = FloatTotalOrder.Instance.Compare(b.Y, a.Y);
            return byY != 0 ? byY : FloatTotalOrder.Instance.Compare(a.X, b.X);
        });

        return RegionGeometry.CollectTextFromMatchedItems(matched, adaptiveThreshold)
            .Replace('\n', ' ')
            .Replace('\r', ' ');
    }

    private static float[] SliceCellBBoxForExpandedRow(StructuredCell cell, int rowIdx, int rowCount)
    {
        var bbox = (float[])cell.PagePtBBox.Clone();
        var top = MathF.Min(bbox[1], bbox[3]);
        var bottom = MathF.Max(bbox[1], bbox[3]);
        var height = bottom - top;
        if (height <= 0.0f || rowCount == 0)
        {
            return bbox;
        }

        var step = height / rowCount;
        bbox[1] = top + (step * rowIdx);
        bbox[3] = rowIdx + 1 == rowCount ? bottom : top + (step * (rowIdx + 1));
        return bbox;
    }

    /// <summary>
    /// Rebuilds the cell list with over-stuffed rows split into the visual
    /// bands their text actually occupies, or null when nothing expands.
    /// </summary>
    private static List<StructuredCell>? TryExpandMultiRowCells(
        IReadOnlyList<StructuredCell> cells,
        IReadOnlyList<TextItem> items,
        float pageHeight,
        RegionCoordSpace coordSpace,
        float adaptiveThreshold)
    {
        if (cells.Count == 0)
        {
            return null;
        }

        var cellLines = cells
            .Select(cell => cell.RowSpan > 1
                ? []
                : ClusterCellTextLines(CollectItemsInCell(items, cell, pageHeight, coordSpace)))
            .ToList();

        var cellsByRow = new SortedDictionary<int, List<int>>();
        for (var idx = 0; idx < cells.Count; idx++)
        {
            if (!cellsByRow.TryGetValue(cells[idx].Row, out var list))
            {
                list = [];
                cellsByRow[cells[idx].Row] = list;
            }

            list.Add(idx);
        }

        var expansions = new Dictionary<int, RowExpansion>();
        foreach (var (row, rowCells) in cellsByRow)
        {
            var coveredByRowspan = cells.Any(cell =>
                cell.RowSpan > 1 && cell.Row <= row && row < cell.Row + cell.RowSpan);
            if (coveredByRowspan)
            {
                continue;
            }

            if (BuildRowExpansion(rowCells, cells, cellLines) is { } expansion)
            {
                expansions[row] = expansion;
            }
        }

        if (expansions.Count == 0)
        {
            return null;
        }

        var expanded = new List<StructuredCell>(cells.Count + expansions.Count);
        var rowShift = 0;
        foreach (var (row, rowCells) in cellsByRow)
        {
            if (expansions.TryGetValue(row, out var expansion))
            {
                for (var bandIdx = 0; bandIdx < expansion.Bands.Count; bandIdx++)
                {
                    foreach (var cellIdx in rowCells)
                    {
                        var cell = Copy(cells[cellIdx]);
                        cell.Row = row + rowShift + bandIdx;
                        cell.RowSpan = 1;
                        cell.Text = TextForBand(
                            cellLines[cellIdx], expansion.Bands[bandIdx], expansion.Tolerance, adaptiveThreshold);
                        cell.PagePtBBox = SliceCellBBoxForExpandedRow(cell, bandIdx, expansion.Bands.Count);
                        expanded.Add(cell);
                    }
                }

                rowShift += expansion.Bands.Count - 1;
            }
            else
            {
                foreach (var cellIdx in rowCells)
                {
                    var cell = Copy(cells[cellIdx]);
                    cell.Row += rowShift;
                    expanded.Add(cell);
                }
            }
        }

        var originalRows = cells.Max(cell => cell.Row + Math.Max(cell.RowSpan, 1));
        var expandedRows = expanded.Count > 0 ? expanded.Max(cell => cell.Row + Math.Max(cell.RowSpan, 1)) : 0;
        return expandedRows > originalRows ? expanded : null;
    }

    /// <summary>
    /// Copies a cell, including its bbox array. C# cells are reference types
    /// where the Rust originals are values, so the bbox has to be cloned or the
    /// expansion's per-band slicing would rewrite the source cell too.
    /// </summary>
    private static StructuredCell Copy(StructuredCell cell) => new()
    {
        Row = cell.Row,
        Col = cell.Col,
        RowSpan = cell.RowSpan,
        ColSpan = cell.ColSpan,
        IsHeader = cell.IsHeader,
        Text = cell.Text,
        PagePtBBox = (float[])cell.PagePtBBox.Clone(),
    };
}
