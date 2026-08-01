namespace PdfInspector.Pdf;

public sealed partial class PdfDocument
{
    /// <summary>
    /// Collects a page's resource dictionaries: the inline <c>/Resources</c> from
    /// the page and each ancestor, nearest first.
    /// </summary>
    public List<PdfDictionary> GetPageResources(PdfDictionary page)
    {
        var result = new List<PdfDictionary>();
        var current = page;

        for (var depth = 0; depth < 64 && current is not null; depth++)
        {
            if (GetDeref(current, "Resources")?.AsDictionary() is { } resources)
            {
                result.Add(resources);
            }

            current = GetDict(current, "Parent");
        }

        return result;
    }

    /// <summary>
    /// The fonts visible to a page, keyed by resource name. Nearer resource
    /// dictionaries shadow inherited ones.
    /// </summary>
    public SortedDictionary<string, PdfDictionary> GetPageFonts(PdfDictionary page)
    {
        var fonts = new SortedDictionary<string, PdfDictionary>(StringComparer.Ordinal);

        foreach (var resources in GetPageResources(page))
        {
            var fontDict = GetDeref(resources, "Font")?.AsDictionary();
            if (fontDict is null)
            {
                continue;
            }

            foreach (var (name, value) in fontDict)
            {
                if (fonts.ContainsKey(name))
                {
                    continue;
                }

                if (Resolve(value).AsDictionary() is { } font)
                {
                    fonts[name] = font;
                }
            }
        }

        return fonts;
    }

    /// <summary>
    /// The object id a font's dictionary was reached through, used as the cache
    /// key for CMaps. Returns null for fonts written inline.
    /// </summary>
    public PdfObjectId? GetFontReference(PdfDictionary page, string fontName)
    {
        foreach (var resources in GetPageResources(page))
        {
            var fontDict = GetDeref(resources, "Font")?.AsDictionary();
            if (fontDict?.Get(fontName)?.AsReference() is { } id)
            {
                return id;
            }
        }

        return null;
    }
}
