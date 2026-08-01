// Ported from reference/src/extractor/xobjects.rs
using PdfInspector.Pdf;
using PdfInspector.Text;
using PdfInspector.ToUnicode;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>What an XObject resource refers to.</summary>
internal abstract record XObjectKind
{
    public sealed record Image : XObjectKind;

    public sealed record Form(PdfObjectId Id) : XObjectKind;
}

/// <summary>Form and image XObject handling.</summary>
internal static class XObjects
{
    private const int MaxFormXObjectDepth = 5;

    /// <summary>Collects a page's XObjects, keyed by resource name.</summary>
    public static Dictionary<string, XObjectKind> GetPageXObjects(PdfDocument doc, PdfDictionary page)
    {
        var result = new Dictionary<string, XObjectKind>(StringComparer.Ordinal);

        foreach (var resources in doc.GetPageResources(page))
        {
            CollectFromResources(doc, resources, result);
        }

        return result;
    }

    /// <summary>Collects the XObjects a Form XObject's own resources declare.</summary>
    private static Dictionary<string, XObjectKind> GetFormXObjects(PdfDocument doc, PdfDictionary formDict)
    {
        var result = new Dictionary<string, XObjectKind>(StringComparer.Ordinal);

        if (doc.GetDeref(formDict, "Resources")?.AsDictionary() is { } resources)
        {
            CollectFromResources(doc, resources, result);
        }

        return result;
    }

    private static void CollectFromResources(
        PdfDocument doc,
        PdfDictionary resources,
        Dictionary<string, XObjectKind> output)
    {
        var xobjects = doc.GetDeref(resources, "XObject")?.AsDictionary();
        if (xobjects is null)
        {
            return;
        }

        foreach (var (name, value) in xobjects)
        {
            if (output.ContainsKey(name) || value.AsReference() is not { } objRef)
            {
                continue;
            }

            if (doc.GetObject(objRef)?.AsStream() is not { } stream)
            {
                continue;
            }

            var subtype = stream.Dictionary.Get("Subtype")?.AsName();
            if (subtype == "Image")
            {
                output[name] = new XObjectKind.Image();
            }
            else if (subtype == "Form")
            {
                output[name] = new XObjectKind.Form(objRef);
            }
        }
    }

    /// <summary>Extracts the text a Form XObject draws, in the parent's coordinate space.</summary>
    public static List<TextItem> ExtractFormXObjectText(
        PdfDocument doc,
        PdfObjectId formId,
        uint pageNum,
        FontCMaps fontCMaps,
        in Matrix parentCtm,
        CMapDecisionCache cmapDecisions,
        FontStyleCache styleCache) =>
        Extract(doc, formId, pageNum, fontCMaps, parentCtm, cmapDecisions, styleCache, 0);

