// Ported from reference/src/extractor/fonts.rs
using System.Text;
using PdfInspector.Fonts;
using PdfInspector.Pdf;
using PdfInspector.Text;

namespace PdfInspector.Extractor;

/// <summary>
/// Document-scoped memo of embedded-font style flags, keyed by the font
/// program's object id. The same program is referenced from every page that
/// uses the font, and decompressing and parsing it dominates
/// <see cref="FontStyles.DescriptorStyleFlags"/> — without the memo that cost
/// repeats per page whenever the descriptor leaves a flag unset, which is the
/// common case since regular fonts report neither italic nor bold.
/// </summary>
internal sealed class FontStyleCache
{
    public Dictionary<PdfObjectId, (bool Italic, bool Bold)> ByFontFile { get; } = [];
}

/// <summary>Reads bold and italic style from font descriptors and embedded programs.</summary>
internal static class FontStyles
{
    /// <summary>
    /// Style flags from the FontDescriptor, which survive subset fonts whose
    /// BaseFont names are opaque tags ("Tc1", "ABCDEF+F1") that defeat the
    /// name-based heuristics.
    ///
    /// Italic comes from an ItalicAngle beyond a few degrees or the Italic flag
    /// bit; bold from the ForceBold flag bit. The angle threshold skips fonts
    /// that declare a token slant.
    /// </summary>
    public static (bool Italic, bool Bold) DescriptorStyleFlags(
        PdfDocument doc,
        PdfDictionary fontDict,
        FontStyleCache styleCache)
    {
        var descriptor = doc.GetDict(fontDict, "FontDescriptor");

        if (descriptor is null)
        {
            // Type0 fonts hang the descriptor off DescendantFonts[0].
            var descFonts = doc.GetArray(fontDict, "DescendantFonts");
            if (descFonts is { Count: > 0 } && doc.Resolve(descFonts[0]).AsDictionary() is { } cidFontDict)
            {
                descriptor = doc.GetDict(cidFontDict, "FontDescriptor");
            }
        }

        if (descriptor is null)
        {
            return (false, false);
        }

        var italicAngle = (float)(descriptor.Get("ItalicAngle")?.AsNumber() ?? 0.0);
        var flags = descriptor.Get("Flags")?.AsInteger() ?? 0;

        var italic = MathF.Abs(italicAngle) >= 4.0f || (flags & (1 << 6)) != 0;
        var bold = (flags & (1 << 18)) != 0;

        // Descriptors lie: subset generators write ItalicAngle 0 for genuinely
        // italic faces. The embedded program keeps the truth.
        if (!italic || !bold)
        {
            if (FontFileReference(descriptor) is { } ffRef)
            {
                if (!styleCache.ByFontFile.TryGetValue(ffRef, out var embedded))
                {
                    embedded = EmbeddedStyleFlags(doc, ffRef);
                    styleCache.ByFontFile[ffRef] = embedded;
                }

                italic = italic || embedded.Italic;
                bold = bold || embedded.Bold;
            }
        }

        return (italic, bold);
    }

    private static (bool Italic, bool Bold) EmbeddedStyleFlags(PdfDocument doc, PdfObjectId ffRef)
    {
        var data = FontFileData(doc, ffRef);
        if (data is null)
        {
            return (false, false);
        }

        if (TrueTypeFace.Parse(data) is { } face)
        {
            return (face.IsItalic || MathF.Abs(face.ItalicAngle) >= 4.0f, face.IsBold);
        }

        // FontFile3 is bare CFF with no sfnt container, so the face parser
        // cannot open it — but the CFF Name INDEX keeps the real PostScript
        // name ("XXXXXX+Amplitude-LightItalic") even when the descriptor was
        // rewritten to claim upright.
        if (CffFontName(data) is { } name)
        {
            return (TextUtils.IsItalicFont(name), TextUtils.IsBoldFont(name));
        }

        return (false, false);
    }

    /// <summary>The first PostScript name from a bare CFF font's Name INDEX (CFF spec §7).</summary>
    internal static string? CffFontName(byte[] data)
    {
        // Header: major, minor, hdrSize, offSize. Major must be 1.
        if (data.Length < 4 || data[0] != 1)
        {
            return null;
        }

        var hdrSize = data[2];
        if (hdrSize + 3 > data.Length)
        {
            return null;
        }

        // Name INDEX: count (u16), offSize (u8), offsets[count+1], then data.
        var count = (data[hdrSize] << 8) | data[hdrSize + 1];
        if (count == 0)
        {
            return null;
        }

        var offSize = data[hdrSize + 2];
        if (offSize is < 1 or > 4)
        {
            return null;
        }

        int? ReadOffset(int index)
        {
            var at = hdrSize + 3 + (index * offSize);
            if (at + offSize > data.Length)
            {
                return null;
            }

            var value = 0;
            for (var i = 0; i < offSize; i++)
            {
                value = (value << 8) | data[at + i];
            }

            return value;
        }

        if (ReadOffset(0) is not { } start || ReadOffset(1) is not { } end)
        {
            return null;
        }

        if (start == 0 || end < start)
        {
            return null;
        }

        // Offsets are 1-based from the byte before the object data.
        var objectsBase = hdrSize + 3 + ((count + 1) * offSize) - 1;
        if (objectsBase + end > data.Length || objectsBase + start > data.Length)
        {
            return null;
        }

        return Encoding.UTF8.GetString(data, objectsBase + start, end - start);
    }

    /// <summary>The FontFile2 or FontFile3 stream reference from a FontDescriptor.</summary>
    private static PdfObjectId? FontFileReference(PdfDictionary descriptor) =>
        descriptor.Get("FontFile2")?.AsReference() ?? descriptor.Get("FontFile3")?.AsReference();

    private static byte[]? FontFileData(PdfDocument doc, PdfObjectId ffRef)
    {
        if (doc.GetObject(ffRef)?.AsStream() is not { } stream)
        {
            return null;
        }

        return stream.DecompressedContent() ?? stream.RawData;
    }

    /// <summary>
    /// The object number under which a font's CMap is stored: the embedded font
    /// program when there is one, otherwise the CIDFont dictionary for a
    /// predefined character collection. Must agree with how
    /// <see cref="ToUnicode.FontCMaps"/> keys its entries.
    /// </summary>
    public static int? GetFontFileObjectNumber(PdfDocument doc, PdfDictionary fontDict)
    {
        var subtype = fontDict.Get("Subtype")?.AsName();

        if (subtype == "Type0")
        {
            var encoding = fontDict.Get("Encoding")?.AsName();
            if (encoding is not ("Identity-H" or "Identity-V"))
            {
                return null;
            }

            var descFonts = doc.GetArray(fontDict, "DescendantFonts");
            if (descFonts is null || descFonts.Count == 0)
            {
                return null;
            }

            var cidFontDict = doc.Resolve(descFonts[0]).AsDictionary();
            if (cidFontDict is null)
            {
                return null;
            }

            var descriptor = doc.GetDict(cidFontDict, "FontDescriptor");
            if (descriptor is not null && FontFileReference(descriptor) is { } ffRef)
            {
                return ffRef.Number;
            }

            // Predefined CIDSystemInfo mappings are keyed by the descendant font.
            return descFonts[0].AsReference()?.Number;
        }

        var simpleDescriptor = doc.GetDict(fontDict, "FontDescriptor");
        return simpleDescriptor is null ? null : FontFileReference(simpleDescriptor)?.Number;
    }
}
