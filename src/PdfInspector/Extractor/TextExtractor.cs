// Ported from reference/src/extractor/mod.rs
using PdfInspector.Pdf;
using PdfInspector.Tables;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>
/// A whole document's extraction: its text items, rectangles and line segments,
/// plus the per-page state later stages need.
/// </summary>
internal sealed class DocumentExtraction
{
    public List<TextItem> Items { get; init; } = [];

    public List<PdfRect> Rects { get; init; } = [];

    public List<PdfLine> Lines { get; init; } = [];

    /// <summary>Per-page adaptive join thresholds, from Canva-style letter-spacing detection.</summary>
    public Dictionary<uint, float> PageThresholds { get; init; } = [];

    /// <summary>Pages whose fonts encode glyph ids with no usable unicode mapping.</summary>
    public HashSet<uint> GidEncodedPages { get; init; } = [];
}

/// <summary>
/// Text extraction from a PDF, with the position information the structure
/// detectors need.
/// </summary>
internal static class TextExtractor
{
    private const string Module = "extractor";

    /// <summary>
    /// Extracts positioned text, rectangles and line segments from a loaded
    /// document, along with the per-page adaptive join thresholds.
    /// </summary>
    public static DocumentExtraction ExtractPositionedText(
        PdfDocument doc,
        FontCMaps fontCMaps,
        IReadOnlySet<uint>? pageFilter = null) =>
        ExtractPositionedTextImpl(doc, fontCMaps, pageFilter, includeInvisible: false);

    /// <summary>
    /// As <see cref="ExtractPositionedText"/>, but keeping invisible (Tr 3) text.
    /// Mixed and template PDFs put their OCR text layer there.
    /// </summary>
    public static DocumentExtraction ExtractPositionedTextIncludeInvisible(
        PdfDocument doc,
        FontCMaps fontCMaps,
        IReadOnlySet<uint>? pageFilter = null) =>
        ExtractPositionedTextImpl(doc, fontCMaps, pageFilter, includeInvisible: true);

    /// <summary>The shared body of both extraction entry points.</summary>
    private static DocumentExtraction ExtractPositionedTextImpl(
        PdfDocument doc,
        FontCMaps fontCMaps,
        IReadOnlySet<uint>? pageFilter,
        bool includeInvisible)
    {
        var result = new DocumentExtraction();

        // Embedded-font style flags are document-scoped: one font program is
        // shared across pages, so it is parsed once rather than once per page.
        var styleCache = new FontStyleCache();

        var pageIds = doc.PageIds;
        for (var index = 0; index < pageIds.Count; index++)
        {
            var pageNum = (uint)(index + 1);
            if (pageFilter is not null && !pageFilter.Contains(pageNum))
            {
                continue;
            }

            var page = doc.GetPage((int)pageNum);
            if (page is null)
            {
                continue;
            }

            var extraction = ContentStreamExtractor.ExtractPageTextItems(
                doc, page, pageNum, fontCMaps, includeInvisible, styleCache);

            var items = extraction.Items;
            var rects = extraction.Rects;
            var lines = extraction.Lines;

            var clippedBox = extraction.CoordsRotated
                ? null
                : ClipToPageBox(doc, pageIds[index], pageNum, items, rects, lines);

            if (extraction.HasGidFonts)
            {
                result.GidEncodedPages.Add(pageNum);
            }

            var threshold = TextUtils.FixLetterspacedItems(items);
            if (threshold > 0.10f)
            {
                result.PageThresholds[pageNum] = threshold;
            }

            SuppressTableUnderlines(items, rects, lines, pageNum);

            Log.Debug(Module, () =>
                $"page {pageNum}: {items.Count} text items, {rects.Count} rects, {lines.Count} lines" +
                (extraction.HasGidFonts ? " [gid-encoded fonts]" : string.Empty));

            result.Items.AddRange(items);
            result.Rects.AddRange(rects);
            result.Lines.AddRange(lines);

            var links = Links.ExtractPageLinks(doc, page, pageNum);

            // Annotations from a neighbouring page are off-box too.
            if (clippedBox is { } box)
            {
                links.RemoveAll(it =>
                {
                    var cx = it.X + (it.Width / 2.0f);

                    // Centre y, not the item's y: a link item carries an annotation
                    // rect, so its y is a box edge — unlike a text item, whose y is a
                    // baseline and reads naturally on its own.
                    var cy = it.Y + (it.Height / 2.0f);
                    return !(cx >= box.X0 - 6.0f && cx <= box.X1 + 6.0f
                        && cy >= box.Y0 - 6.0f && cy <= box.Y1 + 6.0f);
                });
            }

            result.Items.AddRange(links);
        }

        var pageMap = new Dictionary<PdfObjectId, uint>();
        for (var index = 0; index < pageIds.Count; index++)
        {
            pageMap[pageIds[index]] = (uint)(index + 1);
        }

        result.Items.AddRange(Links.ExtractFormFields(doc, pageMap));
        return result;
    }

