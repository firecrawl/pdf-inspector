// Ported from reference/src/extractor/content_stream.rs
using PdfInspector.Pdf;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>A page's extracted text, rectangles, and line segments.</summary>
public sealed class PageExtraction
{
    public List<TextItem> Items { get; init; } = [];

    public List<PdfRect> Rects { get; init; } = [];

    public List<PdfLine> Lines { get; init; } = [];

    /// <summary>True when the page uses fonts with unresolvable glyph-id names.</summary>
    public bool HasGidFonts { get; init; }

    /// <summary>True when the page's coordinates were rotated to undo rotated text.</summary>
    public bool CoordsRotated { get; init; }
}

/// <summary>
/// Walks a page's content stream, tracking the graphics and text state, and
/// emits text items along with the rectangles and lines the table detectors and
/// underline pass consume.
/// </summary>
internal static class ContentStreamExtractor
{
    private const string Module = "extractor";

    /// <summary>Pages beyond this many operators are skipped rather than parsed.</summary>
    private const int MaxOperations = 1_000_000;

    /// <summary>Graphics state saved by <c>q</c> and restored by <c>Q</c>.</summary>
    private struct SavedGraphicsState
    {
        public Matrix Ctm;
        public int TextRenderingMode;
        public float LineWidth;
        public float CharSpacing;
        public float WordSpacing;
        public float TextRise;
        public float TextLeading;
        public string CurrentFont;
        public float CurrentFontSize;
    }

    private sealed class MarkedContentEntry
    {
        public string? ActualText;
        public long? Mcid;
    }

    /// <summary>Counts of show-text operators whose combined matrix reads horizontal versus rotated.</summary>
    private struct RotationVotes
    {
        public uint Horizontal;
        public uint Rotated;
    }

