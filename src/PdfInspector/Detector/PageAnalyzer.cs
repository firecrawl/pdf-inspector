// Ported from reference/src/detector.rs
using PdfInspector.Fonts;
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;

namespace PdfInspector.Detector;

/// <summary>What a page's content stream and resources reveal about it.</summary>
internal sealed class PageAnalysis
{
    /// <summary>How many text-showing operators the page's streams carry.</summary>
    public uint TextOperatorCount { get; set; }

    public bool HasImages { get; set; }

    /// <summary>True when the page carries a large background or template image.</summary>
    public bool HasTemplateImage { get; set; }

    /// <summary>Total image area, in pixels.</summary>
    public ulong TotalImageArea { get; set; }

    /// <summary>How many image XObjects the page's resources reach.</summary>
    public uint ImageCount { get; set; }

    /// <summary>Distinct non-whitespace bytes seen in string operands.</summary>
    public uint UniqueTextChars { get; set; }

    /// <summary>Distinct ASCII letters and digits seen in string operands.</summary>
    public uint UniqueAlphanumChars { get; set; }

    /// <summary>How many path construction and painting operators the page carries.</summary>
    public uint PathOpCount { get; set; }

    /// <summary>True when the page's text is drawn as vector outlines: many path ops, few text ops.</summary>
    public bool HasVectorText { get; set; }

    /// <summary>
    /// True when the page uses Type0 fonts with Identity-H or Identity-V encoding
    /// and no ToUnicode CMap. Those produce garbage text, since raw CID values
    /// cannot map to Unicode.
    /// </summary>
    public bool HasIdentityHNoToUnicode { get; set; }

    /// <summary>
    /// True when every font the page uses is Type3. Type3 fonts render each glyph
    /// as a custom drawing, so without a ToUnicode CMap their character codes
    /// cannot map to Unicode.
    /// </summary>
    public bool HasOnlyType3Fonts { get; set; }

    /// <summary>How many set-font operators the page carries; a high count means many switches.</summary>
    public uint FontChangeCount { get; set; }

    /// <summary>
    /// True when the page has at least one font that can produce decodable text.
    /// CID-encoded text with a ToUnicode CMap yields few distinct raw bytes but is
    /// fully decodable, and this flag keeps it from reading as a scan.
    /// </summary>
    public bool HasDecodableTextFonts { get; set; }
}

/// <summary>
/// Per-page content analysis: a fast scan of content-stream bytes for text,
/// path and image evidence, plus the font checks that decide whether the text
/// can reach Unicode at all.
/// </summary>
internal static class PageAnalyzer
{
    /// <summary>
    /// An image at roughly half a page at 150 DPI. The conservative 500K figure
    /// accommodates varying DPI and page sizes.
    /// </summary>
    private const ulong TemplateImageThreshold = 500_000;

    /// <summary>What a resource dictionary says about one font, without holding the document.</summary>
    private sealed record FontInfo(string? Subtype, string? Encoding, bool HasToUnicode, PdfDictionary Dict);

    /// <summary>
    /// Explains why a page needs OCR, from its content analysis. Undecodable fonts
    /// and vector-outlined text come first, because they persist even when a text
    /// layer is present; otherwise a page with no extractable text is "scanned"
    /// when an image backs it, and "no_text" when nothing does.
    /// </summary>
    public static List<string> PageOcrReasons(PageAnalysis a)
    {
        var reasons = new List<string>();

        if (a.HasIdentityHNoToUnicode || a.HasOnlyType3Fonts)
        {
            reasons.Add(OcrReason.SuspectedGarbledText);
        }

        if (a.HasVectorText)
        {
            reasons.Add(OcrReason.VectorText);
        }

        if (reasons.Count == 0)
        {
            var hasExtractableText = a.TextOperatorCount > 0 && a.UniqueTextChars > 0;
            reasons.Add(!hasExtractableText && !a.HasImages && !a.HasTemplateImage
                ? OcrReason.NoText

                // Image-backed with no usable text, or too little text to trust.
                : OcrReason.Scanned);
        }

        return reasons;
    }