    /// <summary>
    /// Clips a page's content to its visible box. A single-page extract or an
    /// imposed spread keeps the neighbouring pages' content in the stream,
    /// positioned outside the crop box; extracting it interleaves invisible text
    /// into the page and poisons the font statistics.
    /// </summary>
    /// <returns>The box that was clipped to, or null when nothing was clipped.</returns>
    private static (float X0, float Y0, float X1, float Y1)? ClipToPageBox(
        PdfDocument doc,
        PdfObjectId pageId,
        uint pageNum,
        List<TextItem> items,
        List<PdfRect> rects,
        List<PdfLine> lines)
    {
        if (GetPageBox(doc, pageId) is not { } box)
        {
            return null;
        }

        var (bx0, by0, bx1, by1) = box;
        const float Tol = 6.0f;

        bool Outside(TextItem it)
        {
            var cx = it.X + (it.Width / 2.0f);
            return !(cx >= bx0 - Tol && cx <= bx1 + Tol && it.Y >= by0 - Tol && it.Y <= by1 + Tol);
        }

        // Only clip when the off-page material reads as coherent text — a
        // neighbouring page's paragraphs. Curved and rotated display text leaves
        // short glyph fragments with artifact coordinates outside the box, and
        // those must stay.
        var off = items.Where(Outside).ToList();

        // Judge by character mass: a paragraph is dominated by long word runs even
        // when interleaved with short maths fragments, while glyph confetti is
        // short items through and through.
        var totalChars = off.Sum(it => TextUtils.CharCount(it.Text.Trim()));
        var wordyChars = off
            .Select(it => TextUtils.CharCount(it.Text.Trim()))
            .Where(n => n >= 4)
            .Sum();

        // Genuine neighbouring-page content is cleanly separated from on-page text.
        // When an off-page item continues an on-page line — same baseline, adjacent
        // x — the coordinates are artifacts of transforms the extractor mis-models,
        // and clipping those would lose real text.
        var straddles = off.Any(o => items.Any(i =>
            !Outside(i)
            && MathF.Abs(i.Y - o.Y) <= 2.0f
            && MathF.Abs(o.X - (i.X + i.Width)) <= 10.0f));

        var coherent = off.Count >= 10 && wordyChars * 2 >= Math.Max(totalChars, 1) && !straddles;
        if (bx1 - bx0 < 72.0f || by1 - by0 < 72.0f || !coherent)
        {
            return null;
        }

        var before = items.Count;
        items.RemoveAll(Outside);
        if (items.Count >= before)
        {
            return null;
        }

        Log.Debug(Module, () =>
            $"page {pageNum}: clipped {before - items.Count} items outside page box " +
            $"({bx0:F0},{by0:F0})-({bx1:F0},{by1:F0})");

        // Off-page geometry is only pruned where off-page text existed, since that
        // is the same neighbouring-page content.
        bool Overlaps(float x, float y, float w, float h)
        {
            var (x0, x1) = w < 0.0f ? (x + w, x) : (x, x + w);
            var (y0, y1) = h < 0.0f ? (y + h, y) : (y, y + h);
            return x0 < bx1 + Tol && x1 > bx0 - Tol && y0 < by1 + Tol && y1 > by0 - Tol;
        }

        rects.RemoveAll(r => !Overlaps(r.X, r.Y, r.Width, r.Height));
        lines.RemoveAll(l => !Overlaps(
            MathF.Min(l.X1, l.X2),
            MathF.Min(l.Y1, l.Y2),
            MathF.Abs(l.X2 - l.X1),
            MathF.Abs(l.Y2 - l.Y1)));

        return box;
    }