    private static List<TextItem> Extract(
        PdfDocument doc,
        PdfObjectId formId,
        uint pageNum,
        FontCMaps fontCMaps,
        in Matrix parentCtm,
        CMapDecisionCache cmapDecisions,
        FontStyleCache styleCache,
        int depth)
    {
        var items = new List<TextItem>();

        if (doc.GetObject(formId)?.AsStream() is not { } stream)
        {
            return items;
        }

        var contentData = stream.DecompressedContent() ?? stream.RawData;
        var operations = ContentStream.Decode(contentData);

        var formFonts = GetFormFonts(doc, stream.Dictionary);
        // The page's decision cache is shared so a font's CMap verdict stays
        // consistent between the page content and the forms it draws.
        var fontSetup = PageFontContextBuilder.Build(doc, formFonts, fontCMaps, styleCache, cmapDecisions);
        var context = fontSetup.Context;

        var formXObjects = GetFormXObjects(doc, stream.Dictionary);

        // A Form XObject's own /Matrix composes with the parent transform.
        var formMatrix = ReadMatrix(doc.GetArray(stream.Dictionary, "Matrix"));
        var baseCtm = formMatrix.Multiply(parentCtm);

        var currentFont = string.Empty;
        var currentFontSize = 12.0f;
        var textMatrix = Matrix.Identity;
        var inTextBlock = false;
        var fillIsWhite = false;
        var ctm = baseCtm;
        var ctmStack = new Stack<Matrix>();

        string? DecodeOperand(PdfObject operand) => TextDecoder.ExtractTextFromOperand(
            operand, currentFont, fontSetup.BaseNames.GetValueOrDefault(currentFont), context);

        foreach (var op in operations)
        {
            var operands = op.Operands;

            switch (op.Operator)
            {
                case "q":
                    ctmStack.Push(ctm);
                    break;

                case "Q":
                    if (ctmStack.Count > 0)
                    {
                        ctm = ctmStack.Pop();
                    }

                    break;

                case "cm":
                    if (operands.Length >= 6)
                    {
                        ctm = ReadMatrixOperands(operands, defaultDiagonal: false).Multiply(ctm);
                    }

                    break;

                case "Do":
                {
                    if (operands.Length == 0 || operands[0].AsName() is not { } xobjName)
                    {
                        break;
                    }

                    switch (formXObjects.GetValueOrDefault(xobjName))
                    {
                        case XObjectKind.Form nested when depth < MaxFormXObjectDepth:
                            items.AddRange(Extract(
                                doc, nested.Id, pageNum, fontCMaps, ctm, cmapDecisions, styleCache, depth + 1));
                            break;

                        case XObjectKind.Image:
                        {
                            // Mirrors the page-level emission so figures inside
                            // Form XObjects — common in print-to-PDF output —
                            // are not silently dropped.
                            var (x, y, width, height) = Geometry.ImageBoundsFromCtm(ctm);
                            items.Add(new TextItem
                            {
                                Text = $"[Image: {xobjName}]",
                                X = x,
                                Y = y,
                                Width = width,
                                Height = height,
                                Page = pageNum,
                                Kind = ItemKind.Image,
                            });
                            break;
                        }
                    }

                    break;
                }

                case "BT":
                    inTextBlock = true;
                    textMatrix = Matrix.Identity;
                    break;

                case "ET":
                    inTextBlock = false;
                    break;

                case "Tf":
                    if (operands.Length >= 2)
                    {
                        if (operands[0].AsName() is { } fontName)
                        {
                            currentFont = fontName;
                        }

                        currentFontSize = Geometry.GetNumber(operands[1]) ?? 12.0f;
                    }

                    break;

                case "Td":
                case "TD":
                    if (operands.Length >= 2)
                    {
                        var tx = Geometry.GetNumber(operands[0]) ?? 0.0f;
                        var ty = Geometry.GetNumber(operands[1]) ?? 0.0f;
                        textMatrix = textMatrix.TranslatedBy(tx, ty);
                    }

                    break;

                case "Tm":
                    if (operands.Length >= 6)
                    {
                        textMatrix = ReadMatrixOperands(operands, defaultDiagonal: true);
                    }

                    break;

                // White fill marks text drawn to be invisible against the page.
                case "g":
                    fillIsWhite = Geometry.GetNumber(operands.FirstOrDefault() ?? PdfObject.Null) > 0.95f;
                    break;

                case "rg":
                    if (operands.Length >= 3)
                    {
                        fillIsWhite = (Geometry.GetNumber(operands[0]) ?? 0f) > 0.95f
                            && (Geometry.GetNumber(operands[1]) ?? 0f) > 0.95f
                            && (Geometry.GetNumber(operands[2]) ?? 0f) > 0.95f;
                    }

                    break;

                case "k":
                    if (operands.Length >= 4)
                    {
                        fillIsWhite = (Geometry.GetNumber(operands[0]) ?? 1f) < 0.05f
                            && (Geometry.GetNumber(operands[1]) ?? 1f) < 0.05f
                            && (Geometry.GetNumber(operands[2]) ?? 1f) < 0.05f
                            && (Geometry.GetNumber(operands[3]) ?? 1f) < 0.05f;
                    }

                    break;

                case "sc":
                case "scn":
                {
                    var nums = operands.Select(Geometry.GetNumber).Where(n => n is not null).Select(n => n!.Value).ToList();
                    fillIsWhite = nums.Count switch
                    {
                        3 => nums[0] > 0.95f && nums[1] > 0.95f && nums[2] > 0.95f,
                        4 => nums[0] < 0.05f && nums[1] < 0.05f && nums[2] < 0.05f && nums[3] < 0.05f,
                        _ => false,
                    };
                    break;
                }

                case "Tj":
                {
                    if (!inTextBlock || operands.Length == 0)
                    {
                        break;
                    }

                    context.Widths.TryGetValue(currentFont, out var fontInfo);

                    float? widthTs = null;
                    if (fontInfo is not null && operands[0] is PdfString str)
                    {
                        widthTs = FontWidths.ComputeStringWidthTs(
                            str.Bytes, fontInfo, currentFontSize, 0.0f, 0.0f);
                    }

                    if (fillIsWhite)
                    {
                        if (widthTs is { } advance)
                        {
                            textMatrix = textMatrix.TranslatedBy(advance, 0f);
                        }

                        break;
                    }

                    if (DecodeOperand(operands[0]) is not { } text)
                    {
                        break;
                    }

                    var combined = textMatrix.Multiply(ctm);
                    var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());
                    var (x, y) = (combined.E, combined.F);

                    var width = 0.0f;
                    if (widthTs is { } advanceTs)
                    {
                        var scaleX = (textMatrix.A * ctm.A) + (textMatrix.B * ctm.C);
                        textMatrix = textMatrix.TranslatedBy(advanceTs, 0f);
                        width = MathF.Abs(advanceTs * scaleX);
                    }

                    if (text.Trim().Length > 0)
                    {
                        items.Add(BuildItem(text, x, y, width, renderedSize, currentFont, pageNum, fontSetup, mcid: null));
                    }

                    break;
                }

                case "TJ":
                {
                    if (!inTextBlock || operands.Length == 0 || operands[0].AsArray() is not { } array)
                    {
                        break;
                    }

                    context.Widths.TryGetValue(currentFont, out var fontInfo);

                    var segmented = ShowTextArray.Segment(
                        array, fontInfo, currentFontSize, 0.0f, 0.0f, fillIsWhite, DecodeOperand);

                    if (segmented.Runs.Count > 0)
                    {
                        var combined = textMatrix.Multiply(ctm);
                        var renderedSize = TextUtils.EffectiveFontSize(currentFontSize, combined.ToArray());
                        var scaleX = (textMatrix.A * ctm.A) + (textMatrix.B * ctm.C);

                        foreach (var run in segmented.Runs)
                        {
                            var offsetTm = textMatrix.TranslatedBy(run.StartWidthTs, 0f);
                            var runCombined = offsetTm.Multiply(ctm);
                            var width = fontInfo is not null
                                ? MathF.Abs((run.EndWidthTs - run.StartWidthTs) * scaleX)
                                : 0.0f;

                            items.Add(BuildItem(
                                run.Text, runCombined.E, runCombined.F, width, renderedSize,
                                currentFont, pageNum, fontSetup, mcid: null));
                        }
                    }

                    if (fontInfo is not null)
                    {
                        textMatrix = textMatrix.TranslatedBy(segmented.TotalWidthTs, 0f);
                    }

                    break;
                }
            }
        }