    /// <summary>Analyses a page's content streams and resources.</summary>
    public static PageAnalysis AnalyzePageContent(PdfDocument doc, PdfDictionary page)
    {
        var textOps = 0u;
        var hasImages = false;
        var imageCount = 0u;
        var pathOps = 0u;
        var fontChanges = 0u;
        var allUniqueChars = new HashSet<byte>();

        // Fonts are collected by object id, not name, so the same "/F1" defined in
        // different resource dictionaries cannot collide. Each content stream
        // resolves its own font names against its own resources.
        var usedFontIds = new HashSet<PdfObjectId>();
        var fontMap = new Dictionary<PdfObjectId, FontInfo>();

        var pageResources = doc.GetPageResources(page);

        // Page content resolves its font names against the page's resource
        // dictionaries, honouring PDF resource inheritance: the most specific scope
        // wins over an inherited ancestor.
        var content = doc.GetPageContent(page);
        if (content.Length > 0)
        {
            var pageFontNames = new HashSet<string>(StringComparer.Ordinal);
            var scan = ScanContentForTextOperators(content, allUniqueChars, pageFontNames);
            textOps += scan.TextOps;
            imageCount += scan.ImageCount;
            pathOps += scan.PathOps;
            fontChanges += scan.FontChanges;
            hasImages = hasImages || scan.ImageCount > 0;

            ResolveWithShadowing(doc, pageResources, pageFontNames, usedFontIds);
        }

        // Form XObjects carry their own content and resources, and their fonts
        // resolve in their own scope.
        var visited = new HashSet<PdfObjectId>();
        foreach (var resources in pageResources)
        {
            CollectFontsFromResourceDict(doc, resources, fontMap);
            var scan = ScanXObjectsInResources(doc, resources, visited, allUniqueChars, usedFontIds, fontMap);
            textOps += scan.TextOps;
            imageCount += scan.ImageCount;
            pathOps += scan.PathOps;
            fontChanges += scan.FontChanges;
            hasImages = hasImages || scan.ImageCount > 0;
        }

        var (foundImages, totalImageArea, hasTemplateImage) = AnalyzePageImages(doc, page);
        hasImages = hasImages || foundImages;

        var uniqueAlphanumChars = (uint)allUniqueChars.Count(b => char.IsAsciiLetterOrDigit((char)b));

        // Vector-outlined text: many path operators with almost no text ones. Each
        // outlined glyph needs roughly 10 to 30 path commands, so a page of outlined
        // text produces thousands. Few distinct alphanumeric bytes is required too:
        // a real outlined page has very few because each glyph is a path, while a
        // page of selectable text plus decorative paths — column borders, dividers —
        // has many, and is not vector-outlined text.
        var hasVectorText = pathOps >= 1000
            && pathOps > SaturatingMul(textOps, 200)
            && uniqueAlphanumChars < 30;

        return new PageAnalysis
        {
            TextOperatorCount = textOps,
            HasImages = hasImages,
            HasTemplateImage = hasTemplateImage,
            TotalImageArea = totalImageArea,
            ImageCount = imageCount,
            UniqueTextChars = (uint)allUniqueChars.Count,
            UniqueAlphanumChars = uniqueAlphanumChars,
            PathOpCount = pathOps,
            HasVectorText = hasVectorText,

            // Only fonts actually used by a set-font operator are considered, and
            // Form XObject fonts are included.
            HasIdentityHNoToUnicode = textOps > 0 && UsedFontsHaveIdentityHNoToUnicode(usedFontIds, fontMap, doc),
            HasOnlyType3Fonts = textOps > 0 && UsedFontsAreOnlyType3(usedFontIds, fontMap),
            HasDecodableTextFonts = textOps > 0 && UsedFontsHaveDecodableText(usedFontIds, fontMap, doc),
            FontChangeCount = fontChanges,
        };
    }

    /// <summary>Multiplies without overflowing past the maximum.</summary>
    private static uint SaturatingMul(uint a, uint b)
    {
        var product = (ulong)a * b;
        return product > uint.MaxValue ? uint.MaxValue : (uint)product;
    }