    /// <summary>
    /// Clears the underline and strikeout flags on items that a detected table
    /// claims: a table's cell rules are geometry, not emphasis.
    /// </summary>
    private static void SuppressTableUnderlines(
        List<TextItem> items,
        List<PdfRect> rects,
        List<PdfLine> lines,
        uint page)
    {
        if (!items.Any(item => item.IsUnderline || item.IsStrikeout))
        {
            return;
        }

        // A "table" that swallows nearly every text item on the page is a
        // detection artifact — a prose page with boxed callouts and stacked
        // underline rules reading as one giant grid — not a real table. Letting it
        // through here erases every legitimate underline on the page. Real ruled
        // tables share the page with headings, captions and body text, and their
        // cells hold short values; a cell of hundreds of characters means the grid
        // captured flowing prose.
        static bool Plausible(Table table)
        {
            var lens = table.Cells
                .SelectMany(row => row)
                .Where(cell => cell.Trim().Length > 0)
                .Select(TextUtils.CharCount)
                .ToList();

            if (lens.Count == 0)
            {
                return false;
            }

            var longCells = lens.Count(n => n > 100);
            return longCells < lens.Count * 0.3f;
        }

        var tableItemIndices = new HashSet<int>();

        if (rects.Count > 0)
        {
            var (rectTables, _) = RectTables.DetectTablesFromRects(items, rects, page);
            foreach (var table in rectTables.Where(Plausible))
            {
                tableItemIndices.UnionWith(table.ItemIndices);
            }
        }

        if (lines.Count > 0)
        {
            foreach (var table in LineDetector.DetectTablesFromLines(items, lines, page).Where(Plausible))
            {
                tableItemIndices.UnionWith(table.ItemIndices);
            }
        }

        foreach (var index in tableItemIndices)
        {
            if (index >= 0 && index < items.Count)
            {
                items[index].IsUnderline = false;
                items[index].IsStrikeout = false;
            }
        }
    }

    /// <summary>
    /// The page's visible box, preferring the crop box over the media box, and
    /// walking up the page tree for an inherited one.
    /// </summary>
    private static (float X0, float Y0, float X1, float Y1)? GetPageBox(PdfDocument doc, PdfObjectId pageId)
    {
        var v = FindBox(doc, pageId, "CropBox") ?? FindBox(doc, pageId, "MediaBox");
        if (v is null)
        {
            return null;
        }

        return (
            MathF.Min(v[0], v[2]),
            MathF.Min(v[1], v[3]),
            MathF.Max(v[0], v[2]),
            MathF.Max(v[1], v[3]));
    }

    /// <summary>Finds a box array on the page or one of its ancestors.</summary>
    private static List<float>? FindBox(PdfDocument doc, PdfObjectId pageId, string key)
    {
        var id = pageId;

        // A malformed page tree can cycle, so the walk is bounded.
        for (var depth = 0; depth < 32; depth++)
        {
            if (doc.GetObject(id)?.AsDictionary() is not { } dict)
            {
                return null;
            }

            if (dict.TryGetValue(key, out var entry) && doc.Resolve(entry)?.AsArray() is { } arr)
            {
                var vals = arr
                    .Select(o => doc.Resolve(o)?.AsFloat())
                    .Where(v => v is not null)
                    .Select(v => v!.Value)
                    .ToList();
                if (vals.Count >= 4)
                {
                    return vals;
                }
            }

            if (dict.TryGetValue("Parent", out var parent) && parent.AsReference() is { } parentRef)
            {
                id = parentRef;
            }
            else
            {
                return null;
            }
        }

        return null;
    }
}