        return items;
    }

    private static TextItem BuildItem(
        string text,
        float x,
        float y,
        float width,
        float renderedSize,
        string currentFont,
        uint pageNum,
        PageFontContextBuilder.Result fontSetup,
        long? mcid)
    {
        var baseFont = fontSetup.BaseNames.GetValueOrDefault(currentFont, currentFont);
        var (descItalic, descBold) = fontSetup.StyleFlags.GetValueOrDefault(currentFont, (false, false));

        return new TextItem
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
        };
    }

    /// <summary>The fonts a Form XObject's own resource dictionary declares.</summary>
    private static SortedDictionary<string, PdfDictionary> GetFormFonts(PdfDocument doc, PdfDictionary formDict)
    {
        var fonts = new SortedDictionary<string, PdfDictionary>(StringComparer.Ordinal);

        var resources = doc.GetDeref(formDict, "Resources")?.AsDictionary();
        if (resources is null)
        {
            return fonts;
        }

        var fontDict = doc.GetDeref(resources, "Font")?.AsDictionary();
        if (fontDict is null)
        {
            return fonts;
        }

        foreach (var (name, value) in fontDict)
        {
            if (doc.Resolve(value).AsDictionary() is { } font)
            {
                fonts[name] = font;
            }
        }

        return fonts;
    }

    private static Matrix ReadMatrix(PdfArray? array)
    {
        if (array is null || array.Count < 6)
        {
            return Matrix.Identity;
        }

        return new Matrix(
            Geometry.GetNumber(array[0]) ?? 1f,
            Geometry.GetNumber(array[1]) ?? 0f,
            Geometry.GetNumber(array[2]) ?? 0f,
            Geometry.GetNumber(array[3]) ?? 1f,
            Geometry.GetNumber(array[4]) ?? 0f,
            Geometry.GetNumber(array[5]) ?? 0f);
    }

    /// <summary>
    /// Reads six matrix operands. A missing value defaults to the identity
    /// element for its position when <paramref name="defaultDiagonal"/> is set,
    /// and to zero otherwise — matching how the reference build treats <c>cm</c>
    /// and <c>Tm</c> differently.
    /// </summary>
    internal static Matrix ReadMatrixOperands(PdfObject[] operands, bool defaultDiagonal)
    {
        float At(int i)
        {
            var fallback = defaultDiagonal && (i == 0 || i == 3) ? 1f : 0f;
            return Geometry.GetNumber(operands[i]) ?? fallback;
        }

        return new Matrix(At(0), At(1), At(2), At(3), At(4), At(5));
    }
}