    /// <summary>Collects a resource dictionary's fonts into the map, keyed by object id.</summary>
    private static void CollectFontsFromResourceDict(
        PdfDocument doc,
        PdfDictionary resources,
        Dictionary<PdfObjectId, FontInfo> fontMap)
    {
        var fontDict = doc.GetDict(resources, "Font");
        if (fontDict is null)
        {
            return;
        }

        foreach (var (_, value) in fontDict)
        {
            // Only an indirect reference has a stable object id. An inline font
            // dictionary is extremely rare and has none, so it is skipped.
            if (value.AsReference() is not { } fontObjId || fontMap.ContainsKey(fontObjId))
            {
                continue;
            }

            if (doc.GetObject(fontObjId)?.AsDictionary() is not { } fd)
            {
                continue;
            }

            fontMap[fontObjId] = new FontInfo(
                doc.GetName(fd, "Subtype"),
                doc.GetName(fd, "Encoding"),
                fd.ContainsKey("ToUnicode"),
                fd);
        }
    }

    /// <summary>Resolves font names against one resource dictionary's font entries.</summary>
    private static void ResolveFontNamesToIds(
        PdfDocument doc,
        PdfDictionary resources,
        HashSet<string> fontNames,
        HashSet<PdfObjectId> usedFontIds)
    {
        var fontDict = doc.GetDict(resources, "Font");
        if (fontDict is null)
        {
            return;
        }

        foreach (var name in fontNames)
        {
            if (fontDict.TryGetValue(name, out var value) && value.AsReference() is { } r)
            {
                usedFontIds.Add(r);
            }
        }
    }

    /// <summary>
    /// Resolves page-level font names with resource-inheritance shadowing. A page
    /// inherits resources from its ancestors, but a definition in a more specific
    /// scope shadows the same name from an ancestor, so the first dictionary that
    /// defines a name wins.
    /// </summary>
    private static void ResolveWithShadowing(
        PdfDocument doc,
        List<PdfDictionary> resourceChain,
        HashSet<string> names,
        HashSet<PdfObjectId> usedFontIds)
    {
        foreach (var name in names)
        {
            foreach (var resources in resourceChain)
            {
                var fontDict = doc.GetDict(resources, "Font");
                if (fontDict is not null
                    && fontDict.TryGetValue(name, out var value)
                    && value.AsReference() is { } id)
                {
                    usedFontIds.Add(id);
                    break;
                }
            }
        }
    }

    /// <summary>
    /// True when every used font is an Identity-encoded Type0 without a usable
    /// mapping, and none of them is decodable another way.
    /// </summary>
    private static bool UsedFontsHaveIdentityHNoToUnicode(
        HashSet<PdfObjectId> usedFontIds,
        Dictionary<PdfObjectId, FontInfo> fontMap,
        PdfDocument doc)
    {
        var hasUndecodableIdentityH = false;
        var hasOtherDecodableFont = false;

        foreach (var id in usedFontIds)
        {
            if (!fontMap.TryGetValue(id, out var info))
            {
                continue;
            }

            switch (info.Subtype)
            {
                case "Type0":
                {
                    var isIdentity = info.Encoding is "Identity-H" or "Identity-V";
                    if (!isIdentity || info.HasToUnicode || IdentityHFontHasFallback(info.Dict, doc))
                    {
                        hasOtherDecodableFont = true;
                        continue;
                    }

                    hasUndecodableIdentityH = true;
                    break;
                }

                case "Type3":
                    // Handled separately by the Type3-only check.
                    break;

                default:
                    // Type1, TrueType, MMType1 and the rest are generally decodable.
                    hasOtherDecodableFont = true;
                    break;
            }
        }

        return hasUndecodableIdentityH && !hasOtherDecodableFont;
    }

    /// <summary>True when every used font is a Type3 without a ToUnicode CMap.</summary>
    private static bool UsedFontsAreOnlyType3(
        HashSet<PdfObjectId> usedFontIds,
        Dictionary<PdfObjectId, FontInfo> fontMap)
    {
        if (usedFontIds.Count == 0)
        {
            return false;
        }

        var hasType3 = false;
        foreach (var id in usedFontIds)
        {
            if (!fontMap.TryGetValue(id, out var info))
            {
                continue;
            }

            if (info.Subtype != "Type3")
            {
                return false;
            }

            if (info.HasToUnicode)
            {
                return false;
            }

            hasType3 = true;
        }

        return hasType3;
    }

    /// <summary>True when at least one used font can produce decodable Unicode text.</summary>
    private static bool UsedFontsHaveDecodableText(
        HashSet<PdfObjectId> usedFontIds,
        Dictionary<PdfObjectId, FontInfo> fontMap,
        PdfDocument doc)
    {
        foreach (var id in usedFontIds)
        {
            if (!fontMap.TryGetValue(id, out var info))
            {
                continue;
            }

            if (info.HasToUnicode)
            {
                return true;
            }

            switch (info.Subtype)
            {
                case "Type1":
                case "TrueType":
                case "MMType1":
                    return true;

                case "Type0" when IdentityHFontHasFallback(info.Dict, doc):
                    return true;
            }
        }

        return false;
    }