    public static PageExtraction ExtractPageTextItems(
        PdfDocument doc,
        PdfDictionary page,
        uint pageNum,
        FontCMaps fontCMaps,
        bool includeInvisible,
        FontStyleCache styleCache)
    {
        var items = new List<TextItem>();
        var rects = new List<PdfRect>();
        var clipRects = new List<PdfRect>();
        var lines = new List<PdfLine>();
        var underlineLines = new List<UnderlineLine>();

        // Path construction state, feeding line and rect extraction.
        (float X, float Y)? pathSubpathStart = null;
        (float X, float Y)? pathCurrent = null;
        var pendingLines = new List<(float X1, float Y1, float X2, float Y2)>();
        var pendingSubpaths = new List<List<(float X1, float Y1, float X2, float Y2)>>();
        var fillRects = new List<PdfRect>();

        // Rects awaiting a paint operator. Underline detection must see only
        // painted rects: a clip path or discarded path draws nothing, so
        // treating every `re` as ink would underline text merely sitting near an
        // invisible clip boundary.
        var pendingReRects = new List<PdfRect>();
        var paintedRects = new List<PdfRect>();

        var fonts = doc.GetPageFonts(page);
        var fontSetup = PageFontContextBuilder.Build(doc, fonts, fontCMaps, styleCache);
        var context = fontSetup.Context;

        var xobjects = XObjects.GetPageXObjects(doc, page);

        var contentData = doc.GetPageContent(page);

        // Some producers embed comments in content streams that confuse the
        // operator parser into skipping operators such as ET and Q.
        contentData = StripPdfComments(contentData);

        var operations = ContentStream.Decode(contentData);

        if (operations.Count > MaxOperations)
        {
            Log.Warn(Module,
                $"page {pageNum}: skipping extraction — {operations.Count} operations exceeds limit ({MaxOperations})");
            return new PageExtraction();
        }

        var ctm = Matrix.Identity;
        var textRenderingMode = 0;
        var lineWidth = 1.0f;
        var gstateStack = new Stack<SavedGraphicsState>();

        var currentFont = string.Empty;
        var currentFontSize = 12.0f;
        var textLeading = 0.0f;
        var charSpacing = 0.0f;
        var wordSpacing = 0.0f;
        var textRise = 0.0f;
        var textMatrix = Matrix.Identity;
        var lineMatrix = Matrix.Identity;
        var inTextBlock = false;

        var rotationVotes = new RotationVotes();

        var markedContentStack = new List<MarkedContentEntry>();
        var suppressGlyphExtraction = false;
        Matrix? actualTextStartTm = null;
        Matrix? actualTextGlyphTm = null;

        // The item must render at the rise of its glyphs, not whatever rise is
        // in effect by the time the marked-content section ends.
        var actualTextStartRise = 0.0f;
        float? actualTextGlyphRise = null;

        long? CurrentMcid()
        {
            for (var i = markedContentStack.Count - 1; i >= 0; i--)
            {
                if (markedContentStack[i].Mcid is { } mcid)
                {
                    return mcid;
                }
            }

            return null;
        }

        string? DecodeOperand(PdfObject operand) => TextDecoder.ExtractTextFromOperand(
            operand, currentFont, fontSetup.BaseNames.GetValueOrDefault(currentFont), context);

        void AddTextItem(string text, float x, float y, float width, float renderedSize, long? mcid)
        {
            var baseFont = fontSetup.BaseNames.GetValueOrDefault(currentFont, currentFont);
            var (descItalic, descBold) = fontSetup.StyleFlags.GetValueOrDefault(currentFont, (false, false));

            items.Add(new TextItem
            {
                Text = TextUtils.ExpandLigatures(text),
                X = x,
                Y = y,
                Width = width,
                Height = renderedSize,
                Font = currentFont,
                FontSize = renderedSize,
                Page = pageNum,
                IsBold = TextUtils.IsBoldFont(baseFont) || descBold,
                IsItalic = TextUtils.IsItalicFont(baseFont) || descItalic,
                Kind = ItemKind.Text,
                Mcid = mcid,
            });
        }

        void VoteRotation(in Matrix combined)
        {
            if (MathF.Abs(combined.A) >= MathF.Abs(combined.B))
            {
                rotationVotes.Horizontal++;
            }
            else
            {
                rotationVotes.Rotated++;
            }
        }

        void EmitPendingStrokes()
        {
            foreach (var (x1, y1, x2, y2) in pendingLines)
            {
                var (x1d, y1d) = ctm.Apply(x1, y1);
                var (x2d, y2d) = ctm.Apply(x2, y2);

                lines.Add(new PdfLine(x1d, y1d, x2d, y2d, pageNum));
                underlineLines.Add(new UnderlineLine
                {
                    X1 = x1d,
                    Y1 = y1d,
                    X2 = x2d,
                    Y2 = y2d,
                    StrokeWidth = Geometry.TransformedStrokeWidth(lineWidth, ctm, x1, y1, x2, y2),
                    Page = pageNum,
                });
            }

            pendingLines.Clear();
        }

        void CloseCurrentSubpath()
        {
            if (pathCurrent is not { } current || pathSubpathStart is not { } start)
            {
                return;
            }

            if (MathF.Abs(current.X - start.X) > 0.01f || MathF.Abs(current.Y - start.Y) > 0.01f)
            {
                pendingLines.Add((current.X, current.Y, start.X, start.Y));
            }
        }

        foreach (var op in operations)
        {
            var operands = op.Operands;

            switch (op.Operator)
            {
                case "q":
                    gstateStack.Push(new SavedGraphicsState
                    {
                        Ctm = ctm,
                        TextRenderingMode = textRenderingMode,
                        LineWidth = lineWidth,
                        CharSpacing = charSpacing,
                        WordSpacing = wordSpacing,
                        TextRise = textRise,
                        TextLeading = textLeading,
                        CurrentFont = currentFont,
                        CurrentFontSize = currentFontSize,
                    });
                    break;

                case "Q":
                    if (gstateStack.Count > 0)
                    {
                        var saved = gstateStack.Pop();
                        ctm = saved.Ctm;
                        textRenderingMode = saved.TextRenderingMode;
                        lineWidth = saved.LineWidth;
                        charSpacing = saved.CharSpacing;
                        wordSpacing = saved.WordSpacing;
                        textRise = saved.TextRise;
                        textLeading = saved.TextLeading;
                        currentFont = saved.CurrentFont;
                        currentFontSize = saved.CurrentFontSize;
                    }

                    break;

                case "cm":
                    if (operands.Count >= 6)
                    {
                        ctm = ReadCmOperands(operands).Multiply(ctm);
                    }

                    break;

                case "w":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } width)
                    {
                        lineWidth = width;
                    }

                    break;

                case "BT":
                    inTextBlock = true;
                    textMatrix = Matrix.Identity;
                    lineMatrix = Matrix.Identity;
                    textRenderingMode = 0;
                    break;

                case "ET":
                    inTextBlock = false;
                    break;

                case "Tf":
                    if (operands.Count >= 2)
                    {
                        if (operands[0].AsName() is { } fontName)
                        {
                            currentFont = fontName;
                        }

                        if (Geometry.GetNumber(operands[1]) is { } size)
                        {
                            currentFontSize = size;
                        }
                    }

                    break;

                case "TL":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } leading)
                    {
                        textLeading = leading;
                    }

                    break;

                case "Tr":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } mode)
                    {
                        textRenderingMode = (int)mode;
                    }

                    break;

                case "Tc":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } tc)
                    {
                        charSpacing = tc;
                    }

                    break;

                case "Tw":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } tw)
                    {
                        wordSpacing = tw;
                    }

                    break;

                case "Ts":
                    if (operands.Count >= 1 && Geometry.GetNumber(operands[0]) is { } ts)
                    {
                        textRise = ts;
                    }

                    break;

                case "Td":
                case "TD":
                    if (operands.Count >= 2)
                    {
                        var tx = Geometry.GetNumber(operands[0]) ?? 0.0f;
                        var ty = Geometry.GetNumber(operands[1]) ?? 0.0f;
                        lineMatrix = lineMatrix.TranslatedBy(tx, ty);
                        textMatrix = lineMatrix;

                        if (op.Operator == "TD")
                        {
                            textLeading = -ty;
                        }
                    }

                    break;

                case "Tm":
                    if (operands.Count >= 6)
                    {
                        textMatrix = XObjects.ReadMatrixOperands(operands, defaultDiagonal: true);
                        lineMatrix = textMatrix;
                    }

                    break;

                case "T*":
                {
                    var tl = textLeading != 0.0f ? textLeading : currentFontSize * 1.2f;
                    lineMatrix = lineMatrix.TranslatedBy(0f, -tl);
                    textMatrix = lineMatrix;
                    break;
                }

                case "Tj":
                {
                    if (!inTextBlock || operands.Count == 0)
                    {
                        break;
                    }

                    context.Widths.TryGetValue(currentFont, out var fontInfo);

                    float? advanceTs = null;
                    if (fontInfo is not null && operands[0] is PdfString str)
                    {
                        advanceTs = FontWidths.ComputeStringWidthTs(
                            str.Bytes, fontInfo, currentFontSize, charSpacing, wordSpacing);
                    }

                    // ActualText replaces the glyphs, so extraction is suppressed
                    // but the matrix still advances. The first glyph's matrix is
                    // captured as the rendering position: Td operators between the
                    // section start and the first show operator may have moved to
                    // the correct line, while the entry position can be on the
                    // previous one.
                    if (suppressGlyphExtraction)
                    {
                        actualTextGlyphTm ??= textMatrix;
                        actualTextGlyphRise ??= textRise;

                        if (advanceTs is { } suppressedAdvance)
                        {
                            textMatrix = textMatrix.TranslatedBy(suppressedAdvance, 0f);
                        }

                        break;
                    }

                    // Invisible text is skipped but still advances. Template PDFs
                    // opt in to it so the OCR layer behind a scanned image is read.
                    if (textRenderingMode == 3 && !includeInvisible)
                    {
                        if (advanceTs is { } invisibleAdvance)
                        {
                            textMatrix = textMatrix.TranslatedBy(invisibleAdvance, 0f);
                        }

                        break;
                    }

                    if (DecodeOperand(operands[0]) is not { } text)
                    {
                        break;
                    }

                    var combined = Geometry.RiseAdjusted(textMatrix, textRise).Multiply(ctm);
                    var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());
                    var (x, y) = (combined.E, combined.F);
                    VoteRotation(combined);

                    var itemWidth = 0.0f;
                    if (advanceTs is { } advance)
                    {
                        var scaleX = (textMatrix.A * ctm.A) + (textMatrix.B * ctm.C);
                        textMatrix = textMatrix.TranslatedBy(advance, 0f);
                        itemWidth = MathF.Abs(advance * scaleX);
                    }

                    // Whitespace still advanced the matrix above, so gap detection
                    // works, but produces no item of its own.
                    if (text.Trim().Length > 0)
                    {
                        AddTextItem(text, x, y, itemWidth, renderedSize, CurrentMcid());
                    }

                    break;
                }

                case "TJ":
                {
                    if (!inTextBlock || operands.Count == 0 || operands[0].AsArray() is not { } array)
                    {
                        break;
                    }

                    context.Widths.TryGetValue(currentFont, out var fontInfo);
                    var isInvisible = (textRenderingMode == 3 && !includeInvisible) || suppressGlyphExtraction;

                    if (suppressGlyphExtraction)
                    {
                        actualTextGlyphTm ??= textMatrix;
                        actualTextGlyphRise ??= textRise;
                    }

                    var segmented = ShowTextArray.Segment(
                        array, fontInfo, currentFontSize, charSpacing, wordSpacing, isInvisible, DecodeOperand);

                    if (segmented.Runs.Count > 0)
                    {
                        var combined = textMatrix.Multiply(ctm);
                        VoteRotation(combined);

                        var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());
                        var scaleX = (textMatrix.A * ctm.A) + (textMatrix.B * ctm.C);
                        var mcid = CurrentMcid();

                        foreach (var run in segmented.Runs)
                        {
                            var offsetTm = textMatrix.TranslatedBy(run.StartWidthTs, 0f);
                            var runCombined = Geometry.RiseAdjusted(offsetTm, textRise).Multiply(ctm);
                            var runWidth = fontInfo is not null
                                ? MathF.Abs((run.EndWidthTs - run.StartWidthTs) * scaleX)
                                : 0.0f;

                            AddTextItem(run.Text, runCombined.E, runCombined.F, runWidth, renderedSize, mcid);
                        }
                    }

                    if (fontInfo is not null)
                    {
                        textMatrix = textMatrix.TranslatedBy(segmented.TotalWidthTs, 0f);
                    }

                    break;
                }

                case "'":
                {
                    var tl = textLeading != 0.0f ? textLeading : currentFontSize * 1.2f;
                    lineMatrix = lineMatrix.TranslatedBy(0f, -tl);
                    textMatrix = lineMatrix;

                    // Captured after the line move: the section-entry matrix is on
                    // the previous line.
                    if (suppressGlyphExtraction)
                    {
                        actualTextGlyphTm ??= textMatrix;
                        actualTextGlyphRise ??= textRise;
                    }

                    context.Widths.TryGetValue(currentFont, out var fontInfo);

                    float? advanceTs = null;
                    if (fontInfo is not null && operands.Count > 0 && operands[0] is PdfString str)
                    {
                        advanceTs = FontWidths.ComputeStringWidthTs(
                            str.Bytes, fontInfo, currentFontSize, charSpacing, wordSpacing);
                    }

                    var skip = (textRenderingMode == 3 && !includeInvisible)
                        || suppressGlyphExtraction
                        || operands.Count == 0;

                    if (!skip && DecodeOperand(operands[0]) is { } text && text.Trim().Length > 0)
                    {
                        var combined = Geometry.RiseAdjusted(textMatrix, textRise).Multiply(ctm);
                        VoteRotation(combined);

                        var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());
                        var scaleX = (textMatrix.A * ctm.A) + (textMatrix.B * ctm.C);
                        var itemWidth = advanceTs is { } a ? MathF.Abs(a * scaleX) : 0.0f;

                        AddTextItem(text, combined.E, combined.F, itemWidth, renderedSize, CurrentMcid());
                    }

                    // Advances regardless of visibility so later show-text
                    // operators on the same line stay positioned.
                    if (advanceTs is { } advance)
                    {
                        textMatrix = textMatrix.TranslatedBy(advance, 0f);
                    }

                    break;
                }

                case "Do":
                {
                    if (operands.Count == 0 || operands[0].AsName() is not { } xobjName)
                    {
                        break;
                    }

                    switch (xobjects.GetValueOrDefault(xobjName))
                    {
                        case XObjectKind.Image:
                        {
                            // A positional placeholder lets layout-aware consumers
                            // locate raster figures without reparsing the PDF.
                            var (ix, iy, iw, ih) = Geometry.ImageBoundsFromCtm(ctm);
                            items.Add(new TextItem
                            {
                                Text = $"[Image: {xobjName}]",
                                X = ix,
                                Y = iy,
                                Width = iw,
                                Height = ih,
                                Page = pageNum,
                                Kind = ItemKind.Image,
                                Mcid = CurrentMcid(),
                            });
                            break;
                        }

                        case XObjectKind.Form form:
                            items.AddRange(XObjects.ExtractFormXObjectText(
                                doc, form.Id, pageNum, fontCMaps, ctm, context.CMapDecisions, styleCache));
                            break;
                    }

                    break;
                }

                case "BMC":
                    markedContentStack.Add(new MarkedContentEntry());
                    break;

                case "BDC":
                {
                    string? actualText = null;
                    long? mcid = null;

                    if (operands.Count >= 2)
                    {
                        var dict = operands[1] as PdfDictionary ?? doc.Resolve(operands[1]).AsDictionary();
                        if (dict is not null)
                        {
                            if (dict.Get("ActualText") is PdfString actual)
                            {
                                actualText = TextUtils.DecodeTextString(actual.Bytes);
                            }

                            if (dict.Get("MCID")?.AsInteger() is { } id)
                            {
                                mcid = id;
                            }
                        }
                    }

                    if (actualText is not null)
                    {
                        suppressGlyphExtraction = true;
                        actualTextStartTm = textMatrix;
                        actualTextStartRise = textRise;
                        actualTextGlyphTm = null;
                        actualTextGlyphRise = null;
                    }

                    markedContentStack.Add(new MarkedContentEntry { ActualText = actualText, Mcid = mcid });
                    break;
                }

                case "EMC":
                {
                    if (markedContentStack.Count == 0)
                    {
                        break;
                    }

                    var entry = markedContentStack[^1];
                    markedContentStack.RemoveAt(markedContentStack.Count - 1);

                    if (entry.ActualText is not { } replacement)
                    {
                        break;
                    }

                    var glyphTm = actualTextGlyphTm;
                    var glyphRise = actualTextGlyphRise;
                    var entryTm = actualTextStartTm;
                    actualTextGlyphTm = null;
                    actualTextGlyphRise = null;
                    actualTextStartTm = null;

                    if ((glyphTm ?? entryTm) is { } startTm)
                    {
                        var rise = glyphRise ?? actualTextStartRise;
                        var combined = Geometry.RiseAdjusted(startTm, rise).Multiply(ctm);
                        VoteRotation(combined);

                        var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());

                        // The width comes from how far the text matrix advanced
                        // across the suppressed glyphs.
                        var deltaTs = textMatrix.E - startTm.E;
                        var scaleX = (startTm.A * ctm.A) + (startTm.B * ctm.C);
                        var itemWidth = MathF.Abs(deltaTs * scaleX);

                        if (replacement.Trim().Length > 0)
                        {
                            AddTextItem(
                                replacement, combined.E, combined.F, itemWidth, renderedSize,
                                entry.Mcid ?? CurrentMcid());
                        }
                    }

                    suppressGlyphExtraction = markedContentStack.Any(e => e.ActualText is not null);
                    break;
                }

                case "re":
                {
                    if (operands.Count < 4)
                    {
                        break;
                    }

                    var rx = Geometry.GetNumber(operands[0]) ?? 0.0f;
                    var ry = Geometry.GetNumber(operands[1]) ?? 0.0f;
                    var rw = Geometry.GetNumber(operands[2]) ?? 0.0f;
                    var rh = Geometry.GetNumber(operands[3]) ?? 0.0f;

                    var (xDev, yDev) = ctm.Apply(rx, ry);
                    var rect = new PdfRect(xDev, yDev, rw * ctm.A, rh * ctm.D, pageNum);

                    // Held pending until a paint operator confirms the rect draws ink.
                    pendingReRects.Add(rect);
                    rects.Add(rect);
                    break;
                }

                // ── Path construction ────────────────────────────────────

                case "m":
                    if (operands.Count >= 2)
                    {
                        var px = Geometry.GetNumber(operands[0]) ?? 0.0f;
                        var py = Geometry.GetNumber(operands[1]) ?? 0.0f;
                        pathSubpathStart = (px, py);
                        pathCurrent = (px, py);
                    }

                    break;

                case "l":
                    if (operands.Count >= 2 && pathCurrent is { } from)
                    {
                        var px = Geometry.GetNumber(operands[0]) ?? 0.0f;
                        var py = Geometry.GetNumber(operands[1]) ?? 0.0f;
                        pendingLines.Add((from.X, from.Y, px, py));
                        pathCurrent = (px, py);
                    }

                    break;

                case "h":
                    CloseCurrentSubpath();
                    pathCurrent = pathSubpathStart;

                    // The completed subpath is saved for fill-rect extraction;
                    // the clip handler reads the last entry instead.
                    if (pendingLines.Count > 0)
                    {
                        pendingSubpaths.Add([.. pendingLines]);
                        pendingLines.Clear();
                    }

                    break;

                // ── Path painting ────────────────────────────────────────

                case "S":
                case "s":
                    if (op.Operator == "s")
                    {
                        CloseCurrentSubpath();
                    }

                    EmitPendingStrokes();
                    paintedRects.AddRange(pendingReRects);
                    pendingReRects.Clear();
                    pendingSubpaths.Clear();
                    pathSubpathStart = null;
                    pathCurrent = null;
                    break;

                case "B":
                case "B*":
                case "b":
                case "b*":
                    if (op.Operator is "b" or "b*")
                    {
                        CloseCurrentSubpath();
                    }

                    EmitPendingStrokes();
                    paintedRects.AddRange(pendingReRects);
                    pendingReRects.Clear();
                    pendingSubpaths.Clear();
                    pathSubpathStart = null;
                    pathCurrent = null;
                    break;

                case "f":
                case "F":
                case "f*":
                {
                    // Any unclosed segments still count toward a fill.
                    if (pendingLines.Count > 0)
                    {
                        pendingSubpaths.Add([.. pendingLines]);
                        pendingLines.Clear();
                    }

                    foreach (var subpath in pendingSubpaths)
                    {
                        if (RectangleFromSubpath(subpath, pathSubpathStart, ctm, pageNum) is { } filled)
                        {
                            fillRects.Add(filled);
                        }
                    }

                    pendingSubpaths.Clear();
                    paintedRects.AddRange(pendingReRects);
                    pendingReRects.Clear();
                    pendingLines.Clear();
                    pathSubpathStart = null;
                    pathCurrent = null;
                    break;
                }

                case "W":
                case "W*":
                {
                    // Many PDFs define table cells as clipping paths rather than
                    // stroked rects. After `h` the subpath moved to the saved
                    // list, so read from there when nothing is pending.
                    var segs = pendingLines.Count > 0
                        ? [.. pendingLines]
                        : pendingSubpaths.Count > 0 ? new List<(float, float, float, float)>(pendingSubpaths[^1]) : [];

                    if (RectangleFromSubpath(segs, pathSubpathStart, ctm, pageNum) is { } clip)
                    {
                        clipRects.Add(clip);
                    }

                    // pendingLines is deliberately left alone; the following `n` clears it.
                    break;
                }

                case "n":
                    // End the path with no painting, discarding any `re` rects
                    // that were only ever part of a clip path.
                    pendingReRects.Clear();
                    pendingLines.Clear();
                    pendingSubpaths.Clear();
                    pathSubpathStart = null;
                    pathCurrent = null;
                    break;
            }
        }

        // Underline detection reads only painted ink: confirmed `re` rects plus
        // filled subpaths, never clip-only rects.
        var underlineRects = new List<PdfRect>(paintedRects);
        underlineRects.AddRange(fillRects);

        rects = ChooseRects(rects, clipRects, fillRects);

        var (correctedItems, correctedRects, correctedLines, coordsRotated) =
            CorrectRotatedPage(items, rects, lines, rotationVotes);

        if (coordsRotated)
        {
            RotateUnderlineGraphics(underlineRects, underlineLines);
        }

        Underline.MarkUnderlinedItems(correctedItems, underlineRects, underlineLines, pageNum);

        var mergedItems = ItemMerging.MergeSubscriptItems(ItemMerging.MergeTextItems(correctedItems));

        return new PageExtraction
        {
            Items = mergedItems,
            Rects = correctedRects,
            Lines = correctedLines,
            HasGidFonts = fontSetup.HasGidFonts,
            CoordsRotated = coordsRotated,
        };
    }

    /// <summary>
    /// Picks which rectangle source feeds table detection. Explicit <c>re</c>
    /// rects win outright; otherwise clip and fill rects compete.
    /// </summary>
    private static List<PdfRect> ChooseRects(List<PdfRect> rects, List<PdfRect> clipRects, List<PdfRect> fillRects)
    {
        if (rects.Count > 0)
        {
            return rects;
        }

        // Some PDFs wrap every text block in a full-page clip path, producing
        // thousands of identical rects that would yield a degenerate grid.
        DedupRects(clipRects);

        // When fills substantially outnumber clips, the clips are section-level
        // wrappers and the fills are the real cell backgrounds.
        var preferFills = fillRects.Count > 0 && fillRects.Count >= clipRects.Count * 3;

        if (preferFills)
        {
            return fillRects;
        }

        if (clipRects.Count >= 4)
        {
            return clipRects;
        }

        if (fillRects.Count > 0)
        {
            return fillRects;
        }

        return clipRects.Count > 0 ? clipRects : rects;
    }

    /// <summary>
    /// Interprets a subpath as an axis-aligned rectangle in device space, or
    /// returns null when it is not one.
    /// </summary>
    private static PdfRect? RectangleFromSubpath(
        List<(float X1, float Y1, float X2, float Y2)> subpath,
        (float X, float Y)? subpathStart,
        in Matrix ctm,
        uint pageNum)
    {
        var segs = new List<(float X1, float Y1, float X2, float Y2)>(subpath);

        // Three segments plus an implied close still describe a rectangle.
        if (segs.Count == 3)
        {
            var (ex, ey) = (segs[2].X2, segs[2].Y2);
            var (sx, sy) = subpathStart ?? (segs[0].X1, segs[0].Y1);

            if (MathF.Abs(ex - sx) > 0.01f || MathF.Abs(ey - sy) > 0.01f)
            {
                segs.Add((ex, ey, sx, sy));
            }
        }

        if (segs.Count != 4)
        {
            return null;
        }

        var xs = new List<float>(8);
        var ys = new List<float>(8);
        foreach (var (x1, y1, x2, y2) in segs)
        {
            xs.Add(x1);
            xs.Add(x2);
            ys.Add(y1);
            ys.Add(y2);
        }

        var minX = xs.Min();
        var maxX = xs.Max();
        var minY = ys.Min();
        var maxY = ys.Max();
        var w = maxX - minX;
        var h = maxY - minY;

        const float Eps = 0.5f;
        var axisAligned =
            xs.All(x => MathF.Abs(x - minX) < Eps || MathF.Abs(x - maxX) < Eps) &&
            ys.All(y => MathF.Abs(y - minY) < Eps || MathF.Abs(y - maxY) < Eps);

        if (!axisAligned || w <= 1.0f || h <= 1.0f)
        {
            return null;
        }

        var (xDev, yDev) = ctm.Apply(minX, minY);
        return new PdfRect(xDev, yDev, w * ctm.A, h * ctm.D, pageNum);
    }

    /// <summary>
    /// Detects a page whose text is rotated 90 degrees and swaps coordinates so
    /// the layout engine sees horizontal text on a landscape page. Some
    /// producers embed landscape content in a portrait page using a rotated text
    /// matrix.
    /// </summary>
    private static (List<TextItem>, List<PdfRect>, List<PdfLine>, bool) CorrectRotatedPage(
        List<TextItem> items,
        List<PdfRect> rects,
        List<PdfLine> lines,
        RotationVotes votes)
    {
        if (items.Count < 2)
        {
            return (items, rects, lines, false);
        }

        var totalVotes = votes.Horizontal + votes.Rotated;

        // Fewer than about two thirds rotated means this is not a rotated page.
        if (totalVotes == 0 || votes.Rotated * 3 < totalVotes * 2)
        {
            return (items, rects, lines, false);
        }

        Log.Debug(Module, () =>
            $"detected rotated page text: {votes.Rotated}/{totalVotes} text ops are rotated — swapping coordinates");

        // For the common 90-degree counter-clockwise case, increasing device x is
        // visually downward and increasing device y is visually rightward. The
        // layout engine sorts by y descending, so x is negated to put the visual
        // top at the highest new y.
        foreach (var item in items)
        {
            var newX = item.Y;
            var newY = -item.X;
            item.X = newX;
            item.Y = newY;

            // Width along the reading direction was lost to a near-zero scale
            // factor; estimate it from the text length. The font size here is the
            // rendered height in device space, which for a quarter turn is the
            // horizontal extent of one em.
            if (item.Width < 0.5f)
            {
                item.Width = TextUtils.CharCount(item.Text) * item.FontSize * 0.5f;
            }
        }

        foreach (var rect in rects)
        {
            var newX = rect.Y;
            var newY = -(rect.X + MathF.Abs(rect.Width));
            rect.X = newX;
            rect.Y = newY;
            (rect.Width, rect.Height) = (rect.Height, rect.Width);
        }

        foreach (var line in lines)
        {
            var newX1 = line.Y1;
            var newY1 = -line.X1;
            var newX2 = line.Y2;
            var newY2 = -line.X2;
            line.X1 = newX1;
            line.Y1 = newY1;
            line.X2 = newX2;
            line.Y2 = newY2;
        }

        return (items, rects, lines, true);
    }

    private static void RotateUnderlineGraphics(List<PdfRect> rects, List<UnderlineLine> lines)
    {
        foreach (var rect in rects)
        {
            var newX = rect.Y;
            var newY = -(rect.X + MathF.Abs(rect.Width));
            rect.X = newX;
            rect.Y = newY;
            (rect.Width, rect.Height) = (rect.Height, rect.Width);
        }

        foreach (var line in lines)
        {
            var newX1 = line.Y1;
            var newY1 = -line.X1;
            var newX2 = line.Y2;
            var newY2 = -line.X2;
            line.X1 = newX1;
            line.Y1 = newY1;
            line.X2 = newX2;
            line.Y2 = newY2;
        }
    }

    /// <summary>
    /// Removes near-duplicate rects within half a point. Some PDFs emit a
    /// full-page clip path for every text block; after dedup those collapse to
    /// one rect, too few for table detection to act on.
    /// </summary>
    private static void DedupRects(List<PdfRect> rects)
    {
        if (rects.Count <= 1)
        {
            return;
        }

        // Sorted on a half-point grid so near-duplicates become adjacent.
        rects.Sort((a, b) =>
        {
            var byPage = a.Page.CompareTo(b.Page);
            if (byPage != 0)
            {
                return byPage;
            }

            var byX = ((int)(a.X * 2.0f)).CompareTo((int)(b.X * 2.0f));
            if (byX != 0)
            {
                return byX;
            }

            var byY = ((int)(a.Y * 2.0f)).CompareTo((int)(b.Y * 2.0f));
            if (byY != 0)
            {
                return byY;
            }

            var byWidth = ((int)(a.Width * 2.0f)).CompareTo((int)(b.Width * 2.0f));
            return byWidth != 0
                ? byWidth
                : ((int)(a.Height * 2.0f)).CompareTo((int)(b.Height * 2.0f));
        });

        var write = 1;
        for (var read = 1; read < rects.Count; read++)
        {
            var previous = rects[write - 1];
            var current = rects[read];

            var duplicate = current.Page == previous.Page
                && MathF.Abs(current.X - previous.X) < 0.5f
                && MathF.Abs(current.Y - previous.Y) < 0.5f
                && MathF.Abs(current.Width - previous.Width) < 0.5f
                && MathF.Abs(current.Height - previous.Height) < 0.5f;

            if (!duplicate)
            {
                rects[write++] = current;
            }
        }

        rects.RemoveRange(write, rects.Count - write);
    }

    /// <summary>Reads the six operands of a <c>cm</c>, defaulting every missing value to zero.</summary>
    private static Matrix ReadCmOperands(List<PdfObject> operands) => new(
        Geometry.GetNumber(operands[0]) ?? 1.0f,
        Geometry.GetNumber(operands[1]) ?? 0.0f,
        Geometry.GetNumber(operands[2]) ?? 0.0f,
        Geometry.GetNumber(operands[3]) ?? 1.0f,
        Geometry.GetNumber(operands[4]) ?? 0.0f,
        Geometry.GetNumber(operands[5]) ?? 0.0f);

    /// <summary>
    /// Strips <c>%</c> comments from content-stream bytes. Comments inside
    /// string literals are left alone; only top-level ones are removed, and each
    /// is replaced by a space so token separation survives.
    /// </summary>
    internal static byte[] StripPdfComments(byte[] data)
    {
        if (Array.IndexOf(data, (byte)'%') < 0)
        {
            return data;
        }

        var result = new List<byte>(data.Length);
        var i = 0;
        var stringDepth = 0;
        var inHexString = false;

        while (i < data.Length)
        {
            var b = data[i];

            switch (b)
            {
                case (byte)'(' when !inHexString:
                    stringDepth++;
                    result.Add(b);
                    break;

                case (byte)')' when !inHexString && stringDepth > 0:
                    stringDepth--;
                    result.Add(b);
                    break;

                case (byte)'<' when stringDepth == 0 && !inHexString:
                    inHexString = true;
                    result.Add(b);
                    break;

                case (byte)'>' when inHexString:
                    inHexString = false;
                    result.Add(b);
                    break;

                case (byte)'%' when stringDepth == 0 && !inHexString:
                    while (i < data.Length && data[i] != (byte)'\n' && data[i] != (byte)'\r')
                    {
                        i++;
                    }

                    result.Add((byte)' ');
                    continue;

                default:
                    result.Add(b);
                    break;
            }

            i++;
        }

        return [.. result];
    }
}
