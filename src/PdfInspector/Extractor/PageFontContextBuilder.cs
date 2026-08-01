// Ported from reference/src/extractor/content_stream.rs and xobjects.rs
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;

namespace PdfInspector.Extractor;

/// <summary>
/// Assembles everything the decoder needs about a set of fonts: widths,
/// encodings, CMap references, base names, and descriptor style flags. Page
/// content streams and Form XObjects both build one of these over their own
/// resource dictionaries.
/// </summary>
internal static class PageFontContextBuilder
{
    /// <summary>The context plus the per-font metadata the operator loop reads directly.</summary>
    public sealed class Result
    {
        public required PageFontContext Context { get; init; }

        /// <summary>BaseFont name by resource name.</summary>
        public Dictionary<string, string> BaseNames { get; } = [];

        /// <summary>Italic and bold flags derived from the font descriptor, by resource name.</summary>
        public Dictionary<string, (bool Italic, bool Bold)> StyleFlags { get; } = [];

        /// <summary>True when a font uses glyph-id names no CMap resolves.</summary>
        public bool HasGidFonts { get; init; }
    }

    /// <param name="cmapDecisions">
    /// Pass the page's cache when building a Form XObject's context, so the
    /// primary/remapped verdict for a font is shared rather than re-derived.
    /// </param>
    public static Result Build(
        PdfDocument doc,
        IReadOnlyDictionary<string, PdfDictionary> fonts,
        FontCMaps documentCMaps,
        FontStyleCache styleCache,
        CMapDecisionCache? cmapDecisions = null)
    {
        var (encodings, hasGidFonts) = FontEncodings.BuildPageFontEncodings(doc, fonts, documentCMaps);
        var widths = FontWidths.BuildPageFontWidths(doc, fonts);

        var context = new PageFontContext
        {
            DocumentCMaps = documentCMaps,
            Encodings = encodings,
            Widths = widths,
            CMapDecisions = cmapDecisions ?? new CMapDecisionCache(),
        };

        var result = new Result { Context = context, HasGidFonts = hasGidFonts };

        foreach (var (resourceName, fontDict) in fonts)
        {
            if (fontDict.Get("BaseFont")?.AsName() is { } baseName)
            {
                result.BaseNames[resourceName] = baseName;
            }

            // Descriptor style flags rescue subset fonts whose BaseFont names are
            // opaque tags the name heuristics cannot read.
            var style = FontStyles.DescriptorStyleFlags(doc, fontDict, styleCache);
            if (style != (false, false))
            {
                result.StyleFlags[resourceName] = style;
            }

            if (SimpleFontEncoding.Build(doc, fontDict) is { } simpleEncoding)
            {
                context.SimpleEncodings[resourceName] = simpleEncoding;
            }

            var toUnicode = fontDict.Get("ToUnicode");
            if (toUnicode is null)
            {
                // Identity-H/V fonts store their CMap under the font program's
                // object number instead.
                if (FontStyles.GetFontFileObjectNumber(doc, fontDict) is { } fileObjNum)
                {
                    context.ToUnicodeRefs[resourceName] = fileObjNum;
                }

                continue;
            }

            if (toUnicode.AsReference() is { } objRef)
            {
                context.ToUnicodeRefs[resourceName] = objRef.Number;
            }
            else if (toUnicode.AsStream() is { } stream)
            {
                // An inline ToUnicode stream has no object number to key on, so
                // its CMap is built here and carried with the page.
                var data = stream.DecompressedContent() ?? stream.RawData;
                if (BuildCMapEntryFromStream(data, fontDict, doc) is { } entry)
                {
                    context.InlineCMaps[resourceName] = entry;
                }
            }
        }

        return result;
    }

    /// <summary>
    /// Builds a CMap entry from a ToUnicode stream that was written inline
    /// rather than as an indirect object.
    /// </summary>
    public static CMapEntry? BuildCMapEntryFromStream(byte[] data, PdfDictionary fontDict, PdfDocument doc)
    {
        var parsed = ToUnicodeCMap.Parse(data);

        if (parsed is null)
        {
            var recovered = CMapBuilders.FallbackForType0(doc, fontDict)
                ?? CMapBuilders.FallbackForSimple(doc, fontDict);

            return recovered is null ? null : new CMapEntry { Primary = recovered };
        }

        var (primary, remapped) = CMapBuilders.TryRemapSubsetCMap(parsed, fontDict, doc, 0);
        var fallback = CMapBuilders.FallbackFromEncoding(doc, fontDict)
            ?? CMapBuilders.FallbackForType0(doc, fontDict)
            ?? CMapBuilders.FallbackForSimple(doc, fontDict);

        var primaryEntries = primary.EntryCount;
        if (primaryEntries < 10 && fallback is not null)
        {
            remapped = primary;
            primary = fallback;
            fallback = null;
        }

        // When a sequential remap was applied and the fallback has more entries
        // than the primary CMap, prefer the fallback: subset fonts number glyphs
        // by document encounter order, so the sorted sequential remap scrambles
        // characters, while an embedded cmap table is authoritative.
        if (remapped is not null && fallback is { } fb)
        {
            if (fb.EntryCount > primaryEntries)
            {
                (remapped, fallback) = (fb, remapped);
            }
        }

        return new CMapEntry { Primary = primary, Remapped = remapped, Fallback = fallback };
    }
}