    /// <summary>
    /// True when an Identity-encoded Type0 font can still be decoded: either its
    /// CIDs look like Unicode code points, as many generators emit, or its embedded
    /// font program carries a usable cmap table.
    /// </summary>
    private static bool IdentityHFontHasFallback(PdfDictionary fontDict, PdfDocument doc)
    {
        var descFonts = doc.GetArray(fontDict, "DescendantFonts");
        if (descFonts is null || descFonts.Count == 0)
        {
            return false;
        }

        if (doc.Resolve(descFonts[0])?.AsDictionary() is not { } cidFontDict)
        {
            return false;
        }

        // Many PDF generators — Chromium, wkhtmltopdf — emit Identity-H where the
        // CID is the Unicode code point, so passthrough works.
        if (CMapBuilders.CidValuesLookLikeUnicode(cidFontDict))
        {
            return true;
        }

        if (doc.GetDict(cidFontDict, "FontDescriptor") is not { } fontDescriptor)
        {
            return false;
        }

        var fontFile = doc.GetStream(fontDescriptor, "FontFile2") ?? doc.GetStream(fontDescriptor, "FontFile3");
        return fontFile is not null && EmbeddedFontHasCmap(fontFile);
    }

    /// <summary>
    /// True when an embedded TrueType or OpenType font has a cmap table that maps
    /// glyph ids to Unicode code points.
    /// </summary>
    private static bool EmbeddedFontHasCmap(PdfStream stream)
    {
        var data = StreamFilters.Decode(stream);
        if (data is null || data.Length == 0)
        {
            return false;
        }

        var face = TrueTypeFace.Parse(data);
        if (face is null)
        {
            return false;
        }

        foreach (var subtable in face.CmapSubtables)
        {
            if (subtable.IsUnicode || (subtable.PlatformId == TrueTypePlatform.Windows && subtable.EncodingId == 0))
            {
                if (subtable.CodePoints().Any())
                {
                    return true;
                }
            }
        }

        return false;
    }

    /// <summary>The operator counts one content-stream scan produced.</summary>
    private readonly record struct ScanCounts(uint TextOps, uint ImageCount, uint PathOps, uint FontChanges);

    /// <summary>
    /// Walks a resource dictionary's XObjects: a Form's content is scanned and its
    /// own resources recursed into, while an Image simply counts.
    /// </summary>
    private static ScanCounts ScanXObjectsInResources(
        PdfDocument doc,
        PdfDictionary resources,
        HashSet<PdfObjectId> visited,
        HashSet<byte> uniqueChars,
        HashSet<PdfObjectId> usedFontIds,
        Dictionary<PdfObjectId, FontInfo> fontMap)
    {
        var textOps = 0u;
        var imageCount = 0u;
        var pathOps = 0u;
        var fontChanges = 0u;

        var xobjects = doc.GetDict(resources, "XObject");
        if (xobjects is null)
        {
            return new ScanCounts(textOps, imageCount, pathOps, fontChanges);
        }

        foreach (var (_, obj) in xobjects)
        {
            if (obj.AsReference() is not { } objId || !visited.Add(objId))
            {
                continue;
            }

            if (doc.GetObject(objId)?.AsStream() is not { } stream)
            {
                continue;
            }

            var subtype = doc.GetName(stream.Dictionary, "Subtype");

            if (subtype == "Form")
            {
                var content = StreamFilters.Decode(stream) ?? stream.RawData;
                var xobjFontNames = new HashSet<string>(StringComparer.Ordinal);
                var scan = ScanContentForTextOperators(content, uniqueChars, xobjFontNames);
                textOps += scan.TextOps;
                imageCount += scan.ImageCount;
                pathOps += scan.PathOps;
                fontChanges += scan.FontChanges;

                if (doc.GetDict(stream.Dictionary, "Resources") is { } res)
                {
                    // Font names resolve in the XObject's own scope, not by a global
                    // name lookup.
                    ResolveFontNamesToIds(doc, res, xobjFontNames, usedFontIds);
                    CollectFontsFromResourceDict(doc, res, fontMap);

                    var nested = ScanXObjectsInResources(doc, res, visited, uniqueChars, usedFontIds, fontMap);
                    textOps += nested.TextOps;
                    imageCount += nested.ImageCount;
                    pathOps += nested.PathOps;
                    fontChanges += nested.FontChanges;
                }
            }
            else if (subtype == "Image")
            {
                imageCount++;
            }
        }

        return new ScanCounts(textOps, imageCount, pathOps, fontChanges);
    }

