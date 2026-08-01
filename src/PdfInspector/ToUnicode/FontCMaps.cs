// Ported from reference/src/tounicode.rs
using PdfInspector.Pdf;

namespace PdfInspector.ToUnicode;

/// <summary>A primary CMap plus the alternatives the decoder falls back to.</summary>
public sealed class CMapEntry
{
    public required ToUnicodeCMap Primary { get; set; }

    /// <summary>A repaired variant for subset fonts whose glyph ids were renumbered.</summary>
    public ToUnicodeCMap? Remapped { get; set; }

    public ToUnicodeCMap? Fallback { get; set; }
}

/// <summary>
/// The ToUnicode CMaps for a document, keyed by the object number of the stream
/// (or embedded font program) they were derived from.
/// </summary>
public sealed class FontCMaps
{
    private const string Module = "tounicode";

    private readonly Dictionary<int, CMapEntry> _byObjNum = [];

    /// <summary>Builds the CMaps for every page of a document.</summary>
    public static FontCMaps FromDocument(PdfDocument doc) => FromDocumentPages(doc, null);

    /// <summary>Builds the CMaps for the given 1-indexed pages, or all pages when null.</summary>
    public static FontCMaps FromDocumentPages(PdfDocument doc, HashSet<uint>? pageFilter) =>
        Build(doc, pageFilter, skipTrueTypeFallback: false);

    /// <summary>
    /// Builds CMaps without the expensive TrueType fallback parsing. Fonts that
    /// cannot be decoded from their ToUnicode stream alone are left out, so the
    /// affected text extracts as empty or garbage and triggers the OCR path —
    /// the right trade for hybrid pipelines where OCR is always available.
    /// </summary>
    public static FontCMaps FromDocumentPagesFast(PdfDocument doc, HashSet<uint>? pageFilter) =>
        Build(doc, pageFilter, skipTrueTypeFallback: true);

    private static FontCMaps Build(PdfDocument doc, HashSet<uint>? pageFilter, bool skipTrueTypeFallback)
    {
        var result = new FontCMaps();

        for (var pageNum = 1; pageNum <= doc.PageCount; pageNum++)
        {
            if (pageFilter is not null && !pageFilter.Contains((uint)pageNum))
            {
                continue;
            }

            var page = doc.GetPage(pageNum);
            if (page is null)
            {
                continue;
            }

            var fonts = doc.GetPageFonts(page);
            result.CollectFromFonts(doc, page, fonts, skipTrueTypeFallback);

            if (!skipTrueTypeFallback)
            {
                result.CollectFromXObjects(doc, page);
            }
        }

        return result;
    }

    /// <summary>The CMaps for a font, keyed by its ToUnicode or font-file object number.</summary>
    public CMapEntry? GetByObject(int objNum) => _byObjNum.GetValueOrDefault(objNum);

    public int Count => _byObjNum.Count;

    // ── Collection ───────────────────────────────────────────────────────

    private void CollectFromFonts(
        PdfDocument doc,
        PdfDictionary page,
        IReadOnlyDictionary<string, PdfDictionary> fonts,
        bool skipTrueTypeFallback)
    {
        CollectToUnicodeStreams(doc, fonts.Values, skipTrueTypeFallback);

        // Identity-H/V and simple-font passes both need TrueType parsing.
        if (skipTrueTypeFallback)
        {
            return;
        }

        CollectIdentityFonts(doc, fonts.Values);
        CollectSimpleFonts(doc, fonts.Values);
        _ = page;
    }