    /// <summary>
    /// A fast byte scan of a content stream for text-showing operators, set-font
    /// operators and path operators, collecting the distinct bytes that string
    /// operands carry.
    /// </summary>
    /// <remarks>
    /// The <c>Do</c> operator is deliberately not counted as an image: it invokes
    /// any XObject, Form ones included. Real image detection happens in the XObject
    /// walk, which checks the subtype, and in the image analysis, which measures
    /// pixel area.
    /// </remarks>
    private static ScanCounts ScanContentForTextOperators(
        byte[] content,
        HashSet<byte> uniqueChars,
        HashSet<string> usedFontNames)
    {
        var textOps = 0u;
        var pathOps = 0u;
        var fontChanges = 0u;

        bool IsWordStart(int pos) => pos == 0 || IsAsciiWhitespace(content[pos - 1]);
        bool IsWordEnd(int pos) => pos + 1 >= content.Length || IsAsciiWhitespace(content[pos + 1]);

        for (var i = 0; i < content.Length; i++)
        {
            var b = content[i];

            if (b == (byte)'T' && i + 1 < content.Length)
            {
                var next = content[i + 1];
                if (next is (byte)'j' or (byte)'J')
                {
                    if (i + 2 >= content.Length
                        || IsAsciiWhitespace(content[i + 2])
                        || content[i + 2] == (byte)'\n'
                        || content[i + 2] == (byte)'\r')
                    {
                        textOps++;
                        CollectTextCharsBefore(content, i, uniqueChars);
                    }
                }
                else if (next == (byte)'f')
                {
                    // Some PDFs run Tf straight into the next operator with no
                    // whitespace — "25 Tf[<01>..." — so those openers count too.
                    if (i + 2 >= content.Length
                        || IsAsciiWhitespace(content[i + 2])
                        || content[i + 2] is (byte)'\n' or (byte)'\r' or (byte)'[' or (byte)'('
                            or (byte)'<' or (byte)'/')
                    {
                        fontChanges++;
                        if (ExtractFontNameBeforeTf(content, i) is { } name)
                        {
                            usedFontNames.Add(name);
                        }
                    }
                }
            }

            // Single-byte path operators: moveto, lineto, curveto, closepath, fill,
            // stroke, close-and-stroke, fill-and-stroke, and the fill variant. These
            // are the high-volume operators in vector-outlined text.
            if (b is (byte)'m' or (byte)'l' or (byte)'c' or (byte)'h' or (byte)'f'
                or (byte)'S' or (byte)'s' or (byte)'B' or (byte)'F'
                && IsWordStart(i) && IsWordEnd(i))
            {
                pathOps++;
            }
            else if (b == (byte)'r'
                && i + 1 < content.Length
                && content[i + 1] == (byte)'e'
                && IsWordStart(i)
                && (i + 2 >= content.Length || IsAsciiWhitespace(content[i + 2])))
            {
                pathOps++;
            }
            else if (b == (byte)'f'
                && i + 1 < content.Length
                && content[i + 1] == (byte)'*'
                && IsWordStart(i)
                && (i + 2 >= content.Length || IsAsciiWhitespace(content[i + 2])))
            {
                pathOps++;
            }
        }

        return new ScanCounts(textOps, 0, pathOps, fontChanges);
    }

    /// <summary>True for the bytes .NET's char.IsAsciiWhiteSpace accepts.</summary>
    private static bool IsAsciiWhitespace(byte b) =>
        b is (byte)' ' or (byte)'\t' or (byte)'\n' or (byte)'\v' or (byte)'\f' or (byte)'\r';

    /// <summary>
    /// Extracts the font name operand preceding a set-font operator, whose syntax
    /// is "/FontName size Tf". The scan walks back past the size and the whitespace
    /// to the name's leading slash.
    /// </summary>
    private static string? ExtractFontNameBeforeTf(byte[] content, int tfPos)
    {
        var j = tfPos;
        while (j > 0 && IsAsciiWhitespace(content[j - 1]))
        {
            j--;
        }

        while (j > 0 && (char.IsAsciiDigit((char)content[j - 1]) || content[j - 1] is (byte)'.' or (byte)'-'))
        {
            j--;
        }

        while (j > 0 && IsAsciiWhitespace(content[j - 1]))
        {
            j--;
        }

        var nameEnd = j;
        while (j > 0 && content[j - 1] != (byte)'/')
        {
            // A font name is made of regular characters — no whitespace, no delimiters.
            if (IsAsciiWhitespace(content[j - 1]) || content[j - 1] is (byte)'(' or (byte)')')
            {
                return null;
            }

            j--;
        }

        if (j == 0 || content[j - 1] != (byte)'/' || j >= nameEnd)
        {
            return null;
        }

        var lexer = new PdfLexer(content, j);
        return lexer.ReadNameBody();
    }

    /// <summary>
    /// Scans back from a text-showing operator to its string operand and collects
    /// the distinct non-whitespace bytes. Literal strings, hex strings and TJ
    /// arrays are all handled.
    /// </summary>
    private static void CollectTextCharsBefore(byte[] content, int opPos, HashSet<byte> uniqueChars)
    {
        var j = opPos;
        while (j > 0)
        {
            j--;
            if (!IsAsciiWhitespace(content[j]))
            {
                break;
            }
        }

        if (j == 0)
        {
            return;
        }

        var closing = content[j];

        if (closing == (byte)')')
        {
            var depth = 1;
            var k = j;
            while (k > 0 && depth > 0)
            {
                k--;
                if (content[k] == (byte)')' && (k == 0 || content[k - 1] != (byte)'\\'))
                {
                    depth++;
                }
                else if (content[k] == (byte)'(' && (k == 0 || content[k - 1] != (byte)'\\'))
                {
                    depth--;
                }
            }

            if (depth == 0 && k + 1 < j)
            {
                CollectNonWhitespace(content, k + 1, j, uniqueChars);
            }
        }
        else if (closing == (byte)'>')
        {
            var k = j;
            while (k > 0)
            {
                k--;
                if (content[k] == (byte)'<')
                {
                    break;
                }
            }

            if (content[k] == (byte)'<' && k + 1 < j)
            {
                CollectHexBytes(content, k + 1, j, uniqueChars);
            }
        }
        else if (closing == (byte)']')
        {
            var k = j;
            while (k > 0)
            {
                k--;
                if (content[k] == (byte)'[')
                {
                    break;
                }
            }

            if (content[k] != (byte)'[')
            {
                return;
            }

            var m = k + 1;
            while (m < j)
            {
                if (content[m] == (byte)'(')
                {
                    var start = m + 1;
                    var depth = 1;
                    m++;
                    while (m < j && depth > 0)
                    {
                        if (content[m] == (byte)')' && content[m - 1] != (byte)'\\')
                        {
                            depth--;
                        }
                        else if (content[m] == (byte)'(' && content[m - 1] != (byte)'\\')
                        {
                            depth++;
                        }

                        if (depth > 0)
                        {
                            m++;
                        }
                    }

                    CollectNonWhitespace(content, start, m, uniqueChars);
                }
                else if (content[m] == (byte)'<')
                {
                    var hexStart = m + 1;
                    m++;
                    while (m < j && content[m] != (byte)'>')
                    {
                        m++;
                    }

                    CollectHexBytes(content, hexStart, m, uniqueChars);
                }

                m++;
            }
        }
    }

    /// <summary>Collects the non-whitespace bytes in a range.</summary>
    private static void CollectNonWhitespace(byte[] content, int start, int end, HashSet<byte> uniqueChars)
    {
        for (var i = start; i < end && i < content.Length; i++)
        {
            if (!IsAsciiWhitespace(content[i]))
            {
                uniqueChars.Add(content[i]);
            }
        }
    }