    /// <summary>First pass: fonts that carry an explicit <c>/ToUnicode</c> stream.</summary>
    private void CollectToUnicodeStreams(
        PdfDocument doc,
        IEnumerable<PdfDictionary> fonts,
        bool skipTrueTypeFallback)
    {
        foreach (var fontDict in fonts)
        {
            if (fontDict.Get("ToUnicode")?.AsReference() is not { } objRef)
            {
                continue;
            }

            var objNum = objRef.Number;
            if (_byObjNum.ContainsKey(objNum))
            {
                continue;
            }

            if (doc.GetObject(objRef)?.AsStream() is not { } stream)
            {
                continue;
            }

            var data = stream.DecompressedContent() ?? stream.RawData;
            var parsed = ToUnicodeCMap.Parse(data);

            if (parsed is null)
            {
                // A ToUnicode stream that fails to parse still leaves the font
                // undecodable, so try the fallbacks rather than give up.
                var recovered = skipTrueTypeFallback
                    ? CMapBuilders.FallbackForSimple(doc, fontDict)
                    : CMapBuilders.FallbackForType0(doc, fontDict) ?? CMapBuilders.FallbackForSimple(doc, fontDict);

                if (recovered is not null)
                {
                    Log.Debug(Module, () =>
                        $"ToUnicode CMap obj={objNum} parse failed; using fallback (entries={recovered.CharMap.Count})");
                    _byObjNum[objNum] = new CMapEntry { Primary = recovered };
                }

                continue;
            }

            Log.Debug(Module, () =>
                $"CMap obj={objNum,-6} code_byte_length={parsed.CodeByteLength} " +
                $"char_map={parsed.CharMap.Count} ranges={parsed.Ranges.Count}");

            var (primary, remapped) = CMapBuilders.TryRemapSubsetCMap(parsed, fontDict, doc, objNum);

            // Expensive fallbacks are built only when the primary CMap is sparse:
            // parsing a large embedded TrueType font can take seconds.
            var primaryEntries = primary.EntryCount;
            ToUnicodeCMap? fallback;

            if (primaryEntries < 10 && !skipTrueTypeFallback)
            {
                var cheap = CMapBuilders.FallbackFromEncoding(doc, fontDict)
                    ?? CMapBuilders.FallbackForSimple(doc, fontDict);
                fallback = cheap ?? CMapBuilders.FallbackForType0(doc, fontDict);
            }
            else if (primaryEntries < 10)
            {
                fallback = CMapBuilders.FallbackFromEncoding(doc, fontDict)
                    ?? CMapBuilders.FallbackForSimple(doc, fontDict);
            }
            else
            {
                fallback = CMapBuilders.FallbackFromEncoding(doc, fontDict);
            }

            if (primaryEntries < 10 && fallback is not null)
            {
                Log.Debug(Module, () =>
                    $"ToUnicode CMap obj={objNum} too sparse ({primaryEntries} entries); using fallback");
                remapped = primary;
                primary = fallback;
                fallback = null;
            }

            _byObjNum[objNum] = new CMapEntry { Primary = primary, Remapped = remapped, Fallback = fallback };
        }
    }

    /// <summary>Second pass: Identity-H/V CID fonts with no <c>/ToUnicode</c>.</summary>
    private void CollectIdentityFonts(PdfDocument doc, IEnumerable<PdfDictionary> fonts)
    {
        foreach (var fontDict in fonts)
        {
            if (fontDict.Get("ToUnicode") is not null)
            {
                continue;
            }

            var encoding = fontDict.Get("Encoding")?.AsName();
            if (encoding is not ("Identity-H" or "Identity-V"))
            {
                continue;
            }

            var descFonts = doc.GetDeref(fontDict, "DescendantFonts")?.AsArray();
            if (descFonts is null || descFonts.Count == 0)
            {
                continue;
            }

            var cidFontDict = doc.Resolve(descFonts[0]).AsDictionary();
            if (cidFontDict is null)
            {
                continue;
            }

            var descriptor = CMapBuilders.GetFontDescriptor(doc, cidFontDict);
            var fontFileRef = CMapBuilders.GetFontFileReference(descriptor);

            // The key must match what the extractor looks up: the font file's
            // object number when embedded, otherwise the CIDFont dictionary's.
            var lookupKey = fontFileRef?.Number ?? descFonts[0].AsReference()?.Number ?? 0;
            if (lookupKey == 0 || _byObjNum.ContainsKey(lookupKey))
            {
                continue;
            }

            if (fontFileRef is not null &&
                CMapBuilders.ReadFontFile(doc, fontFileRef.Value) is { } data &&
                CMapBuilders.FromTrueType(data) is { } embedded)
            {
                Log.Debug(Module, () =>
                    $"TrueType CMap obj={lookupKey,-6} (embedded font) char_map={embedded.CharMap.Count}");
                _byObjNum[lookupKey] = new CMapEntry { Primary = embedded };
                continue;
            }

            if (CMapBuilders.FromCidSystemInfo(doc, cidFontDict) is { } predefined)
            {
                Log.Debug(Module, () =>
                    $"Predefined CMap obj={lookupKey,-6} (CIDSystemInfo) char_map={predefined.CharMap.Count}");
                _byObjNum[lookupKey] = new CMapEntry { Primary = predefined };
                continue;
            }

            // Last resort: treat the CID as a Unicode code point. Producers such
            // as Chromium emit Identity-H fonts whose CIDs really are Unicode but
            // strip the cmap table and omit ToUnicode. The /W array tells the two
            // cases apart: Unicode-valued CIDs sit at 0x41 and above, subset
            // glyph ids near zero.
            if (CMapBuilders.CidValuesLookLikeUnicode(cidFontDict))
            {
                Log.Debug(Module, () =>
                    $"Identity-H font obj={lookupKey}: W array CIDs look like Unicode — using passthrough");
                _byObjNum[lookupKey] = new CMapEntry
                {
                    Primary = new ToUnicodeCMap { CodeByteLength = 2, CidPassthrough = true },
                };
            }
            else
            {
                Log.Debug(Module, () =>
                    $"Identity-H font obj={lookupKey}: no decoding possible (stripped cmap, GID-based CIDs)");
            }
        }
    }