    /// <summary>Decodes a hex-string range and collects the bytes it yields.</summary>
    private static void CollectHexBytes(byte[] content, int start, int end, HashSet<byte> uniqueChars)
    {
        var digits = new List<byte>();
        for (var i = start; i < end && i < content.Length; i++)
        {
            if (!IsAsciiWhitespace(content[i]))
            {
                digits.Add(content[i]);
            }
        }

        for (var i = 0; i + 1 < digits.Count; i += 2)
        {
            if (HexVal(digits[i]) is { } high && HexVal(digits[i + 1]) is { } low)
            {
                var b = (byte)((high << 4) | low);
                if (b != 0 && b != (byte)' ' && b != (byte)'\t' && b != (byte)'\n')
                {
                    uniqueChars.Add(b);
                }
            }
        }
    }

    /// <summary>The numeric value of a hex ASCII digit, or null when it is not one.</summary>
    private static byte? HexVal(byte b) => b switch
    {
        >= (byte)'0' and <= (byte)'9' => (byte)(b - (byte)'0'),
        >= (byte)'a' and <= (byte)'f' => (byte)(b - (byte)'a' + 10),
        >= (byte)'A' and <= (byte)'F' => (byte)(b - (byte)'A' + 10),
        _ => null,
    };

    /// <summary>
    /// Measures the page's images. A template image covers more than about half a
    /// page; many small tiles that together cover the page count as one too, which
    /// is how JBIG2 strip scans present themselves.
    /// </summary>
    private static (bool HasImages, ulong TotalArea, bool HasTemplateImage) AnalyzePageImages(
        PdfDocument doc,
        PdfDictionary page)
    {
        var state = new ImageScanState();
        var visited = new HashSet<PdfObjectId>();

        if (doc.GetDict(page, "Resources") is { } resources)
        {
            CollectImagesFromResources(doc, resources, state, visited);

            // Tiling patterns can hold image XObjects too — a screenshot pasted into
            // a PDF through a browser's "save as PDF" arrives that way.
            if (doc.GetDict(resources, "Pattern") is { } patternDict)
            {
                foreach (var (_, value) in patternDict)
                {
                    if (value.AsReference() is not { } patRef || !visited.Add(patRef))
                    {
                        continue;
                    }

                    if (doc.GetObject(patRef)?.AsStream() is { } stream
                        && doc.GetDict(stream.Dictionary, "Resources") is { } patRes)
                    {
                        CollectImagesFromResources(doc, patRes, state, visited);
                    }
                }
            }
        }

        // Tiled scans: many small tiles that together cover the page. No single tile
        // reaches the template threshold, but the aggregate clearly marks the page
        // as image-backed.
        if (!state.HasTemplateImage && state.TotalArea >= TemplateImageThreshold * 4)
        {
            state.HasTemplateImage = true;
        }

        return (state.HasImages, state.TotalArea, state.HasTemplateImage);
    }

    /// <summary>Accumulated image evidence for one page.</summary>
    private sealed class ImageScanState
    {
        public bool HasImages { get; set; }

        public ulong TotalArea { get; set; }

        public bool HasTemplateImage { get; set; }
    }

    /// <summary>
    /// Collects image dimensions from a resource dictionary's XObjects, recursing
    /// into Form XObjects for the images nested inside them.
    /// </summary>
    private static void CollectImagesFromResources(
        PdfDocument doc,
        PdfDictionary resources,
        ImageScanState state,
        HashSet<PdfObjectId> visited)
    {
        var xobjectDict = doc.GetDict(resources, "XObject");
        if (xobjectDict is null)
        {
            return;
        }

        foreach (var (_, value) in xobjectDict)
        {
            if (value.AsReference() is not { } xobjRef || !visited.Add(xobjRef))
            {
                continue;
            }

            if (doc.GetObject(xobjRef)?.AsStream() is not { } stream)
            {
                continue;
            }

            var name = doc.GetName(stream.Dictionary, "Subtype");

            if (name == "Image")
            {
                state.HasImages = true;
                var width = (ulong)Math.Max(doc.GetInteger(stream.Dictionary, "Width") ?? 0, 0);
                var height = (ulong)Math.Max(doc.GetInteger(stream.Dictionary, "Height") ?? 0, 0);
                var area = width * height;
                state.TotalArea += area;
                if (area >= TemplateImageThreshold)
                {
                    state.HasTemplateImage = true;
                }
            }
            else if (name == "Form" && doc.GetDict(stream.Dictionary, "Resources") is { } formRes)
            {
                CollectImagesFromResources(doc, formRes, state, visited);
            }
        }
    }
}