    /// <summary>Third pass: simple fonts with neither <c>/ToUnicode</c> nor an explicit encoding.</summary>
    private void CollectSimpleFonts(PdfDocument doc, IEnumerable<PdfDictionary> fonts)
    {
        foreach (var fontDict in fonts)
        {
            if (fontDict.Get("ToUnicode") is not null)
            {
                continue;
            }

            // A font with an explicit encoding decodes through the standard
            // encoding path and needs no fallback CMap.
            if (fontDict.Get("Encoding") is { } enc &&
                (enc.AsName() is not null || enc.AsDictionary() is not null || enc.AsReference() is not null))
            {
                continue;
            }

            var subtype = fontDict.Get("Subtype")?.AsName();
            if (subtype is null || subtype == "Type0")
            {
                continue;
            }

            var fontFileRef = CMapBuilders.GetFontFileReference(CMapBuilders.GetFontDescriptor(doc, fontDict));
            if (fontFileRef is null)
            {
                continue;
            }

            var lookupKey = fontFileRef.Value.Number;
            if (_byObjNum.ContainsKey(lookupKey))
            {
                continue;
            }

            if (doc.GetObject(fontFileRef.Value)?.AsStream()?.DecompressedContent() is not { } data)
            {
                continue;
            }

            if (CMapBuilders.SimpleFromTrueType(data) is { } cmap)
            {
                Log.Debug(Module, () =>
                    $"Simple font cmap obj={lookupKey,-6} (embedded font) char_map={cmap.CharMap.Count}");
                _byObjNum[lookupKey] = new CMapEntry { Primary = cmap };
            }
        }
    }

    /// <summary>Walks the Form XObjects a page references and collects their fonts too.</summary>
    private void CollectFromXObjects(PdfDocument doc, PdfDictionary page)
    {
        var visited = new HashSet<PdfObjectId>();
        foreach (var resources in doc.GetPageResources(page))
        {
            WalkXObjectFonts(doc, resources, visited, 0);
        }
    }

    private void WalkXObjectFonts(PdfDocument doc, PdfDictionary resources, HashSet<PdfObjectId> visited, int depth)
    {
        if (depth > 16)
        {
            return;
        }

        var xobjects = doc.GetDeref(resources, "XObject")?.AsDictionary();
        if (xobjects is null)
        {
            return;
        }

        foreach (var (_, value) in xobjects)
        {
            if (value.AsReference() is not { } id || !visited.Add(id))
            {
                continue;
            }

            if (doc.GetObject(id)?.AsStream() is not { } stream)
            {
                continue;
            }

            if (stream.Dictionary.Get("Subtype")?.AsName() != "Form")
            {
                continue;
            }

            var formResources = doc.GetDeref(stream.Dictionary, "Resources")?.AsDictionary();
            if (formResources is null)
            {
                continue;
            }

            if (doc.GetDeref(formResources, "Font")?.AsDictionary() is { } fontDict)
            {
                var fonts = new SortedDictionary<string, PdfDictionary>(StringComparer.Ordinal);
                foreach (var (name, entry) in fontDict)
                {
                    if (doc.Resolve(entry).AsDictionary() is { } font)
                    {
                        fonts[name] = font;
                    }
                }

                CollectToUnicodeStreams(doc, fonts.Values, skipTrueTypeFallback: false);
                CollectIdentityFonts(doc, fonts.Values);
                CollectSimpleFonts(doc, fonts.Values);
            }

            WalkXObjectFonts(doc, formResources, visited, depth + 1);
        }
    }
}
